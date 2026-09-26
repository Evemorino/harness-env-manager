//! claude 的 MCP 写入：交给它自己的 CLI（spec §9.6 第 1 行）。
//!
//! **为什么不经手字节**：`~/.claude.json` 被 claude 自己频繁整体重写
//! （`numStartups` 之类），我们 merge 进去的键会和它下一次重写竞争——
//! 输的那个是用户。CLI 是唯一知道该文件当前形状的东西。
//!
//! **这是全项目第二个 shell out 点**（第一个是 `src/store/git.rs`）。
//! `tests/read_only_guard.rs` 的 `PROCESS_ALLOWLIST` 按文件豁免，本文件必须
//! 显式列进去并写理由——那张表不许退化成目录前缀。
//!
//! **绝不经过 shell**：`Command::args`，不是 `sh -c`，不做字符串拼接。
//! argv 里也没有密钥：清单的 env 值由 `secrets::parse_ref` 保证是裸 `${VAR}` 引用。

use anyhow::Context as _;
use std::path::Path;

/// `claude mcp add-json -s user -- <name> <json>`。
///
/// **`-s user` 不可省**：默认 scope 是 `local`，那会写进
/// `projects.<cwd>.mcpServers`——一个读路径故意不看的位置。
///
/// **`--` 也不可省**：名字来自清单，而 `manifest::name_is_a_single_component`
/// 只挡路径分隔符——`-h` 是合法的清单键。没有 `--` 时真 CLI 会把 `-h` 读成
/// `--help`，打印帮助并**退出 0**：`deliver_mcps` 只信退出码，于是把一条从未
/// 写下的交付记进账本。详见 `a_name_that_looks_like_a_flag_is_not_read_as_one`。
pub fn claude_add_json(binary: &Path, name: &str, spec_json: &str) -> anyhow::Result<()> {
    run(
        binary,
        &["mcp", "add-json", "-s", "user", "--", name, spec_json],
        name,
    )
}

/// `claude mcp remove -s user -- <name>`。
///
/// `--` 的理由同 `claude_add_json`：名字由清单给，可以是 `-h` 这样的形状。
///
/// **update 必须先做这一步**：实测 `add-json` 对已存在的名字直接失败
/// （`MCP server <name> already exists in user config`），退出码非 0，
/// 没有 `--force` 之类的覆盖开关。所以「改一个已经交付过的 MCP」只能是
/// remove → add 两条命令。
///
/// 对不存在的名字它会退出 1——**不吞这个错**：走到 update 说明 diff 刚看到过它，
/// 这时 remove 失败是真故障，照 §9.7 报 partial。
pub fn claude_remove(binary: &Path, name: &str) -> anyhow::Result<()> {
    run(binary, &["mcp", "remove", "-s", "user", "--", name], name)
}

fn run(binary: &Path, args: &[&str], name: &str) -> anyhow::Result<()> {
    let out = std::process::Command::new(binary)
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "cannot run the claude CLI ({}); is it installed and on PATH?",
                binary.display()
            )
        })?;
    anyhow::ensure!(
        out.status.success(),
        "claude CLI failed for MCP `{name}` ({}): {}",
        out.status,
        // stderr 只可能是 CLI 自己的话——我们传进去的 JSON 里没有密钥（全是 ${VAR} 引用）。
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // `read_state` 是 trait 方法：冒烟测试要在 `ClaudeCode` 上直接调它，
    // 就得把 trait 带进作用域（计划里的测试正文漏了这一行）。
    use crate::adapters::HarnessAdapter as _;
    use crate::testutil::{FakeCli, TempHome};
    use std::path::PathBuf;

    fn args_of(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn add_json_sends_the_exact_argv_and_the_json_verbatim() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        let json = r#"{"command":"npx","args":["-y","pkg"],"env":{"K":"${K}"}}"#;
        claude_add_json(&cli.path(), "context7", json).unwrap();

        let calls = cli.invocations();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            args_of(&calls[0]),
            vec!["mcp", "add-json", "-s", "user", "--", "context7", json],
            "`-s user` 不可省：默认 scope 是 local，读路径不看那里；`--` 见下面的旗标形状那条"
        );
        assert!(calls[0][6].contains("${K}"), "引用不许被展开");
    }

    #[test]
    fn remove_sends_the_exact_argv() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        claude_remove(&cli.path(), "context7").unwrap();
        assert_eq!(
            args_of(&cli.invocations()[0]),
            vec!["mcp", "remove", "-s", "user", "--", "context7"]
        );
    }

    /// **一个长得像旗标的名字，不能被 CLI 读成旗标。**
    ///
    /// 名字来自清单（`mcps:` 的键），而 `manifest::name_is_a_single_component` 只排除
    /// 路径分隔符——`-h` 是合法的清单键。实测真 CLI（`claude 2.1.267`）：
    ///
    /// ```text
    /// $ claude mcp add-json -s user -h '{"command":"npx"}'
    /// Usage: claude mcp add-json [options] <name> <json>   ← 打印帮助，**退出 0**
    /// ```
    ///
    /// 退出 0 而一个字节没写，而 `run` 只看退出码——于是 `deliver_mcps` 会把这一条
    /// 推进 `delivered` 并记进账本：用户被告知「已交付」，盘上什么都没有。
    /// 这正是本项目要消灭的那类假成功。
    ///
    /// 修法是 `--`：它之后的一切都是位置参数，CLI 不再解析。与
    /// `src/store/git.rs` 的 `clone_args` 同一形状（B2 已经这么做了），
    /// 两个 shell out 点因此长得一样。实测真 CLI 接受 `--`：
    /// `claude mcp add-json -s user -- -henv-try '{...}'` → `Added stdio MCP server -henv-try`。
    #[test]
    fn a_name_that_looks_like_a_flag_is_not_read_as_one() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        claude_add_json(&cli.path(), "-h", "{}").unwrap();
        claude_remove(&cli.path(), "-h").unwrap();

        let calls = cli.invocations();
        assert_eq!(
            args_of(&calls[0]),
            vec!["mcp", "add-json", "-s", "user", "--", "-h", "{}"],
            "名字必须排在 `--` 之后，否则 CLI 会把它当旗标"
        );
        assert_eq!(
            args_of(&calls[1]),
            vec!["mcp", "remove", "-s", "user", "--", "-h"],
            "remove 侧同一条：`--` 之后才是名字"
        );
    }

    /// 一条**看起来像 shell 展开**的 env 值：若实现里用了 `sh -c` 或任何拼串，
    /// `${K}` 会被 shell 换掉（这里 K 不存在则变成空串），这条就红。
    /// 这是「密钥永不进 argv」的技术保障的一半——另一半是 manifest 只收裸引用。
    #[test]
    fn the_argv_is_not_interpreted_by_a_shell() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        // 值本身也参与：`;` 与 `$` 若经过 shell 会改变命令语义
        let json = r#"{"command":"npx","env":{"K":"${K}","SEMI":"${A};${B}"}}"#;
        claude_add_json(&cli.path(), "n", json).unwrap();
        assert_eq!(cli.invocations()[0][6], json);
    }

    #[test]
    fn a_failing_cli_becomes_an_error_that_names_the_server_and_carries_stderr() {
        let t = TempHome::new();
        let cli = FakeCli::failing(&t, "bin/claude", 1, "MCP server n already exists");
        let err = claude_add_json(&cli.path(), "n", "{}").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains('n'), "要能看出是哪一条：{msg}");
        assert!(msg.contains("already exists"), "要带上 CLI 的话：{msg}");
    }

    /// CLI 不在 PATH 上（用户没装 claude）：必须是错误，不许当成成功。
    /// 这是 Review Focus 第 1 条的机械钉子。
    #[test]
    fn a_missing_binary_is_an_error_not_a_silent_success() {
        let t = TempHome::new();
        let err = claude_add_json(&t.path("bin/nope"), "n", "{}").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("claude"), "要说清是起不来 CLI：{msg}");
    }

    /// 真 CLI 的冒烟：默认跳过。手动跑 `cargo test -- --ignored`。
    ///
    /// 它证的是假 shim 证不了的那一半：**真 CLI 接受我们这串 argv，而且写进去的东西
    /// 读路径真的看得到**（`-s user` 落在 `~/.claude.json` 顶层）。
    ///
    /// 名字故意取成**旗标形状**（`-henv-smoke`）：这一条因此同时钉住两件事——
    /// `--` 被真 CLI 接受，且 `--` 之后的名字不会被读成选项。名字取个普通串的话，
    /// 把 `--` 删掉这条照样绿（实测：`-h` 之外的名字本来就落在位置参数上）。
    ///
    /// HOME 由 wrapper 脚本钉死在 TempHome 里——这一条绝不能碰真实 `~`。
    #[test]
    #[ignore = "runs the real claude CLI; needs it on PATH"]
    fn the_real_cli_accepts_our_argv_and_the_reader_sees_it() {
        let t = TempHome::new();
        let Some(real) = which("claude") else {
            eprintln!("claude 不在 PATH 上，跳过");
            return;
        };
        // wrapper 而不是直接调用：写路径按继承的环境起进程，测试里没有
        // （也不该有）改进程全局环境的手段，所以把 HOME 写进脚本里。
        let wrapper = t.path("bin/claude");
        std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nHOME={} exec {} \"$@\"\n",
                t.home.root().display(),
                real.display()
            ),
        )
        .unwrap();
        let mut p = std::fs::metadata(&wrapper).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
        std::fs::set_permissions(&wrapper, p).unwrap();

        let json = r#"{"command":"npx","args":["-y","@upstash/context7-mcp"],"env":{"K":"${K}"}}"#;
        let name = "-henv-smoke";
        claude_add_json(&wrapper, name, json).unwrap();

        // 读路径（adapter 的 read_state）必须看见它——这就是「写路径 == 读路径」。
        let adapter = crate::adapters::claude_code::ClaudeCode::new(t.home.clone());
        let caps = adapter.read_state().unwrap();
        let got = caps
            .iter()
            .find(|c| c.id.name == name)
            .expect("读路径必须看得见刚写进去的那条");
        match &got.spec {
            crate::capability::Spec::Mcp { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args[0], "-y");
                assert_eq!(env["K"], "${K}", "引用原样保留");
            }
            other => panic!("{other:?}"),
        }

        // update 那条路：先 remove 再 add，两条都不许报错。
        claude_remove(&wrapper, name).unwrap();
        claude_add_json(&wrapper, name, r#"{"command":"uvx"}"#).unwrap();
        let caps = adapter.read_state().unwrap();
        let got = caps.iter().find(|c| c.id.name == name).unwrap();
        match &got.spec {
            crate::capability::Spec::Mcp { command, .. } => assert_eq!(command, "uvx"),
            other => panic!("{other:?}"),
        }
    }

    /// 在 PATH 里找一个可执行文件。**不用 `Command::new("which")`**：多一个
    /// shell-out 点没有好处，扫 PATH 就够。
    fn which(name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(name))
                .find(|p| std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false))
        })
    }
}
