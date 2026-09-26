//! 写路径守卫：`src/` 下只有白名单目录（`src/apply/`、`src/store/`）允许出现文件写 API。
//!
//! Plan A 的形态是「哪里都不许写」；Plan B 落地后收窄成白名单（spec §9.9）。
//! 收窄的方式是**加一层允许**，不是从 `FORBIDDEN` 里删模式——`Command::new(` 尤其
//! 必须留着（B2 的 git 与 B3 的 `claude mcp add-json` 都要 shell out，那是这个项目
//! 第一次真的调用外部程序），谁把某条模式从列表里删掉，`FORBIDDEN` 就少一道闸。

use std::path::{Path, PathBuf};

/// 唯一允许落盘的目录前缀（相对仓库根，带尾斜杠）。spec §9.9。
const WRITE_WHITELIST: &[&str] = &["src/apply/", "src/store/"];

/// 带 `(` 的模式避免误伤 `fs::symlink_metadata` 这类只读 API。
///
/// `Command::new(` 今天在生产代码里**一个都匹配不上**，收在这里是防将来：Plan B 的真实写
/// 路径就是 shell out 到 `claude mcp add-json`，那是 Plan A 这个只读承诺的第一次破口。
/// 先把模式立好，Plan B 的第一行 shell out 会直接撞红一条测试，而不是悄悄溜进产物。
const FORBIDDEN: &[&str] = &[
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
    "Command::new(",
];

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

fn offenders(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).expect("read src");
    let mut hits = Vec::new();
    // `#[cfg(test)]` 代码不进产物二进制，测试夹具对 TempDir 的写不是生产写路径。
    // 逐行扫时按括号深度跳过 `#[cfg(test)]` 门控的块，只对生产（非测试）代码报写 API。
    let mut test_depth = 0usize;
    let mut pending_cfg_test = false;
    for (i, line) in text.lines().enumerate() {
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
        for pat in FORBIDDEN {
            if line.contains(pat) {
                hits.push(format!("{}:{}: {pat}", path.display(), i + 1));
            }
        }
    }
    hits
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
        if is_whitelisted(&f) {
            continue;
        }
        if f.file_name().and_then(|n| n.to_str()) == Some("testutil.rs") {
            continue;
        }
        all.extend(offenders(&f));
    }
    assert!(
        all.is_empty(),
        "白名单外的生产代码出现写 API：\n{}\n（写路径只允许出现在 {} 下）",
        all.join("\n"),
        WRITE_WHITELIST.join(" / ")
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
