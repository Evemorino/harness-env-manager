//! §10.3 记录型 plugin（`source: snapshot`）在**公开入口**上的样子。
//!
//! 三件事：记录型条目**永不交付**（`diff` 里没有 install/update/undeliverable，
//! 本机那份也不再是 drift）、版本对账产一条 `~ snapshot` 但**不挡门禁**（doctor 退出 0）、
//! 以及 `scan` 把已装的 plugin 收养成这种条目之后 drift 归零且幂等。
//!
//! 夹具全部落在 `TempHome` 下——`HOME` 与 `HENV_HOME` 都指向临时目录，真实 `~` 一个字节不碰。

use henv::testutil::TempHome;
use std::process::Command;

/// 只启 claude-code（另外两家没装，只会让报告多出与本节无关的行）。
const ONLY_CLAUDE: &str = "harnesses:\n  claude-code: { enabled: true }\n  codex: { enabled: false }\n  kimi-code: { enabled: false }\n";

/// 一个「claude-code 装着两个 plugin」的假 HOME：`superpowers@official` 记着版本，
/// `mystery@official` 没有版本（现状侧读不出），两者都真的落在盘上。
fn home_with_plugins() -> TempHome {
    let t = TempHome::new();
    t.mkdir(".claude");
    t.write(
        ".claude/plugins/installed_plugins.json",
        &format!(
            r#"{{"plugins":{{"superpowers@official":[{{"installPath":"{}","version":"6.4.1"}}],"mystery@official":[{{"installPath":"{}"}}]}}}}"#,
            t.path(".claude/plugins/cache/official/superpowers/6.4.1").display(),
            t.path(".claude/plugins/cache/official/mystery/0.1.0").display(),
        ),
    );
    t.write(".claude/settings.json", r#"{"enabledPlugins":{}}"#);
    t
}

fn run(home: &TempHome, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_henv"))
        .args(args)
        .arg("-m")
        .arg(home.path("env.yaml"))
        .env("HENV_HOME", home.home.root())
        .env("HOME", home.home.root())
        .output()
        .unwrap_or_else(|e| panic!("run henv {args:?}: {e}"));
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 记录型条目**永不交付**：版本对上时报告里既没有它，也没有别的动作——
/// 本机那份连 drift 都不是（它是清单要的）。
#[test]
fn a_snapshot_entry_never_delivers_and_its_copy_is_not_drift() {
    let home = home_with_plugins();
    // `mystery` 也记着（只记身份、没有版本）：不记它的话，本机那份就是一个
    // 货真价实的 drift，会把这条测试要看的「不是 drift」搅浑。
    let manifest = format!(
        "version: 1\n\n{ONLY_CLAUDE}\nplugins:\n  superpowers:\n    source: snapshot\n    versions:\n      claude-code: 6.4.1\n  mystery:\n    source: snapshot\n"
    );
    home.write("env.yaml", &manifest);

    let (code, out, err) = run(&home, &["diff"]);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("in-sync"), "{out}");
    assert!(!out.contains("+ install"), "{out}");
    assert!(!out.contains("~ update"), "{out}");
    assert!(!out.contains("! undeliverable"), "{out}");
    assert!(
        !out.contains("plugin/"),
        "版本对上时连对账行都不该有：{out}"
    );
    assert!(
        !out.contains("- drift"),
        "清单记着它们，本机那两份就不是 drift：{out}"
    );
    assert!(out.contains("0 drift"), "{out}");
}

/// 版本不符 / 本机没装 → 一条 `~ snapshot`，兜里那句写死「只记录不交付」；
/// 汇总行此时才多出 `snapshot` 字段。doctor 里它是**警告**，退出码仍是 0。
#[test]
fn a_stale_record_reports_one_line_and_does_not_block_the_gate() {
    let home = home_with_plugins();
    // 记的版本比本机旧；另一个记着、本机压根没有。
    let manifest = format!(
        "version: 1\n\n{ONLY_CLAUDE}\nplugins:\n  superpowers:\n    source: snapshot\n    versions:\n      claude-code: 6.3.0\n  mystery:\n    source: snapshot\n  never-installed:\n    source: snapshot\n    versions:\n      claude-code: v2.0.0\n"
    );
    home.write("env.yaml", &manifest);

    let (code, out, err) = run(&home, &["diff"]);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(
        out.contains("~ snapshot plugin/superpowers (只记录不交付；清单记 6.3.0，本机 6.4.1)"),
        "{out}"
    );
    assert!(
        out.contains("plugin/never-installed (只记录不交付；清单记 v2.0.0，本机没装)"),
        "{out}"
    );
    assert!(
        out.contains(", 2 snapshot\n"),
        "汇总行只有非零时才多这一格：{out}"
    );
    assert!(!out.contains("! undeliverable"), "它不是交付失败：{out}");

    let (code, out, err) = run(&home, &["doctor"]);
    assert_eq!(
        code, 0,
        "记录与现实不符不该让门禁恒红：stdout={out} stderr={err}"
    );
    assert!(out.contains("~ snapshot"), "{out}");
    assert!(out.contains("plugin/superpowers@claude-code"), "{out}");
    assert!(out.contains("problems, 2 warnings"), "{out}");
}

/// `scan` 把已装的 plugin 收养成**记录型**条目：`source: snapshot` + 逐家版本
/// （读不出版本的那条只进 `targets`）。收养之后 `diff` 里它们的 drift 归零，
/// 再跑一次 `scan` 没有可收养的——幂等。
#[test]
fn scan_adopts_plugins_as_snapshot_records_and_the_drift_goes_away() {
    let home = home_with_plugins();
    home.write("env.yaml", &format!("version: 1\n{ONLY_CLAUDE}"));

    let (code, out, err) = run(&home, &["scan", "--dry-run"]);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("plugin/superpowers"), "{out}");
    assert!(out.contains("plugin/mystery"), "{out}");
    assert!(out.contains("source=snapshot"), "{out}");
    assert!(out.contains("(dry run)"), "{out}");

    let (code, out, err) = run(&home, &["scan"]);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    let after = std::fs::read_to_string(home.path("env.yaml")).expect("env.yaml");
    assert!(after.contains("plugins:\n"), "{after}");
    assert!(
        after.contains("  superpowers:\n    source: snapshot\n"),
        "{after}"
    );
    // `6.4.1` 以数字开头，`scalar` 按「宁严勿宽」给它加引号——解出来仍是同一个串。
    assert!(
        after.contains("    versions:\n      claude-code: \"6.4.1\"\n"),
        "{after}"
    );
    // 读不出版本的那条不写 `versions:`——它不是 version 形，记了也只会永远报不可比。
    assert!(
        after.contains("  mystery:\n    source: snapshot\n    targets: [claude-code]\n"),
        "读不出版本的那条只进 targets，不写 `versions:`：{after}"
    );

    let (code, out, err) = run(&home, &["diff"]);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(
        !out.contains("plugin/superpowers") && !out.contains("plugin/mystery"),
        "收养之后它们不该再出现（既不是 drift，也不是待办）：{out}"
    );
    assert!(!out.contains(" snapshot"), "{out}");

    let (code, out, err) = run(&home, &["scan"]);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("nothing to adopt"), "{out}");
}
