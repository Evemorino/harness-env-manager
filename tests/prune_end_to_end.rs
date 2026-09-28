//! §9.8 的 `--prune` 在**公开入口**上的样子。
//!
//! 三条串起来的性质：**没给 `--prune` 就零删除**、**`--prune` 只删账本记着的**、
//! **删成功才落账本**。夹具全部落在 `TempHome` 下——只装 codex，于是没有任何
//! 一条路径会去调真 CLI、也没有一个字节落在真实 `~`。

use henv::AssumeYes;
use henv::testutil::TempHome;

/// 只装 codex。`enabled: false` 那两家连 `pairs` 都不进。
const ONLY_CODEX: &str = "harnesses:\n  claude-code: { enabled: false }\n  codex: { enabled: true }\n  kimi-code: { enabled: false }\n";

/// 一个 **local 源**的 skill：内容放在 `src/foo`，codex 的扫描面里没有它，所以必然要交付。
///
/// 不用 agents-dir：那条来源现在是「共享库——读得到的不拷」（2026-09-28 的修正），
/// codex 会直接判 in-sync，这些测试就钉不住交付与撤销了。
fn manifest_with_foo(t: &TempHome) -> String {
    format!(
        "version: 1\n{ONLY_CODEX}skills:\n  foo:\n    source: local\n    path: {}\n",
        t.path("src/foo").display()
    )
}

fn manifest_without_foo() -> String {
    format!("version: 1\n{ONLY_CODEX}")
}

fn run_diff(t: &TempHome) -> String {
    let mut out = Vec::new();
    henv::run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
    String::from_utf8(out).expect("utf8")
}

fn run_apply(t: &TempHome, prune: bool) -> String {
    let mut out = Vec::new();
    let mut confirm = AssumeYes;
    henv::run_apply(
        &t.path("env.yaml"),
        &t.home,
        &mut out,
        &mut confirm,
        prune,
        None,
    )
    .expect("run_apply");
    String::from_utf8(out).expect("utf8")
}

/// 全程走一遍：交付 → 清单里去掉 → diff 报 drift 且不删 → 不带 `--prune` 还是不删
/// → `--prune` 删掉自己那份、手放的那份逐字节不动 → 账本落盘、再 diff 只剩手放的那条。
#[test]
fn prune_removes_only_the_ledger_recorded_copy() {
    let t = TempHome::new();
    t.write("src/foo/SKILL.md", "# foo");
    t.mkdir(".codex");
    t.write(".codex/skills/handmade/SKILL.md", "# handmade");
    let handmade = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();

    // 1) 交付。
    t.write("env.yaml", &manifest_with_foo(&t));
    let out = run_apply(&t, false);
    assert!(t.path(".codex/skills/foo/SKILL.md").exists(), "{out}");
    let ledger = henv::store::provenance::Ledger::load(&t.home).unwrap();
    assert!(
        ledger
            .find("codex", henv::capability::Kind::Skill, "foo")
            .is_some()
    );

    // 2) 清单里去掉 foo：两份都是 drift，diff 一个字节都不删。
    t.write("env.yaml", &manifest_without_foo());
    let report = run_diff(&t);
    assert!(report.contains("- drift"), "{report}");
    assert!(report.contains("skill/foo"), "{report}");
    assert!(report.contains("skill/handmade"), "{report}");
    assert!(
        t.path(".codex/skills/foo").exists(),
        "diff 只读：\n{report}"
    );

    // 3) 不带 `--prune` 的 apply：还是不删（Review Focus 2）。
    let out = run_apply(&t, false);
    assert!(
        t.path(".codex/skills/foo").exists(),
        "没给 --prune 就不许删：\n{out}"
    );
    assert!(!out.contains("removed"), "{out}");

    // 4) `--prune`：删掉自己那份，手放的那份逐字节不动（Review Focus 1）。
    let out = run_apply(&t, true);
    assert!(out.contains("removed skill/foo"), "{out}");
    assert!(!t.path(".codex/skills/foo").exists(), "{out}");
    assert!(t.path(".codex/skills/handmade").exists(), "{out}");
    assert_eq!(
        std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
        handmade,
        "非 henv 的 drift 逐字节不变"
    );

    let ledger = henv::store::provenance::Ledger::load(&t.home).unwrap();
    assert!(
        ledger
            .find("codex", henv::capability::Kind::Skill, "foo")
            .is_none(),
        "删成功要把账本落盘"
    );

    // 5) 再 diff：只剩手放的那条还是 drift，且报告如实说它还在。
    let report = run_diff(&t);
    assert!(!report.contains("skill/foo"), "{report}");
    assert!(report.contains("skill/handmade"), "{report}");
}

/// 重跑一次 `--prune` 是正常路径：什么都不删，账本仍然是可读的合法 JSON
/// ——「空账本」与「账本坏了」是两回事。
#[test]
fn prune_is_idempotent() {
    let t = TempHome::new();
    t.write("src/foo/SKILL.md", "# foo");
    t.mkdir(".codex");
    t.write("env.yaml", &manifest_with_foo(&t));
    run_apply(&t, false);
    t.write("env.yaml", &manifest_without_foo());

    let first = run_apply(&t, true);
    assert!(first.contains("removed skill/foo"), "{first}");

    let second = run_apply(&t, true);
    assert!(
        !second.contains("removed"),
        "第二次不该再删什么：\n{second}"
    );
    assert!(!t.path(".codex/skills/foo").exists());

    let ledger = henv::store::provenance::Ledger::load(&t.home).unwrap();
    assert!(ledger.entries.is_empty(), "{:?}", ledger.entries);
}

/// 账本是空的（这台机器从没 apply 过）：`--prune` 一个都不删，连账本文件都不建。
#[test]
fn prune_with_an_empty_ledger_deletes_nothing() {
    let t = TempHome::new();
    t.mkdir(".codex");
    t.write(".codex/skills/handmade/SKILL.md", "# handmade");
    let handmade = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();
    t.write("env.yaml", &manifest_without_foo());

    let out = run_apply(&t, true);

    assert!(!out.contains("removed"), "{out}");
    assert!(
        t.path(".codex/skills/handmade").exists(),
        "空账本 = 什么都不敢动"
    );
    assert_eq!(
        std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
        handmade
    );
    assert!(
        !t.home.henv_provenance().exists(),
        "既没交付也没删除 → 不落账本"
    );
}
