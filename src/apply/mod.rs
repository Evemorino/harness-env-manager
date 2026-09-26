//! 执行层（spec §9.2/§9.7）。**这是全项目唯一落盘的地方**（§9.9 白名单）。

use crate::capability::{Capability, Kind};
use crate::diff::{Action, HarnessPlan, Plan};
use crate::home::Home;
use crate::store::hash::hash_dir;
use crate::store::provenance::{Ledger, LedgerEntry};
use crate::store::skills::ContentIndex;
use anyhow::Context as _;
use std::path::Path;

pub mod mcp;
pub mod mcp_claude;
pub mod mcp_json;
pub mod mcp_toml;
pub mod prune;
pub mod write;

pub use write::deliver_skill;

/// 一家 harness 的交付结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessReport {
    pub harness: String,
    /// 本次真正交付的条目 label（如 `skill/foo`），按 label 排好序。
    pub delivered: Vec<String>,
    /// 本次真正**撤销**掉的条目 label（如 `skill/foo`）。只有 `--prune` 会填它；
    /// 撤销不交付任何东西，所以这两个列表在同一次运行里互斥。
    pub removed: Vec<String>,
    /// `Some` = 这家只做成了一部分（§9.7 失败隔离）。没有回滚，重跑就是修复路径。
    pub failure: Option<String>,
}

impl HarnessReport {
    /// 把同一家的第二批交付并进第一批（§9.6：一家有两种写路径，报告只有一个）。
    ///
    /// `delivered` 合并后重排——「按 label 排好序」是这个类型的契约，
    /// 两份各自有序不保证合并后有序。
    pub fn absorb(&mut self, other: HarnessReport) {
        self.delivered.extend(other.delivered);
        self.delivered.sort();
        self.removed.extend(other.removed);
        self.removed.sort();
        if let Some(f) = other.failure {
            self.failure = Some(match self.failure.take() {
                Some(prev) => format!("{prev}; {f}"),
                None => f,
            });
        }
    }
}

/// 一家 harness 的 skill 交付（§9.2 第 2 步）。
///
/// `target_dir` 为 `None`（这家没有用户级 skills 目录）时返回一份空报告——
/// 那不是失败，是这家的事实。
///
/// 失败隔离（§9.7）：单条失败**不中断**这一家剩下的条目，也不影响别家；
/// 失败的条目**不记账**（没交付成功就不该声称交付过）。
/// 返回 `Err` 只留给「这家压根没法开始」的情况（目标目录建不出来）。
pub fn deliver_skills(
    harness: &str,
    target_dir: Option<&Path>,
    slice: &HarnessPlan,
    content: &ContentIndex,
    ledger: &mut Ledger,
) -> anyhow::Result<HarnessReport> {
    let mut report = HarnessReport {
        harness: harness.to_string(),
        delivered: Vec::new(),
        removed: Vec::new(),
        failure: None,
    };
    let Some(target_dir) = target_dir else {
        return Ok(report);
    };

    let mut failures: Vec<String> = Vec::new();
    for e in &slice.entries {
        // 只有 install / update 是「把内容放过去」。其余动作都是「不该放」
        // ——冲突与不可交付由上层拦，in-sync / unknown / drift 没有内容要放。
        if !matches!(e.action, Action::Install | Action::Update { .. }) {
            continue;
        }
        // 源没解析出来（git / bundle / 源不存在）→ 内容层给不出路径。
        let Some(src) = content.sources.get(&e.id) else {
            // 内容层只解析 skill（`Spec::Skill` 是唯一的来源形状），所以 mcp 与 plugin
            // 的 install 条目走到这里时「没有内容」是**正常**的：它们的交付路径还没做
            // （B3）。把那种情况报成失败，会让每一次带 plugin 的 apply 都刷出一串
            // 假故障，真正该被看见的 skill 失败反而被淹掉。
            if e.id.kind == Kind::Skill {
                // skill 计划说要交付、内容层却没有它：这不是「跳过」，是「要做的没做成」。
                // 重跑是修复路径（§9.7），但重跑的前提是用户知道出事了。
                failures.push(format!("{}：源没有解析出内容，未交付", e.id.label()));
            }
            continue;
        };
        let target = target_dir.join(&e.id.name);
        match deliver_skill(&target, &src.path) {
            Ok(()) => {
                report.delivered.push(e.id.label());
                // §9.3：账本记的是**交付完成后目标上**的哈希，不是解析源时的值。
                // 用后者的话，「源在解析与交付之间被改动」会让账本与盘上内容不符，
                // 下一次 diff 报出一个没人能解释的 update。
                let content_hash = hash_dir(&target)
                    .with_context(|| format!("cannot hash {}", target.display()))?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{} 刚交付完却读不出内容——交付与账本会因此不一致",
                            target.display()
                        )
                    })?;
                ledger.record(LedgerEntry {
                    harness: harness.to_string(),
                    kind: e.id.kind,
                    name: e.id.name.clone(),
                    path: target,
                    source_id: src.source_id.clone(),
                    content_hash,
                    delivered_at_unix: now_unix(),
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

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 全部 harness 的交付结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyReport {
    pub harnesses: Vec<HarnessReport>,
}

impl ApplyReport {
    /// 只做成一半的 harness：名字 + 失败原因（§9.7 的 `partial`）。
    pub fn partial(&self) -> Vec<(&str, &str)> {
        self.harnesses
            .iter()
            .filter_map(|h| h.failure.as_deref().map(|f| (h.harness.as_str(), f)))
            .collect()
    }

    pub fn delivered_total(&self) -> usize {
        self.harnesses.iter().map(|h| h.delivered.len()).sum()
    }

    pub fn removed_total(&self) -> usize {
        self.harnesses.iter().map(|h| h.removed.len()).sum()
    }
}

/// 执行计划（§9.2 第 2 步）。**全项目唯一会往 `Home` 下写东西的函数。**
///
/// 冲突检查在**任何**落盘之前做完：§9.3 的「停下报错」如果做成「做到一半停下」，
/// 另一半就已经被改动了，而不变式二承诺的是「绝不覆盖」。
///
/// harness 级的失败进 `HarnessReport.failure` 而不上抛（失败隔离，§9.7）；
/// 返回 `Err` 只留给冲突与「压根没法开始」。没有回滚命令——重跑就是修复路径。
///
/// `desired` 是清单 lower 后的能力表——MCP 的交付要拿它的三段载荷
/// （`plan` 里只有 id 与动作，没有要写进去的字节）。
pub fn execute(
    plan: &Plan,
    desired: &[Capability],
    content: &ContentIndex,
    ledger: &mut Ledger,
    home: &Home,
) -> anyhow::Result<ApplyReport> {
    let conflicts = plan.conflicts();
    anyhow::ensure!(
        conflicts.is_empty(),
        "refusing to apply, {} name conflict(s) found:\n{}",
        conflicts.len(),
        conflicts
            .iter()
            .map(|e| format!(
                "  {} ({})",
                e.id.label(),
                match &e.action {
                    Action::Conflict { reason } => reason.as_str(),
                    _ => "",
                }
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let registry = crate::adapters::registry(home);
    let mut report = ApplyReport::default();
    for hp in &plan.harnesses {
        // 未安装的那家不跑：它的配置树不存在，跑它等于凭空造出一家的目录。
        if !hp.detected {
            continue;
        }
        let Some(adapter) = crate::adapters::lookup(&registry, &hp.harness) else {
            continue;
        };
        let mut harness_report = match adapter.apply(hp, desired, content, ledger) {
            Ok(r) => r,
            // adapter 级失败（连目标目录都建不出来）——也走 partial，不上抛。
            Err(e) => HarnessReport {
                harness: hp.harness.clone(),
                delivered: Vec::new(),
                removed: Vec::new(),
                failure: Some(format!("{e:#}")),
            },
        };
        // §9.8：撤销与交付隔离在**条目**这一级，所以两半各跑各的、合成一份报告。
        // 计划里没有 `Remove` 条目时这是一个空报告——`--prune` 之外零影响，
        // 因为 `diff::compute` 与 `diff` 都不产 `Remove`。
        harness_report.absorb(prune::prune_harness(adapter, hp, ledger));
        report.harnesses.push(harness_report);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::CLAUDE_CODE;
    use crate::capability::{CapId, Kind, SkillSource, Spec};
    use crate::diff::{Action, HarnessPlan, Plan, PlanEntry};
    use crate::store::provenance::{LEDGER_VERSION, Ledger};
    use crate::store::skills::{ContentIndex, ResolvedContent, TargetContent};
    use crate::testutil::TempHome;

    /// 一份「清单要 foo、目标不存在」的最小场景。
    fn scenario(t: &TempHome) -> (HarnessPlan, ContentIndex) {
        t.write("src/foo/SKILL.md", "# foo");
        let id = CapId::new(Kind::Skill, "foo");
        let mut content = ContentIndex::default();
        content.sources.insert(
            id.clone(),
            ResolvedContent {
                path: t.path("src/foo"),
                hash: "ignored-on-purpose".into(),
                source_id: "agents-dir".into(),
                stale: None,
            },
        );
        content
            .targets
            .entry("claude-code".into())
            .or_default()
            .insert("foo".into(), TargetContent::NoContent);
        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            entries: vec![PlanEntry::new(id, Action::Install)],
            actual_by_kind: Default::default(),
        };
        (slice, content)
    }

    #[test]
    fn a_delivered_skill_is_recorded_with_the_hash_recomputed_after_delivery() {
        // Review Focus 2（TOCTOU）：账本记的必须是**交付完成后目标上**的哈希。
        // 这里故意喂一个错的源哈希——若实现偷懒存了 sources 里的值，这条会红。
        let t = TempHome::new();
        let (slice, content) = scenario(&t);
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();

        assert_eq!(report.delivered, vec!["skill/foo".to_string()]);
        assert!(report.failure.is_none());
        let entry = ledger
            .find("claude-code", Kind::Skill, "foo")
            .expect("must be recorded");
        assert_eq!(entry.source_id, "agents-dir");
        assert_eq!(entry.path, t.path(".claude/skills/foo"));
        assert_ne!(entry.content_hash, "ignored-on-purpose");
        assert_eq!(
            Some(entry.content_hash.clone()),
            hash_dir(&t.path(".claude/skills/foo")).unwrap(),
            "账本哈希必须等于交付后目标内容哈希"
        );
        assert_eq!(ledger.version, LEDGER_VERSION);
        assert!(entry.delivered_at_unix > 0);
    }

    #[test]
    fn a_failing_entry_does_not_stop_the_others() {
        // §9.7 失败隔离：单条失败不中断其余条目，失败要报出来。
        let t = TempHome::new();
        t.write("src/ok/SKILL.md", "# ok");
        let id_bad = CapId::new(Kind::Skill, "bad");
        let id_ok = CapId::new(Kind::Skill, "ok");
        let mut content = ContentIndex::default();
        // 一个不存在的源路径：哈希字段有没有都不影响，deliver 会在 canonicalize 处失败。
        content.sources.insert(
            id_bad.clone(),
            ResolvedContent {
                path: t.path("src/does-not-exist"),
                hash: "h".into(),
                source_id: "agents-dir".into(),
                stale: None,
            },
        );
        content.sources.insert(
            id_ok.clone(),
            ResolvedContent {
                path: t.path("src/ok"),
                hash: "h".into(),
                source_id: "agents-dir".into(),
                stale: None,
            },
        );
        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            // 按 label 排序：bad 在前，证明后面的 ok 仍然被交付。
            entries: vec![
                PlanEntry::new(id_bad, Action::Install),
                PlanEntry::new(id_ok, Action::Install),
            ],
            actual_by_kind: Default::default(),
        };
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();

        assert_eq!(report.delivered, vec!["skill/ok".to_string()]);
        let failure = report.failure.expect("失败必须被报出来");
        assert!(failure.contains("skill/bad"), "{failure}");
        assert!(t.path(".claude/skills/ok/SKILL.md").exists());
        assert!(
            ledger.find("claude-code", Kind::Skill, "bad").is_none(),
            "失败的条目不记账"
        );
    }

    #[test]
    fn entries_that_are_not_deliverable_are_skipped() {
        let t = TempHome::new();
        let (mut slice, content) = scenario(&t);
        slice.entries = vec![
            PlanEntry::new(
                CapId::new(Kind::Skill, "foo"),
                Action::Undeliverable { reason: "x".into() },
            ),
            PlanEntry::new(CapId::new(Kind::Skill, "foo"), Action::InSync),
            PlanEntry::new(CapId::new(Kind::Skill, "foo"), Action::Unknown),
            PlanEntry::new(CapId::new(Kind::Skill, "foo"), Action::Drift),
        ];
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();
        assert!(report.delivered.is_empty());
        assert!(report.failure.is_none());
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn a_planned_delivery_without_content_is_a_reported_failure() {
        // 计划说要装，内容层却没有它。原来这里是 `continue`——一个字节不写，
        // 也一个字不说：用户看到的是「我说了 y，然后什么都没发生」。
        // 就算 diff 已经把这种条目变成可见的 undeliverable（B2），这一层也要
        // 自己兜住：万一以后有人改了 `compute`，坏消息必须仍有一个出口。
        let t = TempHome::new();
        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            entries: vec![PlanEntry::new(
                CapId::new(Kind::Skill, "ghost"),
                Action::Install,
            )],
            actual_by_kind: Default::default(),
        };
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &ContentIndex::default(),
            &mut ledger,
        )
        .unwrap();

        let failure = report.failure.expect("要交付却没有内容，必须报出来");
        assert!(
            failure.contains("skill/ghost"),
            "要点名是哪一条，实际：{failure}"
        );
        assert!(!t.path(".claude/skills/ghost").exists(), "什么都没写才对");
        assert!(ledger.entries.is_empty(), "失败的条目不记账");
    }

    #[test]
    fn a_plugin_install_without_content_is_not_a_failure() {
        // 这条是上一条的绊线，钉住它的作用范围。内容层只解析 skill（`Spec::Skill`
        // 是唯一的来源形状），所以 plugin / mcp 的 install 条目「没有内容」是**正常**
        // 状态——它们的交付路径还没做（B3）。无差别地把那段 `continue` 改成失败，
        // 每一次带 plugin 的 apply 都会刷出一串假故障，真正的 skill 失败反而被淹掉。
        let t = TempHome::new();
        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            entries: vec![PlanEntry::new(
                CapId::new(Kind::Plugin, "superpowers"),
                Action::Install,
            )],
            actual_by_kind: Default::default(),
        };
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &ContentIndex::default(),
            &mut ledger,
        )
        .unwrap();

        assert!(
            report.failure.is_none(),
            "plugin 的交付还没做，那不是失败，实际 {report:?}"
        );
    }

    #[test]
    fn the_delivered_content_matches_the_source() {
        // 3、4 两步合起来就是 §9.10 的 roundtrip 断言：交付完的内容是清单要的。
        let t = TempHome::new();
        let (slice, content) = scenario(&t);
        let mut ledger = Ledger::default();
        deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();
        assert_eq!(
            hash_dir(&t.path(".claude/skills/foo")).unwrap(),
            hash_dir(&t.path("src/foo")).unwrap()
        );
    }

    /// 在一个 TempHome 上真的跑一次 execute：claude 装了、codex 装了、kimi 没装。
    fn end_to_end(t: &TempHome) -> (Plan, Vec<Capability>, ContentIndex) {
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");
        let manifest = crate::manifest::Manifest::parse("version: 1\n").unwrap();
        let desired = vec![crate::capability::Capability {
            id: CapId::new(Kind::Skill, "foo"),
            origin: crate::capability::Origin::Manifest,
            spec: Spec::Skill {
                source: SkillSource::Local {
                    path: t.path("src/foo"),
                },
                path: None,
            },
            targets: crate::capability::Targets::All,
        }];
        let registry = crate::adapters::registry(&t.home);
        let states: Vec<crate::diff::HarnessState> = registry
            .iter()
            .map(|a| crate::diff::HarnessState {
                name: a.name().to_string(),
                detected: a.detect(),
                actual: a.read_state().expect("read_state"),
            })
            .collect();
        let content = ContentIndex {
            sources: crate::store::skills::resolve_sources(
                &desired,
                &t.home,
                crate::store::git::SourceMode::Fetch,
                &crate::store::skills::BundleTable::new(),
            )
            .unwrap()
            .sources,
            targets: Default::default(),
            unresolved: Default::default(),
            notes: Default::default(),
        };
        let inputs: Vec<crate::diff::HarnessInput> = states
            .iter()
            .zip(registry.iter())
            .map(|(state, adapter)| crate::diff::HarnessInput {
                state,
                adapter: &**adapter,
            })
            .collect();
        let plan = crate::diff::compute(
            &desired,
            &inputs,
            &crate::lower::DegradeTable::new(&manifest),
            &content,
            &Ledger::default(),
            &crate::lower::SourceHarnessTable::default(),
        );
        (plan, desired, content)
    }

    #[test]
    fn execute_delivers_to_both_harnesses_that_have_a_skills_directory() {
        let t = TempHome::new();
        let (plan, desired, content) = end_to_end(&t);
        let mut ledger = Ledger::default();
        let report = execute(&plan, &desired, &content, &mut ledger, &t.home).unwrap();

        let names: Vec<&str> = report
            .harnesses
            .iter()
            .map(|h| h.harness.as_str())
            .collect();
        assert_eq!(
            names,
            vec![CLAUDE_CODE, "codex"],
            "kimi 没装 → 不进报告；未安装的 harness 不该被跑"
        );
        assert!(report.partial().is_empty(), "没有失败就不该有 partial");
        assert_eq!(report.delivered_total(), 2);
        assert!(t.path(".claude/skills/foo/SKILL.md").exists());
        assert!(t.path(".codex/skills/foo/SKILL.md").exists());
        assert!(!t.path(".kimi-code/skills").exists(), "kimi 没有这个目录");
        assert!(ledger.find(CLAUDE_CODE, Kind::Skill, "foo").is_some());
        assert!(ledger.find("codex", Kind::Skill, "foo").is_some());
    }

    #[test]
    fn execute_refuses_the_whole_plan_when_there_is_a_conflict() {
        // §9.3 不变式二 + 「停下」的字面意思：一个字节都不许写，
        // 包括别的 harness 那一份。
        let t = TempHome::new();
        let (mut plan, desired, content) = end_to_end(&t);
        // 给 claude 造一个冲突，给 codex 造一个正常的 install。
        let conflict = PlanEntry::new(
            CapId::new(Kind::Skill, "taken"),
            Action::Conflict {
                reason: "目标已存在同名条目且不是 henv 交付的".into(),
            },
        );
        plan.harnesses[0].entries.push(conflict);
        let mut ledger = Ledger::default();
        let err = format!(
            "{:#}",
            execute(&plan, &desired, &content, &mut ledger, &t.home).unwrap_err()
        );
        assert!(err.contains("skill/taken"), "{err}");
        assert!(
            !t.path(".claude/skills/foo").exists() && !t.path(".codex/skills/foo").exists(),
            "冲突必须阻断全部交付，而不是做到一半停下"
        );
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn one_harnesss_failure_leaves_the_other_alone() {
        // §9.7 失败隔离跨 harness 也成立。
        let t = TempHome::new();
        let (plan, desired, mut content) = end_to_end(&t);
        // 把源换成不存在的路径：两家都会失败，但都只报 partial，不整体报错。
        for src in content.sources.values_mut() {
            src.path = t.path("src/gone");
        }
        let mut ledger = Ledger::default();
        let report = execute(&plan, &desired, &content, &mut ledger, &t.home).unwrap();
        assert_eq!(report.harnesses.len(), 2);
        assert_eq!(report.partial().len(), 2, "两家各自报自己的失败");
        assert_eq!(report.delivered_total(), 0);
        assert!(ledger.entries.is_empty());
    }

    /// §9.6：一家有两种写路径，报告只有一个。合并后 `delivered` 必须重排——
    /// 两份各自有序不保证合并后有序，而「按 label 排好序」是这个类型的契约。
    #[test]
    fn absorb_merges_two_reports_of_the_same_harness() {
        let mut skills = HarnessReport {
            harness: "claude-code".into(),
            delivered: vec!["skill/z".into()],
            removed: Vec::new(),
            failure: Some("skill/a: boom".into()),
        };
        skills.absorb(HarnessReport {
            harness: "claude-code".into(),
            delivered: vec!["mcp/context7".into(), "mcp/b".into()],
            removed: Vec::new(),
            failure: Some("mcp/c: also boom".into()),
        });

        assert_eq!(
            skills.delivered,
            vec!["mcp/b", "mcp/context7", "skill/z"],
            "合并后必须重排：mcp/b 排在 mcp/context7 前面，两者都在 skill/z 前面"
        );
        assert_eq!(
            skills.failure.as_deref(),
            Some("skill/a: boom; mcp/c: also boom"),
            "两半的失败都要留着，不能被后一半覆盖掉"
        );
        assert_eq!(skills.harness, "claude-code", "harness 名以收方那份为准");
    }

    /// 收方本来没有失败时，`absorb` 不该凭空造出 `Some("; ...")` 这种带分隔符的形状。
    #[test]
    fn absorb_takes_the_failure_when_there_was_none() {
        let mut skills = HarnessReport {
            harness: "codex".into(),
            delivered: Vec::new(),
            removed: Vec::new(),
            failure: None,
        };
        skills.absorb(HarnessReport {
            harness: "codex".into(),
            delivered: vec!["mcp/context7".into()],
            removed: Vec::new(),
            failure: Some("mcp/context7: nope".into()),
        });
        assert_eq!(skills.failure.as_deref(), Some("mcp/context7: nope"));
        assert_eq!(skills.delivered, vec!["mcp/context7"]);
    }

    /// `--prune` 的删除结果与交付结果同构：合并后重排，且两份一条都不丢。
    #[test]
    fn absorb_merges_removals_too() {
        let mut a = HarnessReport {
            harness: "claude-code".into(),
            delivered: vec!["skill/z".into()],
            removed: vec!["mcp/context7".into()],
            failure: None,
        };
        a.absorb(HarnessReport {
            harness: "claude-code".into(),
            delivered: Vec::new(),
            removed: vec!["skill/a".into(), "mcp/b".into()],
            failure: None,
        });

        assert_eq!(a.removed, vec!["mcp/b", "mcp/context7", "skill/a"]);
        assert_eq!(a.delivered, vec!["skill/z"], "交付那一半不受影响");
    }

    /// 账本保存条件读它。少了这条，删成功却不落盘的那次运行会把已删条目
    /// 在下一次 diff 里再报一遍 drift。
    #[test]
    fn removed_total_sums_across_harnesses() {
        let r = ApplyReport {
            harnesses: vec![
                HarnessReport {
                    harness: "claude-code".into(),
                    delivered: Vec::new(),
                    removed: vec!["skill/x".into()],
                    failure: None,
                },
                HarnessReport {
                    harness: "codex".into(),
                    delivered: Vec::new(),
                    removed: vec!["skill/y".into(), "mcp/z".into()],
                    failure: None,
                },
            ],
        };
        assert_eq!(r.removed_total(), 3);
        assert_eq!(r.delivered_total(), 0);
    }

    #[test]
    fn execute_skips_an_undetected_harness() {
        // 没装的那家不跑：它的 skills 目录不存在，跑它等于无端建目录
        // ——那不是「让目标变成清单的样子」，是凭空造出一家的配置树。
        let t = TempHome::new();
        let (mut plan, desired, content) = end_to_end(&t);
        plan.harnesses[0].detected = false;
        let mut ledger = Ledger::default();
        let report = execute(&plan, &desired, &content, &mut ledger, &t.home).unwrap();
        assert_eq!(report.harnesses.len(), 1);
        assert!(!t.path(".claude/skills/foo").exists());
        assert!(t.path(".codex/skills/foo").exists(), "另一家照常交付");
    }
}
