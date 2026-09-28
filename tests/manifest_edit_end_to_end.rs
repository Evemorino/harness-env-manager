//! `add` / `remove` / `upgrade` 三个编辑命令在**公开入口**上的样子。
//!
//! 单元测试已经分别钉过文本编辑原语与 git 选版本规则；这一条要证的是它们串起来之后仍然
//! 成立：清单真的被改了、盘上真的动了、失败的路径真的一个字节都没写。
//!
//! 夹具全部落在 `TempHome` 下，一个字节都不碰真实 `~`。

use henv::manifest::SkillSourceKind;
use henv::testutil::TempHome;
use henv::{AddRequest, RemoveRequest, UpgradeRequest};

const MANIFEST: &str = "env.yaml";

fn base_manifest() -> &'static str {
    "version: 1\n\nharnesses:\n  claude-code: { enabled: true }\n  codex: { enabled: true }\n"
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).expect("读清单")
}

fn add(t: &TempHome, req: AddRequest<'_>) -> anyhow::Result<String> {
    let mut out = Vec::new();
    let mut confirm = henv::AssumeYes;
    henv::run_add(&req, &t.home, &mut out, &mut confirm, None)?;
    Ok(String::from_utf8(out).expect("utf8"))
}

fn remove(t: &TempHome, names: &[&str], keep: bool) -> anyhow::Result<()> {
    let manifest = t.path(MANIFEST);
    let mut out = Vec::new();
    let mut confirm = henv::AssumeYes;
    henv::run_remove(
        &RemoveRequest {
            names: names.iter().map(|n| (*n).to_string()).collect(),
            manifest: &manifest,
            dry_run: false,
            keep,
        },
        &t.home,
        &mut out,
        &mut confirm,
        None,
    )
}

/// 一个「装了 claude-code 与 codex」的假 HOME，外加共享库里的 `foo`。
fn temp_home_with_a_shared_skill() -> TempHome {
    let t = TempHome::new();
    t.mkdir(".claude");
    t.mkdir(".codex");
    t.write(".agents/skills/foo/SKILL.md", "# foo");
    t.write(MANIFEST, base_manifest());
    t
}

fn add_shared_foo(
    t: &TempHome,
    manifest: &std::path::Path,
    no_apply: bool,
) -> anyhow::Result<String> {
    add(
        t,
        AddRequest {
            name: "foo",
            source: SkillSourceKind::AgentsDir,
            repo: None,
            path: None,
            git_ref: None,
            targets: None,
            manifest,
            dry_run: false,
            no_apply,
        },
    )
}

#[test]
fn add_writes_the_entry_and_delivers_to_installed_harnesses() {
    let t = temp_home_with_a_shared_skill();
    let manifest = t.path(MANIFEST);
    let out = add_shared_foo(&t, &manifest, false).expect("add 应当成功");

    let text = read(&manifest);
    assert!(text.contains("  foo:\n"), "{text}");
    assert!(text.contains("    source: agents-dir\n"), "{text}");
    assert!(
        t.path(".claude/skills/foo/SKILL.md").exists(),
        "claude 要收到"
    );
    // codex 与 kimi 原生读 `~/.agents/skills`：共享库来源的能力它们已经能用，
    // 不该再往自己的目录拷一份（2026-09-28 的修正）。
    assert!(
        !t.path(".codex/skills/foo").exists(),
        "共享库的东西不往 codex 重拷"
    );
    assert!(out.contains("delivered"), "{out}");
}

#[test]
fn add_with_no_apply_only_touches_the_manifest() {
    let t = temp_home_with_a_shared_skill();
    let manifest = t.path(MANIFEST);
    let out = add_shared_foo(&t, &manifest, true).expect("add 应当成功");

    assert!(read(&manifest).contains("  foo:\n"));
    assert!(!t.path(".claude/skills/foo").exists(), "no-apply 不交付");
    assert!(out.contains("--no-apply"), "{out}");
}

#[test]
fn add_keeps_the_comments_that_were_already_in_the_manifest() {
    let t = temp_home_with_a_shared_skill();
    let manifest = t.path(MANIFEST);
    let with_comment = format!("# 我的能力清单\n{}", base_manifest());
    std::fs::write(&manifest, &with_comment).unwrap();

    add_shared_foo(&t, &manifest, true).expect("add");

    let text = read(&manifest);
    assert!(text.starts_with("# 我的能力清单\n"), "注释要留着：{text}");
    assert!(text.contains("  foo:\n"), "{text}");
}

#[test]
fn add_refuses_a_name_that_is_already_there_and_leaves_the_manifest_alone() {
    let t = temp_home_with_a_shared_skill();
    let manifest = t.path(MANIFEST);
    let with_foo = format!(
        "{}skills:\n  foo:\n    source: agents-dir\n",
        base_manifest()
    );
    std::fs::write(&manifest, &with_foo).unwrap();

    let err = add_shared_foo(&t, &manifest, true).expect_err("重名必须报错");

    assert!(err.to_string().contains("已经有"), "{err}");
    assert_eq!(read(&manifest), with_foo, "报错时清单一个字节都不能动");
}

#[test]
fn add_refuses_a_source_that_is_not_there_and_leaves_the_manifest_alone() {
    let t = TempHome::new();
    t.write(MANIFEST, base_manifest());
    let manifest = t.path(MANIFEST);
    let before = read(&manifest);

    let err = add(
        &t,
        AddRequest {
            name: "ghost",
            source: SkillSourceKind::Local,
            repo: None,
            path: Some("/private/tmp/henv-does-not-exist-9f3a"),
            git_ref: None,
            targets: None,
            manifest: &manifest,
            dry_run: false,
            no_apply: true,
        },
    )
    .expect_err("源取不到必须报错");

    assert!(err.to_string().contains("不是目录"), "{err}");
    assert_eq!(read(&manifest), before, "验证失败时清单一个字节都不能动");
}

#[test]
fn remove_deletes_the_entry_and_prunes_only_what_henv_delivered() {
    let t = temp_home_with_a_shared_skill();
    let manifest = t.path(MANIFEST);
    add_shared_foo(&t, &manifest, false).expect("add");
    // 用户手放的一份：它不在账本里，撤销时必须原样留着。
    t.write(".codex/skills/handmade/SKILL.md", "# mine");

    remove(&t, &["foo"], false).expect("remove");

    assert!(!read(&manifest).contains("  foo:\n"), "清单里该删掉了");
    assert!(!t.path(".claude/skills/foo").exists(), "henv 交付的要撤销");
    assert!(!t.path(".codex/skills/foo").exists(), "henv 交付的要撤销");
    assert!(
        t.path(".codex/skills/handmade/SKILL.md").exists(),
        "用户手放的不许删"
    );
}

#[test]
fn remove_keep_only_touches_the_manifest() {
    let t = temp_home_with_a_shared_skill();
    let manifest = t.path(MANIFEST);
    add_shared_foo(&t, &manifest, false).expect("add");

    remove(&t, &["foo"], true).expect("remove --keep");

    assert!(!read(&manifest).contains("  foo:\n"));
    assert!(t.path(".claude/skills/foo/SKILL.md").exists(), "副本要留着");
}

#[test]
fn remove_refuses_a_name_that_is_not_in_the_manifest() {
    let t = temp_home_with_a_shared_skill();
    let before = read(&t.path(MANIFEST));
    let err = remove(&t, &["nope"], true).expect_err("不存在必须报错");
    assert!(err.to_string().contains("没有 skill"), "{err}");
    assert_eq!(read(&t.path(MANIFEST)), before);
}

/// 本地仓库三个 tag：`upgrade` 要挑**数字段最大**的那个，并且第二次跑不动文件。
#[test]
fn upgrade_moves_the_ref_and_is_idempotent() {
    let t = TempHome::new();
    t.mkdir(".claude");
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# v1");
    repo.commit("init");
    repo.tag("v1.0.0");
    repo.write("skills/foo/SKILL.md", "# v2");
    repo.commit("second");
    repo.tag("v1.9.0");
    repo.write("skills/foo/SKILL.md", "# v3");
    repo.commit("third");
    repo.tag("v1.10.0");

    let manifest = t.path(MANIFEST);
    std::fs::write(
        &manifest,
        format!(
            "version: 1\n\nharnesses:\n  claude-code: {{ enabled: true }}\n\n\
             skills:\n  foo:\n    source: git\n    repo: {}\n    path: skills/foo\n    \
             ref: v1.0.0  # 钉住\n",
            repo.url
        ),
    )
    .unwrap();

    let mut confirm = henv::AssumeYes;
    let mut first = Vec::new();
    henv::run_upgrade(
        &UpgradeRequest {
            names: Vec::new(),
            manifest: &manifest,
            to: None,
            dry_run: false,
            no_apply: true,
        },
        &t.home,
        &mut first,
        &mut confirm,
        None,
    )
    .expect("upgrade");

    let text = read(&manifest);
    assert!(
        text.contains("ref: v1.10.0  # 钉住"),
        "要挑数字段最大的 tag，并且留住行尾注释：{text}"
    );

    let mut second = Vec::new();
    henv::run_upgrade(
        &UpgradeRequest {
            names: Vec::new(),
            manifest: &manifest,
            to: None,
            dry_run: false,
            no_apply: true,
        },
        &t.home,
        &mut second,
        &mut confirm,
        None,
    )
    .expect("upgrade");
    let second = String::from_utf8(second).unwrap();
    assert!(second.contains("没有需要升级的条目"), "{second}");
    assert_eq!(read(&manifest), text, "幂等：第二次一个字节都不改");
}

#[test]
fn upgrade_without_a_version_shaped_tag_asks_for_to() {
    let t = TempHome::new();
    t.mkdir(".claude");
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# v1");
    repo.commit("init");
    repo.tag("nightly");

    let manifest = t.path(MANIFEST);
    let before = format!(
        "version: 1\n\nharnesses:\n  claude-code: {{ enabled: true }}\n\n\
         skills:\n  foo:\n    source: git\n    repo: {}\n    path: skills/foo\n    ref: nightly\n",
        repo.url
    );
    std::fs::write(&manifest, &before).unwrap();

    let mut out = Vec::new();
    let mut confirm = henv::AssumeYes;
    henv::run_upgrade(
        &UpgradeRequest {
            names: Vec::new(),
            manifest: &manifest,
            to: None,
            dry_run: false,
            no_apply: true,
        },
        &t.home,
        &mut out,
        &mut confirm,
        None,
    )
    .expect("upgrade 不该因为一个仓库没有 tag 就整次失败");

    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("--to"), "要告诉用户怎么指定：{out}");
    assert_eq!(read(&manifest), before, "没有可升的版本就别动清单");
}
