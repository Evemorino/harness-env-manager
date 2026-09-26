//! doctor：只读门禁（spec §5、§10.1）。
//!
//! 它做两件 `diff` 不做的事：**把计划里已经算出来的事实重新分类**（哪些挡路、哪些
//! 只是信息），以及**报告退出码**——`diff` 有 drift 也是 `Ok(())`，门禁语义一直是
//! 留给这个命令的（`run_diff` 的 rustdoc）。
//!
//! 三条边界值得先写明：
//!
//! - **只读**：不写任何文件、不碰账本，规划走 `SourceMode::Cached`（只读缓存）。
//! - **不回答「同步了没有」**：`install`（清单里有、本机还没装）不是 finding——
//!   那是 apply 的活。把它当问题会让 doctor 在每台新机器上恒红。
//! - **判据与 `diff` 同源**：冲突与交付不了直接取同一次规划里的 `Action`，
//!   不另写一套判据、也不在健康路径上再读一遍盘。

use crate::diff::{Action, Plan};
use crate::home::Home;
use crate::render::{NAME_WIDTH, action_cell};
use crate::store::skills::SourceMode;
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

/// 一条检查结果的落点：决定它渲染在哪个段、以及是否影响退出码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// 环境问题（`${VAR}` 缺失）——必须处理。
    Env,
    /// 计划问题（conflict / undeliverable）——必须处理。
    Plan,
    /// 只报告，不影响退出码。
    Warning,
}

/// 一段检查项。`text` 是段名之后、行尾之前的整段文本（已按 `render` 的列宽对齐），
/// 渲染器因此不认识任何种类——加一种检查只需加一条 `Check`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub kind: CheckKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// 「必须处理」的条数。`Env` 与 `Plan` 都算——它们的区别只在渲染在哪个段。
    pub fn problems(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.kind != CheckKind::Warning)
            .count()
    }

    pub fn warnings(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.kind == CheckKind::Warning)
            .count()
    }

    /// 门禁的全部语义就这一句：**没有必须处理的问题**。警告不影响它。
    pub fn healthy(&self) -> bool {
        self.problems() == 0
    }
}

/// 计划里已经算出来的事实 → 检查项。判据与 `diff` 同源（同一个 `Action`），
/// 只是分类不同：挡路的进 `problems`，其余进警告。
fn plan_checks(plan: &Plan) -> Vec<Check> {
    let mut checks = Vec::new();
    // 行的主语带 `@harness`：doctor 的报告是扁平的（不像 `diff` 那样按家分节），
    // 不带这一截，同一条 skill 在两家的两行会长得一模一样——真机上 `skill/my-draft`
    // 的「源目录不存在」就并排出现了两次，读的人无从知道该去修哪一家。
    // `(global)` 沿用 `render` 的段名：挂不进任何一家的条目就写它。
    let entries = plan
        .harnesses
        .iter()
        .flat_map(|h| h.entries.iter().map(move |e| (h.harness.as_str(), e)))
        .chain(plan.unattached.iter().map(|e| ("(global)", e)));

    for (harness, e) in entries {
        let subject = format!("{}@{harness}", e.id.label());
        match &e.action {
            // 同名冲突：apply 见到它就一个字节都不写，所以它挡路。
            Action::Conflict { reason } => checks.push(Check {
                kind: CheckKind::Plan,
                text: detail_line("! conflict", &subject, reason),
            }),
            // 清单要求了、实际交付不了：报告里一直有它，门禁里它也是问题。
            Action::Undeliverable { reason } => checks.push(Check {
                kind: CheckKind::Plan,
                text: detail_line("! undeliverable", &subject, reason),
            }),
            Action::Drift => checks.push(Check {
                kind: CheckKind::Warning,
                text: detail_line(
                    "- drift",
                    &subject,
                    e.note.as_deref().unwrap_or("未在清单中"),
                ),
            }),
            Action::Unknown => checks.push(Check {
                kind: CheckKind::Warning,
                text: detail_line("- unknown", &subject, "版本无法判定"),
            }),
            // **已经交付过**、但内容与源不同 = 环境里有过期物（§9.4）。警告，不是问题：
            // 过期是 apply 的正常输入，不是事故。
            //
            // `from == "断链"` 是 §9.3 的**自愈判定**（盘上那条解不开，apply 会把它换成
            // 真拷贝）——§2.1 说条目分类是「doctor 断链报告」的基础，那报告就是这一行，
            // 只是前缀换成 `- link`：断链与过期是两件事，混在一个词里读不出来。
            Action::Update { from, to } => {
                let (prefix, detail) = if from == "断链" {
                    (
                        "- link",
                        format!("盘上是断链，apply 会换成真拷贝（→ {to}）"),
                    )
                } else {
                    ("- stale", format!("副本与源不同（{from} → {to}）"))
                };
                checks.push(Check {
                    kind: CheckKind::Warning,
                    text: detail_line(prefix, &subject, &detail),
                });
            }
            // `install`：清单里有、本机还没装。**不是 finding**——那是 apply 的活。
            // `in-sync`：本来就是「一切正常」。
            Action::Install | Action::InSync => {}
            // `Remove` 只由 `prune_plan` 在 `--prune` 下产出，而 doctor 从不调用它——
            // 这条在 doctor 的输入里不可达。真出现说明计划来路不对（那是会删东西的动作），
            // 所以宁可报成问题，也不静默放过。
            Action::Remove => checks.push(Check {
                kind: CheckKind::Plan,
                text: detail_line("! remove", &subject, "这条计划要求删除，doctor 不该看到它"),
            }),
        }

        // 行上的注记（「源未被上游确认」「副本被手改过」）不改变动作，但它正是
        // 「这次判定没被上游确认」这件事本身——不许丢，也不能把动作改个吓人的名字。
        if let Some(note) = &e.note {
            if !matches!(e.action, Action::Drift) {
                checks.push(Check {
                    kind: CheckKind::Warning,
                    text: detail_line("- note", &subject, note),
                });
            }
        }
    }
    checks
}

/// `! conflict skill/foo@claude-code (目标已存在同名条目…)`。列宽与动作前缀都借
/// `render` 的——两份报告摆在一起看时才像同一个工具输出的。`subject` 由调用方拼好
/// （计划类的行带 `@harness`，环境类的行写 `mcp \`x\``）。
fn detail_line(prefix: &str, subject: &str, detail: &str) -> String {
    format!("{}{subject} ({detail})", action_cell(prefix))
}

/// 段落顺序：问题在前（先 `(env)` 后 `(plan)`——环境不可信时计划判定也没意义），
/// 警告在后；没有内容的段一个字符都不输出，页脚永远是最后一行。
pub fn render_doctor(report: &DoctorReport) -> String {
    let mut out = String::new();
    for (kind, head) in [
        (CheckKind::Env, "(env)"),
        (CheckKind::Plan, "(plan)"),
        (CheckKind::Warning, "(warn)"),
    ] {
        let rows: Vec<&Check> = report.checks.iter().filter(|c| c.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        for (i, c) in rows.iter().enumerate() {
            // 首行带段名，续行按同一视觉列缩进（与 `render` 的表格同一个写法）。
            let lead = if i == 0 {
                format!("{:<NAME_WIDTH$} ", head)
            } else {
                " ".repeat(NAME_WIDTH + 1)
            };
            let _ = writeln!(out, "{lead}{}", c.text);
        }
    }
    if !report.checks.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "Doctor: {} problems, {} warnings",
        report.problems(),
        report.warnings()
    );
    out
}

/// 体检一次。I/O 只在这个函数里发生：规划一次（与 `diff` 同一份），分类与渲染都是纯的。
pub fn run_doctor(
    manifest_path: &Path,
    home: &Home,
    out: &mut dyn Write,
) -> anyhow::Result<DoctorReport> {
    let planned = crate::plan_for(manifest_path, home, SourceMode::Cached, None)?;
    let report = DoctorReport {
        checks: plan_checks(&planned.plan),
    };
    write!(out, "{}", render_doctor(&report))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 各段的渲染：问题两段在前、警告一段在后，页脚收尾。
    ///
    /// 这份 golden 同时钉住「`- note` 不带 detail 重复」——注记是独立一条，
    /// 不与它所在一行的动作前缀合成一句话（两者说的是两件事）。也钉住「断链走
    /// `- link`、过期走 `- stale`」：两者都长成 `Update`，混在一个词里读不出来。
    #[test]
    fn a_problem_report_renders_every_section() {
        let report = DoctorReport {
            checks: vec![
                Check {
                    kind: CheckKind::Env,
                    text: detail_line(
                        "! env",
                        "mcp `context7`",
                        "${CONTEXT7_API_KEY} 在当前环境里没有值",
                    ),
                },
                Check {
                    kind: CheckKind::Plan,
                    text: detail_line(
                        "! conflict",
                        "skill/foo@claude-code",
                        "目标已存在同名条目且不是 henv 交付的（a1b2c3d），apply 不会覆盖",
                    ),
                },
                Check {
                    kind: CheckKind::Plan,
                    text: detail_line(
                        "! undeliverable",
                        "hook/superpowers:pre-commit@codex",
                        "no hook support",
                    ),
                },
                Check {
                    kind: CheckKind::Warning,
                    text: detail_line(
                        "- drift",
                        "mcp/experimental-thing@codex",
                        "未在清单中，只报不删",
                    ),
                },
                Check {
                    kind: CheckKind::Warning,
                    text: detail_line("- unknown", "skill/legacy@kimi-code", "版本无法判定"),
                },
                Check {
                    kind: CheckKind::Warning,
                    text: detail_line(
                        "- stale",
                        "skill/alpha@claude-code",
                        "副本与源不同（v2.1 → v2.2）",
                    ),
                },
                Check {
                    kind: CheckKind::Warning,
                    text: detail_line(
                        "- link",
                        "skill/beta@claude-code",
                        "盘上是断链，apply 会换成真拷贝（→ 9f8e7d6c）",
                    ),
                },
                Check {
                    kind: CheckKind::Warning,
                    text: detail_line(
                        "- note",
                        "skill/gamma@codex",
                        "源这次沿用缓存，未经上游确认（§9.4）",
                    ),
                },
            ],
        };
        insta::assert_snapshot!(render_doctor(&report));
    }

    /// 空报告是**空操作**：没有段、没有空行，只有页脚。
    #[test]
    fn a_clean_report_is_only_the_footer() {
        let out = render_doctor(&DoctorReport::default());
        assert_eq!(out, "Doctor: 0 problems, 0 warnings\n");
    }

    /// 计划里那些「不是 finding」的动作一条都不许进报告：`install` 与 `in-sync`
    /// 都必须是安静的。这条防止「把还没装的东西报成问题」这种恒红 bug 回来。
    #[test]
    fn install_and_in_sync_rows_are_not_findings() {
        use crate::capability::{CapId, Kind};
        use crate::diff::{HarnessPlan, PlanEntry};
        use std::collections::BTreeMap;

        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "codex".into(),
                detected: true,
                entries: vec![
                    PlanEntry::new(CapId::new(Kind::Skill, "not-yet"), Action::Install),
                    PlanEntry::new(CapId::new(Kind::Skill, "already"), Action::InSync),
                ],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        assert!(plan_checks(&plan).is_empty());
    }

    /// 同一条能力在两家的两行**必须能区分开**：doctor 的报告是扁平的，没有
    /// `diff` 那种按家分节的上下文。回归的是真机上看到的一幕——`skill/my-draft`
    /// 的两条「源目录不存在」并排出现，读的人不知道该去修哪一家。
    #[test]
    fn the_same_capability_on_two_harnesses_renders_as_two_distinct_rows() {
        use crate::capability::{CapId, Kind};
        use crate::diff::{HarnessPlan, PlanEntry};
        use std::collections::BTreeMap;

        let undeliverable = || Action::Undeliverable {
            reason: "源目录不存在或解不开：/nope".into(),
        };
        let plan = Plan {
            harnesses: vec![
                HarnessPlan {
                    harness: "claude-code".into(),
                    detected: true,
                    entries: vec![PlanEntry::new(
                        CapId::new(Kind::Skill, "my-draft"),
                        undeliverable(),
                    )],
                    actual_by_kind: BTreeMap::new(),
                },
                HarnessPlan {
                    harness: "codex".into(),
                    detected: true,
                    entries: vec![PlanEntry::new(
                        CapId::new(Kind::Skill, "my-draft"),
                        undeliverable(),
                    )],
                    actual_by_kind: BTreeMap::new(),
                },
            ],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        let checks = plan_checks(&plan);
        assert_eq!(checks.len(), 2);
        assert!(
            checks[0].text.contains("skill/my-draft@claude-code"),
            "{}",
            checks[0].text
        );
        assert!(
            checks[1].text.contains("skill/my-draft@codex"),
            "{}",
            checks[1].text
        );
    }
}
