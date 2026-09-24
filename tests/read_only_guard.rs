//! Plan A 只读守卫：`src/` 里除 `testutil.rs` 外不得出现任何文件写 API。
//!
//! 写路径（`.bak` + temp + rename、`claude mcp add-json` shell out）属 Plan B。
//! 在 Plan B 落地前，这个测试是「henv diff 绝不改我的配置」这句承诺的唯一证据。

use std::path::{Path, PathBuf};

/// 带 `(` 的模式避免误伤 `fs::symlink_metadata` 这类只读 API。
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

fn offenders(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).expect("read src");
    let mut hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
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

#[test]
fn plan_a_source_has_no_write_api_outside_testutil() {
    let mut all = Vec::new();
    for f in src_files() {
        if f.file_name().and_then(|n| n.to_str()) == Some("testutil.rs") {
            continue;
        }
        all.extend(offenders(&f));
    }
    assert!(
        all.is_empty(),
        "Plan A 必须只读，发现写 API：\n{}\n（若确需写入，说明你在做 Plan B 的活）",
        all.join("\n")
    );
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
