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
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessState {
    pub name: String,
    pub detected: bool,
    pub actual: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
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
}

impl Action {
    /// 输出分组顺序：install → update → undeliverable → drift → in-sync
    pub fn rank(&self) -> u8 {
        match self {
            Action::Install => 0,
            Action::Update { .. } => 1,
            Action::Undeliverable { .. } => 2,
            Action::Drift => 3,
            Action::InSync => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub id: CapId,
    pub action: Action,
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
    pub install: usize,
    pub update: usize,
    pub remove: usize,
    pub undeliverable: usize,
    pub drift: usize,
}

impl Plan {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for h in &self.harnesses {
            for e in &h.entries {
                match &e.action {
                    Action::Install => c.install += 1,
                    Action::Update { .. } => c.update += 1,
                    Action::Undeliverable { .. } => c.undeliverable += 1,
                    Action::Drift => c.drift += 1,
                    Action::InSync => {}
                }
            }
        }
        c
    }

    pub fn is_clean(&self) -> bool {
        let c = self.counts();
        c.install == 0 && c.update == 0 && c.remove == 0 && c.undeliverable == 0 && c.drift == 0
    }
}

pub struct HarnessInput<'a> {
    pub state: &'a HarnessState,
    pub adapter: &'a dyn HarnessAdapter,
}

pub fn compute(desired: &[Capability], inputs: &[HarnessInput], degrade: &DegradeTable) -> Plan {
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
                    if degrade.action(cap) == DegradeAction::Report {
                        entries.push(PlanEntry {
                            id: cap.id.clone(),
                            action: Action::Undeliverable { reason },
                        });
                    }
                }
                Support::Native | Support::Degraded { .. } => {
                    matched.insert(cap.id.clone());
                    let action = match state.actual.iter().find(|a| a.id == cap.id) {
                        None => Action::Install,
                        Some(a) => match compare_versions(&cap.spec, &a.spec) {
                            // Unknown 按 in-sync：只读工具不该误报（spec §2.1 第 3 条）
                            VersionMatch::Same | VersionMatch::Unknown => Action::InSync,
                            VersionMatch::Different { from, to } => Action::Update { from, to },
                        },
                    };
                    entries.push(PlanEntry {
                        id: cap.id.clone(),
                        action,
                    });
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
            entries.push(PlanEntry {
                id: a.id.clone(),
                action: Action::Drift,
            });
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
    use crate::testutil::FakeAdapter;

    fn no_degrade() -> DegradeTable {
        DegradeTable::new(&Manifest::parse("version: 1\n").unwrap())
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
        let plan = compute(desired, &inputs, &no_degrade());
        plan.harnesses.into_iter().next().unwrap()
    }

    fn labels(hp: &HarnessPlan) -> Vec<(String, String)> {
        hp.entries
            .iter()
            .map(|e| {
                (
                    e.id.label(),
                    match &e.action {
                        Action::Install => "install".to_string(),
                        Action::Update { .. } => "update".to_string(),
                        Action::InSync => "in-sync".to_string(),
                        Action::Undeliverable { .. } => "undeliverable".to_string(),
                        Action::Drift => "drift".to_string(),
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
    fn unknown_version_match_counts_as_in_sync() {
        // codex 不记录 plugin 版本 → Unknown → 不误报 update
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
            vec![("plugin/superpowers".to_string(), "in-sync".to_string())]
        );
    }

    #[test]
    fn same_sha_across_differing_version_strings_is_in_sync() {
        // spec §2.1 第 3 条的实测场景
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
            vec![("plugin/superpowers".to_string(), "in-sync".to_string())]
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

        let plan = compute(&[hook], &inputs, &table);
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

        let plan = compute(&[hook], &inputs, &table);
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
            vec![("plugin/superpowers".to_string(), "in-sync".to_string())],
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
        let plan = compute(&d, &inputs, &no_degrade());
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
        let plan = compute(
            &d,
            &[HarnessInput {
                state: &s,
                adapter: &a,
            }],
            &no_degrade(),
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
        let plan = compute(
            &[],
            &[HarnessInput {
                state: &s,
                adapter: &a,
            }],
            &no_degrade(),
        );
        assert!(plan.is_clean());
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
        }
        let s = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![],
        };
        let a = DegradingAdapter;
        let plan = compute(
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
            &no_degrade(),
        );
        assert_eq!(plan.counts().install, 1, "Degraded 与 Native 同路径");
    }
}
