//! render：`Plan` → 终端表格。格式变化会被 insta golden 捕捉（spec §6）。

use crate::capability::Kind;
use crate::diff::{Action, HarnessPlan, Plan, PlanEntry};
use std::fmt::Write as _;

/// harness 名列宽。spec §5 示例里内容一律从第 15 列起 → `{:<13}` 再补一个空格。
const NAME_WIDTH: usize = 13;
/// 动作前缀列宽。`+ install`/`~ update`/`- drift`/`✓ in-sync` 都对齐到 11 列。
const ACTION_WIDTH: usize = 11;

pub fn render(plan: &Plan) -> String {
    let mut out = String::new();
    for h in &plan.harnesses {
        render_harness(h, &mut out);
    }
    if !plan.harnesses.is_empty() {
        out.push('\n');
    }
    let c = plan.counts();
    let _ = writeln!(
        out,
        "Plan: {} install, {} update, {} remove, {} undeliverable, {} drift",
        c.install, c.update, c.remove, c.undeliverable, c.drift
    );
    out
}

fn render_harness(h: &HarnessPlan, out: &mut String) {
    let head = format!("{:<NAME_WIDTH$} ", h.harness);

    if !h.detected {
        let _ = writeln!(out, "{head}⚠ not installed (skipped)");
        return;
    }

    let pending: Vec<&PlanEntry> = h
        .entries
        .iter()
        .filter(|e| !matches!(e.action, Action::InSync))
        .collect();

    // 全同步 → 单行汇总实有数量；有待办 → 逐条列出（汇总行此时是噪音）。
    if pending.is_empty() {
        let _ = writeln!(out, "{head}{}{}", action_cell("✓ in-sync"), summary(h));
        return;
    }

    let mut i = 0;
    while i < pending.len() {
        let e = pending[i];
        i += 1;
        // lead：该 harness 的首行带名字，后续行用等宽缩进对齐。
        let lead = if i == 1 { head.clone() } else { indent() };
        match &e.action {
            // install 无附加说明，合并成一行；diff 已按 label 排好序，故连续。
            Action::Install => {
                let mut labels = vec![e.id.label()];
                while i < pending.len() && matches!(pending[i].action, Action::Install) {
                    labels.push(pending[i].id.label());
                    i += 1;
                }
                let _ = writeln!(
                    out,
                    "{lead}{}{}",
                    action_cell("+ install"),
                    labels.join(", ")
                );
            }
            Action::Update { from, to } => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} ({from} → {to})",
                    action_cell("~ update"),
                    e.id.label()
                );
            }
            Action::Undeliverable { reason } => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} ({reason})",
                    action_cell("! undeliverable"),
                    e.id.label()
                );
            }
            Action::Drift => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} (未在清单中，--prune 可删)",
                    action_cell("- drift"),
                    e.id.label()
                );
            }
            // pending 已过滤掉 InSync；match 需穷尽故留空臂，不 panic。
            Action::InSync => {}
        }
    }
}

fn indent() -> String {
    " ".repeat(NAME_WIDTH + 1)
}

/// 前缀对齐到 `ACTION_WIDTH` 列；超长的 `! undeliverable`(15) 退化为两空格分隔。
fn action_cell(prefix: &str) -> String {
    let n = prefix.chars().count();
    if n >= ACTION_WIDTH {
        format!("{prefix}  ")
    } else {
        format!("{prefix}{}", " ".repeat(ACTION_WIDTH - n))
    }
}

/// `skills:42 mcps:6 plugins:3`——按 `Kind::all()` 顺序，计数 0 的种类跳过。
fn summary(h: &HarnessPlan) -> String {
    let parts: Vec<String> = Kind::all()
        .into_iter()
        .filter_map(|k| {
            let n = h.actual_by_kind.get(&k).copied().unwrap_or(0);
            (n > 0).then(|| format!("{}s:{n}", k.as_str()))
        })
        .collect();
    if parts.is_empty() {
        "(nothing installed)".to_string()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Kind};
    use crate::diff::{Action, HarnessPlan, Plan, PlanEntry};
    use std::collections::BTreeMap;

    fn hp(
        harness: &str,
        detected: bool,
        entries: Vec<PlanEntry>,
        actual: Vec<(Kind, usize)>,
    ) -> HarnessPlan {
        HarnessPlan {
            harness: harness.to_string(),
            detected,
            entries,
            actual_by_kind: actual.into_iter().collect::<BTreeMap<_, _>>(),
        }
    }

    fn pe(kind: Kind, name: &str, action: Action) -> PlanEntry {
        PlanEntry {
            id: CapId::new(kind, name),
            action,
        }
    }

    /// spec §5 的示例场景：claude-code 全同步，codex 有 install/update/undeliverable，
    /// kimi-code 有 install/drift。
    fn spec_plan() -> Plan {
        Plan {
            harnesses: vec![
                hp(
                    "claude-code",
                    true,
                    vec![],
                    vec![(Kind::Skill, 42), (Kind::Mcp, 6), (Kind::Plugin, 3)],
                ),
                hp(
                    "codex",
                    true,
                    vec![
                        pe(Kind::Mcp, "browser-use", Action::Install),
                        pe(Kind::Mcp, "context7", Action::Install),
                        pe(
                            Kind::Skill,
                            "dingtalk-mail",
                            Action::Update {
                                from: "v2.1".into(),
                                to: "v2.2".into(),
                            },
                        ),
                        pe(
                            Kind::Hook,
                            "superpowers:pre-commit",
                            Action::Undeliverable {
                                reason: "no hook support".into(),
                            },
                        ),
                    ],
                    vec![(Kind::Skill, 30)],
                ),
                hp(
                    "kimi-code",
                    true,
                    vec![
                        pe(Kind::Mcp, "context7", Action::Install),
                        pe(Kind::Mcp, "experimental-thing", Action::Drift),
                    ],
                    vec![(Kind::Mcp, 1)],
                ),
            ],
        }
    }

    #[test]
    fn spec_example_matches_design_doc() {
        insta::assert_snapshot!("spec_example_matches_design_doc", render(&spec_plan()));
    }

    #[test]
    fn harness_name_column_is_14_wide() {
        // `claude-code`(11) + 3 空格；`codex`(5) + 9 空格 —— 内容都从第 15 列起。
        let out = render(&spec_plan());
        let mut lines = out.lines();
        assert!(
            lines.next().unwrap().starts_with("claude-code   ✓"),
            "{out}"
        );
        let codex = lines.next().unwrap();
        assert!(codex.starts_with("codex         + install  "), "{codex:?}");
        // 续行缩进 14 空格，与首行内容起点对齐
        let cont = lines.next().unwrap();
        assert!(cont.starts_with("              ~ update   "), "{cont:?}");
    }

    #[test]
    fn installs_are_coalesced_onto_one_line_in_sorted_order() {
        let out = render(&spec_plan());
        assert!(
            out.contains("+ install  mcp/browser-use, mcp/context7\n"),
            "{out}"
        );
        assert!(
            !out.contains("mcp/context7, mcp/browser-use"),
            "排序必须确定性，非示例顺序"
        );
    }

    #[test]
    fn in_sync_summary_only_when_harness_has_no_pending_work() {
        let out = render(&spec_plan());
        assert!(
            out.contains("claude-code   ✓ in-sync  skills:42 mcps:6 plugins:3\n"),
            "{out}"
        );
        // codex 有 30 个实有 skill，但有待办 → 不出汇总行
        assert!(!out.contains("skills:30"), "{out}");
    }

    #[test]
    fn in_sync_summary_kinds_follow_kind_all_order() {
        // Kind::all() = [Skill, Mcp, Command, Agent, Hook, Plugin]。六个种类**全部给非零计数**：
        // 只要有一个是 0，交换它与相邻种类的排列就检不出来（0 计数会被跳过）。
        // 这条是 `Kind::all()` 顺序在 Task 2 之外的第二道闸，两道都要在。
        let plan = Plan {
            harnesses: vec![hp(
                "codex",
                true,
                vec![],
                vec![
                    (Kind::Skill, 1),
                    (Kind::Mcp, 2),
                    (Kind::Command, 3),
                    (Kind::Agent, 4),
                    (Kind::Hook, 5),
                    (Kind::Plugin, 6),
                ],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("✓ in-sync  skills:1 mcps:2 commands:3 agents:4 hooks:5 plugins:6\n"),
            "{out}"
        );
    }

    #[test]
    fn empty_harness_says_nothing_installed() {
        let plan = Plan {
            harnesses: vec![hp("codex", true, vec![], vec![])],
        };
        assert!(render(&plan).contains("codex         ✓ in-sync  (nothing installed)\n"));
    }

    #[test]
    fn undetected_harness_is_reported_not_listed_as_install() {
        let plan = Plan {
            harnesses: vec![hp(
                "codex",
                false,
                // 即便 diff 误产了条目，未检测到的 harness 也只显示跳过原因
                vec![pe(Kind::Skill, "x", Action::Install)],
                vec![(Kind::Skill, 9)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("codex         ⚠ not installed (skipped)\n"),
            "{out}"
        );
        // 页脚计数字里本来就有 "install" 这个词，故断言动作前缀而非词
        assert!(
            !out.contains("+ install"),
            "未安装的 harness 不该产出一堆待装噪音：{out}"
        );
        assert!(!out.contains("skills:9"), "{out}");
    }

    #[test]
    fn undeliverable_prefix_is_not_padded_to_eleven() {
        // `! undeliverable` 是 15 字符，超过 11 列宽 → 退化为两空格分隔（spec 示例形态）。
        let plan = Plan {
            harnesses: vec![hp(
                "kimi-code",
                true,
                vec![pe(
                    Kind::Skill,
                    "my-draft",
                    Action::Undeliverable {
                        reason: "kimi-code 无用户级 skills 目录".into(),
                    },
                )],
                vec![],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("! undeliverable  skill/my-draft (kimi-code 无用户级 skills 目录)\n"),
            "{out}"
        );
    }

    #[test]
    fn drift_line_carries_the_prune_hint() {
        let out = render(&spec_plan());
        assert!(
            out.contains("- drift    mcp/experimental-thing (未在清单中，--prune 可删)\n"),
            "{out}"
        );
    }

    #[test]
    fn update_line_shows_from_then_to() {
        let out = render(&spec_plan());
        assert!(
            out.contains("~ update   skill/dingtalk-mail (v2.1 → v2.2)\n"),
            "{out}"
        );
    }

    #[test]
    fn footer_counts_come_from_plan_not_from_lines() {
        let out = render(&spec_plan());
        assert!(
            out.ends_with("\nPlan: 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift\n"),
            "{out:?}"
        );
    }

    #[test]
    fn empty_plan_has_no_leading_blank_line() {
        let out = render(&Plan::default());
        assert_eq!(
            out,
            "Plan: 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift\n"
        );
    }
}
