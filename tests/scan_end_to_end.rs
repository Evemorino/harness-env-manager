//! `henv scan` 在**真实二进制**上的样子（Plan C2 Task 2）。
//!
//! 三件事：`--dry-run` 一个字节都不写、真写是**追加**（原文本逐字节保留 + `.henv.bak`）、
//! 以及**幂等**（第二次没有可收养的）。夹具全部落在 `TempHome` 下——`HENV_HOME` 与 `HOME`
//! 都指向那个临时目录，所以真实 `~` 一个字节都不会被碰。

use henv::testutil::TempHome;
use std::process::Command;

/// 只启 claude-code。另外两家关掉：否则「启用了但本机没装」的家会让报告多出与收养无关的行
/// （那些有自己的测试）。
const ONLY_CLAUDE: &str = "harnesses:\n  claude-code: { enabled: true }\n  codex: { enabled: false }\n  kimi-code: { enabled: false }\n";

/// 跑真二进制。`HOME` 与 `HENV_HOME` 都指到夹具上：adapter 看前者，henv 自己的缓存与状态
/// 看后者（`Home::resolve` 优先 `HENV_HOME`）。
fn scan(home: &TempHome, manifest: &str, dry_run: bool) -> (i32, String, String) {
    home.write("env.yaml", manifest);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_henv"));
    cmd.arg("scan")
        .arg("-m")
        .arg(home.path("env.yaml"))
        .env("HENV_HOME", home.home.root())
        .env("HOME", home.home.root());
    if dry_run {
        cmd.arg("--dry-run");
    }
    let out = cmd.output().expect("run henv scan");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 一份「本机已有、清单还没写」的 skill：`.claude/skills/handwritten` 是真实目录
/// （不是指向共享库的链接），所以它归 `HarnessLocal` → 收养成 `source: local`。
fn handwritten() -> TempHome {
    let home = TempHome::new();
    home.write(".claude/skills/handwritten/SKILL.md", "hand written\n");
    home
}

fn manifest_text(home: &TempHome) -> String {
    std::fs::read_to_string(home.path("env.yaml")).expect("env.yaml")
}

/// `--dry-run`：报告里有将要追加的条目，清单**逐字节不变**，也没有 `.henv.bak`。
#[test]
fn dry_run_prints_and_writes_nothing() {
    let home = handwritten();
    let before = format!("version: 1\n{ONLY_CLAUDE}");

    let (code, out, err) = scan(&home, &before, true);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("(adopt)"), "{out}");
    assert!(out.contains("skill/handwritten"), "{out}");
    assert!(out.contains("targets=[claude-code]"), "{out}");
    assert!(out.contains("(dry run)"), "{out}");
    assert_eq!(manifest_text(&home), before, "--dry-run 不许改清单");
    assert!(
        !home.path("env.yaml.henv.bak").exists(),
        "--dry-run 不许落备份"
    );
}

/// 真写：新条目出现在清单里、原有字节保持、`.henv.bak` 是改动前的原文。
#[test]
fn a_real_run_appends_and_keeps_a_backup() {
    let home = handwritten();
    let before = format!("version: 1\n{ONLY_CLAUDE}");

    let (code, out, err) = scan(&home, &before, false);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("(appended to env.yaml)"), "{out}");

    let after = manifest_text(&home);
    assert!(
        after.starts_with(&before),
        "原有字节必须原样保留（追加只发生在末尾）：\n{after}"
    );
    assert!(after.contains("skills:\n  handwritten:\n"), "{after}");
    assert!(after.contains("    source: local\n"), "{after}");
    assert!(after.contains("targets: [claude-code]"), "{after}");

    let bak = std::fs::read_to_string(home.path("env.yaml.henv.bak")).expect(".henv.bak");
    assert_eq!(bak, before, "备份必须是改动前的原文");

    // 追加出来的清单必须仍然可解析——后续命令全靠它。
    let (diff_code, diff_out, diff_err) = run_diff(&home);
    assert_eq!(diff_code, 0, "stdout={diff_out} stderr={diff_err}");
}

/// 幂等：第二次没有可收养的，清单逐字节不变。
#[test]
fn a_second_run_has_nothing_left_to_adopt() {
    let home = handwritten();
    let (first_code, ..) = scan(&home, &format!("version: 1\n{ONLY_CLAUDE}"), false);
    assert_eq!(first_code, 0);
    let after_first = manifest_text(&home);

    let (code, out, err) = scan(&home, &after_first, false);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("nothing to adopt"), "{out}");
    assert!(
        out.contains("(nothing written)"),
        "零收养时不许说「已追加」（清单与 .bak 都没动）：{out}"
    );
    assert_eq!(manifest_text(&home), after_first, "第二次不许再改清单");
}

/// 某家配置读不出来 = 工具没跑成 → 2，且清单一个字节不动（也不落备份）。
#[test]
fn an_unreadable_harness_config_exits_two_and_leaves_the_manifest_alone() {
    let home = TempHome::new();
    home.mkdir(".codex");
    home.write(".codex/config.toml", "= = =\n");
    let before = "version: 1\n";

    let (code, out, err) = scan(&home, before, false);
    assert_eq!(code, 2, "stdout={out} stderr={err}");
    assert!(err.contains("Error:"), "{err}");
    assert!(!out.contains("Scan:"), "没跑成就不该有报告：{out}");
    assert_eq!(manifest_text(&home), before);
    assert!(!home.path("env.yaml.henv.bak").exists());
}

/// 收养来的条目 `targets` 只含**发现它的那一家**：写进去之后 `diff` 不该冒出「另外两家要装它」，
/// 也不该冒出 `undeliverable`（kimi 吃不下散装 skill——若 `targets` 写成 `all`，这一行立刻出现）。
#[test]
fn an_adopted_entry_only_targets_the_harnesses_that_had_it() {
    let home = handwritten();
    let all_three = "harnesses:\n  claude-code: { enabled: true }\n  codex: { enabled: true }\n  kimi-code: { enabled: true }\n";
    let (code, out, err) = scan(&home, &format!("version: 1\n{all_three}"), false);
    assert_eq!(code, 0, "stdout={out} stderr={err}");

    let (diff_code, diff_out, diff_err) = run_diff(&home);
    assert_eq!(diff_code, 0, "stderr={diff_err}");
    assert!(
        !diff_out.contains("+ install"),
        "收养不该让另外两家多出 install 行：\n{diff_out}"
    );
    assert!(
        !diff_out.contains("! undeliverable"),
        "收养不该造出 undeliverable 行（那是 `targets: all` 的后果）：\n{diff_out}"
    );
    assert!(diff_out.contains("in-sync"), "{diff_out}");
}

/// 只读的既存命令仍然只读：`diff` 跑在刚写过的清单上，也必须成功（追加没有破坏结构）。
fn run_diff(home: &TempHome) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_henv"))
        .arg("diff")
        .arg("-m")
        .arg(home.path("env.yaml"))
        .env("HENV_HOME", home.home.root())
        .env("HOME", home.home.root())
        .output()
        .expect("run henv diff");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}
