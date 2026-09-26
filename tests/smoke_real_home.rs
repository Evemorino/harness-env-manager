//! 真实 HOME 只读冒烟（spec §6）。默认不跑：
//!   cargo test --test smoke_real_home -- --ignored --nocapture
//!
//! 只读性不靠 mtime 断言（真实 HOME 上运行中的 harness 会自己改 `.claude.json`，
//! 那种断言必然抖动），而靠 `tests/read_only_guard.rs` 的静态守卫 + 本测试只调
//! `run_diff`（Plan A 里它没有任何写路径可达）。

#[test]
#[ignore = "读真实 HOME；手动跑：cargo test --test smoke_real_home -- --ignored --nocapture"]
fn diff_again_real_home_succeeds_and_reports_every_detected_harness() {
    let home = henv::home::Home::resolve().expect("resolve HOME");
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/env.yaml");

    let mut out = Vec::new();
    henv::run_diff(&manifest, &home, &mut out).expect("run_diff on real HOME must not error");
    let text = String::from_utf8(out).expect("utf8");

    println!("{text}");

    // 只钉「末行是页脚」，不钉它**以哪个词结尾**：页脚的字段从 180213a 起是
    // `conflict … undeliverable, drift, unknown`，写死 `ends_with("drift")` 会在
    // 字段增减时静默失效——这条 `#[ignore]` 的验收就是这么烂掉过一次的。
    let footer = text.lines().last().unwrap_or_default();
    assert!(footer.starts_with("Plan: "), "缺 Plan 页脚：{text}");
    // 逐个字段名核一遍（不核顺序、不核数值）：页脚渲染坏了而前缀还在，也要拦住。
    for field in [
        "conflict",
        "install",
        "update",
        "remove",
        "undeliverable",
        "drift",
        "unknown",
    ] {
        assert!(footer.contains(field), "页脚缺 `{field}`：{footer}");
    }
    // 本机三家都装着（spec §2）：一行都不该是 "not installed"
    for name in ["claude-code", "codex", "kimi-code"] {
        let line = text
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("报告里没有 {name} 行：{text}"));
        assert!(!line.contains("not installed"), "{line}");
    }
}
