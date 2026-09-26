//! 写路径守卫：`src/` 下只有白名单目录（`src/apply/`、`src/store/`）允许出现文件写 API；
//! **外部进程调用另算**——它不按目录豁免，只按具体文件豁免（spec §9.9）。
//!
//! Plan A 的形态是「哪里都不许写」；Plan B 落地后收窄成白名单。收窄的方式是**加一层允许**，
//! 不是把模式从表里删掉：拆表之后，每一条模式仍然被某张表和某条测试看着，谁把表改松，
//! 对应那条反向钉子就红（`the_allowlist_is_not_the_whole_tree`、`the_process_allowlist_…`）。
//!
//! 两张表分开的理由见 `PROCESS_CALLS` 的注释：写文件与起进程不是一个量级的权限。

use std::path::{Path, PathBuf};

/// 唯一允许落盘的目录前缀（相对仓库根，带尾斜杠）。spec §9.9。
const WRITE_WHITELIST: &[&str] = &["src/apply/", "src/store/"];

/// 写 API：带 `(` 的模式避免误伤 `fs::symlink_metadata` 这类只读 API。
/// 只允许出现在 `WRITE_WHITELIST` 下面的目录里。
const WRITE_API: &[&str] = &[
    "fs::write(",
    "File::create(",
    "OpenOptions",
    "fs::remove_file(",
    "fs::remove_dir",
    "fs::create_dir",
    "fs::rename(",
    "fs::copy(",
    "fs::symlink(",
    "fs::hard_link(",
    "fs::set_permissions(",
];

/// 外部进程调用：**不按目录豁免**，只按具体文件豁免（§9.9）。
///
/// 为什么单独一张表：`src/store/` 允许写文件，而 shell out 比写文件强得多——它能做这个
/// 进程权限内的任何事。目录级豁免会把「这个目录可以写文件」悄悄升级成「这个目录可以起
/// 进程」。B2 的 git 与 B3 的 `claude mcp add-json` 是这个项目第一次真的调用外部程序，
/// 绊线必须收紧到文件名。
const PROCESS_CALLS: &[&str] = &["Command::new("];

/// 允许 shell out 的文件，白名单精确到路径全等。
///
/// - `src/store/git.rs`：git 源解析，整个模块的存在理由就是调 git（§9.12）。
/// - `src/testutil.rs`：测试夹具要造本地 git 仓库，但它只在 `cfg(test)` 下编译。
const PROCESS_ALLOWLIST: &[&str] = &["src/store/git.rs", "src/testutil.rs"];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
    out.sort();
}

/// 相对仓库根的路径，一律用 `/` 分隔——白名单按这个形态匹配。
fn rel_path(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_whitelisted(path: &Path) -> bool {
    let rel = rel_path(path);
    WRITE_WHITELIST.iter().any(|w| rel.starts_with(w))
}

/// 只留下**生产代码**的文本：`#[cfg(test)]` 门控的块整段去掉，注释行也去掉。
///
/// 为什么去掉测试块：`#[cfg(test)]` 代码不进产物二进制，测试夹具对 TempDir 的写不是生产
/// 写路径。逐行扫时按括号深度跟踪，只把生产代码留给调用方。
/// 为什么去掉注释行：文档与说明里提到这些 API 是正常的（本文件自己就写了满篇模式）。
fn strip_cfg_test_blocks(text: &str) -> String {
    let mut out = String::new();
    let mut test_depth = 0usize;
    let mut pending_cfg_test = false;
    for line in text.lines() {
        let opens = line.matches('{').count();
        let closes = line.matches('}').count();

        if test_depth > 0 {
            // 还在测试块内：只记账括号，不报写 API。saturating：未来某个 fixture 里多一个
            // 不配对的 `}` 时，`usize` 下溢在 debug 下 panic、在 release 下静默回绕成天文数字，
            // 于是扫描器把整个文件当成测试块——守卫就这样哑掉，而且失败方向是「静默放过」。
            test_depth = test_depth.saturating_add(opens).saturating_sub(closes);
            continue;
        }
        if line.trim() == "#[cfg(test)]" {
            pending_cfg_test = true;
            continue;
        }
        if pending_cfg_test {
            pending_cfg_test = false;
            // `#[cfg(test)] mod tests {` 才开门控块；`use …;` / `mod testutil;` 不产生块
            // （use 的花括号在同一行成对），这类行本身不含写 API，跳过即可。
            if opens > closes {
                test_depth = test_depth.saturating_add(opens - closes);
            }
            continue;
        }
        // 文档与说明性注释里提到这些词是正常的，只看代码行
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 一段文本里命中了哪些模式（每张表每个模式最多报一次，报模式本身）。
///
/// 只要模式字符串，是因为调用方要拿它**回表判定**（这个模式属于哪张表、这个文件有没有豁免）。
fn offenders_of_text(text: &str) -> Vec<String> {
    let stripped = strip_cfg_test_blocks(text);
    let mut found: Vec<String> = Vec::new();
    for pat in WRITE_API.iter().chain(PROCESS_CALLS.iter()) {
        if stripped.contains(pat) && !found.contains(&pat.to_string()) {
            found.push(pat.to_string());
        }
    }
    found
}

fn offenders(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => offenders_of_text(&text),
        Err(_) => Vec::new(),
    }
}

/// 这个文件里**真正违规**的模式：按两张表分别判定豁免。
fn violations(path: &Path) -> Vec<String> {
    let rel = rel_path(path);
    offenders(path)
        .into_iter()
        .filter(|pat| {
            if PROCESS_CALLS.contains(&pat.as_str()) {
                // 外部进程调用：只有名字在允许位里的文件能过。
                !PROCESS_ALLOWLIST.contains(&rel.as_str())
            } else {
                // 写 API：目录级白名单照旧。
                !is_whitelisted(path)
            }
        })
        .collect()
}

fn src_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut v);
    v
}

/// D4 的机械闸门：生产路径一律走 `try_read_skill_entries`；便利包装是 lossy 的，
/// 会把「目录读不了」吞成「这个 harness 没装 skills」。没有这条检查时，D4 只靠注释约束。
#[test]
fn production_code_never_calls_the_lossy_skill_scanner() {
    let mut hits = Vec::new();
    for f in src_files() {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "testutil.rs" || name == "skills_dir.rs" {
            continue; // 前者是测试件，后者定义并包装（自身测试也会用）
        }
        for (i, line) in std::fs::read_to_string(&f)
            .expect("read src")
            .lines()
            .enumerate()
        {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(pos) = line.find("read_skill_entries(") {
                // try_read_skill_entries( 也含这个子串，按前缀排除
                if pos == 0 || !line[..pos].ends_with("try_") {
                    hits.push(format!(
                        "{}:{}: lossy read_skill_entries",
                        f.display(),
                        i + 1
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "生产路径必须用 try_read_skill_entries，否则读不了的目录会被报成没装：\n{}",
        hits.join("\n")
    );
}

#[test]
fn a_shell_out_outside_the_allowlist_is_refused() {
    // 绊线本身要可证伪：拿一段合成文本，证明扫描器真的会认出 Command::new(。
    // 合成文本比真文件可靠——它不会因为哪天 git.rs 改了实现而漂移。
    let synthetic = "fn f() { let _ = Command::new(\"git\"); }";
    assert_eq!(
        offenders_of_text(synthetic),
        vec![PROCESS_CALLS[0].to_string()]
    );
}

/// 允许位是「这一个文件」，不是「这一片目录」。目录级豁免正是 B2 要避免的那件事：
/// `src/store/` 已经可以写文件，若它同时豁免 shell out，守卫就等于没写。
#[test]
fn the_process_allowlist_names_files_not_directories() {
    for p in PROCESS_ALLOWLIST {
        assert!(p.ends_with(".rs"), "{p} 不是具体文件");
        assert!(!p.ends_with('/'), "{p} 看着像目录");
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(p).exists(),
            "{p} 不存在：允许位不允许指向幽灵文件"
        );
    }
}

#[test]
fn scanner_is_not_vacuous() {
    // 反证扫描器有效：testutil.rs 故意含写 API，必须被抓到。
    // 若这条失败，说明 walk() 没走到文件，上一条的 PASS 毫无意义。
    let files = src_files();
    let testutil = files
        .iter()
        .find(|f| f.file_name().and_then(|n| n.to_str()) == Some("testutil.rs"))
        .expect("src/testutil.rs 必须存在且被扫描到");
    let hits = offenders(testutil);
    assert!(hits.iter().any(|h| h.contains("fs::write(")), "{hits:?}");
    assert!(
        hits.iter().any(|h| h.contains("fs::create_dir")),
        "{hits:?}"
    );
    assert!(
        files.len() >= 10,
        "只扫到 {} 个文件，walk 可能坏了",
        files.len()
    );
}

/// §9.9：Plan A 的「哪里都不许写」收窄成「只许在这两个目录写」。
/// 这条测试钉的是**机制**本身——它今天就是绿的（那两个目录还不存在，扫不到东西），
/// 意义在于：Task 6/7 往里放写 API 时放错目录会被它抓住，而放对目录不会被它拦。
#[test]
fn write_api_is_allowed_only_inside_the_whitelist() {
    let mut all = Vec::new();
    for f in src_files() {
        // testutil.rs 由 `#[cfg(test)] pub mod testutil;` 在模块声明处门控——
        // 文件内部的括号跟踪看不到这层门，所以在这里按名字放行（它不进产物二进制）。
        if f.file_name().and_then(|n| n.to_str()) == Some("testutil.rs") {
            continue;
        }
        for pat in violations(&f) {
            all.push(format!("{}: {pat}", rel_path(&f)));
        }
    }
    assert!(
        all.is_empty(),
        "白名单外的生产代码出现写 API 或未获豁免的 shell out：\n{}\n\
         （写路径只允许出现在 {} 下；shell out 只允许 {} ）",
        all.join("\n"),
        WRITE_WHITELIST.join(" / "),
        PROCESS_ALLOWLIST.join(" / ")
    );
}

/// 白名单前缀必须**真能**匹配，且必须**不**误放行别的目录。
/// 少了这条，把 `WRITE_WHITELIST` 写成 `&["src/"]` 也能让上一条全绿——
/// 那等于把守卫整个删掉还留着一条「通过」的记录。
#[test]
fn whitelist_matches_by_directory_prefix() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(is_whitelisted(&root.join("src/store/provenance.rs")));
    assert!(is_whitelisted(&root.join("src/apply/write.rs")));
    assert!(
        !is_whitelisted(&root.join("src/diff.rs")),
        "diff 是纯函数，不许写盘"
    );
    assert!(
        !is_whitelisted(&root.join("src/store.rs")),
        "前缀必须带尾斜杠：`src/store` 不该放行一个叫 src/store.rs 的文件"
    );
    assert!(!is_whitelisted(&root.join("src/adapters/codex.rs")));
}

/// 允许位必须点到文件，不能退化成目录前缀；且允许位之外的文件里出现 `Command::new(`
/// 必须被拦下。少了这条，把 `PROCESS_ALLOWLIST` 写成 `&["src/store/"]` 也能让上面几条全绿。
#[test]
fn the_allowlist_is_not_the_whole_tree() {
    for p in PROCESS_ALLOWLIST {
        assert_ne!(p, &"src/");
        assert_ne!(p, &"src/store/");
    }
    // 走一遍「读文件 → 找模式 → 按表判定」的完整路径：合成文本只测了前半段，
    // 判定靠的是 rel 路径全等，这条测的正是它不会被目录前缀蒙混过去。
    let dir = std::env::temp_dir().join(format!("henv-guard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let evil = dir.join("evil.rs");
    std::fs::write(&evil, "fn f() { let _ = Command::new(\"git\"); }\n").expect("write");
    assert_eq!(violations(&evil), vec![PROCESS_CALLS[0].to_string()]);
    std::fs::remove_dir_all(&dir).expect("cleanup");

    // 既有文件里不该有 shell out：出现了就该显式加进 PROCESS_ALLOWLIST 并写理由，
    // 而不是把判定放宽。用真实文件断言，是为了让这条在代码长出来时真的会响。
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/capability.rs");
    assert!(real.exists(), "这条断言针对真实文件，它改名了要跟着改");
    assert!(
        violations(&real).is_empty(),
        "src/capability.rs 里出现 shell out —— 要显式加进 PROCESS_ALLOWLIST 并说明理由"
    );
}

/// 白名单不能是摆设：它必须**真的**放行了一批写 API。少了这条，
/// 把白名单指向一个空目录、或者哪天把写路径整体挪出白名单，守卫都会照常绿。
#[test]
fn the_whitelist_actually_covers_real_write_code() {
    let mut hits = Vec::new();
    for f in src_files() {
        if is_whitelisted(&f) {
            hits.extend(offenders(&f));
        }
    }
    assert!(
        hits.iter()
            .any(|h| h.contains("fs::write(") || h.contains("fs::create_dir")),
        "白名单目录里一个写 API 都没有，说明白名单已经和写路径脱节了：{hits:?}"
    );
}
