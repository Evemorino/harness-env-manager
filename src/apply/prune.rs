//! `--prune` 的执行层（spec §9.8/§9.7）：撤销 henv 自己交付过的东西。
//!
//! **与 [`crate::apply::deliver_skills`] 同一条失败隔离**：逐条执行，单条失败进报告、
//! 不中断其余；**只有成功的那条才从账本摘除**——账本说「这里还有 henv 的东西」，
//! 而失败时它确实还在。
//!
//! **不变式二在这里的落点**：进这个函数的条目已经是 [`Action::Remove`]，
//! 而那个动作只可能由 [`crate::diff::prune_plan`] 产生——它要求账本里有一条
//! `(harness, kind, name)` 记录。执行层因此不再判一次「这是不是我们的」，
//! 它做的只是：**按账本给的路径删，删成功才摘账**。

use crate::adapters::HarnessAdapter;
use crate::apply::mcp::McpMechanism;
use crate::apply::write::remove_any;
use crate::apply::{HarnessReport, mcp_claude, mcp_json, mcp_toml};
use crate::capability::Kind;
use crate::diff::{Action, HarnessPlan};
use crate::store::provenance::Ledger;

/// 撤销一家人家清单里已经不要、但账本记着是 henv 放的东西（§9.8）。
///
/// 返回的 [`HarnessReport`] 里 `delivered` 恒空——撤销不交付任何东西；
/// `removed` 是成功摘掉的 label（`skill/foo` / `mcp/context7`）。
///
/// 失败隔离（§9.7）：逐条执行，单条失败进报告、不中断其余。本函数**永不上抛**——
/// 它没有「压根没法开始」这种状态（每一条都是独立的、可单独失败的）。
pub fn prune_harness(
    adapter: &dyn HarnessAdapter,
    slice: &HarnessPlan,
    ledger: &mut Ledger,
) -> HarnessReport {
    let harness = adapter.name().to_string();
    let mut report = HarnessReport {
        harness: harness.clone(),
        delivered: Vec::new(),
        removed: Vec::new(),
        failure: None,
    };
    let mut failures: Vec<String> = Vec::new();

    for e in &slice.entries {
        if !matches!(e.action, Action::Remove) {
            continue;
        }
        let kind = e.id.kind;
        let name = e.id.name.clone();
        let label = e.id.label();

        // 撤销的**授权**只能来自账本（`ledger.find`）：`PlanEntry` 里没有路径，这是
        // 刻意的——计划层不该知道写在哪。账本同时给出 skill 的落点（记的就是交付时
        // 写的那条路径）；MCP 不同——它是**配置文件里的一个键**，落点跟着 adapter 的
        // `mcp_target()` 走，与 diff 读的是同一处，才不会「报告说在这儿、删除却去了别处」。
        // `recorded.path` 在 MCP 这条路上因此只用来在报错时指认现场（下面的 None 臂）。
        let Some(recorded) = ledger.find(&harness, kind, &name) else {
            failures.push(format!("{label}：账本里没有这条，无法撤销"));
            continue;
        };
        let path = recorded.path.clone();

        let outcome = match kind {
            Kind::Skill => remove_any(&path),
            Kind::Mcp => match adapter.mcp_target() {
                Some(target) => match &target.mechanism {
                    McpMechanism::ClaudeCli { binary } => mcp_claude::claude_remove(binary, &name),
                    McpMechanism::CodexToml => mcp_toml::remove_codex_mcp(&target.path, &name),
                    McpMechanism::KimiJson => mcp_json::remove_kimi_mcp(&target.path, &name),
                },
                None => Err(anyhow::anyhow!(
                    "这家没有 MCP 落点，账本记录却指向 {}",
                    path.display()
                )),
            },
            // B3 只交付 skill 与 mcp，账本里因此只可能有这两种。剩下的 kind 若出现，
            // 说明账本被人手改过——报出来比猜一条删除路径安全。
            other => Err(anyhow::anyhow!("`{}` 还没有撤销路径", other.as_str())),
        };

        match outcome {
            Ok(()) => {
                report.removed.push(label);
                // §9.7：**只有成功才摘账**。失败时账本原样保留——
                // 「这里还有 henv 的东西」在失败时是事实，重跑还得靠它。
                ledger.remove(&harness, kind, &name);
            }
            Err(err) => failures.push(format!("{label}: {err:#}")),
        }
    }

    report.removed.sort();
    if !failures.is_empty() {
        report.failure = Some(failures.join("; "));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{self, CODEX};
    use crate::capability::CapId;
    use crate::diff::PlanEntry;
    use crate::store::provenance::{Ledger as L, LedgerEntry};
    use crate::testutil::TempHome;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn remove_entry(kind: Kind, name: &str) -> PlanEntry {
        PlanEntry::new(CapId::new(kind, name), Action::Remove)
    }

    fn slice(harness: &str, entries: Vec<PlanEntry>) -> HarnessPlan {
        HarnessPlan {
            harness: harness.into(),
            detected: true,
            entries,
            actual_by_kind: BTreeMap::new(),
        }
    }

    fn record(ledger: &mut L, kind: Kind, name: &str, path: PathBuf) {
        ledger.record(LedgerEntry {
            harness: CODEX.into(),
            kind,
            name: name.into(),
            path,
            source_id: "agents-dir".into(),
            content_hash: "h".into(),
            delivered_at_unix: 1_700_000_000,
        });
    }

    /// 正路：账本记着的 skill 拷贝被删掉，且**只有成功的那条**被摘账。
    #[test]
    fn a_recorded_skill_copy_is_removed_and_unrecorded() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/foo/SKILL.md", "# foo");

        let mut ledger = L::default();
        record(&mut ledger, Kind::Skill, "foo", t.path(".codex/skills/foo"));

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Skill, "foo")]),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["skill/foo"]);
        assert!(report.failure.is_none());
        assert!(report.delivered.is_empty(), "撤销不交付任何东西");
        assert!(!t.path(".codex/skills/foo").exists(), "目录要整棵删掉");
        assert!(
            ledger.find(CODEX, Kind::Skill, "foo").is_none(),
            "成功才摘账"
        );
    }

    /// 盘上已经没有了、账本还在 → 仍然是成功（目标状态已达成，§9.8），照常摘账。
    /// 重跑一次 `--prune` 走的就是这条路。
    #[test]
    fn removing_an_already_absent_copy_is_a_success() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();

        let mut ledger = L::default();
        record(&mut ledger, Kind::Skill, "foo", t.path(".codex/skills/foo"));

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Skill, "foo")]),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["skill/foo"]);
        assert!(report.failure.is_none());
        assert!(ledger.find(CODEX, Kind::Skill, "foo").is_none());
    }

    /// MCP 正路：`[mcp_servers.context7]` 被摘掉，别的键还在。
    #[test]
    fn a_recorded_codex_mcp_key_is_removed() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(
            ".codex/config.toml",
            "model = \"gpt-5\"\n\n[mcp_servers.context7]\ncommand = \"npx\"\n",
        );

        let mut ledger = L::default();
        record(
            &mut ledger,
            Kind::Mcp,
            "context7",
            t.path(".codex/config.toml"),
        );

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Mcp, "context7")]),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["mcp/context7"]);
        let text = std::fs::read_to_string(t.path(".codex/config.toml")).unwrap();
        assert!(!text.contains("context7"), "{text}");
        assert!(text.contains("model"), "{text}");
        assert!(ledger.find(CODEX, Kind::Mcp, "context7").is_none());
    }

    /// Review Focus 3（失败隔离）：一条删不掉，其余照删，且**只有成功的那条被摘账**。
    /// 这里用一份坏掉的 `config.toml` 让 MCP 那条失败——skill 那条不受影响。
    #[test]
    fn one_failure_does_not_stop_the_rest_and_only_successes_are_unrecorded() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/foo/SKILL.md", "# foo");
        t.write(".codex/config.toml", "[[[not toml\n");

        let mut ledger = L::default();
        record(&mut ledger, Kind::Skill, "foo", t.path(".codex/skills/foo"));
        record(
            &mut ledger,
            Kind::Mcp,
            "context7",
            t.path(".codex/config.toml"),
        );

        let report = prune_harness(
            adapter,
            &slice(
                CODEX,
                vec![
                    remove_entry(Kind::Mcp, "context7"),
                    remove_entry(Kind::Skill, "foo"),
                ],
            ),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["skill/foo"], "成功的那条照删");
        let why = report.failure.expect("失败要被报出来");
        assert!(why.contains("mcp/context7"), "{why}");
        assert!(!t.path(".codex/skills/foo").exists());
        assert!(ledger.find(CODEX, Kind::Skill, "foo").is_none());
        assert!(
            ledger.find(CODEX, Kind::Mcp, "context7").is_some(),
            "失败的那条不许摘账——否则重跑就没有依据了"
        );
    }

    /// Review Focus 1 的执行层一半：**非 `Remove` 的条目一个都不许动**。
    /// 账本里就算记着它，计划判的是 drift，执行层就不许越过计划自己改主意。
    #[test]
    fn non_remove_entries_are_never_touched() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/handmade/SKILL.md", "# handmade");
        let before = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();

        let mut ledger = L::default();
        record(
            &mut ledger,
            Kind::Skill,
            "handmade",
            t.path(".codex/skills/handmade"),
        );

        let report = prune_harness(
            adapter,
            &slice(
                CODEX,
                vec![PlanEntry::new(
                    CapId::new(Kind::Skill, "handmade"),
                    Action::Drift,
                )],
            ),
            &mut ledger,
        );

        assert!(report.removed.is_empty());
        assert!(report.failure.is_none());
        assert_eq!(
            std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
            before
        );
        assert!(t.path(".codex/skills/handmade").exists());
    }

    /// 计划说要删、账本里却没有 → **不猜路径、不删**，报出来。
    /// `prune_plan` 不会产这种计划，所以真出现了就是计划与账本不同源。
    #[test]
    fn a_remove_without_a_ledger_record_is_reported_and_deletes_nothing() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/foo/SKILL.md", "# foo");

        let mut ledger = L::default();

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Skill, "foo")]),
            &mut ledger,
        );

        assert!(report.removed.is_empty());
        assert!(
            report.failure.unwrap().contains("账本里没有这条"),
            "要说清为什么不删"
        );
        assert!(t.path(".codex/skills/foo").exists(), "没有账本授权就不许删");
    }
}
