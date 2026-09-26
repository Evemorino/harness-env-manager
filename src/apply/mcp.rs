//! MCP 的交付（spec §9.6）。落点由 adapter 的 `mcp_target()` 给出，
//! **字节由这里写**——读路径与写路径指向同一个文件是结构性的，不靠测试兜。
//!
//! 三家三种机制，因为三家的配置文件性质不同：
//! - claude：`~/.claude.json` 被 harness 频繁整体重写，直接 merge 会与它并发写竞争，
//!   所以交给它自己的 CLI（§9.6 第 1 行）。
//! - codex：`config.toml` 是用户手写的 TOML，只能子树级 merge（`toml_edit`）。
//! - kimi：`mcp.json` 结构简单，顶层键 merge（serde_json）。

use crate::apply::HarnessReport;
use crate::apply::{mcp_claude, mcp_json, mcp_toml};
use crate::capability::{Capability, Kind, Spec};
use crate::diff::{Action, HarnessPlan};
use crate::store::hash::hash_mcp_spec;
use crate::store::provenance::{Ledger, LedgerEntry};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 一家的 MCP 写目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTarget {
    /// 落点文件。claude 也填：它由 CLI 写，但填上读路径那个文件才让
    /// 「写路径 == 读路径」在三个 adapter 上长得一样；它同时是账本里的 `path`。
    pub path: PathBuf,
    pub mechanism: McpMechanism,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpMechanism {
    /// shell out `claude mcp add-json -s user`（§9.6）。
    ///
    /// `binary` 生产里是裸的 `"claude"`（走 PATH）；测试里是一条 shim 的绝对路径。
    /// **不是测试后门**：这一族接口本来就要能指定可执行文件，否则「CLI 不在 PATH 上」
    /// 这条真实故障没法被表达成测试。
    ClaudeCli { binary: PathBuf },
    /// `toml_edit` 只改 `[mcp_servers]` 子树，其余字节原样保留。
    CodexToml,
    /// serde_json 改 `mcpServers` 键，其余顶层键保留。
    KimiJson,
}

/// 一家的 MCP 交付（§9.6）。与 [`crate::apply::deliver_skills`] 同一条失败隔离：
/// 单条失败不中断这一家剩下的条目，失败的条目不记账，返回 `Err` 只留给「压根没法开始」。
///
/// `target` 为 `None`（这家没有用户级 MCP 配置）时返回一份空报告——不是失败，是这家的事实。
pub fn deliver_mcps(
    harness: &str,
    target: Option<&McpTarget>,
    slice: &HarnessPlan,
    desired: &[Capability],
    ledger: &mut Ledger,
) -> anyhow::Result<HarnessReport> {
    let mut report = HarnessReport {
        harness: harness.to_string(),
        delivered: Vec::new(),
        failure: None,
    };
    let Some(target) = target else {
        return Ok(report);
    };

    let mut failures: Vec<String> = Vec::new();
    for e in &slice.entries {
        if e.id.kind != Kind::Mcp {
            continue;
        }
        // 只有 install / update 是「把内容放过去」。其余动作都是「不该放」。
        if !matches!(e.action, Action::Install | Action::Update { .. }) {
            continue;
        }
        // 清单里取回这条的三段载荷。计划说要装、清单里却没有 → 是要做的事没做成。
        let Some(d) = desired.iter().find(|c| c.id == e.id) else {
            failures.push(format!("{}：计划里有它、清单里没有，未交付", e.id.label()));
            continue;
        };
        let Spec::Mcp { command, args, env } = &d.spec else {
            failures.push(format!("{}：清单里这条不是 MCP 规格，未交付", e.id.label()));
            continue;
        };

        // update 在 claude 上要先摘再写（实测 add-json 对已存在的名字直接失败）。
        // 其余两家是整棵子树替换，天然就是 update。
        let replace = matches!(e.action, Action::Update { .. });
        match dispatch(target, &e.id.name, command, args, env, replace) {
            Ok(()) => {
                report.delivered.push(e.id.label());
                ledger.record(LedgerEntry {
                    harness: harness.to_string(),
                    kind: Kind::Mcp,
                    name: e.id.name.clone(),
                    path: target.path.clone(),
                    // B3a 的 MCP 来源只有清单一条，格式与 skill 的 `agents-dir` 并列。
                    source_id: format!("manifest:mcp/{}", e.id.name),
                    // 记的是我们写下去的三个字段。skill 那条路记的是交付完成后在目标上
                    // **重算**的哈希（源与目标之间隔着一次拷贝）；MCP 没有那层——
                    // 写入器要么成功要么报错，不存在「成功但写了别的」。
                    // 这个选择由「apply 之后 diff 必须是 in-sync」的性质测试兜底。
                    content_hash: hash_mcp_spec(command, args, env),
                    delivered_at_unix: super::now_unix(),
                });
            }
            Err(err) => failures.push(format!("{}: {err:#}", e.id.label())),
        }
    }
    report.delivered.sort();
    if !failures.is_empty() {
        report.failure = Some(failures.join("; "));
    }
    Ok(report)
}

/// 按机制分派到三家的写入器。
fn dispatch(
    target: &McpTarget,
    name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    replace: bool,
) -> anyhow::Result<()> {
    match &target.mechanism {
        McpMechanism::CodexToml => {
            mcp_toml::upsert_codex_mcp(&target.path, name, command, args, env)
        }
        McpMechanism::KimiJson => mcp_json::upsert_kimi_mcp(&target.path, name, command, args, env),
        McpMechanism::ClaudeCli { binary } => {
            if replace {
                // 先摘再写。remove 失败就停在这儿——add 会覆盖掉一份我们并不认识的东西。
                mcp_claude::claude_remove(binary, name)?;
            }
            mcp_claude::claude_add_json(binary, name, &claude_spec_json(command, args, env))
        }
    }
}

/// claude CLI 收的 `--json` 载荷。键名与它自己写出来的形状一致。
fn claude_spec_json(command: &str, args: &[String], env: &BTreeMap<String, String>) -> String {
    let mut v = serde_json::Map::new();
    v.insert("command".into(), command.into());
    if !args.is_empty() {
        v.insert("args".into(), serde_json::json!(args));
    }
    if !env.is_empty() {
        v.insert("env".into(), serde_json::json!(env));
    }
    serde_json::Value::Object(v).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Kind};
    use crate::diff::{Action, HarnessPlan, PlanEntry};
    use crate::store::provenance::Ledger;
    use crate::testutil::{FakeAdapter, FakeCli, TempHome};
    use std::collections::BTreeMap;

    fn plan_with_mcp(name: &str, action: Action) -> HarnessPlan {
        HarnessPlan {
            harness: "codex".into(),
            detected: true,
            entries: vec![PlanEntry::new(CapId::new(Kind::Mcp, name), action)],
            actual_by_kind: BTreeMap::new(),
        }
    }

    #[test]
    fn an_install_writes_the_mcp_and_records_it() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let d = FakeAdapter::mcp("context7", "npx");
        let mut l = Ledger::default();

        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan_with_mcp("context7", Action::Install),
            std::slice::from_ref(&d),
            &mut l,
        )
        .unwrap();

        assert_eq!(r.delivered, vec!["mcp/context7"]);
        assert!(r.failure.is_none());
        let entry = l.find("codex", Kind::Mcp, "context7").expect("要记账");
        assert_eq!(entry.path, target.path);
        assert_eq!(entry.source_id, "manifest:mcp/context7");
        let Spec::Mcp { command, args, env } = &d.spec else {
            unreachable!()
        };
        assert_eq!(entry.content_hash, hash_mcp_spec(command, args, env));
    }

    /// 这家没有 MCP 写目标（`mcp_target()` 给 `None`）：空报告，不是失败。
    #[test]
    fn no_target_is_an_empty_report_not_a_failure() {
        let mut l = Ledger::default();
        let r = deliver_mcps(
            "kimi-code",
            None,
            &plan_with_mcp("context7", Action::Install),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();
        assert!(r.delivered.is_empty() && r.failure.is_none());
    }

    /// 单条失败不记账（§9.7：没交付成功就不该声称交付过），也不上抛。
    #[test]
    fn a_failed_write_is_recorded_as_a_failure_and_not_ledgered() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        t.write(".codex/config.toml", "mcp_servers = \"oops\"\n");
        let mut l = Ledger::default();

        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan_with_mcp("context7", Action::Install),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();

        assert!(r.delivered.is_empty());
        assert!(r.failure.as_deref().unwrap().contains("mcp/context7"));
        assert!(
            l.find("codex", Kind::Mcp, "context7").is_none(),
            "失败的不许记账"
        );
    }

    /// 失败隔离：一条坏了，同一次里别的一条照样交付。
    ///
    /// 坏的那条走「计划里有它、清单里没有」这条失败路径——机制不是重点，
    /// 重点是顺序执行里一条失败**不中断**这个切片的其余条目。
    /// （与 `execute` 的冲突即停不冲突：那是计划层的全停，这里是单条失败后
    /// 已成功的仍然留在盘上、账本上也仍然记着。）
    #[test]
    fn one_failure_does_not_stop_the_rest_of_this_harness() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let mut l = Ledger::default();
        let mut plan = plan_with_mcp("good", Action::Install);
        plan.entries.push(PlanEntry::new(
            CapId::new(Kind::Mcp, "bad"),
            Action::Install,
        ));

        // 清单里只有 good —— bad 因此失败。
        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan,
            &[FakeAdapter::mcp("good", "npx")],
            &mut l,
        )
        .unwrap();

        assert_eq!(r.delivered, vec!["mcp/good"], "好的一条要交付");
        assert!(
            r.failure.as_deref().unwrap().contains("mcp/bad"),
            "坏的一条要报出来：{:?}",
            r.failure
        );
    }

    /// 计划说要装、清单里却没有这一条 → 是要做的事没做成，不是「跳过」（§9.1）。
    #[test]
    fn a_planned_mcp_missing_from_the_manifest_is_a_failure() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let mut l = Ledger::default();
        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan_with_mcp("ghost", Action::Install),
            &[],
            &mut l,
        )
        .unwrap();
        assert!(r.delivered.is_empty());
        assert!(r.failure.as_deref().unwrap().contains("mcp/ghost"));
    }

    /// in-sync / conflict / drift / unknown 都不是「把内容放过去」，一条都不许写。
    #[test]
    fn only_install_and_update_write() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let mut l = Ledger::default();
        let d = FakeAdapter::mcp("context7", "npx");
        for action in [
            Action::InSync,
            Action::Conflict { reason: "x".into() },
            Action::Drift,
            Action::Unknown,
            Action::Undeliverable { reason: "x".into() },
        ] {
            let r = deliver_mcps(
                "codex",
                Some(&target),
                &plan_with_mcp("context7", action.clone()),
                std::slice::from_ref(&d),
                &mut l,
            )
            .unwrap();
            assert!(r.delivered.is_empty(), "{action:?} 不该写盘");
            assert!(r.failure.is_none(), "{action:?} 不该报失败");
        }
        assert!(!t.path(".codex/config.toml").exists(), "一个字节都不该写");
    }

    /// **update 在 claude 上是两条命令，顺序不能反**（§9.6）。
    /// 假 shim 正好能证这件事：真 CLI 证「它接受这串 argv」，shim 证「我们每次发了什么」。
    #[test]
    fn a_claude_update_removes_first_then_adds() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        let target = McpTarget {
            path: t.home.claude_json(),
            mechanism: McpMechanism::ClaudeCli { binary: cli.path() },
        };
        let d = FakeAdapter::mcp("context7", "npx");
        let mut l = Ledger::default();

        let r = deliver_mcps(
            "claude-code",
            Some(&target),
            &plan_with_mcp(
                "context7",
                Action::Update {
                    from: "old".into(),
                    to: "npx".into(),
                },
            ),
            &[d],
            &mut l,
        )
        .unwrap();

        assert_eq!(r.delivered, vec!["mcp/context7"]);
        // 先绑到局部变量：`invocations()` 返回的是自有 Vec，
        // 直接在临时值上 `.iter()` 借不出 `&str`（E0716）。
        let invocations = cli.invocations();
        let calls: Vec<Vec<&str>> = invocations
            .iter()
            .map(|v| v.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(calls.len(), 2, "update 是两条命令");
        // remove 那条比全：名字也在里面，钉住「删的是这一条」。
        assert_eq!(calls[0], ["mcp", "remove", "-s", "user", "--", "context7"]);
        // add-json 那条截到名字为止——末尾是 JSON 载荷，由 mcp_claude 自己的测试钉。
        assert_eq!(
            calls[1][..6],
            ["mcp", "add-json", "-s", "user", "--", "context7"]
        );
    }

    /// install 只有一条命令——`add-json` 对不存在的名字是能成的，
    /// 多来一次 `remove` 会失败（对不存在的名字退出 1），把 install 变成假故障。
    #[test]
    fn a_claude_install_is_a_single_add() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        let target = McpTarget {
            path: t.home.claude_json(),
            mechanism: McpMechanism::ClaudeCli { binary: cli.path() },
        };
        let mut l = Ledger::default();
        deliver_mcps(
            "claude-code",
            Some(&target),
            &plan_with_mcp("context7", Action::Install),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();

        let calls = cli.invocations();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][1], "add-json");
    }

    /// remove 失败（CLI 说这个名字不存在）→ 真故障，报 partial，且**不许接着 add**：
    /// add 会覆盖掉一份我们并不认识的东西。
    #[test]
    fn a_failed_remove_stops_before_the_add() {
        let t = TempHome::new();
        let cli = FakeCli::failing(&t, "bin/claude", 1, "No MCP server named \"context7\"");
        let target = McpTarget {
            path: t.home.claude_json(),
            mechanism: McpMechanism::ClaudeCli { binary: cli.path() },
        };
        let mut l = Ledger::default();
        let r = deliver_mcps(
            "claude-code",
            Some(&target),
            &plan_with_mcp(
                "context7",
                Action::Update {
                    from: "old".into(),
                    to: "npx".into(),
                },
            ),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();

        assert!(r.delivered.is_empty());
        assert!(r.failure.is_some());
        assert_eq!(cli.invocations().len(), 1, "remove 失败就不该再 add");
        assert!(l.find("claude-code", Kind::Mcp, "context7").is_none());
    }
}
