//! `henv browse`：跨 harness 的**可装清单**视图（§9.8）。
//!
//! 它回答两个别处答不了的问题：
//!
//! 1. **有哪些可装**——把各家的 marketplace 摆到一张桌子上；
//! 2. **同一个名字在各家分别是什么状态**——装了没有、什么版本。
//!
//! 与 `status` 的分工：`status` 说「本机装了什么」，`browse` 说「**还能装什么**，以及你装了
//! 哪些」。两者都只读、都不看清单。
//!
//! **范围限于 plugin**：只有 plugin 有「市场」这个概念。skill 与 MCP 没有登记「有哪些可
//! 下载」的地方，所以对它们不存在「推荐」——skill 的发现走 `henv show <repo>`。
//!
//! **读的是各家的内部文件**（claude 的 `.claude-plugin/marketplace.json`、codex 的
//! `api_marketplace.json`）。这是本模块最脆的地方，所以每一处读取都**降级而不失败**：
//! 读不出某一家就记一条 note 继续走其余部分，绝不让一个格式变动把整个命令打掉。

use crate::adapters::{CLAUDE_CODE, CODEX};
use crate::capability::{Kind, Spec};
use crate::home::Home;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// marketplace 里的一条可装 plugin。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// 这个市场属于哪一家。
    pub harness: &'static str,
    /// marketplace 自己的名字（claude 有多个，codex 一个）。
    pub marketplace: String,
    pub name: String,
    /// 一句话说明。claude 的清单带 `description`，codex 的只有 `category`——取到哪个用哪个。
    pub blurb: String,
}

/// 同一个名字的各家状态：市场在哪提供它、以及本机各家装没装。
#[derive(Debug, Default)]
struct Merged {
    markets: Vec<Listing>,
    installed: Vec<(String, Option<String>)>,
}

/// 列出可装与已装的对照。`harness` 限定只看哪一家的市场，`pattern` 按名字做子串匹配。
pub fn run_browse(
    home: &Home,
    out: &mut dyn Write,
    harness: Option<&str>,
    pattern: Option<&str>,
) -> anyhow::Result<()> {
    let (listings, notes) = read_markets(home, harness);
    let mut merged: BTreeMap<String, Merged> = BTreeMap::new();
    for l in listings {
        merged.entry(l.name.clone()).or_default().markets.push(l);
    }
    // 已装的那些：**本机的 plugin 记录**。它们未必出现在任何市场里（kimi 的 zip 装法、
    // 手动放的都算），所以这一趟是独立的，不能只按市场里的名字去找。
    for state in crate::read_states(home, harness)? {
        if !state.detected {
            continue;
        }
        for cap in &state.actual {
            if cap.id.kind != Kind::Plugin {
                continue;
            }
            let version = match &cap.spec {
                Spec::Plugin { version, .. } => version.clone(),
                _ => None,
            };
            merged
                .entry(cap.id.name.clone())
                .or_default()
                .installed
                .push((state.name.clone(), version));
        }
    }

    let keep: Vec<(&String, &Merged)> = merged
        .iter()
        .filter(|(name, _)| pattern.is_none_or(|p| name.contains(p)))
        .collect();

    if keep.is_empty() {
        let _ = writeln!(out, "没有匹配的 plugin。");
    } else if pattern.is_some() {
        render_detail(out, &keep);
    } else {
        render_list(out, &keep);
    }

    // 降级说明放在最后：它们是「这份清单不完整」的注脚，不该挡在正文前面。
    for note in &notes {
        let _ = writeln!(out, "(note) {note}");
    }
    Ok(())
}

/// 名字匹配时给细节：市场在哪、三家分别装了什么。
fn render_detail(out: &mut dyn Write, keep: &[(&String, &Merged)]) {
    for (i, (name, m)) in keep.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        let _ = writeln!(out, "{name}");
        let markets: Vec<String> = m
            .markets
            .iter()
            .map(|l| format!("{} ({})", l.marketplace, l.harness))
            .collect();
        let _ = writeln!(
            out,
            "  markets    {}",
            if markets.is_empty() {
                "（没有市场提供它——本机装的是原生/手工来源）".to_string()
            } else {
                markets.join(" · ")
            }
        );
        let installed: Vec<String> = m
            .installed
            .iter()
            .map(|(h, v)| match v {
                Some(v) => format!("{h} {v}"),
                None => format!("{h}（版本读不出）"),
            })
            .collect();
        let _ = writeln!(
            out,
            "  installed  {}",
            if installed.is_empty() {
                "未安装".to_string()
            } else {
                installed.join(" · ")
            }
        );
        if let Some(blurb) = m
            .markets
            .first()
            .map(|l| l.blurb.as_str())
            .filter(|b| !b.is_empty())
        {
            let _ = writeln!(out, "  about      {}", trim(blurb, 88));
        }
    }
}

/// 全量/按家时给列表：一行一条，`●` 表示至少有一家装了。
fn render_list(out: &mut dyn Write, keep: &[(&String, &Merged)]) {
    let width = keep
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    for (name, m) in keep {
        let mark = if m.installed.is_empty() { "○" } else { "●" };
        let markets: Vec<&str> = m.markets.iter().map(|l| l.marketplace.as_str()).collect();
        let where_ = if markets.is_empty() {
            "原生/手工来源".to_string()
        } else {
            markets.join(", ")
        };
        let _ = writeln!(out, "{mark} {name:<width$}  {where_}");
    }
    let _ = writeln!(out, "\n● = 本机已装（至少一家），○ = 未装");
}

/// 一句描述截断到 `max` 个字符——`browse` 的列表模式要能一屏扫过去。
fn trim(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let head: String = one_line.chars().take(max).collect();
    format!("{head}…")
}

/// 读各家的 marketplace。返回 `(可装清单, 降级说明)`。
fn read_markets(home: &Home, only: Option<&str>) -> (Vec<Listing>, Vec<String>) {
    let mut listings = Vec::new();
    let mut notes = Vec::new();
    if only.is_none_or(|h| h == CLAUDE_CODE) {
        read_claude(home, &mut listings, &mut notes);
    }
    if only.is_none_or(|h| h == CODEX) {
        read_codex(home, &mut listings, &mut notes);
    }
    // kimi 没有本地市场清单：它的 plugin 从远端 zip 取（`installed.json` 的 `originalSource`
    // 记着），可装清单不在盘上。**说清楚**，而不是显示成「这家什么都没有」。
    if only == Some(crate::adapters::KIMI_CODE) {
        notes.push(
            "kimi-code 没有本地市场清单：它的 plugin 从远端 zip 装，可装清单读不到".to_string(),
        );
    }
    (listings, notes)
}

/// claude 每个 marketplace 一个 JSON：`plugins/marketplaces/<name>/.claude-plugin/marketplace.json`。
fn read_claude(home: &Home, out: &mut Vec<Listing>, notes: &mut Vec<String>) {
    let dir = home.claude_dir().join("plugins").join("marketplaces");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        notes.push(format!(
            "读不到 claude 的 marketplace 目录：{}",
            dir.display()
        ));
        return;
    };
    for entry in entries.flatten() {
        let file = entry.path().join(".claude-plugin").join("marketplace.json");
        if !file.is_file() {
            continue;
        }
        match parse_market(&file, CLAUDE_CODE) {
            Ok(mut ls) => out.append(&mut ls),
            Err(err) => notes.push(format!("跳过 {}：{err:#}", file.display())),
        }
    }
}

/// codex 的清单在它的插件树下（`.tmp/plugins/...`）——那是它的内部位置，可能被刷新重建，
/// 所以读不到只是降级。
fn read_codex(home: &Home, out: &mut Vec<Listing>, notes: &mut Vec<String>) {
    let file = home
        .codex_dir()
        .join(".tmp")
        .join("plugins")
        .join(".agents")
        .join("plugins")
        .join("api_marketplace.json");
    if !file.is_file() {
        notes.push(format!("读不到 codex 的市场清单：{}", file.display()));
        return;
    }
    match parse_market(&file, CODEX) {
        Ok(mut ls) => out.append(&mut ls),
        Err(err) => notes.push(format!("跳过 {}：{err:#}", file.display())),
    }
}

/// 解析一个 marketplace 文件。**按字段取值而不是强类型反序列化**：这是别人的内部格式，
/// 多一个字段不该让整个命令失败，少一个字段也不该。
fn parse_market(file: &Path, harness: &'static str) -> anyhow::Result<Vec<Listing>> {
    let text = std::fs::read_to_string(file)?;
    let root: serde_json::Value = serde_json::from_str(&text)?;
    let marketplace = root
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("(unnamed)")
        .to_string();
    let mut out = Vec::new();
    for p in root
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = p.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        // claude 有 `description`，codex 只有 `category`——取到哪个用哪个。
        let blurb = p
            .get("description")
            .and_then(serde_json::Value::as_str)
            .or_else(|| p.get("category").and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .to_string();
        out.push(Listing {
            harness,
            marketplace: marketplace.clone(),
            name: name.to_string(),
            blurb,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    /// 造一个 claude 的 marketplace 文件。
    fn claude_market(t: &TempHome, market: &str, entries: &[(&str, &str)]) {
        let plugins: Vec<String> = entries
            .iter()
            .map(|(n, d)| format!(r#"{{"name":"{n}","description":"{d}"}}"#))
            .collect();
        t.write(
            &format!(".claude/plugins/marketplaces/{market}/.claude-plugin/marketplace.json"),
            &format!(r#"{{"name":"{market}","plugins":[{}]}}"#, plugins.join(",")),
        );
    }

    /// 造一个 codex 的市场清单（字段只有 category，没有 description——实测就是这样）。
    fn codex_market(t: &TempHome, entries: &[&str]) {
        let plugins: Vec<String> = entries
            .iter()
            .map(|n| format!(r#"{{"name":"{n}","category":"Tools"}}"#))
            .collect();
        t.write(
            ".codex/.tmp/plugins/.agents/plugins/api_marketplace.json",
            &format!(
                r#"{{"name":"openai-api-curated","plugins":[{}]}}"#,
                plugins.join(",")
            ),
        );
    }

    /// 让 claude 侧「已装」某个 plugin（走真实的 installed_plugins.json 路径）。
    fn claude_installed(t: &TempHome, entry: &str, version: &str) {
        let market = "claude-plugins-official";
        let root = t.path(&format!(".claude/plugins/cache/{market}/{entry}/{version}"));
        t.mkdir(&format!(".claude/plugins/cache/{market}/{entry}/{version}"));
        t.write(
            ".claude/plugins/installed_plugins.json",
            &format!(
                r#"{{"plugins":{{"{entry}@{market}":[{{"installPath":"{}","version":"{version}"}}]}}}}"#,
                root.display()
            ),
        );
        t.write(".claude/settings.json", r#"{"enabledPlugins":{}}"#);
    }

    fn browse(t: &TempHome, harness: Option<&str>, pattern: Option<&str>) -> String {
        let mut out = Vec::new();
        run_browse(&t.home, &mut out, harness, pattern).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// 同名条目要在两个市场之间合并，并标出各家装没装。
    #[test]
    fn the_same_name_is_merged_across_markets() {
        let t = TempHome::new();
        t.mkdir(".claude").mkdir(".codex");
        claude_market(
            &t,
            "claude-plugins-official",
            &[("superpowers", "teaches the workflow")],
        );
        codex_market(&t, &["superpowers", "sentry"]);
        claude_installed(&t, "superpowers", "6.4.1");

        let s = browse(&t, None, Some("superpowers"));

        assert!(s.contains("claude-plugins-official (claude-code)"), "{s}");
        assert!(s.contains("openai-api-curated (codex)"), "{s}");
        assert!(s.contains("claude-code 6.4.1"), "已装的版本要看得见：{s}");
        assert!(
            s.contains("teaches the workflow"),
            "描述来自 claude 那份：{s}"
        );
    }

    /// 位置参数写家名 → 只看那家的市场；这里顺带钉住「名字认得出是家名」的那条判据。
    #[test]
    fn a_harness_name_narrows_to_that_market() {
        let t = TempHome::new();
        t.mkdir(".claude").mkdir(".codex");
        claude_market(&t, "claude-plugins-official", &[("only-in-claude", "d")]);
        codex_market(&t, &["only-in-codex"]);

        let claude = browse(&t, Some(CLAUDE_CODE), None);
        assert!(claude.contains("only-in-claude"), "{claude}");
        assert!(!claude.contains("only-in-codex"), "{claude}");

        let codex = browse(&t, Some(CODEX), None);
        assert!(codex.contains("only-in-codex"), "{codex}");
        assert!(!codex.contains("only-in-claude"), "{codex}");
    }

    /// 没装过的条目在列表里标 `○`，装过的标 `●`。
    #[test]
    fn the_list_marks_installed_entries() {
        let t = TempHome::new();
        t.mkdir(".claude");
        claude_market(
            &t,
            "claude-plugins-official",
            &[("got-it", "d"), ("not-yet", "d")],
        );
        claude_installed(&t, "got-it", "1.0.0");

        let s = browse(&t, Some(CLAUDE_CODE), None);
        let got = s.lines().find(|l| l.contains("got-it")).unwrap();
        let not = s.lines().find(|l| l.contains("not-yet")).unwrap();
        assert!(got.starts_with('●'), "{got}");
        assert!(not.starts_with('○'), "{not}");
    }

    /// kimi 没有本地市场清单：要**说明原因**，而不是显示成「这家什么都没有」。
    #[test]
    fn kimi_says_why_it_has_no_market() {
        let t = TempHome::new();
        t.mkdir(".kimi-code");
        let s = browse(&t, Some(crate::adapters::KIMI_CODE), None);
        assert!(s.contains("没有本地市场清单"), "{s}");
    }

    /// 市场文件读不出（格式变了/被删了）只是降级：其余部分照常，并留下一条 note。
    #[test]
    fn an_unreadable_market_degrades_instead_of_failing() {
        let t = TempHome::new();
        t.mkdir(".claude").mkdir(".codex");
        claude_market(&t, "claude-plugins-official", &[("fine", "d")]);
        // 一个坏掉的市场文件。
        t.write(
            ".claude/plugins/marketplaces/broken/.claude-plugin/marketplace.json",
            "{not json",
        );
        codex_market(&t, &["also-fine"]);

        let s = browse(&t, None, None);
        assert!(s.contains("fine"), "{s}");
        assert!(s.contains("also-fine"), "{s}");
        assert!(s.contains("(note)"), "降级要有 note：{s}");
    }
}
