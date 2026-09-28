//! 清单文本的**定点编辑**（§5 的 `scan` 与 §9.8 的 `add`/`remove`/`upgrade` 共用）。
//!
//! 清单是给人读的文件：注释、条目间距、缩进都属于用户的排版。所以这里的每个原语都只做
//! **文本级**变换，从不「解析成结构再序列化」——后者会把注释和排版整个抹掉。
//!
//! 三条不变式：
//!
//! - **已有字节一个不动**：插入只做拼接；删除只切条目自己的行；改字段只重写那一行。
//! - **不给用户做决定**：段头带内联值（`skills: {}`）时拒绝改写而不是替他展开。
//! - **缩进跟着已有内容走**：用户用 4 空格，新条目也 4 空格。
//!
//! 本模块**不碰盘**：变换出来的是 `String`，落盘由调用方走
//! `crate::apply::write::write_config`（原子写 + 单槽 `.bak`）。

use crate::capability::Kind;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// 清单的三个可收养段。段的顺序就是渲染顺序，也是 `section.as_str()` 的拼写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Skills,
    Mcps,
    Plugins,
}

impl Section {
    pub fn as_str(&self) -> &'static str {
        match self {
            Section::Skills => "skills",
            Section::Mcps => "mcps",
            Section::Plugins => "plugins",
        }
    }

    /// `Kind` → 它落在哪个段；表达不了的 kind 返回 `None`（§4 的 schema 只有三段）。
    pub fn of(kind: Kind) -> Option<Section> {
        match kind {
            Kind::Skill => Some(Section::Skills),
            Kind::Mcp => Some(Section::Mcps),
            Kind::Plugin => Some(Section::Plugins),
            Kind::Command | Kind::Agent | Kind::Hook => None,
        }
    }
}

/// 一条要写进清单的条目。
///
/// `body` 是条目**字段行**的 YAML 文本：每行 2 空格缩进（相对条目头，往下一级）、以 `\n`
/// 结尾，**不含** `name:` 那一行。缩进是**相对**的：清单里已有的段用几个空格缩进，插入时
/// 就补几个，这样我们不会把用户的排版改掉。
#[derive(Debug, Clone, Copy)]
pub struct EntryRef<'a> {
    pub section: Section,
    pub name: &'a str,
    pub body: &'a str,
}

/// 把条目插进清单文本的对应段，**已有字节一个不动**（只做拼接，从不重写既有的行）。
/// 返回 `None` = 没有条目可插（空输入是空操作）。
///
/// 段定位：只认**列 0** 的顶层键行（`^[A-Za-z0-9_-]+:`，`#` 注释与空行不算）。段的范围
/// 到下一个顶层键行为止；新条目接在该段的**最后一个非空非注释行之后**。段不存在就在文件
/// 末尾新建。段头带了内联值（`mcps: {}` / `mcps: [a]`）时**拒绝**——把一个 flow 形态展开成
/// block 而不动用户的其他字节做不到，宁可让他自己改一行。
///
/// 缩进跟着**已有段的第一行内容**走：用户的清单用 4 空格缩进条目，新条目也是 4 空格。
pub fn insert_entries(text: &str, entries: &[EntryRef<'_>]) -> anyhow::Result<Option<String>> {
    if entries.is_empty() {
        return Ok(None);
    }
    let mut by_section: BTreeMap<Section, Vec<&EntryRef<'_>>> = BTreeMap::new();
    for e in entries {
        by_section.entry(e.section).or_default().push(e);
    }
    let mut out = text.to_string();
    // 逐段处理，每次都基于「当前文本」重新定位：前一次插入挪动了偏移，重扫一遍最省心
    // （清单是几百行的量级，不值得为它上一套偏移账本）。
    for (section, group) in by_section {
        out = insert_section(&out, section, &group)?;
    }
    Ok(Some(out))
}

/// 删掉一条条目，返回新文本。`None` = 清单里没有这条（调用方决定这是不是错误）。
///
/// 条目的范围从它的头行到**下一个同级或更浅的条目头**（或段尾）为止：条目自己的字段行、
/// 内联数组的续行、块标量的续行都跟着走。条目后面的空行一并带走，避免留下连续空行。
pub fn remove_entry(text: &str, section: Section, name: &str) -> anyhow::Result<Option<String>> {
    let lines = line_ranges(text);
    let at = |i: usize| -> &str { &text[lines[i].0..lines[i].1] };
    let (header, block_end) = section_bounds(text, &lines, section)?;
    let Some(header) = header else {
        return Ok(None);
    };
    let Some((start, end)) = find_entry(at, header, block_end, name) else {
        return Ok(None);
    };
    // 条目与下一段之间的空行留给清单：全删掉会让两个段贴在一起，虽然还能解析，
    // 但那是排版上的倒退——用户写的空行不属于这条条目。
    let mut end = end;
    while end > start + 1 && at(end - 1).trim().is_empty() {
        end -= 1;
    }
    let from = lines[start].0;
    let to = lines.get(end).map(|(s, _)| *s).unwrap_or(text.len());
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..from]);
    out.push_str(&text[to..]);
    Ok(Some(out))
}

/// 改一个条目里某个字段的值，返回新文本。`None` = 没有这条条目，或它没有这个字段。
///
/// 只重写那一行的值，缩进原样保留；行尾注释（`ref: v1  # 锁`）也保留——它同样是用户写的。
pub fn set_entry_field(
    text: &str,
    section: Section,
    name: &str,
    field: &str,
    value: &str,
) -> anyhow::Result<Option<String>> {
    let lines = line_ranges(text);
    let at = |i: usize| -> &str { &text[lines[i].0..lines[i].1] };
    let (header, block_end) = section_bounds(text, &lines, section)?;
    let Some(header) = header else {
        return Ok(None);
    };
    let Some((start, end)) = find_entry(at, header, block_end, name) else {
        return Ok(None);
    };
    for (from, to) in &lines[start + 1..end] {
        let (from, to) = (*from, *to);
        let line = &text[from..to];
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent <= indent_of(at(start)) {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix(field) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let comment = trailing_comment(rest);
        let mut out = String::with_capacity(text.len() + value.len());
        out.push_str(&text[..from]);
        out.push_str(&line[..indent]);
        let _ = write!(out, "{field}: {}", scalar(value));
        if let Some(c) = comment {
            out.push_str(c);
        }
        out.push_str(&text[to..]);
        return Ok(Some(out));
    }
    Ok(None)
}

/// YAML 标量渲染。安全就裸写，否则双引号包住并转义——**规则宁严勿宽**：多引一对引号
/// 只是不好看，少引一对可能让清单解不开或解成别的值。
///
/// 判「安全」的条件：非空、首字符是字母或 `_`（数字开头会被 YAML 读成整数）、
/// 其余字符属于 `[A-Za-z0-9_.\-/]`、且不是 `true/false/null/yes/no/on/off` 这类
/// 会被隐式解析成别的东西的词。
pub fn scalar(s: &str) -> String {
    let mut chars = s.chars();
    let plain = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        }
        _ => false,
    } && !matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off"
    );
    if plain {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// 一行的范围（`start` 到 `end`，`end` 不含换行符）。
fn line_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        out.push((at, at + body.len()));
        at += line.len();
    }
    out
}

/// 列 0 的顶层键行 → 键名。缩进行、注释行、空行、以及不是 `key:` 形状的行都不是。
fn top_level_key(line: &str) -> Option<&str> {
    if line.is_empty() || line.starts_with([' ', '\t', '#']) {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(key)
}

/// 段内容里「非空」的行。**注释也算**：它属于这一段，新条目该排在它下面。
fn is_nonblank(line: &str) -> bool {
    !line.trim().is_empty()
}

/// 段缩进只看**非空非注释**的行：注释的缩进是排版的自由，不能拿来当基准。
fn is_indented_content(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with('#')
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// 段头行号与段尾行号（`end` 不含）。段头带内联值时拒绝——展开它必然要动用户的字节。
fn section_bounds(
    text: &str,
    lines: &[(usize, usize)],
    section: Section,
) -> anyhow::Result<(Option<usize>, usize)> {
    let name = section.as_str();
    let mut header: Option<usize> = None;
    let mut block_end = lines.len();
    for (i, (start, end)) in lines.iter().enumerate() {
        let line = &text[*start..*end];
        let Some(key) = top_level_key(line) else {
            continue;
        };
        match header {
            None => {
                if key == name {
                    if line.trim_end() != format!("{name}:") {
                        anyhow::bail!(
                            "`{name}:` 带了内联值（`{}`）；不会改写用户的排版，\
                             请把它改成 block 形态（单独一行 `{name}:`）再来",
                            line.trim()
                        );
                    }
                    header = Some(i);
                }
            }
            Some(_) => {
                if key != name {
                    block_end = i;
                    break;
                }
            }
        }
    }
    Ok((header, block_end))
}

/// 段内找条目：返回 (头行, 尾行)。头行是缩进的 `<name>:` 行；尾行是下一个同级或更浅的
/// 内容行（或段尾）——条目自己的字段行与各种续行都落在两者之间。
fn find_entry<'a>(
    at: impl Fn(usize) -> &'a str,
    header: usize,
    block_end: usize,
    name: &str,
) -> Option<(usize, usize)> {
    let mut start: Option<(usize, usize)> = None; // (行号, 缩进)
    for i in header + 1..block_end {
        let line = at(i);
        if !is_indented_content(line) {
            continue;
        }
        let indent = indent_of(line);
        match start {
            None => {
                if indent > 0 && entry_key(line).is_some_and(|k| k == name) {
                    start = Some((i, indent));
                }
            }
            Some((begin, base)) => {
                if indent <= base {
                    return Some((begin, i));
                }
            }
        }
    }
    start.map(|(begin, _)| (begin, block_end))
}

/// 缩进行 → 条目键名。`"weird name":` 这种带引号的写法也认。
fn entry_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let (key, _) = trimmed.split_once(':')?;
    let key = key.trim();
    let key = key
        .strip_prefix('"')
        .and_then(|k| k.strip_suffix('"'))
        .unwrap_or(key);
    if key.is_empty() {
        return None;
    }
    Some(key)
}

/// 行尾注释：返回 `#` 与它前面的空白（`#` 前必须有空白，且这一行里还没出现过引号——
/// values 里的 `#` 不构成注释）；找不到返回 `None`。空白一并带走，是为了原样还原用户
/// 的排版（`ref: v1  # 锁` 里的两个空格不该被压成一个）。
fn trailing_comment(rest: &str) -> Option<&str> {
    let mut quoted = false;
    let bytes = rest.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            b'#' if !quoted && i > 0 && bytes[i - 1].is_ascii_whitespace() => {
                let mut start = i;
                while start > 0 && bytes[start - 1].is_ascii_whitespace() {
                    start -= 1;
                }
                return Some(&rest[start..]);
            }
            _ => {}
        }
    }
    None
}

fn insert_section(
    text: &str,
    section: Section,
    entries: &[&EntryRef<'_>],
) -> anyhow::Result<String> {
    let name = section.as_str();
    let lines = line_ranges(text);
    let at = |i: usize| -> &str { &text[lines[i].0..lines[i].1] };
    let (header, block_end) = section_bounds(text, &lines, section)?;

    // 缩进与插入点：段内第一个有意义的行的缩进，以及最后一个有意义行之后。
    let Some(header) = header else {
        return Ok(append_section(text, name, &render_entries(entries, 2)));
    };
    let nonblank: Vec<usize> = (header + 1..block_end)
        .filter(|i| is_nonblank(at(*i)))
        .collect();
    let indent = nonblank
        .iter()
        .find(|i| is_indented_content(at(**i)))
        .map(|i| indent_of(at(*i)))
        .unwrap_or(2);
    let after = nonblank.last().map(|i| i + 1).unwrap_or(header + 1);
    let insert_at = lines.get(after).map(|(s, _)| *s).unwrap_or(text.len());

    let mut out = String::with_capacity(text.len() + 128);
    out.push_str(&text[..insert_at]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&render_entries(entries, indent));
    out.push_str(&text[insert_at..]);
    Ok(out)
}

/// 条目文本：`<indent><name>:` + body。body 的 2 空格是**相对**缩进，这里补上段缩进——
/// 所以新条目的字段永远比条目头深一级（2 空格），无论用户的旧条目用的是 2 还是 4 空格
/// 一级：YAML 只要求子节点比父节点深，不要求兄弟节点的子节点对齐。
fn render_entries(entries: &[&EntryRef<'_>], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    for e in entries {
        let _ = writeln!(out, "{pad}{}:", scalar(e.name));
        for line in e.body.lines() {
            let _ = writeln!(out, "{pad}{line}");
        }
    }
    out
}

/// 段不存在 → 在文件末尾新建，与已有内容之间隔一个空行（好读，也不影响解析）。
fn append_section(text: &str, name: &str, rendered: &str) -> String {
    let mut out = String::with_capacity(text.len() + rendered.len() + 24);
    out.push_str(text);
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    out.push_str(name);
    out.push_str(":\n");
    out.push_str(rendered);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    const SAMPLE: &str = "\
# 顶部注释：清单说明
version: 1

skills:
  # 用户自己的备注
  alpha:
    source: local
    path: ~/a
  beta:
    source: git
    repo: a/b
    path: skills/beta
    ref: v1.0.0  # 锁在这里

mcps:
  ctx:
    command: npx
";

    #[test]
    fn removing_an_entry_keeps_every_other_byte() {
        let out = remove_entry(SAMPLE, Section::Skills, "alpha")
            .unwrap()
            .expect("alpha 存在");
        assert_eq!(
            out,
            "\
# 顶部注释：清单说明
version: 1

skills:
  # 用户自己的备注
  beta:
    source: git
    repo: a/b
    path: skills/beta
    ref: v1.0.0  # 锁在这里

mcps:
  ctx:
    command: npx
"
        );
        Manifest::parse(&out).expect("删完还要能解回来");
    }

    #[test]
    fn removing_the_last_entry_keeps_the_blank_line_before_the_next_section() {
        let out = remove_entry(SAMPLE, Section::Skills, "beta")
            .unwrap()
            .expect("beta 存在");
        assert!(out.contains("    path: ~/a\n\nmcps:\n"), "{out}");
        assert!(!out.contains("beta"), "{out}");
        Manifest::parse(&out).expect("删完还要能解回来");
    }

    #[test]
    fn removing_a_missing_entry_is_a_no_op() {
        assert_eq!(remove_entry(SAMPLE, Section::Skills, "nope").unwrap(), None);
        // 段不存在时也不是错误：清单里本来就没有这一节。
        assert_eq!(
            remove_entry("version: 1\n", Section::Skills, "a").unwrap(),
            None
        );
    }

    #[test]
    fn setting_a_field_keeps_the_indent_and_the_trailing_comment() {
        let out = set_entry_field(SAMPLE, Section::Skills, "beta", "ref", "v2.0.0")
            .unwrap()
            .expect("beta 有 ref");
        assert!(
            out.contains("    ref: v2.0.0  # 锁在这里\n"),
            "缩进与行尾注释都要留住：{out}"
        );
        assert!(out.contains("path: skills/beta"), "别的字段不许动：{out}");
        Manifest::parse(&out).expect("改完还要能解回来");
    }

    #[test]
    fn setting_a_missing_field_is_a_no_op() {
        assert_eq!(
            set_entry_field(SAMPLE, Section::Skills, "beta", "nope", "x").unwrap(),
            None
        );
        assert_eq!(
            set_entry_field(SAMPLE, Section::Skills, "missing", "ref", "x").unwrap(),
            None
        );
    }

    #[test]
    fn a_quoted_entry_name_is_still_found() {
        // `scalar` 会给怪名字加引号（数字开头、含空格等），条目定位要认这种写法。
        let text = "version: 1\nskills:\n  \"2fast\":\n    source: local\n    path: ~/a\n";
        let out = remove_entry(text, Section::Skills, "2fast")
            .unwrap()
            .expect("带引号的条目也要认");
        assert_eq!(out, "version: 1\nskills:\n");
    }

    #[test]
    fn a_deeper_field_does_not_end_the_entry() {
        // mcp 的 env 是嵌套表：条目的范围不能被更深一层的键截断。
        let text = "\
version: 1
mcps:
  ctx:
    command: npx
    env:
      A: b
  other:
    command: node
";
        let out = remove_entry(text, Section::Mcps, "ctx")
            .unwrap()
            .expect("ctx 存在");
        assert_eq!(out, "version: 1\nmcps:\n  other:\n    command: node\n");
    }
}
