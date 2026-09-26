//! render：`Plan` → 终端表格。格式变化会被 insta golden 捕捉（spec §6）。

use crate::capability::Kind;
use crate::diff::{Action, HarnessPlan, Plan, PlanEntry};
use std::fmt::Write as _;

/// harness 名列宽。spec §5 示例里内容一律从第 15 列起 → `{:<13}` 再补一个空格。
const NAME_WIDTH: usize = 13;
/// 动作前缀列宽。`+ install`/`~ update`/`- drift`/`✓ in-sync` 都对齐到 11 列。
const ACTION_WIDTH: usize = 11;
/// §9.5 全局段的段名。宽度与 harness 名一致，好让动作列对齐。
const GLOBAL_SECTION: &str = "(global)";

pub fn render(plan: &Plan) -> String {
    let mut out = String::new();
    for h in &plan.harnesses {
        render_harness(h, &mut out);
    }
    // §9.5 全局段：挂不进任何 harness 节的条目（一家 harness 都没装时那条
    // `undeliverable`）。没有它，这一行会被静默丢掉。空的时候输出逐字节不变。
    if !plan.unattached.is_empty() {
        render_unattached(&plan.unattached, &mut out);
    }
    if !plan.harnesses.is_empty() || !plan.unattached.is_empty() {
        out.push('\n');
    }
    let c = plan.counts();
    let _ = writeln!(
        out,
        "Plan: {} conflict, {} install, {} update, {} remove, {} undeliverable, {} drift, {} unknown",
        c.conflict, c.install, c.update, c.remove, c.undeliverable, c.drift, c.unknown
    );
    out
}

fn render_harness(h: &HarnessPlan, out: &mut String) {
    let head = format!("{:<NAME_WIDTH$} ", h.harness);

    if !h.detected {
        let _ = writeln!(out, "{head}⚠ not installed (skipped)");
        return;
    }

    // InSync 只在**不带注记**时才折进汇总行：注记是「内容可能过时 / 未经过
    // 上游确认」这类必须被看见的信息（§9.4 的 stale），汇总成一行就丢了。
    let pending: Vec<&PlanEntry> = h
        .entries
        .iter()
        .filter(|e| !matches!(e.action, Action::InSync) || e.note.is_some())
        .collect();

    // 全同步 → 单行汇总实有数量；有待办 → 逐条列出（汇总行此时是噪音）。
    if pending.is_empty() {
        let _ = writeln!(out, "{head}{}{}", action_cell("✓ in-sync"), summary(h));
        return;
    }

    render_entries(&pending, &head, out);
}

/// §9.5 的全局段：挂不进任何 harness 节的条目。段名不是 harness，落在
/// `render_harness` 之外的这一段里，好让人一眼看出「这条不属于任何一家」。
fn render_unattached(entries: &[PlanEntry], out: &mut String) {
    let head = format!("{:<NAME_WIDTH$} ", GLOBAL_SECTION);
    let pending: Vec<&PlanEntry> = entries.iter().collect();
    render_entries(&pending, &head, out);
}

/// 逐条渲染一个节的待办行。`head` 是该节首行的名字列（含对齐），后续行用等宽缩进。
fn render_entries(pending: &[&PlanEntry], head: &str, out: &mut String) {
    let mut i = 0;
    while i < pending.len() {
        let e = pending[i];
        i += 1;
        // lead：该节的首行带名字，后续行用等宽缩进对齐。
        let lead = if i == 1 { head.to_string() } else { indent() };
        match &e.action {
            // install 无附加说明，合并成一行；diff 已按 label 排好序，故连续。
            // **带注记的 install 不参与合并**：合并行没有地方放注记，一旦并进去
            // 就是静默——「没报失败」又变回「什么都没说」。
            Action::Install if e.note.is_none() => {
                let mut labels = vec![e.id.label()];
                while i < pending.len()
                    && matches!(pending[i].action, Action::Install)
                    && pending[i].note.is_none()
                {
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
            Action::Install => {
                let line = format!("{lead}{}{}", action_cell("+ install"), e.id.label());
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
            Action::Update { from, to } => {
                let line = format!(
                    "{lead}{}{} ({from} → {to})",
                    action_cell("~ update"),
                    e.id.label()
                );
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
            Action::Undeliverable { reason } => {
                let line = format!(
                    "{lead}{}{} ({reason})",
                    action_cell("! undeliverable"),
                    e.id.label()
                );
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
            Action::Drift => {
                // 括号里那句话由 `compute` 按账本给出（「--prune 可删」/「只报不删」）——
                // 它预告的是 `--prune` 会做什么，写死在这里就必然与 `prune_plan` 的
                // 判据分家（§9.3 不变式二）。没有注记时退回中性说法。
                let reason = e.note.as_deref().unwrap_or("未在清单中");
                let line = format!(
                    "{lead}{}{} ({reason})",
                    action_cell("- drift"),
                    e.id.label()
                );
                let _ = writeln!(out, "{line}");
            }
            Action::Unknown => {
                let line = format!(
                    "{lead}{}{} (版本无法判定)",
                    action_cell("? unknown"),
                    e.id.label()
                );
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
            Action::Conflict { reason } => {
                let line = format!(
                    "{lead}{}{} ({reason})",
                    action_cell("! conflict"),
                    e.id.label()
                );
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
            // §9.8：`--prune` 的删除行。它与 drift 是两回事——drift 只说「清单里没有」，
            // 这一行说的是「这次会真的删掉，因为账本记着它是 henv 放的」。
            Action::Remove => {
                let line = format!(
                    "{lead}{}{} (从 henv 账本移除)",
                    action_cell("- remove"),
                    e.id.label()
                );
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
            // 走到这里的 in-sync 一定带着注记（filter 只放这种进来）。
            Action::InSync => {
                let line = format!("{lead}{}{}", action_cell("✓ in-sync"), e.id.label());
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
        }
    }
}

fn indent() -> String {
    " ".repeat(NAME_WIDTH + 1)
}

/// 行尾注记（§9.3）。合并行不用它——合并只发生在**不带注记**的 install 之间，
/// 所以每一处 `with_note` 都落在逐条渲染的行上。
fn with_note(mut line: String, note: Option<&String>) -> String {
    if let Some(n) = note {
        let _ = write!(line, " [{n}]");
    }
    line
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
        PlanEntry::new(CapId::new(kind, name), action)
    }

    /// spec §5 的示例场景：claude-code 全同步，codex 有 install/update/undeliverable，
    /// kimi-code 有 install/drift。
    fn spec_plan() -> Plan {
        Plan {
            unattached: Vec::new(),
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
                        // §9.8 的例子：这是用户**手配**的一条（不在账本里），所以提示是
                        // 「只报不删」——`--prune` 只删 henv 自己交付过的。
                        pe(Kind::Mcp, "experimental-thing", Action::Drift)
                            .with_note(Some("未在清单中，只报不删".into())),
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
            unattached: Vec::new(),
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

    /// `entries` 全是 `InSync`（而非空表）时，那行 `pending` 过滤器必须把汇总行显出来。
    /// **删掉过滤器这条测试是不是还绿？不是**：不过滤时走的是「有待办」分支，逐条打印
    /// 时六个 arm 里 `InSync` 那一臂什么都不写，汇总行 `✓ in-sync` 连同实有数量一起消失，
    /// 于是「全同步」在报告里表现为**什么都没有**——正是本工具要回答的那句话。
    /// 空 entries 的那两条（`empty_harness_says_nothing_installed` 等）拦不住它：
    /// 空表本来就走 `pending.is_empty()` 分支，删掉过滤器照样绿。
    #[test]
    fn in_sync_entries_render_only_the_summary_line() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    pe(Kind::Skill, "dingtalk-mail", Action::InSync),
                    pe(Kind::Mcp, "context7", Action::InSync),
                ],
                vec![(Kind::Skill, 2), (Kind::Mcp, 1)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("claude-code   ✓ in-sync  skills:2 mcps:1\n"),
            "全同步必须出汇总行：{out}"
        );
        for prefix in ["+ install", "~ update", "! undeliverable", "- drift"] {
            assert!(!out.contains(prefix), "全同步不该有动作行：{out}");
        }
        assert_eq!(
            out.lines().filter(|l| l.starts_with("claude-code")).count(),
            1,
            "这一行的唯一来源是汇总行；过滤器没了就一条都不剩：{out}"
        );
    }

    #[test]
    fn empty_harness_says_nothing_installed() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp("codex", true, vec![], vec![])],
        };
        assert!(render(&plan).contains("codex         ✓ in-sync  (nothing installed)\n"));
    }

    #[test]
    fn undetected_harness_is_reported_not_listed_as_install() {
        let plan = Plan {
            unattached: Vec::new(),
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
            unattached: Vec::new(),
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

    /// drift 行括号里那句话由 `compute` 按账本给出（「--prune 可删」/「只报不删」），
    /// render 只负责排版：它手上没有账本，写死一句就必然与 `prune_plan` 的判据分家
    /// （§9.3 不变式二）。
    #[test]
    fn drift_line_takes_its_parenthetical_from_the_entry_note() {
        let out = render(&spec_plan());
        assert!(
            out.contains("- drift    mcp/experimental-thing (未在清单中，只报不删)\n"),
            "{out}"
        );

        // 没有注记时退回中性说法，而不是凭空断言「可删」。
        let bare = Plan {
            harnesses: vec![HarnessPlan {
                harness: "kimi-code".into(),
                detected: true,
                entries: vec![pe(Kind::Mcp, "experimental-thing", Action::Drift)],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
        };
        assert!(
            render(&bare).contains("- drift    mcp/experimental-thing (未在清单中)\n"),
            "{bare:#?}"
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
            out.ends_with(
                "\nPlan: 0 conflict, 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift, 0 unknown\n"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn empty_plan_has_no_leading_blank_line() {
        let out = render(&Plan::default());
        assert_eq!(
            out,
            "Plan: 0 conflict, 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"
        );
    }

    #[test]
    fn unknown_action_renders_a_line_with_question_mark() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![pe(Kind::Plugin, "superpowers", Action::Unknown)],
                vec![(Kind::Plugin, 1)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("claude-code   ? unknown  plugin/superpowers (版本无法判定)\n"),
            "{out}"
        );
        assert!(
            out.ends_with(
                "Plan: 0 conflict, 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 1 unknown\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn footer_counts_come_from_plan_and_include_conflicts() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    pe(Kind::Skill, "a", Action::Install),
                    pe(
                        Kind::Skill,
                        "b",
                        Action::Conflict {
                            reason: "目标已存在同名条目且不是 henv 交付的".into(),
                        },
                    ),
                ],
                vec![(Kind::Skill, 2)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.ends_with(
                "Plan: 1 conflict, 1 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("! conflict skill/b (目标已存在同名条目且不是 henv 交付的)\n"),
            "{out}"
        );
    }

    /// 冲突行前缀必须与别的动作一样对齐到 11 列，否则报告会错位。
    #[test]
    fn conflict_prefix_is_padded_like_every_other_action() {
        assert_eq!(action_cell("! conflict"), "! conflict ");
        assert_eq!(action_cell("! conflict").chars().count(), ACTION_WIDTH);
    }

    /// 内容层给的注记渲染成行尾的 ` [note]`（§9.3「副本被手改过」）。
    #[test]
    fn an_update_note_is_rendered_at_the_end_of_the_line() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    PlanEntry::new(
                        CapId::new(Kind::Skill, "a"),
                        Action::Update {
                            from: "aaaa".into(),
                            to: "bbbb".into(),
                        },
                    )
                    .with_note(Some("副本被手改过".into())),
                ],
                vec![(Kind::Skill, 1)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("~ update   skill/a (aaaa → bbbb) [副本被手改过]\n"),
            "{out}"
        );
    }

    /// 注记必须活下来，而合并行是注记的头号杀手：`+ install  a, b, c` 的价值
    /// 是短，可当其中一条带着「内容没经过上游确认」（§9.4 的 stale）时，短就是错。
    ///
    /// fixture 按 `compute` 的排序取（同动作内按 label），带注记的那条放**最前**：
    /// 这样既验「注记条目不参与合并」，也验「它不会把后面的无注记条目一起吞掉」。
    #[test]
    fn a_noted_install_is_not_merged_into_the_coalesced_line() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    PlanEntry::new(CapId::new(Kind::Skill, "a"), Action::Install)
                        .with_note(Some("未能刷新上游".into())),
                    pe(Kind::Skill, "b", Action::Install),
                    pe(Kind::Skill, "c", Action::Install),
                ],
                vec![(Kind::Skill, 3)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("+ install  skill/a [未能刷新上游]\n"),
            "带注记的条目要单独一行：\n{out}"
        );
        assert!(
            out.contains("+ install  skill/b, skill/c\n"),
            "无注记的相邻 install 照旧合并：\n{out}"
        );
    }

    /// 汇总行 `✓ in-sync  skills:1` 装不下注记，而一条 stale 的 in-sync 恰恰是
    /// 「看着没事、其实没经过上游确认」——最需要被看见的那种。
    #[test]
    fn a_noted_in_sync_is_not_folded_into_the_summary() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    PlanEntry::new(CapId::new(Kind::Skill, "a"), Action::InSync)
                        .with_note(Some("未能刷新上游".into())),
                ],
                vec![(Kind::Skill, 1)],
            )],
        };
        let out = render(&plan);
        assert!(out.contains("skill/a"), "注记行的条目要露出来：\n{out}");
        assert!(out.contains("未能刷新上游"), "注记必须显示出来：\n{out}");
    }

    /// 带注记的 update 与 install 相邻时，install 的合并逻辑不能被打乱。
    ///
    /// 条目顺序按 `Action::rank()`：install(1) 在 update(2) 之前，且合并只并
    /// **连续**的 install——这正是 `compute` 排完序后的形态（两处 install 中间
    /// 夹一条 update 是它产不出来的）。
    #[test]
    fn a_note_does_not_break_install_coalescing() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    pe(Kind::Skill, "a", Action::Install),
                    pe(Kind::Skill, "c", Action::Install),
                    PlanEntry::new(
                        CapId::new(Kind::Skill, "b"),
                        Action::Update {
                            from: "1".into(),
                            to: "2".into(),
                        },
                    )
                    .with_note(Some("副本被手改过".into())),
                ],
                vec![(Kind::Skill, 3)],
            )],
        };
        let out = render(&plan);
        assert!(out.contains("+ install  skill/a, skill/c\n"), "{out}");
        assert!(out.contains(" [副本被手改过]\n"), "{out}");
    }

    /// 冲突是阻断性的，必须在最前——一行 `+ install` 出现在冲突之前会让人
    /// 以为改动已经可以做了。
    #[test]
    fn conflicts_render_before_installs() {
        let plan = Plan {
            unattached: Vec::new(),
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    pe(
                        Kind::Skill,
                        "z-last-name",
                        Action::Conflict { reason: "r".into() },
                    ),
                    pe(Kind::Skill, "a-first-name", Action::Install),
                ],
                vec![(Kind::Skill, 2)],
            )],
        };
        let out = render(&plan);
        let conflict_at = out.find("! conflict").expect("conflict line");
        let install_at = out.find("+ install").expect("install line");
        assert!(conflict_at < install_at, "{out}");
    }

    /// §9.5 全局段：`unattached` 的行要渲染出来，段名 `(global)`。
    ///
    /// 这是「一家 harness 都没装」时那条 `undeliverable` 唯一的去处。没有全局段，
    /// 它会被静默丢掉——`diff` 报 `0 undeliverable`，用户以为一切正常。
    #[test]
    fn an_unattached_entry_renders_under_the_global_section() {
        let plan = Plan {
            harnesses: vec![hp("codex", false, Vec::new(), Vec::new())],
            unattached: vec![pe(
                Kind::Plugin,
                "superpowers",
                Action::Undeliverable {
                    reason: "源家没装".into(),
                },
            )],
        };
        let out = render(&plan);
        assert!(out.contains("(global)"), "{out}");
        assert!(out.contains("! undeliverable"), "{out}");
        assert!(out.contains("plugin/superpowers"), "{out}");
        assert!(
            out.contains("1 undeliverable"),
            "汇总要把全局段数进去：{out}"
        );
    }

    /// §9.8：`- remove` 行。前缀必须与其余动作列对齐（与 golden 里
    /// `- drift    ...` 同一套 `action_cell`）。
    #[test]
    fn a_remove_entry_renders_with_its_own_prefix() {
        let plan = Plan {
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![pe(Kind::Skill, "gone", Action::Remove)],
                Vec::new(),
            )],
            unattached: Vec::new(),
        };
        let out = render(&plan);
        assert!(out.contains("- remove   skill/gone"), "{out}");
        assert!(out.contains("1 remove"), "汇总要数得到删除：{out}");
    }
}
