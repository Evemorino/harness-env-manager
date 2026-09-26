//! diff：`f(desired, actual) → Plan`，纯函数、幂等、只读（spec §3 决策 2）。
//!
//! 清单定义「必须有的」，不定义「不许有的」：actual 多出来的报 drift 但**不**删，
//! 删除需显式 `apply --prune`（Plan B）。故 `Counts::remove` 在本计划中恒为 0。
//!
//! 已知局限：remote MCP（仅 url、无 command）在 adapter 层归一为空 `Spec::Mcp`，diff 只按
//! name 比较身份，解析延后到 Plan B。

use crate::adapters::HarnessAdapter;
use crate::capability::{
    CapId, Capability, Kind, SkillSource, Spec, Support, VersionMatch, compare_versions,
};
use crate::lower::DegradeTable;
use crate::manifest::DegradeAction;
use crate::store::provenance::Ledger;
use crate::store::skills::{ContentIndex, TargetContent};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessState {
    pub name: String,
    pub detected: bool,
    pub actual: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 目标位置已存在一个**内容不同、又不是 henv 交付的**同名条目（§9.3 不变式二）。
    /// `diff` 把它显式报出来，好让人自己决定；`apply` 见到它就**停下报错、一个字节都不写**。
    ///
    /// 它排在 `rank()` 最前：冲突是阻断性的，其余动作都是「能做什么」，
    /// 只有它决定「能不能做」。
    Conflict {
        reason: String,
    },
    Install,
    /// from = 现状，to = 期望
    Update {
        from: String,
        to: String,
    },
    InSync,
    Undeliverable {
        reason: String,
    },
    Drift,
    /// 版本判不了（spec §2.1 第 3 条）：不改装、不报 update，但也**不**假装同步。
    Unknown,
}

impl Action {
    /// 输出分组顺序：conflict → install → update → undeliverable → drift → unknown → in-sync
    pub fn rank(&self) -> u8 {
        match self {
            Action::Conflict { .. } => 0,
            Action::Install => 1,
            Action::Update { .. } => 2,
            Action::Undeliverable { .. } => 3,
            Action::Drift => 4,
            Action::Unknown => 5,
            Action::InSync => 6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub id: CapId,
    pub action: Action,
    /// 可选的附加说明，渲染成行尾的 ` [note]`。内容层用它标注 §9.3 的
    /// 「副本被手改过」——动作本身仍是 `update`，但人要知道这次覆盖掉了什么。
    pub note: Option<String>,
}

impl PlanEntry {
    pub fn new(id: CapId, action: Action) -> Self {
        PlanEntry {
            id,
            action,
            note: None,
        }
    }

    /// 没有注记时是空操作——内容层缺席的路径因此不必写 `if let`。
    pub fn with_note(mut self, note: Option<String>) -> Self {
        self.note = note;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessPlan {
    pub harness: String,
    pub detected: bool,
    pub entries: Vec<PlanEntry>,
    /// in-sync 汇总行要显示的各类实有数量
    pub actual_by_kind: BTreeMap<Kind, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub harnesses: Vec<HarnessPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub conflict: usize,
    pub install: usize,
    pub update: usize,
    pub remove: usize,
    pub undeliverable: usize,
    pub drift: usize,
    pub unknown: usize,
}

impl Plan {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for h in &self.harnesses {
            for e in &h.entries {
                match &e.action {
                    Action::Conflict { .. } => c.conflict += 1,
                    Action::Install => c.install += 1,
                    Action::Update { .. } => c.update += 1,
                    Action::Undeliverable { .. } => c.undeliverable += 1,
                    Action::Drift => c.drift += 1,
                    Action::Unknown => c.unknown += 1,
                    Action::InSync => {}
                }
            }
        }
        c
    }

    pub fn is_clean(&self) -> bool {
        let c = self.counts();
        c.conflict == 0
            && c.install == 0
            && c.update == 0
            && c.remove == 0
            && c.undeliverable == 0
            && c.drift == 0
            && c.unknown == 0
    }

    /// 全部同名冲突（§9.3 不变式二）。非空即「不许交付」——
    /// `apply` 在任何一次落盘之前必须先问这里。
    pub fn conflicts(&self) -> Vec<&PlanEntry> {
        self.harnesses
            .iter()
            .flat_map(|h| h.entries.iter())
            .filter(|e| matches!(e.action, Action::Conflict { .. }))
            .collect()
    }
}

pub struct HarnessInput<'a> {
    pub state: &'a HarnessState,
    pub adapter: &'a dyn HarnessAdapter,
}

/// 内容层给出的裁决：动作 + 可选的注记。
struct Verdict {
    action: Action,
    note: Option<String>,
}

/// §9.3 的内容裁决点。只有**清单侧解析出了内容**、且**目标侧扫到了这个 skill**
/// 时才走这里；任一条件不满足返回 `None`，`compute` 落回 Plan A 的 `compare_versions`。
///
/// 判定顺序不是随手排的：先判「内容相等」且**不看账本**，是因为本机那 14 条用户自建
/// 的软链内容与源一模一样，必须原样留着而不是被「修」成拷贝；把账本判断提前会把它们
/// 全变成冲突。
fn content_verdict(
    harness: &str,
    cap: &Capability,
    content: &ContentIndex,
    ledger: &Ledger,
) -> Option<Verdict> {
    let src = content.sources.get(&cap.id)?;
    let target = content.targets.get(harness)?.get(&cap.id.name)?;
    match target {
        TargetContent::NoContent => Some(Verdict {
            action: Action::Update {
                from: "断链".to_string(),
                to: short_hash(&src.hash),
            },
            note: None,
        }),
        TargetContent::Has(h) if h == &src.hash => Some(Verdict {
            action: Action::InSync,
            note: None,
        }),
        TargetContent::Has(h) => {
            let action = Action::Update {
                from: short_hash(h),
                to: short_hash(&src.hash),
            };
            match ledger.find(harness, &cap.id.name) {
                // 账本记的就是盘上这个哈希：内容是 henv 放的，也没人动过。
                Some(entry) if &entry.content_hash == h => Some(Verdict { action, note: None }),
                // 账本说这里该是别的哈希：有人手改过，或上次交付只成功了一半。
                // 两种情况都照常覆盖（它终究是 henv 放的），但报告要显式说出来。
                Some(_) => Some(Verdict {
                    action,
                    note: Some("副本被手改过".to_string()),
                }),
                // 盘上有内容、不是我们放的、又和清单要的不一样 → 不动它。
                None => Some(Verdict {
                    action: Action::Conflict {
                        reason: format!(
                            "目标已存在同名条目且不是 henv 交付的（{}），apply 不会覆盖",
                            short_hash(h)
                        ),
                    },
                    note: None,
                }),
            }
        }
    }
}

/// 报告里只显示哈希前 8 位：完整 64 位在终端里是噪音，而人要看的只是
/// 「两端是不是同一个东西」，8 位十六进制足够区分。
fn short_hash(h: &str) -> String {
    h.chars().take(8).collect()
}

/// `f(desired, actual) → Plan`，纯函数、幂等、只读（spec §3 决策 2）。
///
/// `content` 与 `ledger` 让「字节一不一样、是谁放的」参与判决（§9.3）。两者都为
/// 空时行为与 Plan A 逐字节相同——`ContentIndex::default()` + `Ledger::default()`
/// 就是那条路径。
pub fn compute(
    desired: &[Capability],
    inputs: &[HarnessInput],
    degrade: &DegradeTable,
    content: &ContentIndex,
    ledger: &Ledger,
) -> Plan {
    let mut harnesses = Vec::with_capacity(inputs.len());

    for input in inputs {
        let state = input.state;
        let harness = state.name.clone();

        let mut actual_by_kind: BTreeMap<Kind, usize> = BTreeMap::new();
        for a in &state.actual {
            *actual_by_kind.entry(a.id.kind).or_insert(0) += 1;
        }

        // 未安装的 harness 不产出「全部待装」的噪音，由 render 显示跳过原因
        if !state.detected {
            harnesses.push(HarnessPlan {
                harness,
                detected: false,
                entries: Vec::new(),
                actual_by_kind,
            });
            continue;
        }

        let mut entries: Vec<PlanEntry> = Vec::new();
        let mut matched: HashSet<CapId> = HashSet::new();

        let desired_plugins: HashSet<String> = desired
            .iter()
            .filter(|c| c.id.kind == Kind::Plugin && c.targets.includes(&harness))
            .map(|c| c.id.name.clone())
            .collect();

        for cap in desired {
            if !cap.targets.includes(&harness) {
                continue;
            }
            match input.adapter.supports(cap) {
                Support::Unsupported { reason } => {
                    // 这条 arm **不**写 `matched`（下面那个循环据此把 actual 里多出来的东西
                    // 报成 Drift）。前提假设：报 Unsupported 的 adapter 不会同时把这个能力列在
                    // `actual` 里。真有自相矛盾的 adapter 时，最坏结果是同一条能力出现两行
                    // （undeliverable + drift），而不是静默的假 in-sync。
                    if degrade.action(cap) == DegradeAction::Report {
                        entries.push(PlanEntry::new(
                            cap.id.clone(),
                            Action::Undeliverable { reason },
                        ));
                    }
                }
                Support::Native | Support::Degraded { .. } => {
                    matched.insert(cap.id.clone());
                    let (verdict_action, verdict_note) =
                        match content_verdict(&state.name, cap, content, ledger) {
                            Some(v) => (Some(v.action), v.note),
                            None => (None, None),
                        };
                    let action = match state.actual.iter().find(|a| a.id == cap.id) {
                        None => Action::Install,
                        Some(a) => match &a.spec {
                            // 装了但被禁用：plugin 的 skills/hooks/commands 一个都不生效，报 in-sync
                            // 等于把「切 harness 会失效」反过来念。这里用 Undeliverable + reason，与
                            // `Support::Unsupported`（harness 支持不了）区分开：这条是用户自己关的。
                            Spec::Plugin { enabled: false, .. } => Action::Undeliverable {
                                reason: "plugin 已安装但未启用".to_string(),
                            },
                            // 内容层的裁决盖过版本比较：在能读到内容的地方，
                            // 「字节一不一样」比「版本串一不一样」是更强的证据。
                            _ => match verdict_action {
                                Some(a) => a,
                                None => match compare_versions(&cap.spec, &a.spec) {
                                    VersionMatch::Same => Action::InSync,
                                    VersionMatch::Different { from, to } => {
                                        Action::Update { from, to }
                                    }
                                    // 判不了不再折成 in-sync：报可见的 unknown（spec §2.1 第 3 条）
                                    VersionMatch::Unknown => Action::Unknown,
                                },
                            },
                        },
                    };
                    entries.push(PlanEntry::new(cap.id.clone(), action).with_note(verdict_note));
                }
            }
        }

        for a in &state.actual {
            if matched.contains(&a.id) {
                continue;
            }
            // plugin bundle 分发出来的 skill 不是漂移
            if let Spec::Skill {
                source: SkillSource::PluginBundle { plugin },
                ..
            } = &a.spec
            {
                if desired_plugins.contains(plugin) {
                    continue;
                }
            }
            entries.push(PlanEntry::new(a.id.clone(), Action::Drift));
        }

        entries.sort_by(|x, y| {
            x.action
                .rank()
                .cmp(&y.action.rank())
                .then_with(|| x.id.label().cmp(&y.id.label()))
        });

        harnesses.push(HarnessPlan {
            harness,
            detected: true,
            entries,
            actual_by_kind,
        });
    }

    Plan { harnesses }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Targets};
    use crate::lower::DegradeTable;
    use crate::manifest::Manifest;
    use crate::store::provenance::{Ledger, LedgerEntry};
    use crate::store::skills::{ContentIndex, ResolvedContent, TargetContent};
    use crate::testutil::{FakeAdapter, TempHome};
    use std::path::PathBuf;

    fn no_degrade() -> DegradeTable {
        DegradeTable::new(&Manifest::parse("version: 1\n").unwrap())
    }

    /// 既有测试的默认入参：内容层缺席 = Plan A 行为。
    fn plan_a(desired: &[Capability], inputs: &[HarnessInput]) -> Plan {
        compute(
            desired,
            inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
        )
    }

    fn desired(kind: Kind, name: &str, spec: Spec) -> Capability {
        Capability::new(CapId::new(kind, name), spec)
    }

    fn mcp_spec(cmd: &str) -> Spec {
        Spec::Mcp {
            command: cmd.into(),
            args: vec![],
            env: Default::default(),
        }
    }

    fn run(desired: &[Capability], adapter: FakeAdapter) -> HarnessPlan {
        let state = HarnessState {
            name: adapter.name().to_string(),
            detected: adapter.detect(),
            actual: adapter.read_state().unwrap(),
        };
        let inputs = vec![HarnessInput {
            state: &state,
            adapter: &adapter,
        }];
        let plan = plan_a(desired, &inputs);
        plan.harnesses.into_iter().next().unwrap()
    }

    fn labels(hp: &HarnessPlan) -> Vec<(String, String)> {
        hp.entries
            .iter()
            .map(|e| {
                (
                    e.id.label(),
                    match &e.action {
                        Action::Conflict { .. } => "conflict".to_string(),
                        Action::Install => "install".to_string(),
                        Action::Update { .. } => "update".to_string(),
                        Action::InSync => "in-sync".to_string(),
                        Action::Undeliverable { .. } => "undeliverable".to_string(),
                        Action::Drift => "drift".to_string(),
                        Action::Unknown => "unknown".to_string(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn missing_capability_is_install() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex"),
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/context7".to_string(), "install".to_string())]
        );
    }

    #[test]
    fn identical_capability_is_in_sync() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "npx")]),
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/context7".to_string(), "in-sync".to_string())]
        );
    }

    #[test]
    fn changed_command_is_update_with_actual_then_desired() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "old")]),
        );
        assert_eq!(hp.entries.len(), 1);
        match &hp.entries[0].action {
            Action::Update { from, to } => {
                assert_eq!(from, "old", "from = 现状");
                assert_eq!(to, "npx", "to = 期望");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_version_match_counts_as_unknown() {
        // codex 不记录 plugin 版本 → Unknown → 报可见的 ? unknown，而不是假装 in-sync
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: None,
                install_path: None,
                enabled: true,
            },
        );
        let actual = FakeAdapter::plugin("superpowers", None, None);
        let hp = run(&[d], FakeAdapter::native("codex").with_caps(vec![actual]));
        assert_eq!(
            labels(&hp),
            vec![("plugin/superpowers".to_string(), "unknown".to_string())]
        );
    }

    #[test]
    fn same_sha_across_differing_version_strings_is_update() {
        // spec §2.1 第 3 条修订后的核心场景：sha 相同不再判 Same（gitCommitSha 陈旧，
        // §2.2 第 7 条），version 6.4.1 vs v6.3.0 归一化后不同 → Different → update。
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: Some(sha.into()),
                install_path: None,
                enabled: true,
            },
        );
        let actual = FakeAdapter::plugin("superpowers", Some("6.4.1"), Some(sha));
        let hp = run(
            &[d],
            FakeAdapter::native("claude-code").with_caps(vec![actual]),
        );
        assert_eq!(
            labels(&hp),
            vec![("plugin/superpowers".to_string(), "update".to_string())]
        );
    }

    /// 用户显式关掉的 plugin **不能**报 in-sync。`Spec::Plugin.enabled` 三家 adapter 都读，
    /// 而在 `compute()` 里被丢掉；codex 的 plugin 没有 version/sha → `compare_versions` 给
    /// `Unknown` → 旧实现按 in-sync 处理。那就是把本工具的核心问题反过来回答：
    /// 「切 harness 会失效吗」对一条**现在就失效**的能力说了「不会」。
    #[test]
    fn a_disabled_actual_plugin_is_undeliverable_not_in_sync() {
        let d = desired(
            Kind::Plugin,
            "p",
            Spec::Plugin {
                marketplace: None,
                entry: "p".into(),
                version: Some("v1.0.0".into()),
                sha: None,
                install_path: None,
                enabled: true,
            },
        );
        let actual = FakeAdapter::cap(
            Kind::Plugin,
            "p",
            Spec::Plugin {
                marketplace: None,
                entry: "p".into(),
                version: None,
                sha: None,
                install_path: None,
                enabled: false,
            },
        );
        let hp = run(&[d], FakeAdapter::native("codex").with_caps(vec![actual]));

        assert_eq!(hp.entries.len(), 1, "{:?}", labels(&hp));
        assert_ne!(
            hp.entries[0].action,
            Action::InSync,
            "禁用 ≠ 已同步：这条断言就是本测试的全部意义"
        );
        match &hp.entries[0].action {
            Action::Undeliverable { reason } => assert_eq!(reason, "plugin 已安装但未启用"),
            other => panic!("禁用 plugin 必须报 undeliverable，得到 {other:?}"),
        }
    }

    /// 上一条的反方向守卫：新 arm 只认 `enabled: false`，正常启用的 plugin 仍走比较逻辑，
    /// 版本相同就是 in-sync（不许被新 arm 连坐成 undeliverable）。
    #[test]
    fn an_enabled_actual_plugin_with_a_matching_version_is_still_in_sync() {
        let d = desired(
            Kind::Plugin,
            "p",
            Spec::Plugin {
                marketplace: None,
                entry: "p".into(),
                version: Some("v1.0.0".into()),
                sha: None,
                install_path: None,
                enabled: true,
            },
        );
        let hp = run(
            &[d],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::plugin(
                "p",
                Some("v1.0.0"),
                None,
            )]),
        );
        assert_eq!(
            labels(&hp),
            vec![("plugin/p".to_string(), "in-sync".to_string())]
        );
    }

    #[test]
    fn unsupported_capability_is_undeliverable_with_reason() {
        let d = Capability {
            id: CapId::new(Kind::Skill, "my-draft"),
            origin: Origin::Manifest,
            spec: Spec::Skill {
                source: SkillSource::Local { path: "/x".into() },
                path: None,
            },
            targets: Targets::All,
        };
        let hp = run(
            &[d],
            FakeAdapter::native("kimi-code")
                .refusing(Kind::Skill, "kimi-code 无用户级 skills 目录"),
        );
        assert_eq!(hp.entries.len(), 1);
        assert_eq!(hp.entries[0].id.label(), "skill/my-draft");
        match &hp.entries[0].action {
            Action::Undeliverable { reason } => assert!(reason.contains("kimi-code"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn degrade_skip_suppresses_the_undeliverable_entry() {
        let man = Manifest::parse(
            "version: 1\nplugins:\n  sp:\n    source: github\n    repo: obra/superpowers\n    entry: superpowers\n    degrade:\n      hooks: skip\n",
        )
        .unwrap();
        let table = DegradeTable::new(&man);

        let hook = Capability {
            id: CapId::new(Kind::Hook, "superpowers:session-start"),
            origin: Origin::Plugin {
                name: "superpowers".into(),
            },
            spec: Spec::Hook {
                event: "SessionStart".into(),
                command: "run".into(),
            },
            targets: Targets::All,
        };
        let state = HarnessState {
            name: "kimi-code".into(),
            detected: true,
            actual: vec![],
        };
        let adapter = FakeAdapter::native("kimi-code").refusing(Kind::Hook, "no hook support");
        let inputs = vec![HarnessInput {
            state: &state,
            adapter: &adapter,
        }];

        let plan = compute(
            &[hook],
            &inputs,
            &table,
            &ContentIndex::default(),
            &Ledger::default(),
        );
        assert!(plan.harnesses[0].entries.is_empty(), "skip means silent");
    }

    #[test]
    fn degrade_report_keeps_the_undeliverable_entry() {
        let man = Manifest::parse(
            "version: 1\nplugins:\n  sp:\n    source: github\n    repo: obra/superpowers\n    entry: superpowers\n    degrade:\n      hooks: report\n",
        )
        .unwrap();
        let table = DegradeTable::new(&man);
        let hook = Capability {
            id: CapId::new(Kind::Hook, "superpowers:session-start"),
            origin: Origin::Plugin {
                name: "superpowers".into(),
            },
            spec: Spec::Hook {
                event: "SessionStart".into(),
                command: "run".into(),
            },
            targets: Targets::All,
        };
        let state = HarnessState {
            name: "kimi-code".into(),
            detected: true,
            actual: vec![],
        };
        let adapter = FakeAdapter::native("kimi-code").refusing(Kind::Hook, "no hook support");
        let inputs = vec![HarnessInput {
            state: &state,
            adapter: &adapter,
        }];

        let plan = compute(
            &[hook],
            &inputs,
            &table,
            &ContentIndex::default(),
            &Ledger::default(),
        );
        assert_eq!(plan.harnesses[0].entries.len(), 1);
        assert_eq!(plan.counts().undeliverable, 1);
    }

    #[test]
    fn extra_actual_capability_is_drift() {
        let hp = run(
            &[],
            FakeAdapter::native("kimi-code")
                .with_caps(vec![FakeAdapter::mcp("experimental-thing", "c")]),
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/experimental-thing".to_string(), "drift".to_string())]
        );
    }

    #[test]
    fn bundled_skill_of_a_desired_plugin_is_not_drift() {
        let plugin = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: None,
                sha: None,
                install_path: None,
                enabled: true,
            },
        );
        let actual = vec![
            FakeAdapter::plugin("superpowers", None, None),
            FakeAdapter::bundled_skill("brainstorming", "superpowers"),
            FakeAdapter::bundled_skill("test-driven-development", "superpowers"),
        ];
        let hp = run(
            &[plugin],
            FakeAdapter::native("kimi-code").with_caps(actual),
        );
        let got = labels(&hp);
        assert_eq!(
            got,
            vec![("plugin/superpowers".to_string(), "unknown".to_string())],
            "bundle 分发出来的 skill 不是漂移: {got:?}"
        );
    }

    #[test]
    fn bundled_skill_of_an_undesired_plugin_is_drift() {
        let hp = run(
            &[],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::bundled_skill(
                "brainstorming",
                "superpowers",
            )]),
        );
        assert_eq!(
            labels(&hp),
            vec![("skill/brainstorming".to_string(), "drift".to_string())]
        );
    }

    #[test]
    fn targets_exclude_harness_entirely() {
        let mut d = desired(Kind::Mcp, "context7", mcp_spec("npx"));
        d.targets = Targets::Only(vec!["codex".into()]);
        let hp = run(&[d], FakeAdapter::native("kimi-code"));
        assert!(hp.entries.is_empty(), "not targeted here → no entry at all");
    }

    #[test]
    fn targeted_elsewhere_but_installed_here_is_drift() {
        // spec §5「targets 不含它 → 忽略」+「actual 有、desired 无 → drift」的合流结果
        let mut d = desired(Kind::Mcp, "context7", mcp_spec("npx"));
        d.targets = Targets::Only(vec!["codex".into()]);
        let hp = run(
            &[d],
            FakeAdapter::native("kimi-code").with_caps(vec![FakeAdapter::mcp("context7", "npx")]),
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/context7".to_string(), "drift".to_string())]
        );
    }

    #[test]
    fn undetected_harness_produces_no_entries() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_detected(false),
        );
        assert!(!hp.detected);
        assert!(hp.entries.is_empty(), "未安装 ≠ 全部待装");
    }

    #[test]
    fn entries_are_grouped_by_action_then_label() {
        let actual = vec![
            FakeAdapter::mcp("synced", "npx"),
            FakeAdapter::mcp("zzz-drift", "c"),
            FakeAdapter::mcp("aaa-drift", "c"),
        ];
        let adapter = FakeAdapter::native("codex")
            .with_caps(actual)
            .refusing(Kind::Hook, "no hooks");
        let d = vec![
            desired(Kind::Mcp, "synced", mcp_spec("npx")),
            desired(Kind::Mcp, "new-one", mcp_spec("npx")),
            Capability {
                id: CapId::new(Kind::Hook, "h"),
                origin: Origin::Manifest,
                spec: Spec::Hook {
                    event: "E".into(),
                    command: "c".into(),
                },
                targets: Targets::All,
            },
        ];
        let hp = run(&d, adapter);
        let order: Vec<String> = hp.entries.iter().map(|e| e.id.label()).collect();
        assert_eq!(
            order,
            vec![
                "mcp/new-one",
                "hook/h",
                "mcp/aaa-drift",
                "mcp/zzz-drift",
                "mcp/synced"
            ],
            "install → undeliverable → drift → in-sync，组内按 label: {order:?}"
        );
    }

    #[test]
    fn actual_by_kind_counts_all_actual_capabilities() {
        let actual = vec![
            FakeAdapter::mcp("a", "c"),
            FakeAdapter::mcp("b", "c"),
            FakeAdapter::plugin("superpowers", None, None),
        ];
        let hp = run(&[], FakeAdapter::native("codex").with_caps(actual));
        assert_eq!(hp.actual_by_kind.get(&Kind::Mcp), Some(&2));
        assert_eq!(hp.actual_by_kind.get(&Kind::Plugin), Some(&1));
        assert_eq!(hp.actual_by_kind.get(&Kind::Skill), None);
    }

    #[test]
    fn counts_aggregate_across_harnesses() {
        let d = vec![desired(Kind::Mcp, "context7", mcp_spec("npx"))];
        let s1 = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![],
        };
        let s2 = HarnessState {
            name: "kimi-code".into(),
            detected: true,
            actual: vec![FakeAdapter::mcp("context7", "npx")],
        };
        let a1 = FakeAdapter::native("codex");
        let a2 = FakeAdapter::native("kimi-code");
        let inputs = vec![
            HarnessInput {
                state: &s1,
                adapter: &a1,
            },
            HarnessInput {
                state: &s2,
                adapter: &a2,
            },
        ];
        let plan = plan_a(&d, &inputs);
        let c = plan.counts();
        assert_eq!(c.install, 1);
        assert_eq!(c.update, 0);
        assert_eq!(c.remove, 0, "remove 恒为 0，prune 属 Plan B");
        assert_eq!(c.undeliverable, 0);
        assert_eq!(c.drift, 0);
        assert!(!plan.is_clean());
    }

    #[test]
    fn is_clean_when_everything_in_sync() {
        let d = vec![desired(Kind::Mcp, "context7", mcp_spec("npx"))];
        let s = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![FakeAdapter::mcp("context7", "npx")],
        };
        let a = FakeAdapter::native("codex");
        let plan = plan_a(
            &d,
            &[HarnessInput {
                state: &s,
                adapter: &a,
            }],
        );
        assert!(plan.is_clean());
    }

    #[test]
    fn is_clean_when_nothing_desired_and_nothing_installed() {
        let s = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![],
        };
        let a = FakeAdapter::native("codex");
        let plan = plan_a(
            &[],
            &[HarnessInput {
                state: &s,
                adapter: &a,
            }],
        );
        assert!(plan.is_clean());
    }

    #[test]
    fn unknown_counts_and_marks_plan_not_clean() {
        // 判不了 ≠ 干净：counts().unknown 参与统计，is_clean() 必须把它计入。
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: None,
                install_path: None,
                enabled: true,
            },
        );
        let state = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![FakeAdapter::plugin("superpowers", None, None)],
        };
        let adapter = FakeAdapter::native("codex");
        let plan = plan_a(
            &[d],
            &[HarnessInput {
                state: &state,
                adapter: &adapter,
            }],
        );
        assert_eq!(plan.counts().unknown, 1);
        assert!(!plan.is_clean(), "判不了 ≠ 干净");
    }

    #[test]
    fn degraded_support_follows_the_same_path_as_native() {
        // Plan A 无 adapter 构造 Degraded，但 diff 必须已就绪（Plan B 的拆解降级走这条）
        struct DegradingAdapter;
        impl crate::adapters::HarnessAdapter for DegradingAdapter {
            fn name(&self) -> &str {
                "codex"
            }
            fn detect(&self) -> bool {
                true
            }
            fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
                Ok(vec![])
            }
            fn supports(&self, _cap: &Capability) -> Support {
                Support::Degraded { as_kind: Kind::Mcp }
            }
            // 这个替身只喂纯函数 diff，永远不走交付路径；`None` 是它的事实陈述。
            fn skills_target_dir(&self) -> Option<PathBuf> {
                None
            }
        }
        let s = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![],
        };
        let a = DegradingAdapter;
        let plan = plan_a(
            &[desired(
                Kind::Plugin,
                "bundle",
                Spec::Plugin {
                    marketplace: None,
                    entry: "bundle".into(),
                    version: None,
                    sha: None,
                    install_path: None,
                    enabled: true,
                },
            )],
            &[HarnessInput {
                state: &s,
                adapter: &a,
            }],
        );
        assert_eq!(plan.counts().install, 1, "Degraded 与 Native 同路径");
    }

    /// 一条清单侧 skill 的能力（desired 形态）。
    fn wanted_skill(name: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Skill, name),
            origin: Origin::Manifest,
            spec: Spec::Skill {
                source: SkillSource::Local {
                    path: PathBuf::from(format!("/src/{name}")),
                },
                path: None,
            },
            targets: Targets::All,
        }
    }

    /// 说得出名字的源内容，绕过磁盘（内容层是纯函数，这里就该纯着测）。
    fn content_with(name: &str, source_hash: &str, target: TargetContent) -> ContentIndex {
        let id = CapId::new(Kind::Skill, name);
        let mut idx = ContentIndex::default();
        idx.sources.insert(
            id,
            ResolvedContent {
                path: PathBuf::from(format!("/src/{name}")),
                hash: source_hash.into(),
                source_id: "local:/src".into(),
            },
        );
        idx.targets
            .entry("claude-code".to_string())
            .or_default()
            .insert(name.to_string(), target);
        idx
    }

    fn ledger_claiming(name: &str, hash: &str) -> Ledger {
        let mut l = Ledger::default();
        l.record(LedgerEntry {
            harness: "claude-code".into(),
            kind: Kind::Skill,
            name: name.into(),
            path: PathBuf::from("/target").join(name),
            source_id: "local:/src".into(),
            content_hash: hash.into(),
            delivered_at_unix: 1,
        });
        l
    }

    fn content_plan(actual: Vec<Capability>, content: &ContentIndex, ledger: &Ledger) -> Plan {
        let fake = FakeAdapter::native("claude-code").with_caps(actual);
        let state = HarnessState {
            name: "claude-code".into(),
            detected: true,
            actual: fake.caps.clone(),
        };
        let inputs = vec![HarnessInput {
            state: &state,
            adapter: &fake,
        }];
        compute(
            &[wanted_skill("foo")],
            &inputs,
            &DegradeTable::new(&Manifest::parse("version: 1\n").unwrap()),
            content,
            ledger,
        )
    }

    fn first_entry(plan: &Plan) -> &PlanEntry {
        &plan.harnesses[0].entries[0]
    }

    /// 现状侧的一条 skill，路径落在临时 HOME 里（内容层不读它，只是要个真路径）。
    fn actual_skill(t: &TempHome, name: &str) -> Capability {
        FakeAdapter::cap(
            Kind::Skill,
            name,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(&format!(".claude/skills/{name}"))),
            },
        )
    }

    #[test]
    fn equal_content_is_in_sync_even_without_a_ledger_entry() {
        // §9.3 的核心：本机那 14 条用户自建的软链，内容与源相同 → 原样留着。
        // 若这里判成 Conflict 或 Update，那 14 条会被无端改写。
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "foo")];
        let content = content_with("foo", "aaaa", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &Ledger::default());
        assert!(matches!(first_entry(&plan).action, Action::InSync));
        assert!(first_entry(&plan).note.is_none());
    }

    #[test]
    fn differing_content_in_the_ledger_is_an_update() {
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "foo")];
        let content = content_with("foo", "bbbb", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &ledger_claiming("foo", "aaaa"));
        match &first_entry(&plan).action {
            Action::Update { from, to } => {
                assert_eq!(from, "aaaa", "from 是实有，短哈希");
                assert_eq!(to, "bbbb", "to 是清单要的");
            }
            other => panic!("期望 update，实际 {other:?}"),
        }
        assert!(
            first_entry(&plan).note.is_none(),
            "账本记的就是盘上这个哈希 → 没被手改过"
        );
    }

    #[test]
    fn a_hand_modified_copy_is_an_update_with_a_visible_note() {
        // 账本说这里该是 cccc，盘上却是 aaaa —— 有人动过。apply 会覆盖它
        // （它终究是 henv 放的），但报告必须说出来。
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "foo")];
        let content = content_with("foo", "bbbb", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &ledger_claiming("foo", "cccc"));
        assert!(matches!(first_entry(&plan).action, Action::Update { .. }));
        assert_eq!(
            first_entry(&plan).note.as_deref(),
            Some("副本被手改过"),
            "手改必须可见，不能静默覆盖"
        );
    }

    #[test]
    fn differing_content_outside_the_ledger_is_a_conflict() {
        // §9.3 不变式二：同名冲突停下报错，绝不覆盖。
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "foo")];
        let content = content_with("foo", "bbbb", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &Ledger::default());
        match &first_entry(&plan).action {
            Action::Conflict { reason } => {
                assert!(reason.contains("henv"), "理由要能读，实际 {reason}");
            }
            other => panic!("期望 conflict，实际 {other:?}"),
        }
        assert_eq!(plan.conflicts().len(), 1);
        assert!(!plan.is_clean(), "有冲突就不能算干净");
    }

    #[test]
    fn a_dangling_link_is_an_update_whatever_the_ledger_says() {
        // §9.3：断链没有内容，覆盖它不破坏任何东西——旧形态在重跑中自愈。
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "foo")];
        let content = content_with("foo", "bbbb", TargetContent::NoContent);
        let plan = content_plan(actual, &content, &Ledger::default());
        match &first_entry(&plan).action {
            Action::Update { from, .. } => assert_eq!(from, "断链"),
            other => panic!("期望 update，实际 {other:?}"),
        }
        assert!(
            plan.conflicts().is_empty(),
            "断链不是冲突：它没有内容可被破坏"
        );
    }

    #[test]
    fn an_empty_content_index_reproduces_plan_a_exactly() {
        // 这个 Default 是既有 306 条测试的前提：内容层缺席时，判决完全由
        // compare_versions 给出。agents-dir/local 源在那条路上落到 Unknown。
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "foo")];
        let plan = content_plan(actual, &ContentIndex::default(), &Ledger::default());
        assert!(
            matches!(first_entry(&plan).action, Action::Unknown),
            "内容层缺席时必须退回 Plan A 的判定（AgentsDir 是 Unknown）"
        );
    }

    #[test]
    fn a_source_with_no_target_content_verdict_falls_back() {
        // 源解析出来了，但这个 harness 上没扫到同名条目 → 表里没有 → 落回
        // compare_versions，而不是凭空判一个 in-sync。
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "foo")];
        let content = content_with("other", "bbbb", TargetContent::Has("bbbb".into()));
        let plan = content_plan(actual, &content, &Ledger::default());
        assert!(matches!(first_entry(&plan).action, Action::Unknown));
    }

    #[test]
    fn conflicts_sort_first_and_are_counted() {
        assert_eq!(Action::Conflict { reason: "x".into() }.rank(), 0);
        assert!(Action::Conflict { reason: "x".into() }.rank() < Action::Install.rank());
    }
}
