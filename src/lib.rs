//! henv：跨 harness 能力环境管理器。
//!
//! Plan A 只读：解析 `env.yaml` → 归一化能力 → 扫本机三家现状 → 纯函数 diff → 渲染。
//! 全链路不写 `Home` 下任何文件；该不变量由 `tests/read_only_guard.rs` 静态守卫。

use adapters::HarnessAdapter;
use anyhow::Context as _;
use home::Home;
use manifest::Manifest;
use std::io::Write;
use std::path::Path;

pub mod adapters;
pub mod capability;
pub mod diff;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod render;
pub mod secrets;
pub mod skills_dir;

// 只有测试引用这三个常量；不加 cfg 会让 `cargo build` 报 unused import。
#[cfg(test)]
use adapters::{CLAUDE_CODE, CODEX, KIMI_CODE};

#[cfg(test)]
mod testutil;

/// 只读比对：清单为期望，本机为现状，输出「切 harness 时什么会失效」。
///
/// 恒以 `Ok(())` 结束（有 drift 也是 `Ok`）——`diff` 是报告不是断言，
/// 门禁语义留给 Plan C 的 `doctor`。
///
/// 注：`degrade:` 在 Plan A 只是声明性占位——plugin 尚未拆解，策略两侧都不生效，
/// 真正按策略静音/降级要等 Plan B 拆解落地。
pub fn run_diff(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("cannot read manifest: {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", manifest_path.display()))?;

    let desired = lower::lower(&manifest);
    let degrade = lower::DegradeTable::new(&manifest);

    let registry = adapters::registry(home);
    // state 与它的 adapter 必须成对交给 compute，故先收进同一个 Vec 再借用。
    let mut pairs: Vec<(diff::HarnessState, &dyn HarnessAdapter)> = Vec::new();
    for a in &registry {
        if !is_enabled(&manifest, a.name()) {
            continue;
        }
        let detected = a.detect();
        // 未安装 → 空现状 + detected=false，由 render 显示跳过原因；
        // 不去 read_state，免得对不存在的目录做无谓 IO。
        let actual = if detected {
            a.read_state()
                .with_context(|| format!("cannot scan {} state", a.name()))?
        } else {
            Vec::new()
        };
        pairs.push((
            diff::HarnessState {
                name: a.name().to_string(),
                detected,
                actual,
            },
            &**a,
        ));
    }

    let inputs: Vec<diff::HarnessInput> = pairs
        .iter()
        .map(|(state, adapter)| diff::HarnessInput {
            state,
            adapter: *adapter,
        })
        .collect();

    let plan = diff::compute(&desired, &inputs, &degrade);
    write!(out, "{}", render::render(&plan))?;
    Ok(())
}

/// `harnesses` 是 opt-out 列表：未列出 = 启用，只有显式 `enabled: false` 排除。
fn is_enabled(m: &Manifest, name: &str) -> bool {
    m.harnesses.get(name).map(|h| h.enabled).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    /// 跑一次 run_diff，返回渲染文本。
    /// 用 `TempHome::write` 而非 `std::fs::write`：read_only_guard 扫的是整个
    /// `src/lib.rs`（含 `#[cfg(test)]`），只豁免 `testutil.rs`。
    fn diff(t: &TempHome, manifest: &str) -> String {
        t.write("env.yaml", manifest);
        let mut out = Vec::new();
        run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
        String::from_utf8(out).expect("utf8")
    }

    #[test]
    fn harness_absent_from_manifest_is_enabled_by_default() {
        let m = Manifest::parse("version: 1\nharnesses:\n  codex: { enabled: false }\n").unwrap();
        assert!(is_enabled(&m, CLAUDE_CODE), "未列出 = 启用");
        assert!(is_enabled(&m, KIMI_CODE), "未列出 = 启用");
        assert!(!is_enabled(&m, CODEX), "显式 false 才排除");
    }

    #[test]
    fn empty_home_reports_all_three_as_skipped() {
        let t = TempHome::new();
        let out = diff(&t, "version: 1\n");
        assert!(
            out.contains("claude-code   ⚠ not installed (skipped)\n"),
            "{out}"
        );
        assert!(
            out.contains("codex         ⚠ not installed (skipped)\n"),
            "{out}"
        );
        assert!(
            out.contains("kimi-code     ⚠ not installed (skipped)\n"),
            "{out}"
        );
        assert!(
            out.ends_with("Plan: 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift\n"),
            "{out}"
        );
    }

    #[test]
    fn disabled_harness_is_omitted_entirely() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let out = diff(&t, "version: 1\nharnesses:\n  codex: { enabled: false }\n");
        assert!(!out.contains("codex"), "{out}");
        // 另外两家未安装，仍照常报告
        assert!(
            out.contains("claude-code   ⚠ not installed (skipped)\n"),
            "{out}"
        );
    }

    #[test]
    fn desired_mcp_present_in_claude_json_is_in_sync_and_missing_one_is_install() {
        let t = TempHome::new();
        t.mkdir(".claude").write(
            ".claude.json",
            r#"{ "numStartups": 7,
                 "mcpServers": { "context7": { "command": "npx", "args": ["-y", "ctx7"] } } }"#,
        );
        let manifest = "version: 1\nmcps:\n  context7:\n    command: npx\n    args: [\"-y\", \"ctx7\"]\n  browser-use:\n    command: npx\n    args: [\"-y\", \"bu\"]\n";
        let out = diff(&t, manifest);

        assert!(
            out.contains("claude-code   + install  mcp/browser-use\n"),
            "{out}"
        );
        // 有一条 install 待办 → 不出 in-sync 汇总行，但 context7 确实是实有的那 1 个 mcp
        assert!(!out.contains("✓ in-sync"), "{out}");
        assert!(
            out.ends_with("Plan: 1 install, 0 update, 0 remove, 0 undeliverable, 0 drift\n"),
            "{out}"
        );
        // 无关字段（numStartups）被读但不被报告
        assert!(!out.contains("numStartups"), "{out}");
    }

    #[test]
    fn fully_satisfied_manifest_renders_the_summary_line() {
        let t = TempHome::new();
        t.mkdir(".claude").write(
            ".claude.json",
            r#"{ "mcpServers": { "context7": { "command": "npx", "args": ["-y", "ctx7"] } } }"#,
        );
        let manifest =
            "version: 1\nmcps:\n  context7:\n    command: npx\n    args: [\"-y\", \"ctx7\"]\n";
        let out = diff(&t, manifest);
        assert!(out.contains("claude-code   ✓ in-sync  mcps:1\n"), "{out}");
    }

    #[test]
    fn unsupported_loose_skill_produces_the_bang_line_on_kimi() {
        // spec 的核心价值主张：切到 kimi 时这条 skill 会静默失效，diff 必须先喊出来。
        let t = TempHome::new();
        t.mkdir(".kimi-code");
        let manifest =
            "version: 1\nskills:\n  my-draft:\n    source: local\n    path: /tmp/my-draft\n";
        let out = diff(&t, manifest);
        assert!(out.contains("! undeliverable  skill/my-draft ("), "{out}");
        assert!(out.contains("kimi-code 无用户级 skills 目录"), "{out}");
    }

    #[test]
    fn missing_manifest_file_error_carries_the_path() {
        let t = TempHome::new();
        let mut out = Vec::new();
        let err = run_diff(&t.path("nope.yaml"), &t.home, &mut out)
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope.yaml"), "{err}");
    }

    #[test]
    fn invalid_manifest_error_carries_the_path_and_the_validation_detail() {
        let t = TempHome::new();
        t.write(
            "env.yaml",
            "version: 1\nmcps:\n  c:\n    command: npx\n    env:\n      K: sk-live-abc123\n",
        );
        let mut out = Vec::new();
        let err = format!(
            "{:#}",
            run_diff(&t.path("env.yaml"), &t.home, &mut out).unwrap_err()
        );
        assert!(err.contains("env.yaml"), "{err}");
        assert!(err.contains("mcps.c"), "{err}");
        // 密钥门禁：错误里不许回显值
        assert!(
            !err.contains("sk-live-abc123"),
            "错误信息不得回显密钥值：{err}"
        );
        assert!(out.is_empty(), "出错时不输出半截报告");
    }
}
