//! §9.10 的三条性质在 git 源上的样子：**不联网**、**幂等**、**离线也要能用**。
//!
//! 这里走的是**公开入口**（`run_diff` / `run_apply`）而不是内部函数：单元测试
//! 已经分别钉过解析、判定与渲染，本条要证的是它们串起来之后仍然成立。
//! 夹具用 `henv::testutil`（`testutil` feature），路径全部落在 `TempHome` 里，
//! 仓库是本地 `file://`——一次网络都不碰。
//!
//! §9.10 的第四条「冲突即停」不在这里：它是交付层的性质、与源是不是 git 无关，
//! 由 `apply::tests::execute_refuses_the_whole_plan_when_there_is_a_conflict` 钉着。

use henv::AssumeYes;
use henv::testutil::TempHome;

/// 一份「一个 git 源 skill」的清单。`path` 指向仓库里那份 skill 的目录。
fn manifest(repo: &str) -> String {
    format!(
        "version: 1\n\n\
         skills:\n  foo:\n    source: git\n    repo: {repo}\n    ref: main\n    path: skills/foo\n"
    )
}

/// 一个「本机装了 claude-code」的假 HOME。
///
/// 必须显式建这个目录：`detect()` 只认目录存在，没装的那家整段被跳过，
/// 于是一条计划都不会产生——那时报告里当然也没有 git 源的行。这是检测层的
/// 行为，不是 git 源的行为，不该混进本文件的断言里。清单没写 `targets`，
/// 默认三家全要，所以 codex / kimi-code 仍会以 `not installed` 出现在报告里，
/// 这是真实的形状。
fn temp_home_with_claude_code() -> TempHome {
    let t = TempHome::new();
    t.mkdir(".claude");
    t
}

fn run_diff_to_string(t: &TempHome) -> String {
    let mut out = Vec::new();
    henv::run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
    String::from_utf8(out).expect("utf8")
}

fn run_apply_to_string(t: &TempHome) -> String {
    let mut out = Vec::new();
    let mut confirm = AssumeYes;
    henv::run_apply(
        &t.path("env.yaml"),
        &t.home,
        &mut out,
        &mut confirm,
        false,
        None,
    )
    .expect("run_apply");
    String::from_utf8(out).expect("utf8")
}

#[test]
fn a_git_skill_goes_from_absent_to_delivered_to_in_sync() {
    let t = temp_home_with_claude_code();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");
    t.write("env.yaml", &manifest(&repo.url));

    // 1) diff 不联网（§9.4）：缓存是空的，所以这次判不了——但它必须**说出来**，
    //    而且一个字节都不写。
    let before = run_diff_to_string(&t);
    assert!(
        before.contains("不联网"),
        "判不了的原因要写在报告里，实际：\n{before}"
    );
    assert!(
        !t.path(".claude/skills/foo").exists(),
        "diff 不许写盘：\n{before}"
    );

    // 2) apply 会去取（Fetch 模式），并真的把内容交付到各家。
    run_apply_to_string(&t);
    assert_eq!(
        std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
        "# foo"
    );

    // 3) 幂等：再跑一次 diff，无事可做。
    //    断言落在 summary 上而不是 `!contains("install")`——页脚永远有 `0 install`
    //    这几个字，按子串否定等于什么都没验。
    let after = run_diff_to_string(&t);
    assert!(
        after.contains("claude-code   ✓ in-sync  skills:1"),
        "第二次判定应当是 in-sync：\n{after}"
    );
    assert!(
        after.contains("Plan: 0 conflict, 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown"),
        "第二次应当无事可做：\n{after}"
    );

    // 4) 账本记着来源是 `<repo>@<ref>@<sha>`（§9.3），sha 是那次真实解析到的提交。
    let ledger = std::fs::read_to_string(t.home.henv_provenance()).expect("账本要写出来");
    let expect = format!("{}@main@", repo.url);
    let at = ledger
        .find(&expect)
        .unwrap_or_else(|| panic!("账本里没有 {expect}：{ledger}"));
    let tail = &ledger[at + expect.len()..];
    assert!(
        tail.len() >= 40 && tail[..40].chars().all(|c| c.is_ascii_hexdigit()),
        "`@main@` 后面要跟一个 40 位的提交 sha，实际：{:.60}",
        tail
    );

    // 5) 幂等不是「看起来一样」，是真的没有第二次写盘：目标已 in-sync 时，这一次
    //    apply 不该再交付任何东西。
    //
    //    这里**不能**用 mtime 当判据：macOS 的 `std::fs::copy` 走 `fcopyfile`，
    //    会连时间戳一起复制，所以「重写一遍同样的内容」在盘上留下的 mtime 与
    //    原来分毫不差——那样的断言在本平台上永远为真，等于没有。交付报告才是
    //    可观测的事实，而它恰好也是用户看得见的那一个。
    let second = run_apply_to_string(&t);
    assert!(
        !second.contains("delivered"),
        "无事可做的 apply 不该再交付一遍：\n{second}"
    );
}

#[test]
fn a_second_apply_picks_up_an_upstream_commit() {
    // §9.4 是「拷贝相对直链多付出的代价」的兑现：上游前进了，**重跑要拿到新提交**。
    // 这一条盯的是整条链路的用户可见结果（目标内容 + 账本 sha），而 `resolve` 对着
    // 哪个引用解是它的机制——`git fetch` 只动 `origin/*`，本地分支停在 clone 那天。
    let t = temp_home_with_claude_code();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo v1");
    let first = repo.commit("init");
    t.write("env.yaml", &manifest(&repo.url));

    run_apply_to_string(&t);
    assert_eq!(
        std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
        "# foo v1"
    );

    repo.write("skills/foo/SKILL.md", "# foo v2");
    let second = repo.commit("v2");
    assert_ne!(first, second, "夹具要真的推进了上游");

    let out = run_apply_to_string(&t);
    assert_eq!(
        std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
        "# foo v2",
        "上游前进后再 apply 必须交付新提交，实际报告：\n{out}"
    );
    assert!(
        !out.contains("未能刷新"),
        "这次 fetch 是成功的，不该标 stale：\n{out}"
    );

    // 账本要记新提交：否则下一次 diff 又会拿旧 sha 比对，把自己骗过去。
    let ledger = std::fs::read_to_string(t.home.henv_provenance()).expect("账本要写出来");
    assert!(
        ledger.contains(&format!("{}@main@{second}", repo.url)),
        "账本要记下这次解析到的提交 {second}：{ledger}"
    );
}

#[test]
fn an_offline_rerun_reports_stale_instead_of_failing() {
    // §9.4 的核心承诺：离线也要能用，且不假装成功。
    let t = temp_home_with_claude_code();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");
    t.write("env.yaml", &manifest(&repo.url));
    run_apply_to_string(&t);

    // 上游没了（离线的最强形式）。
    std::fs::remove_dir_all(&repo.dir).unwrap();
    // 目标被清空，迫使计划里出现「要装」这条——stale 才有地方显示。
    std::fs::remove_dir_all(t.path(".claude/skills/foo")).unwrap();

    let out = run_apply_to_string(&t);
    assert!(
        out.contains("未能刷新"),
        "沿用缓存时要标出来，实际：\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
        "# foo",
        "离线也要真的把内容装上去"
    );
}
