//! `henv doctor` 在**真实二进制**上的样子（Plan C1 Task 1）。
//!
//! 三件事：**退出码承载语义**（0/1/2 各是什么意思）、**问题与警告分开**、
//! 以及这条命令只读。夹具全部落在 `TempHome` 下——`HENV_HOME` 与 `HOME` 都指向
//! 那个临时目录，所以真实 `~` 一个字节都不会被碰。

use henv::testutil::TempHome;
use std::process::Command;

/// 只启 claude-code。另外两家关掉：否则「启用了但本机没装」会各带一条警告，
/// 把「干净环境」的页脚搅浑（那条警告有自己的测试）。
const ONLY_CLAUDE: &str = "harnesses:\n  claude-code: { enabled: true }\n  codex: { enabled: false }\n  kimi-code: { enabled: false }\n";

/// 跑真二进制。`HOME` 与 `HENV_HOME` 都指到夹具上：adapter 看前者，henv 自己的
/// 缓存与状态看后者（`Home::resolve` 优先 `HENV_HOME`）。
fn doctor(home: &TempHome, manifest: &str) -> (i32, String, String) {
    home.write("env.yaml", manifest);
    let out = Command::new(env!("CARGO_BIN_EXE_henv"))
        .arg("doctor")
        .arg("-m")
        .arg(home.path("env.yaml"))
        .env("HENV_HOME", home.home.root())
        .env("HOME", home.home.root())
        // `${VAR}` 检查的夹具变量：显式清掉，结果不受跑测试的这台机器影响。
        .env_remove("HENV_TEST_MISSING_VAR_9F3A")
        .output()
        .expect("run henv doctor");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 干净环境：没有冲突、没有交付不了的东西、没有任何警告 → 0。
#[test]
fn a_clean_environment_exits_zero() {
    let home = TempHome::new();
    // 装了的家：`detect()` 就是「目录在不在」。没有它，这条测试保不住
    // 「干净 = 0 问题 0 警告」——报告会多出一条「启用了但没装」的警告。
    home.mkdir(".claude");
    let (code, out, err) = doctor(&home, &format!("version: 1\n{ONLY_CLAUDE}"));
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.ends_with("Doctor: 0 problems, 0 warnings\n"), "{out}");
}

/// 同名冲突（§9.3 不变式二）：目标上有一个内容不同、又不是 henv 交付的同名 skill。
/// 这是**必须处理**的问题——apply 见到它就一个字节都不写 → 1。
#[test]
fn a_name_conflict_exits_one() {
    let home = TempHome::new();
    home.write(".claude/skills/foo/SKILL.md", "hand written\n");
    home.write(".agents/skills/foo/SKILL.md", "from the shared library\n");
    let manifest = format!(
        "version: 1\n{ONLY_CLAUDE}skills:\n  foo:\n    source: agents-dir\n    targets: [claude-code]\n"
    );

    let (code, out, err) = doctor(&home, &manifest);
    assert_eq!(code, 1, "stdout={out} stderr={err}");
    assert!(out.contains("(plan)"), "{out}");
    assert!(out.contains("conflict"), "{out}");
    assert!(out.contains("1 problems"), "{out}");
}

/// 清单引用了当前环境里没有的 `${VAR}` → 问题（exit 1）：`apply` 会把未展开的引用
/// 原样写进 harness 配置，所以这不能只是警告。报告只出现变量名，**绝不回显值**。
#[test]
fn a_missing_env_reference_exits_one() {
    let home = TempHome::new();
    let manifest = "version: 1\nmcps:\n  context7:\n    command: npx\n    env:\n      K: ${HENV_TEST_MISSING_VAR_9F3A}\n";
    let (code, out, err) = doctor(&home, manifest);
    assert_eq!(code, 1, "stdout={out} stderr={err}");
    assert!(out.contains("(env)"), "{out}");
    assert!(out.contains("mcp `context7`"), "{out}");
    assert!(out.contains("HENV_TEST_MISSING_VAR_9F3A"), "{out}");
    assert!(
        !out.contains("HENV_TEST_MISSING_VAR_9F3A="),
        "只出现名字，不出现「名字=值」：{out}"
    );
    // `(env)` 段在 `(plan)` 段之前：环境不可信时，计划判定也没意义。
    let env_at = out.find("(env)").expect("(env) section");
    if let Some(plan_at) = out.find("(plan)") {
        assert!(env_at < plan_at, "{out}");
    }
}

/// 清单解不开 = henv 没跑成 → 2（**不是** 1：脚本要能把「环境有问题」与
/// 「工具自己坏了」分开）。
#[test]
fn a_broken_manifest_exits_two() {
    let home = TempHome::new();
    let (code, out, err) = doctor(&home, "version: nope\n");
    assert_eq!(code, 2, "stdout={out} stderr={err}");
    assert!(!out.contains("Doctor:"), "没跑成就不该有报告：{out}");
    assert!(err.contains("Error:"), "{err}");
}

/// §2.1：条目分类是 doctor 断链报告的基础。断链是**警告**——§9.3 说 apply 会把它
/// 换成真拷贝（旧的脆弱形态在重跑中自愈），所以它不挡路，退出码仍是 0。
#[test]
fn a_broken_link_is_reported_as_a_warning_and_does_not_fail() {
    let home = TempHome::new();
    home.write(".agents/skills/foo/SKILL.md", "from the shared library\n");
    home.symlink(".claude/skills/foo", "/nonexistent/henv-doctor-test");
    let manifest = format!(
        "version: 1\n{ONLY_CLAUDE}skills:\n  foo:\n    source: agents-dir\n    targets: [claude-code]\n"
    );

    let (code, out, err) = doctor(&home, &manifest);
    assert_eq!(code, 0, "断链不该挡路：stdout={out} stderr={err}");
    assert!(out.contains("- link"), "{out}");
    assert!(out.contains("skill/foo@claude-code"), "{out}");
    assert!(out.contains("1 warnings"), "{out}");
}

/// 清单里启用了、本机却没装的家：只警告（**先写清单后装 harness 是正常流程**），
/// exit 仍是 0，harness 行上写明状态。
#[test]
fn an_enabled_but_absent_harness_only_warns() {
    let home = TempHome::new();
    let manifest = "version: 1\nharnesses:\n  claude-code: { enabled: false }\n  codex: { enabled: false }\n  kimi-code: { enabled: true }\n";
    let (code, out, err) = doctor(&home, manifest);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("kimi-code"), "{out}");
    assert!(out.contains("not installed"), "{out}");
    assert!(out.contains("Doctor: 0 problems, 1 warnings"), "{out}");
}

/// 配置读不出来 = 这一家的判定不可信 → **问题**（exit 1），而且要说清是哪一家、
/// 以及「计划那一段这轮没跑」——不然「没有 conflict」会被误读成「没有冲突」。
#[test]
fn an_unreadable_harness_config_exits_one() {
    let home = TempHome::new();
    home.mkdir(".codex");
    home.write(".codex/config.toml", "= = =\n");
    let (code, out, err) = doctor(&home, "version: 1\n");
    assert_eq!(code, 1, "stdout={out} stderr={err}");
    assert!(out.contains("! config unreadable"), "{out}");
    assert!(out.contains("! harness"), "{out}");
    assert!(out.contains("codex"), "{out}");
    assert!(out.contains("diff 段"), "{out}");
}
