//! diff：`f(desired, actual) → Plan`，纯函数、幂等、只读（spec §3 决策 2）。
//!
//! 清单定义「必须有的」，不定义「不许有的」：actual 多出来的报 drift 但**不**删，
//! 删除需显式 `apply --prune`（§9.8）。`compute` 因此**永不产** [`Action::Remove`]
//! ——它由 [`prune_plan`] 在账本授权下改判出来，故 `diff` 报告的 `remove` 恒为 0。
//!
//! 已知局限：remote MCP（仅 url、无 command）在 adapter 层归一为空 `Spec::Mcp`，diff 只按
//! name 比较身份，解析延后到 Plan B。

use crate::adapters::HarnessAdapter;
use crate::capability::{
    CapId, Capability, Kind, McpTransport, Origin, RecordedMatch, SkillSource, Spec, Support,
    VersionMatch, compare_recorded_version, compare_versions, mcp_summary,
};
use crate::lower::{DegradeTable, SnapshotTable, SourceHarnessTable};
use crate::manifest::DegradeAction;
use crate::store::hash::hash_mcp_spec;
use crate::store::provenance::Ledger;
use crate::store::skills::{ContentIndex, ResolvedContent, TargetContent, Unresolved};
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
    /// §9.8：`apply --prune` 独有——撤销一条 henv 自己交付过、清单里已经不要了的东西。
    ///
    /// **`compute` 与 `diff` 永不产它**：只有 [`prune_plan`] 会把账本记着的
    /// [`Action::Drift`] 改判成它。rank 排在 conflict 之后、install 之前——
    /// 删除与交付同属「这次要动手做什么」，而冲突仍是唯一决定「能不能做」的那个。
    Remove,
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
    /// 记录型 plugin（§10.3，`source: snapshot`）的对账行：清单记的版本与本机不符，
    /// 或者本机根本没装。
    ///
    /// **不是可执行动作**：`apply` 只认 `install`/`update`/`remove`（`apply/mod.rs`
    /// 的 filter），这个变体天然落在它外面——记录型条目一个字节都不写。`doctor` 把它
    /// 算**警告**：记录与现实不符是提醒，不是门禁（本机没装它可以是正常状态）。
    Snapshot {
        /// 清单 `versions.<harness>` 记的那个串（原值，未归一）
        recorded: String,
        /// 本机 `version` 位；`None` = 本机没装。
        /// 装着但版本读不出（codex 恒如此）走 `Incomparable`，**不产这一行**。
        actual: Option<String>,
    },
}

impl Action {
    /// 输出分组顺序：conflict → remove → install → update → undeliverable → drift → unknown
    /// → snapshot → in-sync
    ///
    /// `snapshot` 排在最后两个之间：它排在所有「盘上的事」之后（它不是盘面，
    /// 是清单侧的记录），又必须排在 in-sync 之前（in-sync 会被折进汇总行，
    /// 记录不符不能被折掉）。
    pub fn rank(&self) -> u8 {
        match self {
            Action::Conflict { .. } => 0,
            Action::Remove => 1,
            Action::Install => 2,
            Action::Update { .. } => 3,
            Action::Undeliverable { .. } => 4,
            Action::Drift => 5,
            Action::Unknown => 6,
            Action::Snapshot { .. } => 7,
            Action::InSync => 8,
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
    /// 挂不进任何 harness 节的条目（§9.5）。
    ///
    /// 今天只有一种来源：**一家 harness 都没装**时，拆解条目的 `undeliverable`
    /// 落不到任何**会被渲染**的节——未安装的家整节被 `render` 跳过。过去这一行会被
    /// 静默丢掉（`diff` 报 `0 undeliverable`，用户以为一切正常，而清单里明明有一条
    /// 根本无法满足的要求）。全局段把它接住，`render` 渲染成一个 `(global)` 段。
    pub unattached: Vec<PlanEntry>,
    /// §9.5「装不下的部分」的报告说明：plugin 身份 → 一句人话。
    ///
    /// **不是能力、不是动作**：不进 `counts()`、不影响 `is_clean()`，`apply` 不为它写
    /// 任何字节。`render` 把它渲染成报告末尾的 `(notes)` 段（排在所有 harness 节与
    /// `(global)` 段之后、`Plan:` 汇总行之前），每条 plugin **一行**。
    ///
    /// 为什么要有这一层，而不是像过去那样挂在拆出来的 skill 行尾：这句话是**每条
    /// plugin 一句**，而 skill 行是**每个 skill 一行**（真机上 14 条），挂行尾就是
    /// 同一句说 14 遍；挂在其中一行上，则那一行一旦不在（未安装的家整节跳过、
    /// `--harness` 视图、这条本来就没有行）说明就静默消失——而「报告里说一句」正是
    /// `degrade.<kind> = report` 与 `skip` 唯一的区别。
    pub notes: BTreeMap<String, String>,
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
    /// 记录型 plugin 的对账行。**不进 `is_clean()`**：`apply` 对它无事可做
    /// （不是待办），但 `render` 的汇总行要数它——不然这行就没人看得见。
    pub snapshot: usize,
}

impl Plan {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for e in self
            .harnesses
            .iter()
            .flat_map(|h| h.entries.iter())
            .chain(self.unattached.iter())
        {
            match &e.action {
                Action::Conflict { .. } => c.conflict += 1,
                Action::Remove => c.remove += 1,
                Action::Install => c.install += 1,
                Action::Update { .. } => c.update += 1,
                Action::Undeliverable { .. } => c.undeliverable += 1,
                Action::Drift => c.drift += 1,
                Action::Unknown => c.unknown += 1,
                Action::Snapshot { .. } => c.snapshot += 1,
                Action::InSync => {}
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
            .chain(self.unattached.iter())
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
            note: stale_note(src),
        }),
        TargetContent::Has(h) if h == &src.hash => Some(Verdict {
            action: Action::InSync,
            note: stale_note(src),
        }),
        TargetContent::Has(h) => {
            let action = Action::Update {
                from: short_hash(h),
                to: short_hash(&src.hash),
            };
            match ledger.find(harness, cap.id.kind, &cap.id.name) {
                // 账本记的就是盘上这个哈希：内容是 henv 放的，也没人动过。
                Some(entry) if &entry.content_hash == h => Some(Verdict {
                    action,
                    note: stale_note(src),
                }),
                // 账本说这里该是别的哈希：有人手改过，或上次交付只成功了一半。
                // 两种情况都照常覆盖（它终究是 henv 放的），但报告要显式说出来。
                // 「手改过」与「源没刷新上」可以同时成立，两句话都要留着。
                Some(_) => Some(Verdict {
                    action,
                    note: Some(match stale_note(src) {
                        Some(s) => format!("副本被手改过；{s}"),
                        None => "副本被手改过".to_string(),
                    }),
                }),
                // 盘上有内容、不是我们放的、又和清单要的不一样 → 不动它。
                None => Some(Verdict {
                    action: Action::Conflict {
                        reason: format!(
                            "目标已存在同名条目且不是 henv 交付的（{}），apply 不会覆盖",
                            short_hash(h)
                        ),
                    },
                    note: stale_note(src),
                }),
            }
        }
    }
}

/// 取出 MCP 的三段载荷；不是 MCP 就 `None`。
fn mcp_fields(spec: &Spec) -> Option<&McpTransport> {
    match spec {
        Spec::Mcp { transport } => Some(transport),
        _ => None,
    }
}

/// 从账本层裁决一条 MCP（§9.6 的判定表）。
///
/// 「是谁放的」这件事，内容层（skill）从字节读得出来，MCP 读不出来——目标侧只有
/// 一份配置，没有来源痕迹。所以这里的证据是账本：**本次交付时记下的哈希**。
///
/// 返回 `None` = 这条不是 MCP（或现状不是），交给原来的 `compare_versions`。
fn mcp_verdict(
    harness: &str,
    cap: &Capability,
    actual: &Capability,
    ledger: &Ledger,
) -> Option<Verdict> {
    let dt = mcp_fields(&cap.spec)?;
    let at = mcp_fields(&actual.spec)?;

    // 第一行：字节相同就不看账本。与 content_verdict 同一条道理——
    // 目标已经是对的了，它当初是谁放的不改变这个事实。
    if dt == at {
        return Some(Verdict {
            action: Action::InSync,
            note: None,
        });
    }

    let to = mcp_summary(dt);
    let from = mcp_summary(at);
    match ledger.find(harness, Kind::Mcp, &cap.id.name) {
        // 不同 + 无账本：目标上有一个同名、内容不同、又不是我们放的东西。
        // §9.3 不变式二：绝不覆盖，报出来让人自己决定。
        None => Some(Verdict {
            action: Action::Conflict {
                reason: format!("目标已存在同名 MCP 且不是 henv 交付的（{from}），apply 不会覆盖"),
            },
            note: None,
        }),
        // 哈希算的是**现状**（从配置里读回来的那份），不是期望。
        // 拿期望去比，改一次清单就会让所有 MCP 假报「被手改过」。
        Some(entry) if entry.content_hash == hash_mcp_spec(at) => Some(Verdict {
            action: Action::Update { from, to },
            note: None,
        }),
        Some(_) => Some(Verdict {
            action: Action::Update { from, to },
            note: Some("副本被手改过".to_string()),
        }),
    }
}

/// 内容层判得动就内容层，判不动就账本层（§9.6）。
///
/// 两层的适用面互斥：`content_verdict` 只认 `Spec::Skill`（第一行 `content.sources.get`），
/// `mcp_verdict` 只认 `Spec::Mcp`——所以这个 `or_else` 不是「谁优先」的策略，
/// 是「按 kind 分派」的写法。
fn ownership_verdict(
    harness: &str,
    cap: &Capability,
    actual: Option<&Capability>,
    content: &ContentIndex,
    ledger: &Ledger,
) -> Option<Verdict> {
    content_verdict(harness, cap, content, ledger).or_else(|| {
        let actual = actual?;
        mcp_verdict(harness, cap, actual, ledger)
    })
}

/// 源这次是「沿用缓存」时给行尾的注记（§9.4）。它不参与任何判定，
/// 只在报告里把「你看到的这份内容没经过上游确认」说出来。
fn stale_note(src: &ResolvedContent) -> Option<String> {
    src.stale.clone()
}

/// 报告里只显示哈希前 8 位：完整 64 位在终端里是噪音，而人要看的只是
/// 「两端是不是同一个东西」，8 位十六进制足够区分。
fn short_hash(h: &str) -> String {
    h.chars().take(8).collect()
}

/// 这条 plugin 条目拆出了几条 skill（§9.5 的「拆不出东西」判据）。
///
/// 按 `desired` 数而不是另立一张计数表：拆解产的 skill 就在 `desired` 里，
/// 数它是最直接的读法，也省掉一处会在两次运行之间漂移的状态。
fn decomposed_skill_count(desired: &[Capability], plugin: &str) -> usize {
    desired
        .iter()
        .filter(|c| {
            c.id.kind == Kind::Skill
                && matches!(&c.origin, Origin::Plugin { name } if name == plugin)
        })
        .count()
}

/// 拆解条目那批 `Kind::Plugin` 行的落点（§9.5「开关与目标」）。
///
/// 顺利拆解时**一条都不产生**；三种情况各产生一条 `undeliverable`：
/// 定位不到、交付目标差集为空、拆不出任何 skill。这第三种是「一片干净」的缺口——
/// 一个只有 `hooks/` 的 bundle 若什么都不报，用户看到的报告与「一切正常」分不开。
///
/// **落点**：源 harness 那一节——三种情况的问题都在它身上（它是内容来源）。
/// 它没有一节时（在 `harnesses:` 里被关掉、或本机没装）落第一家有节的；一家有节的
/// 都没有时进 `unattached`，由 `render` 的全局段接住。三种落点都**不许让这一行消失**：
/// 「清单里有一条根本无法满足的要求」必须出现在报告里。
///
/// **未安装的家不算「有节」**：`render` 对它只打一行 `not installed (skipped)`，
/// 挂上去等于丢掉——那正是全局段要消灭的静默。
fn detached_plugin_rows(
    desired: &[Capability],
    inputs: &[HarnessInput],
    content: &ContentIndex,
    sources: &SourceHarnessTable,
) -> DetachedRows {
    let known: Vec<&str> = inputs.iter().map(|i| i.state.name.as_str()).collect();
    let mut rows = DetachedRows::default();

    for cap in desired {
        if cap.id.kind != Kind::Plugin {
            continue;
        }
        let Some(source) = sources.get(&cap.id.name) else {
            continue; // 没写 `source_harness:`：走 `Spec` 那条常规路，不在这里分行
        };
        let reason = match content.unresolved.get(&cap.id) {
            // 内容层已经写好了整句（定位不到、manifest 解不开），原样带上。
            Some(Unresolved::Failed(r) | Unresolved::Deferred(r)) => r.clone(),
            None if cap.targets.minus(source, &known).is_empty() => format!(
                "plugin `{}`：交付目标 = targets − `{source}`，差集为空（目标只有源 harness 自己）（§9.5）",
                cap.id.name
            ),
            // 拆不出 skill → 这条 plugin 拿不到任何 skill 行，但那句
            // `degrade.<kind> = report` 的说明**不在这里印**：它统一进 `Plan.notes`
            // （`(notes)` 段），与拆得出 skill 的情形同一个落点。这一行只回答
            // 「它为什么交付不了」，不再兼任注记的载体。
            //
            // 注记本身不会因此丢掉：`content.notes` 整张表都进 `Plan.notes`，
            // 与这里的分支无关（终审 Minor#2 那条「`report` 不许退化成 `skip`」的
            // 性质由 `(notes)` 段守，比挂在某一行上更强）。
            None if decomposed_skill_count(desired, &cap.id.name) == 0 => format!(
                "plugin `{}`：bundle `{source}` 里拆不出任何 skill（§9.5）",
                cap.id.name
            ),
            None => continue,
        };
        let slot = inputs
            .iter()
            .find(|i| i.state.name == source && i.state.detected)
            .or_else(|| inputs.iter().find(|i| i.state.detected));
        let entry = PlanEntry::new(cap.id.clone(), Action::Undeliverable { reason });
        match slot {
            Some(slot) => rows
                .by_harness
                .entry(slot.state.name.clone())
                .or_default()
                .push(entry),
            // 一家有节的都没有：进全局段，而不是丢掉。
            None => rows.unattached.push(entry),
        }
    }
    rows
}

/// `detached_plugin_rows` 的产物：按 harness 分好的行，以及挂不进任何节的。
#[derive(Debug, Default)]
struct DetachedRows {
    by_harness: BTreeMap<String, Vec<PlanEntry>>,
    unattached: Vec<PlanEntry>,
}

/// §9.8：`--prune` 的规划层。纯函数、幂等、只读——与 `compute` 同一条纪律。
///
/// 把「账本里记着**且**本轮判为 [`Action::Drift`]」的条目改写成 [`Action::Remove`]，
/// 其余 drift 原样保留。**这是不变式二在规划层的落点**：账本说这个 slot 是 henv 的，
/// 才有资格删；用户手放的条目在账本里没有记录，于是永远是 `Drift`——
/// `--prune` 因此**不是**「让 harness 目录等于清单」。
///
/// 键必须带 `harness`：账本本来就是逐 harness 的，`codex` 名下的一条记录
/// 不能授权删掉 `claude-code` 的同名条目。
///
/// `Plan::unattached` 不参与：它只装 `undeliverable`（§9.5），没有 drift 可改判。
pub fn prune_plan(plan: &Plan, ledger: &Ledger) -> Plan {
    let mut out = plan.clone();
    for h in &mut out.harnesses {
        for e in &mut h.entries {
            if !matches!(e.action, Action::Drift) {
                continue;
            }
            if ledger.find(&h.harness, e.id.kind, &e.id.name).is_none() {
                continue;
            }
            e.action = Action::Remove;
            // 那句「--prune 可删」只对 drift 行成立：改判成 remove 之后它已经不
            // 「可删」而是「即将删」。留着会让 remove 行挂一句反话。
            e.note = None;
        }
        // 改判改变了 rank，必须重排：`render` 依赖「同组连续」。
        h.entries.sort_by(|x, y| {
            x.action
                .rank()
                .cmp(&y.action.rank())
                .then_with(|| x.id.label().cmp(&y.id.label()))
        });
    }
    out
}

/// §9.5：henv **不代装原生 plugin**（§9.1）。没写 `source_harness:` 的条目
/// 落在这个理由上——它仍然做版本对账，只是不再假装能装。
const NO_NATIVE_INSTALL: &str =
    "henv 不代装原生 plugin；写 `source_harness:` 可从某家的已安装 bundle 拆解交付（§9.5）";

/// `f(desired, actual) → Plan`，纯函数、幂等、只读（spec §3 决策 2）。
///
/// `content` 与 `ledger` 让「字节一不一样、是谁放的」参与判决（§9.3）。两者都为
/// 空时行为与 Plan A 逐字节相同——`ContentIndex::default()` + `Ledger::default()`
/// 就是那条路径。
///
/// `sources` 是 §9.5 的旁路表：这一条 plugin 的内容从哪一家取。
/// 它只回答「拆不拆、从哪拆」，不进 `Spec`、不参与版本判决。
///
/// `snapshots` 是 §10.3 的旁路表：哪些 plugin 条目是**记录型**、逐家版本记的是什么。
/// 它回答「这条只对账、不交付」，同样不进 `Spec`。
pub fn compute(
    desired: &[Capability],
    inputs: &[HarnessInput],
    degrade: &DegradeTable,
    content: &ContentIndex,
    ledger: &Ledger,
    sources: &SourceHarnessTable,
    snapshots: &SnapshotTable,
) -> Plan {
    // §9.5：拆解条目的报错行按落点分好，等各节出场时取走。
    let DetachedRows {
        mut by_harness,
        mut unattached,
    } = detached_plugin_rows(desired, inputs, content, sources);
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
        // 挂到这一节的拆解报错行（源家定位不到、差集为空、拆不出 skill）。
        entries.extend(by_harness.remove(&harness).unwrap_or_default());
        let mut matched: HashSet<CapId> = HashSet::new();

        let desired_plugins: HashSet<String> = desired
            .iter()
            .filter(|c| c.id.kind == Kind::Plugin && c.targets.includes(&harness))
            .map(|c| c.id.name.clone())
            .collect();

        for cap in desired {
            // §10.3：记录型条目（`source: snapshot`）**只对账、不交付**。分岔必须在
            // 任何 `supports()` / 版本比较**之前**：那两条路都会把一条 henv 修不了的
            // 要求变成 `install`/`update`→`undeliverable`（§9.5 说的那种「假成功」）。
            //
            // 吸收（`matched`）跟着 `targets` 走，与别的条目同一条判据：清单说
            // 「只记在 claude」而 kimi 也装着一份时，kimi 那份照旧报 drift——
            // 那是「它铺到别的家去了」，是该看见的事。
            if cap.id.kind == Kind::Plugin {
                if let Some(recorded_versions) = snapshots.get(&cap.id.name) {
                    if cap.targets.includes(&harness) {
                        matched.insert(cap.id.clone());
                        if let Some(recorded) = recorded_versions.get(&harness) {
                            let actual = state.actual.iter().find(|a| a.id == cap.id);
                            match actual {
                                // 清单记着它，本机没有：记录过期了。仍然只是警告
                                // （本机没装可以是正常状态，§10.1）。
                                None => entries.push(PlanEntry::new(
                                    cap.id.clone(),
                                    Action::Snapshot {
                                        recorded: recorded.clone(),
                                        actual: None,
                                    },
                                )),
                                Some(a) => {
                                    let actual_version = match &a.spec {
                                        Spec::Plugin { version, .. } => version.as_deref(),
                                        _ => None,
                                    };
                                    // `Same` 与 `Incomparable` 都安静：后者是 sha 形目录名、
                                    // 分支名、哨兵、codex 的「版本恒 None」——报它就是报一条
                                    // **永远修不掉**的 warning（§2.1 第 3 条）。
                                    if compare_recorded_version(recorded, actual_version)
                                        == RecordedMatch::Different
                                    {
                                        entries.push(PlanEntry::new(
                                            cap.id.clone(),
                                            Action::Snapshot {
                                                recorded: recorded.clone(),
                                                actual: actual_version.map(str::to_string),
                                            },
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
            }
            // §9.5：写了 `source_harness:` 的条目**不产生 `Kind::Plugin` 行**——
            // 顺利拆解时由拆出来的 skill 行承担，不顺利时由上面那批挂着的行承担。
            //
            // 源家那一段必须**先于** targets 判据：源家通常不在 targets 里
            // （交付目标 = targets − 源），但它的 `read_state()` 一定扫得到那个
            // plugin（我们刚从这里取的 `install_path`），不先吸收掉就会掉进
            // 下面的 drift 段，被报成「不在清单里的东西」——而它恰恰是清单要求的。
            if cap.id.kind == Kind::Plugin {
                if let Some(src) = sources.get(&cap.id.name) {
                    if harness == src {
                        matched.insert(cap.id.clone());
                        continue;
                    }
                    // 其余家：只要它是交付目标就吸收掉（这条的 plugin 行由拆出来的
                    // skill 承担）。**不在 targets 里的家照旧走 drift 判据**——
                    // 「清单说这条只给 codex」而 claude 也有时，今天报 drift，
                    // 这个行为不属于 B3b 要改的范围。
                    if cap.targets.includes(&harness) {
                        matched.insert(cap.id.clone());
                        continue;
                    }
                }
            }
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
                    // 实际条目先找出来：内容层（skill）与账本层（mcp）的裁决都需要它。
                    let actual = state.actual.iter().find(|a| a.id == cap.id);
                    let (verdict_action, verdict_note) =
                        match ownership_verdict(&state.name, cap, actual, content, ledger) {
                            Some(v) => (Some(v.action), v.note),
                            // 两层都判不了（不是 skill 也不是 mcp，或目标侧没扫到）不等于
                            // 「没什么好说的」：源这次是沿用没刷新的缓存时，「你看到的这份内容
                            // 没经过上游确认」照样成立，而且 install 是它最要紧的一条路——
                            // 要写盘的正是这些没确认过的字节。stale 是源的属性，不该因为
                            // 目标侧扫不到就消失。
                            None => (None, content.sources.get(&cap.id).and_then(stale_note)),
                        };
                    let action = match actual {
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
                    // 源拿不到，怎么落到这一行（§9.1 说清楚 + §9.3 的失败隔离）：
                    // - `Failed` 而这条动作**会写盘** → 换成可见的 undeliverable：
                    //   计划承诺了要做，实际做不到，那是要报出来的事，不是「跳过」。
                    // - 不写盘的动作（in-sync / unknown / conflict）只加注记：把一个
                    //   准确的判定改成一个吓人的名词，是另一种失真。
                    // - `Deferred`（diff 不联网）永远只加注记：apply 会去取。
                    let unresolved = content.unresolved.get(&cap.id);
                    let action = match (unresolved, &action) {
                        (
                            Some(Unresolved::Failed(reason)),
                            Action::Install | Action::Update { .. },
                        ) => Action::Undeliverable {
                            reason: reason.clone(),
                        },
                        _ => action,
                    };
                    // §9.5：没写 `source_harness:` 的条目不再假装能装（§9.1）。
                    // 只改 install/update 两条——`in-sync`/`unknown`/`conflict` 是
                    // 「对账结果」，照旧保留：这条目仍然回答「本机是不是清单要的那个版本」。
                    let action =
                        if cap.id.kind == Kind::Plugin && sources.get(&cap.id.name).is_none() {
                            match &action {
                                Action::Install => Action::Undeliverable {
                                    reason: NO_NATIVE_INSTALL.to_string(),
                                },
                                Action::Update { from, to } => Action::Undeliverable {
                                    reason: format!(
                                        "{NO_NATIVE_INSTALL}（清单要 {to}，本机是 {from}）"
                                    ),
                                },
                                other => other.clone(),
                            }
                        } else {
                            action
                        };
                    // 已经是 undeliverable 的行自带 reason，再挂一遍注记是同一句话说两遍。
                    let note = match unresolved {
                        Some(Unresolved::Deferred(r) | Unresolved::Failed(r))
                            if !matches!(action, Action::Undeliverable { .. }) =>
                        {
                            verdict_note.or_else(|| Some(r.clone()))
                        }
                        _ => verdict_note,
                    };
                    // §9.5：bundle 里装不下的部分（hooks/commands/…）那句说明**不挂在
                    // 行尾**——它进 `Plan.notes`，由 `(notes)` 段每条 plugin 印一次
                    // （行尾版本会随拆出来的 skill 数重复；两种说明同时在时也各归各位：
                    // 「源过期 / 副本被手改」是这一行的判定说明，留在行尾）。
                    entries.push(PlanEntry::new(cap.id.clone(), action).with_note(note));
                }
            }
        }

        for a in &state.actual {
            if matched.contains(&a.id) {
                continue;
            }
            // plugin bundle 分发出来的 skill 不是漂移。两种情形都算：
            // 1. 这条 plugin 的交付目标里有本家——它会作为拆出来的 skill 行出现；
            // 2. **本家就是它的 `source_harness`**——源家自己装着的那一份是内容来源
            //    （与上面 plugin 行 `harness == src` 那条吸收同一个理由）。显式写
            //    `targets: [codex]` 时源家不在 `desired_plugins` 里，只看那个集合就会
            //    把清单要的东西报成「不在清单里的东西」（§9.5、Review Focus 3）。
            if let Spec::Skill {
                source: SkillSource::PluginBundle { plugin },
                ..
            } = &a.spec
            {
                if desired_plugins.contains(plugin) || sources.get(plugin) == Some(harness.as_str())
                {
                    continue;
                }
            }
            // 这句提示必须与 `prune_plan` 的判据同源（§9.3 不变式二）：它要预告
            // `--prune` **会**对这条做什么。写死一句「可删」会把用户手放的东西
            // 也算进去——那正是 §9.8 明说的「非 henv 条目一律只报不删」。
            let hint = match ledger.find(&harness, a.id.kind, &a.id.name) {
                Some(_) => "未在清单中，--prune 可删",
                None => "未在清单中，只报不删",
            };
            entries.push(
                PlanEntry::new(a.id.clone(), Action::Drift).with_note(Some(hint.to_string())),
            );
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

    // 兜底：`by_harness` 的键都取自 detected 的家，而每个 detected 的家都会在上面
    // 取走自己那批，所以这里正常情况下已经空了。真有空隙也不能丢——那正是全局段
    // 要消灭的静默。
    for (_, rows) in by_harness {
        unattached.extend(rows);
    }
    unattached.sort_by(|x, y| {
        x.action
            .rank()
            .cmp(&y.action.rank())
            .then_with(|| x.id.label().cmp(&y.id.label()))
    });

    Plan {
        harnesses,
        unattached,
        // §9.5：整张注记表原样进报告，与上面任何一条判定无关——它的落点是
        // `(notes)` 段，不是某一行。
        notes: content.notes.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Targets};
    use crate::lower::{DegradeTable, SourceHarnessTable};
    use crate::manifest::Manifest;
    use crate::store::provenance::{Ledger, LedgerEntry};
    use crate::store::skills::{ContentIndex, ResolvedContent, TargetContent, Unresolved};
    use crate::testutil::{FakeAdapter, TempHome};
    use std::path::PathBuf;

    fn no_degrade() -> DegradeTable {
        DegradeTable::new(&Manifest::parse("version: 1\n").unwrap())
    }

    fn no_source_harness() -> SourceHarnessTable {
        SourceHarnessTable::default()
    }

    /// 一条写了 `source_harness:` 的 plugin 能力（`lower` 的产物形状 + 旁路表里有它）。
    fn plugin_with_source(name: &str) -> (Capability, SourceHarnessTable) {
        let m = crate::manifest::Manifest::parse(&format!(
            "version: 1\n\nplugins:\n  {name}:\n    source: github\n    repo: x/y\n    source_harness: claude-code\n"
        ))
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let table = SourceHarnessTable::new(&m);
        let cap = desired
            .into_iter()
            .find(|c| c.id.kind == Kind::Plugin)
            .expect("lower 会产出这条 plugin");
        (cap, table)
    }

    fn fake(harness: &str) -> FakeAdapter {
        FakeAdapter::native(harness) // `native` 已经 detected: true
    }

    /// 一批 adapter → 一批现状。
    ///
    /// **不返回 `HarnessInput`**：那个类型借 `state` 与 `adapter` 两样
    /// （`pub struct HarnessInput<'a> { state: &'a HarnessState, adapter: &'a dyn HarnessAdapter }`），
    /// 借不出函数体内的局部量。所以两样都由调用方持有，各返回一份。
    /// 既有的 `run_with_ledger` 在函数里内联了这两行；本 Task 的测试要一次给
    /// `compute` 两三家，才提出来。
    fn harness_states(adapters: Vec<FakeAdapter>) -> (Vec<HarnessState>, Vec<FakeAdapter>) {
        let states = adapters
            .iter()
            .map(|a| HarnessState {
                name: a.name().to_string(),
                detected: a.detect(),
                actual: a.read_state().unwrap(),
            })
            .collect();
        (states, adapters)
    }

    fn harness_inputs<'a>(
        states: &'a [HarnessState],
        adapters: &'a [FakeAdapter],
    ) -> Vec<HarnessInput<'a>> {
        states
            .iter()
            .zip(adapters)
            .map(|(state, adapter)| HarnessInput { state, adapter })
            .collect()
    }

    /// §9.5：写了 `source_harness:`、顺利拆解 → **不产生 `Kind::Plugin` 行**。
    /// 今天它会落一条 `+ install plugin/x`——而 apply 一个字节都写不出来。
    #[test]
    fn a_decomposed_plugin_produces_no_plugin_row() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        // 拆出来的那条 skill：目标是除源之外的家
        let skill = Capability {
            id: CapId::new(Kind::Skill, "brainstorming"),
            origin: Origin::Plugin { name: "p".into() },
            spec: Spec::Skill {
                source: SkillSource::PluginBundle { plugin: "p".into() },
                path: None,
            },
            targets: Targets::Only(vec!["codex".into(), "claude-code".into()]),
        };

        let plan = compute(
            &[plugin.clone(), skill],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );

        for h in &plan.harnesses {
            assert!(
                !h.entries.iter().any(|e| e.id.kind == Kind::Plugin),
                "{} 不该有 plugin 行：{:?}",
                h.harness,
                h.entries
            );
        }
    }

    /// §9.5 第 3 条：差集为空（`targets` 只有源家）→ 一条 `undeliverable`，
    /// 理由写明「目标只有源 harness 自己」。落点在源家那一节。
    #[test]
    fn an_empty_delivery_target_set_lands_as_one_row_on_the_source() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n    source_harness: claude-code\n    targets: [claude-code]\n",
        )
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let sources = SourceHarnessTable::new(&m);
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );

        let claude = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        let row = claude
            .entries
            .iter()
            .find(|e| e.id.kind == Kind::Plugin)
            .expect("要有一条 plugin 行");
        match &row.action {
            Action::Undeliverable { reason } => {
                assert!(reason.contains("差集为空"), "{reason}");
                assert!(reason.contains("claude-code"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            plan.harnesses
                .iter()
                .flat_map(|h| &h.entries)
                .filter(|e| e.id.kind == Kind::Plugin)
                .count(),
            1,
            "整份计划里只许有一条 plugin 行"
        );
    }

    /// §9.5：**一家有节的都没有**时，行进 `unattached`（全局段），而不是消失。
    ///
    /// 这就是「一家 harness 都没装」的机器：未安装的家整节被 `render` 跳过，
    /// 挂上去等于丢掉。过去这一行会被静默吞掉——`diff` 报 `0 undeliverable`，
    /// 而清单里明明有一条根本无法满足的要求。
    #[test]
    fn an_undeliverable_row_with_no_rendered_section_goes_to_the_global_bucket() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n    source_harness: claude-code\n",
        )
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let sources = SourceHarnessTable::new(&m);
        let (states, adapters) = harness_states(vec![
            fake("claude-code").with_detected(false),
            fake("codex").with_detected(false),
        ]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );

        assert!(
            plan.harnesses.iter().all(|h| h.entries.is_empty()),
            "未安装的家本来就该是空的：{:?}",
            plan.harnesses
        );
        assert_eq!(
            plan.unattached.len(),
            1,
            "这一行必须被接住：{:?}",
            plan.unattached
        );
        assert_eq!(plan.unattached[0].id.kind, Kind::Plugin);
        assert_eq!(plan.counts().undeliverable, 1, "汇总要数得到它");
        assert!(!plan.is_clean(), "有 undeliverable 就不是 clean");
    }

    /// §9.5 第 3 条后半：拆不出任何 skill 时也要有一条 `undeliverable`。
    /// 少了这一条，一个只有 `hooks/` 的 bundle 在报告里会是**一片干净**。
    #[test]
    fn a_bundle_that_yields_no_skill_still_reports_one_row() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        // 清单里只有那条 plugin，`desired` 里一条 `Origin::Plugin` 的 skill 都没有
        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );
        let row = plan
            .harnesses
            .iter()
            .flat_map(|h| &h.entries)
            .find(|e| e.id.kind == Kind::Plugin)
            .expect("拆不出东西要报出来");
        match &row.action {
            Action::Undeliverable { reason } => assert!(reason.contains("拆不出"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    /// §9.5「来源与定位」：定位不到 → 整条 undeliverable，理由原样来自内容层，
    /// 不再被包一层通用话（那是 `ResolvedContent` 那条路的行为，这里是 plugin 的）。
    #[test]
    fn an_unlocated_bundle_carries_the_content_layer_reason() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        let mut content = ContentIndex::default();
        content.unresolved.insert(
            plugin.id.clone(),
            Unresolved::Failed("plugin `p`：`claude-code` 的安装记录里没有安装路径（§9.5）".into()),
        );

        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &content,
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );
        let row = plan
            .harnesses
            .iter()
            .flat_map(|h| &h.entries)
            .find(|e| e.id.kind == Kind::Plugin)
            .expect("要报出来");
        match &row.action {
            Action::Undeliverable { reason } => {
                assert!(reason.contains("没有安装路径"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    /// §9.5 的 drift 陷阱：源家**一定**扫得到那个 plugin（我们刚从这里取的
    /// `install_path`）。它绝不能被报成 `- drift`——那是「不在清单里的东西」，
    /// 而它恰恰是清单要求的。
    #[test]
    fn the_installed_copy_on_the_source_harness_is_not_drift() {
        let (plugin, sources) = plugin_with_source("p");
        // 源家的现状里有这条 plugin（Origin::Adopted，与 adapter 扫出来的同形）
        let (states, adapters) = harness_states(vec![
            fake("claude-code").with_caps(vec![FakeAdapter::plugin("p", None, None)]),
            fake("codex"),
        ]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );
        let claude = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert!(
            claude.entries.iter().all(|e| e.action != Action::Drift),
            "源家那份是内容来源，不是 drift：{:?}",
            claude.entries
        );
    }

    /// 上面那条只放了 `Kind::Plugin`，走的是 `harness == src` 那条吸收。
    /// 这条钉的是**同一陷阱的第二个入口**：bundle 分发出来的 skill。
    /// 清单写 `targets: [codex]`（「把 claude 的 bundle 交给 codex」，最自然的写法）
    /// 时源家不在原始 targets 里，`desired_plugins` 因而是空的——
    /// 只按它判就会把源家自己那份报成 `- drift`（终审 Important#1）。
    #[test]
    fn a_bundled_skill_on_the_source_harness_is_not_drift_when_targets_exclude_it() {
        let (mut plugin, sources) = plugin_with_source("p");
        plugin.targets = Targets::Only(vec!["codex".into()]);
        // 源家 claude-code 的现状：那个 plugin 本身 + 它分发出来的一条 skill
        // （软链形状，`src/adapters/claude_code.rs` 扫出来的就是这两条）。
        let (states, adapters) = harness_states(vec![
            fake("claude-code").with_caps(vec![
                FakeAdapter::plugin("p", None, None),
                FakeAdapter::bundled_skill("brainstorming", "p"),
            ]),
            fake("codex"),
        ]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );
        let claude = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        assert!(
            claude.entries.iter().all(|e| e.action != Action::Drift),
            "源家那份是内容来源，不在 targets 里也一样：{:?}",
            claude.entries
        );
    }

    /// 反面控制：**不是**源家、又不在 targets 里的家，它自己装的那份照旧报 drift。
    /// §9.5 要的是「源家别误报」，不是「谁装了都别报」——
    /// 清单说这条只给 codex 而 kimi 也有时，那是用户该看见的事。
    #[test]
    fn a_bundled_skill_on_a_non_source_harness_still_drifts() {
        let (mut plugin, sources) = plugin_with_source("p");
        plugin.targets = Targets::Only(vec!["codex".into()]);
        let (states, adapters) = harness_states(vec![
            fake("claude-code"),
            fake("codex"),
            fake("kimi-code").with_caps(vec![FakeAdapter::bundled_skill("brainstorming", "p")]),
        ]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );
        let kimi = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "kimi-code")
            .unwrap();
        assert_eq!(
            kimi.entries.len(),
            1,
            "非源家的多出来那份要报出来：{:?}",
            kimi.entries
        );
        assert_eq!(kimi.entries[0].action, Action::Drift);
    }

    /// §9.5：一个只有 `hooks/`、拆不出任何 skill 的 bundle，`degrade.<kind> = report`
    /// 的那句「还有 hook 未拆解」不能因为没有 skill 行就丢掉——
    /// 丢了的话 `report` 事实上退化成 `skip`（终审 Minor#2）。
    ///
    /// 它现在落在 `(notes)` 段，而**不是**那条 `undeliverable` 的 reason 里：
    /// 那一行回答「为什么这条交付不了」，注记回答「还有什么没拆」。两件事各说各的，
    /// 合在一句里读起来像是同一句说了两遍，也让注记的去向跟着行的去留起落。
    #[test]
    fn a_bundle_with_no_skills_still_reports_the_leftover_note() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        let mut content = ContentIndex::default();
        content.notes.insert(
            "p".into(),
            "bundle `claude-code` 里还有 hook，未拆解（degrade.<kind> = report；要静音把它改成 skip）（§9.5）"
                .into(),
        );

        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &content,
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );
        let note = plan.notes.get("p").expect("拆不出 skill 也得留话");
        assert!(note.contains("hook"), "{note}");

        let row = plan
            .harnesses
            .iter()
            .flat_map(|h| &h.entries)
            .find(|e| matches!(e.action, Action::Undeliverable { .. }))
            .expect("拆不出 skill 要有 undeliverable 行");
        let Action::Undeliverable { reason } = &row.action else {
            unreachable!()
        };
        assert!(reason.contains("拆不出任何 skill"), "{reason}");
        assert!(
            !reason.contains("hook"),
            "注记不该在 reason 里再说一遍：{reason}"
        );
    }

    /// §9.5「开关与目标」后半：**没写** `source_harness:` 的条目，
    /// `install` 改判 `undeliverable`（不再假装能装），理由指路。
    #[test]
    fn an_install_for_a_plugin_without_a_source_harness_becomes_undeliverable() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n",
        )
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let (states, adapters) = harness_states(vec![fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
            &SnapshotTable::default(),
        );
        match &plan.harnesses[0].entries[0].action {
            Action::Undeliverable { reason } => {
                assert!(reason.contains("source_harness"), "指路要指对：{reason}");
                assert!(reason.contains("§9.5"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 同一条道理推到 `update`：理由里要带上「清单要什么、本机是什么」。
    #[test]
    fn an_update_for_a_plugin_without_a_source_harness_becomes_undeliverable() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n    ref: v2.0\n",
        )
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let (states, adapters) =
            harness_states(vec![fake("codex").with_caps(vec![FakeAdapter::plugin(
                "p",
                Some("v1.0"),
                None,
            )])]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
            &SnapshotTable::default(),
        );
        match &plan.harnesses[0].entries[0].action {
            Action::Undeliverable { reason } => {
                assert!(reason.contains("v2") && reason.contains("v1"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// **`in-sync` 与 `unknown` 原样保留**（§9.5）：没写 `source_harness:` 的条目
    /// 仍然做版本对账，只是不再假装能装。改判范围只到 install/update 为止。
    #[test]
    fn a_plugin_without_a_source_harness_keeps_in_sync_and_unknown() {
        // 版本同 → in-sync
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n    ref: v1.0\n",
        )
        .unwrap();
        let desired = crate::lower::lower(&m);
        let (states, adapters) =
            harness_states(vec![fake("codex").with_caps(vec![FakeAdapter::plugin(
                "p",
                Some("v1.0"),
                None,
            )])]);
        let inputs = harness_inputs(&states, &adapters);
        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
            &SnapshotTable::default(),
        );
        assert_eq!(plan.harnesses[0].entries[0].action, Action::InSync);

        // 版本无法判定 → unknown（codex 的常态：它不记版本）
        let (states, adapters) = harness_states(vec![
            fake("codex").with_caps(vec![FakeAdapter::plugin("p", None, None)]),
        ]);
        let inputs = harness_inputs(&states, &adapters);
        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
            &SnapshotTable::default(),
        );
        assert_eq!(plan.harnesses[0].entries[0].action, Action::Unknown);
    }

    /// 造一份记录型清单 + 它的旁路表。`versions` 是 YAML 片段（缩进由调用方给）。
    fn snapshot_manifest(body: &str) -> (crate::manifest::Manifest, SnapshotTable) {
        let m = crate::manifest::Manifest::parse(&format!(
            "version: 1\n\nplugins:\n  p:\n    source: snapshot\n{body}"
        ))
        .expect("清单合法");
        let t = SnapshotTable::new(&m);
        (m, t)
    }

    fn run_with_snapshots(
        m: &crate::manifest::Manifest,
        snapshots: &SnapshotTable,
        adapters: Vec<FakeAdapter>,
    ) -> Plan {
        let desired = crate::lower::lower(m);
        let (states, adapters) = harness_states(adapters);
        let inputs = harness_inputs(&states, &adapters);
        compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
            snapshots,
        )
    }

    /// §10.3 的核心不变式：记录型条目**永不交付**。本机装着同版本时安静地
    /// 被吸收（不再是 drift），任何一条 `install`/`update`/`undeliverable`
    /// 都不许出现（Review Focus 1）。
    #[test]
    fn a_snapshot_entry_never_delivers_and_absorbs_the_copy() {
        let (m, snapshots) = snapshot_manifest("    versions:\n      codex: v1.0\n");
        let plan = run_with_snapshots(
            &m,
            &snapshots,
            vec![fake("codex").with_caps(vec![FakeAdapter::plugin("p", Some("v1.0"), None)])],
        );
        let c = plan.counts();
        assert_eq!(c.install, 0, "{plan:?}");
        assert_eq!(c.update, 0, "{plan:?}");
        assert_eq!(c.undeliverable, 0, "{plan:?}");
        assert_eq!(c.drift, 0, "本机那份是清单记的，不该再报 drift：{plan:?}");
        assert_eq!(c.snapshot, 0, "版本对上了就没有对账行：{plan:?}");
        assert_eq!(plan.harnesses[0].entries, vec![]);
    }

    /// 版本不符 → 一条对账行，两边都是原值。
    #[test]
    fn a_recorded_version_that_differs_produces_one_snapshot_line() {
        let (m, snapshots) = snapshot_manifest("    versions:\n      codex: v1.0\n");
        let plan = run_with_snapshots(
            &m,
            &snapshots,
            vec![fake("codex").with_caps(vec![FakeAdapter::plugin("p", Some("v2.0"), None)])],
        );
        assert_eq!(
            plan.harnesses[0].entries[0].action,
            Action::Snapshot {
                recorded: "v1.0".into(),
                actual: Some("v2.0".into()),
            }
        );
        assert_eq!(plan.counts().snapshot, 1);
    }

    /// 版本**逐家**对账（§2.2 第 6 条）：同一个 plugin 在两家可以是两个版本，
    /// 各自记、各自比。`6.4.1` 与 `v6.3.0` 都做归一化后相等 ⇒ 两家都安静。
    #[test]
    fn a_recorded_version_is_compared_per_harness() {
        let (m, snapshots) =
            snapshot_manifest("    versions:\n      claude-code: 6.4.1\n      kimi-code: v6.3.0\n");
        let plan = run_with_snapshots(
            &m,
            &snapshots,
            vec![
                fake("claude-code").with_caps(vec![FakeAdapter::plugin("p", Some("6.4.1"), None)]),
                fake("kimi-code").with_caps(vec![FakeAdapter::plugin("p", Some("6.3.0"), None)]),
            ],
        );
        assert_eq!(plan.counts().snapshot, 0, "两家都该安静：{plan:?}");
        assert_eq!(plan.counts().drift, 0);
    }

    /// §10.3：**同一个 plugin 身份可以同时有一条交付条目和一条记录条目**，各管几家。
    /// 真机上的形状就是它——claude 侧是 bundle（拆解交给 codex），kimi 侧自带一份同名
    /// plugin、只做记录。两条条目的 `entry` 同名、`targets` 不相交。
    ///
    /// 不这么写、只把交付条目的 `targets` 收窄，那家就从「要求交付但交付不了」
    /// （`undeliverable`，问题）滑成「不在清单里」（`drift`，警告）——数字好看，
    /// 但清单不再如实描述本机。这条钉住记录条目确实把它吸收掉。
    #[test]
    fn a_record_entry_can_share_an_identity_with_a_delivery_entry() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\n\
             plugins:\n  bundle:\n    source: github\n    repo: x/y\n    entry: superpowers\n    \
             source_harness: claude-code\n    targets: [claude-code, codex]\n  \
             kimi-copy:\n    source: snapshot\n    entry: superpowers\n    \
             versions:\n      kimi-code: v6.3.0\n    targets: [kimi-code]\n",
        )
        .expect("同身份的两条条目是合法清单");
        let desired = crate::lower::lower(&m);
        let snapshots = SnapshotTable::new(&m);
        let sources = SourceHarnessTable::new(&m);
        let (states, adapters) = harness_states(vec![
            fake("claude-code").with_caps(vec![FakeAdapter::plugin("superpowers", None, None)]),
            fake("codex"),
            fake("kimi-code").with_caps(vec![FakeAdapter::plugin(
                "superpowers",
                Some("v6.3.0"),
                None,
            )]),
        ]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
            &snapshots,
        );

        assert_eq!(plan.counts().conflict, 0, "{plan:?}");
        let kimi = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "kimi-code")
            .expect("kimi-code 在报告里");
        assert_eq!(
            kimi.entries,
            vec![],
            "记录条目要吸收 kimi 自带的那份，而不是报 drift：{plan:?}"
        );
    }

    /// 清单记着它、本机没有 → 一条行（`actual: None`）。记录过期了要说出来，
    /// 但它在 doctor 里是警告（Task 3），不是门禁。
    #[test]
    fn a_snapshot_entry_recorded_but_not_installed_reports_missing() {
        let (m, snapshots) = snapshot_manifest("    versions:\n      codex: v1.0\n");
        let plan = run_with_snapshots(&m, &snapshots, vec![fake("codex")]);
        assert_eq!(
            plan.harnesses[0].entries[0].action,
            Action::Snapshot {
                recorded: "v1.0".into(),
                actual: None,
            }
        );
    }

    /// 不可比的版本一律安静（Review Focus 3）：sha 形、哨兵、本机读不出（codex）
    /// 都不许变成一条**永远修不掉**的 warning。
    #[test]
    fn an_incomparable_recorded_version_stays_quiet() {
        for record in ["db467cc56673", "unknown", "main"] {
            let (m, snapshots) =
                snapshot_manifest(&format!("    versions:\n      codex: {record}\n"));
            // 本机装了、版本是 version 形：只要清单那侧不可比就必须安静。
            let plan = run_with_snapshots(
                &m,
                &snapshots,
                vec![fake("codex").with_caps(vec![FakeAdapter::plugin("p", Some("v1.0"), None)])],
            );
            assert_eq!(plan.counts().snapshot, 0, "`{record}` 不该产行：{plan:?}");
            assert_eq!(plan.counts().drift, 0, "`{record}` 仍在清单里：{plan:?}");
        }

        // 反向：清单记的是 version 形、本机读不出（codex 恒 `None`）→ 同样安静。
        let (m, snapshots) = snapshot_manifest("    versions:\n      codex: v1.0\n");
        let plan = run_with_snapshots(
            &m,
            &snapshots,
            vec![fake("codex").with_caps(vec![FakeAdapter::plugin("p", None, None)])],
        );
        assert_eq!(plan.counts().snapshot, 0, "{plan:?}");
    }

    /// 吸收范围跟着 `targets` 走（与别的条目同一条判据）：清单说「只记在
    /// claude」，而 kimi 也装着一份 → kimi 那份照旧 drift。那是「它铺到别的家
    /// 去了」，是该看见的事，不是记录型条目要吞掉的噪音。
    #[test]
    fn a_snapshot_entry_only_absorbs_the_harnesses_it_records() {
        let (m, snapshots) = snapshot_manifest(
            "    targets: [claude-code]\n    versions:\n      claude-code: v1.0\n",
        );
        let plugin = || FakeAdapter::plugin("p", Some("v1.0"), None);
        let plan = run_with_snapshots(
            &m,
            &snapshots,
            vec![
                fake("claude-code").with_caps(vec![plugin()]),
                fake("kimi-code").with_caps(vec![plugin()]),
            ],
        );
        let kimi = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "kimi-code")
            .expect("kimi 有一节");
        assert_eq!(kimi.entries.len(), 1, "{kimi:?}");
        assert_eq!(kimi.entries[0].action, Action::Drift);
        let claude = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .expect("claude 有一节");
        assert_eq!(claude.entries, vec![]);
    }

    /// §9.5「交付什么」：bundle 里装不下的部分（hooks/commands/...）进的是
    /// **该条目的报告说明**（`Plan.notes` / `(notes)` 段），不是 diff 行、
    /// 也不为它们新建能力身份。
    ///
    /// 而且**只落一次**：它是「每条 plugin 一句」，不是「每个 skill 一句」。
    /// 挂在拆出来的 skill 行尾会让同一句话随 skill 数量重复（真机上 14 条），
    /// 那正是这层载体存在要消灭的东西。
    #[test]
    fn the_leftover_note_lands_in_the_plan_notes_once() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        let skill = Capability {
            id: CapId::new(Kind::Skill, "brainstorming"),
            origin: Origin::Plugin { name: "p".into() },
            spec: Spec::Skill {
                source: SkillSource::PluginBundle { plugin: "p".into() },
                path: None,
            },
            targets: Targets::Only(vec!["codex".into()]),
        };
        let mut content = ContentIndex::default();
        content.notes.insert(
            "p".into(),
            "bundle `claude-code` 里还有 hook，未拆解（degrade.<kind> = report）".into(),
        );

        let plan = compute(
            &[plugin, skill],
            &inputs,
            &no_degrade(),
            &content,
            &Ledger::default(),
            &sources,
            &SnapshotTable::default(),
        );
        let note = plan.notes.get("p").expect("装不下的部分要留话");
        assert!(note.contains("hook"), "{note}");
        // 关键回归：这一行不许再挂一遍——不然 14 条 skill 就是同一句话印 14 遍。
        for row in plan.harnesses.iter().flat_map(|h| &h.entries) {
            assert_eq!(row.note, None, "{} 不该带注记", row.id.label());
        }
    }

    /// 既有测试的默认入参：内容层缺席 = Plan A 行为。
    fn plan_a(desired: &[Capability], inputs: &[HarnessInput]) -> Plan {
        compute(
            desired,
            inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
            &SnapshotTable::default(),
        )
    }

    fn desired(kind: Kind, name: &str, spec: Spec) -> Capability {
        Capability::new(CapId::new(kind, name), spec)
    }

    fn mcp_spec(cmd: &str) -> Spec {
        Spec::Mcp {
            transport: McpTransport::Stdio {
                command: cmd.into(),
                args: vec![],
                env: Default::default(),
            },
        }
    }

    /// 既有测试的默认入口：空账本。
    fn run(desired: &[Capability], adapter: FakeAdapter) -> HarnessPlan {
        run_with_ledger(desired, adapter, &Ledger::default())
    }

    fn plan_a_with_ledger(
        desired: &[Capability],
        inputs: &[HarnessInput],
        ledger: &Ledger,
    ) -> Plan {
        compute(
            desired,
            inputs,
            &no_degrade(),
            &ContentIndex::default(),
            ledger,
            &no_source_harness(),
            &SnapshotTable::default(),
        )
    }

    fn run_with_ledger(
        desired: &[Capability],
        adapter: FakeAdapter,
        ledger: &Ledger,
    ) -> HarnessPlan {
        let state = HarnessState {
            name: adapter.name().to_string(),
            detected: adapter.detect(),
            actual: adapter.read_state().unwrap(),
        };
        let inputs = vec![HarnessInput {
            state: &state,
            adapter: &adapter,
        }];
        plan_a_with_ledger(desired, &inputs, ledger)
            .harnesses
            .into_iter()
            .next()
            .unwrap()
    }

    fn labels(hp: &HarnessPlan) -> Vec<(String, String)> {
        hp.entries
            .iter()
            .map(|e| {
                (
                    e.id.label(),
                    match &e.action {
                        Action::Conflict { .. } => "conflict".to_string(),
                        Action::Remove => "remove".to_string(),
                        Action::Install => "install".to_string(),
                        Action::Update { .. } => "update".to_string(),
                        Action::InSync => "in-sync".to_string(),
                        Action::Undeliverable { .. } => "undeliverable".to_string(),
                        Action::Drift => "drift".to_string(),
                        Action::Unknown => "unknown".to_string(),
                        Action::Snapshot { .. } => "snapshot".to_string(),
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

    fn mcp_entry(harness: &str, name: &str, hash: &str) -> LedgerEntry {
        LedgerEntry {
            harness: harness.into(),
            kind: Kind::Mcp,
            name: name.into(),
            path: PathBuf::from("/cfg").join(harness),
            source_id: format!("manifest:mcp/{name}"),
            content_hash: hash.into(),
            delivered_at_unix: 1_700_000_000,
        }
    }

    /// §9.6 表第一行：字节相同就不看账本。账本里有没有这条、是不是 henv 交付的，
    /// 都不改变「它已经是对的了」。
    #[test]
    fn an_identical_mcp_is_in_sync_even_without_a_ledger_entry() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "npx")]),
        );
        assert_eq!(labels(&hp), vec![("mcp/context7".into(), "in-sync".into())]);
    }

    /// §9.6 表第二行：不同 + 无账本 = 不是 henv 交付的 → conflict。
    /// 这条**取代**了 B1 时期那个「不同即 update」的旧结论。
    #[test]
    fn a_differing_mcp_without_a_ledger_entry_is_a_conflict() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "other")]),
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/context7".into(), "conflict".into())]
        );
        match &hp.entries[0].action {
            Action::Conflict { reason } => assert!(
                reason.contains("不是 henv 交付的"),
                "理由要说清为什么不动手：{reason}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// §9.6 表第三行：不同 + 有账本 + 账本哈希 == 现状 → 是我们交付的旧版本，放心 update。
    /// 账本哈希算的是**现状**（从配置里读回来的那份），不是期望。
    #[test]
    fn a_differing_mcp_we_delivered_is_an_update() {
        let actual = FakeAdapter::mcp("context7", "old");
        let Spec::Mcp { transport } = &actual.spec else {
            unreachable!()
        };
        let mut l = Ledger::default();
        l.record(mcp_entry(
            "codex",
            "context7",
            &crate::store::hash::hash_mcp_spec(transport),
        ));

        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![actual]),
            &l,
        );
        match &hp.entries[0].action {
            Action::Update { from, to } => {
                assert_eq!(from, "old", "from = 现状");
                assert_eq!(to, "npx", "to = 期望");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(hp.entries[0].note, None, "没被手改过就不该有注");
    }

    /// §9.6 表第四行：账本哈希 != 现状 → 交付之后有人改了它。仍然 update
    /// （账本说这块地是我们的），但要把这件事说出来。
    #[test]
    fn a_delivered_mcp_edited_by_hand_gets_the_note() {
        let mut l = Ledger::default();
        l.record(mcp_entry("codex", "context7", "hash-of-something-else"));

        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "old")]),
            &l,
        );
        assert!(matches!(hp.entries[0].action, Action::Update { .. }));
        assert_eq!(hp.entries[0].note.as_deref(), Some("副本被手改过"));
    }

    /// 改造之后**不能再**拿期望的哈希去比账本：那样清单每改一次，
    /// 所有 MCP 都会假报「被手改过」——这正是本 Task 最容易写错的一处。
    /// 构造：账本记的是**旧期望**的哈希，而现状是同一份内容。
    #[test]
    fn changing_the_manifest_does_not_fake_a_hand_edit() {
        let old_desired = FakeAdapter::mcp("context7", "npx");
        let Spec::Mcp { transport } = &old_desired.spec else {
            unreachable!()
        };
        let mut l = Ledger::default();
        l.record(mcp_entry(
            "codex",
            "context7",
            &crate::store::hash::hash_mcp_spec(transport),
        ));

        // 现状与账本哈希一致 → 没被手改过，尽管期望已经变了
        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("uvx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "npx")]),
            &l,
        );
        assert_eq!(hp.entries[0].note, None, "改了清单不是「被手改过」");
    }

    /// 同名的一件事：skill 与 mcp 的账本槽不能串（Task 1 的签名在 diff 里的兑现）。
    #[test]
    fn an_mcp_verdict_does_not_read_the_skill_ledger_slot() {
        let mut l = Ledger::default();
        // 只记了 skill 那条，mcp 那条没有
        l.record(LedgerEntry {
            harness: "codex".into(),
            kind: Kind::Skill,
            name: "context7".into(),
            path: PathBuf::from("/skills/context7"),
            source_id: "agents-dir".into(),
            content_hash: "skill-hash".into(),
            delivered_at_unix: 1,
        });

        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "other")]),
            &l,
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/context7".into(), "conflict".into())],
            "skill 的账本条目不许被当成 mcp 的交付凭证"
        );
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
    fn same_sha_across_differing_version_strings_is_undeliverable() {
        // spec §2.1 第 3 条修订后的核心场景：sha 相同不再判 Same（gitCommitSha 陈旧，
        // §2.2 第 7 条），version 6.4.1 vs v6.3.0 归一化后不同 → Different。
        // §9.5 第 3 条再把这条 `update` 改判 `undeliverable`（没写 source_harness:
        // 不再假装能装）。compare_versions 的 `Different` 仍在跑——from/to 落进理由串；
        // 断 undeliverable 仍能抓住 compare_versions 的回归：判成 Same/Unknown 会
        // 得 in-sync/unknown，这里就失败。
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
            vec![(
                "plugin/superpowers".to_string(),
                "undeliverable".to_string()
            )]
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
            &no_source_harness(),
            &SnapshotTable::default(),
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
            &no_source_harness(),
            &SnapshotTable::default(),
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
            fn mcp_target(&self) -> Option<crate::apply::mcp::McpTarget> {
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
            &[desired(Kind::Mcp, "bundle", mcp_spec("x"))],
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
                stale: None,
            },
        );
        idx.targets
            .entry("claude-code".to_string())
            .or_default()
            .insert(name.to_string(), target);
        idx
    }

    /// `content_plan` 只喂了 claude-code 一家，取它那一行。
    fn entry_of<'a>(plan: &'a Plan, label: &str) -> &'a PlanEntry {
        let hp = &plan.harnesses[0];
        hp.entries
            .iter()
            .find(|e| e.id.label() == label)
            .unwrap_or_else(|| panic!("没有 {label} 这一行：{:?}", labels(hp)))
    }

    /// 一条现状侧的 skill（adapter 扫到的形态）。
    fn adopted_skill(name: &str) -> Capability {
        let mut c = FakeAdapter::cap(
            Kind::Skill,
            name,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(PathBuf::from(format!("/target/{name}"))),
            },
        );
        c.origin = Origin::Adopted {
            harness: "claude-code".into(),
        };
        c
    }

    fn content_with_unresolved(name: &str, u: Unresolved) -> ContentIndex {
        let mut idx = ContentIndex::default();
        idx.unresolved.insert(CapId::new(Kind::Skill, name), u);
        idx
    }

    /// 与 `content_with` 同一份内容，只是这次是**沿用缓存**拿到的（§9.4）。
    fn stale_content(
        name: &str,
        source_hash: &str,
        target: TargetContent,
        why: &str,
    ) -> ContentIndex {
        let mut idx = content_with(name, source_hash, target);
        idx.sources
            .get_mut(&CapId::new(Kind::Skill, name))
            .expect("content_with 刚插进去的那个源")
            .stale = Some(why.into());
        idx
    }

    #[test]
    fn a_stale_source_marks_every_entry_it_touches() {
        // §9.4：沿用缓存内容时条目要标 stale，且**不报失败**——离线也要能用。
        // 但「不报失败」不等于「什么都不说」：用户得知道这份内容没经过上游确认。
        let content = stale_content(
            "foo",
            &"a".repeat(64),
            TargetContent::NoContent,
            "未能刷新上游",
        );
        let plan = content_plan(vec![], &content, &Ledger::default());
        let e = entry_of(&plan, "skill/foo");
        assert!(matches!(e.action, Action::Install), "stale 不改动作");
        assert!(
            e.note.as_deref().unwrap_or_default().contains("未能刷新"),
            "实际 note：{:?}",
            e.note
        );
    }

    #[test]
    fn a_stale_source_still_marks_an_entry_the_target_side_never_scanned() {
        // stale 是**源**的属性，不是「目标侧扫到了什么」的属性。目标目录还不存在
        // （第一次 apply 之前的样子）时内容层给不出裁决，于是落回 Plan A 的
        // `compare_versions`——但「这份内容没经过上游确认」依然成立，而且恰恰是
        // 在 install 这条路上最要紧：要写盘的正是那些没确认过的字节。
        let mut content = stale_content(
            "foo",
            &"a".repeat(64),
            TargetContent::NoContent,
            "未能刷新上游",
        );
        content.targets.clear(); // 目标侧一条都没扫到
        let plan = content_plan(vec![], &content, &Ledger::default());
        let e = entry_of(&plan, "skill/foo");
        assert!(
            matches!(e.action, Action::Install),
            "内容层判不了就落回 Plan A 的动作，不改判定"
        );
        assert!(
            e.note.as_deref().unwrap_or_default().contains("未能刷新"),
            "换到哪条判定路径上，stale 这个事实都不能丢，实际 note：{:?}",
            e.note
        );
    }

    #[test]
    fn a_failed_source_turns_a_promised_install_into_undeliverable() {
        // 清单说了要装，源却拿不到——这不是「跳过」，是「承诺了的事做不到」。
        // 计划里就必须看得见（§9.1），不能等交付层什么都不做、什么都不说。
        let content =
            content_with_unresolved("foo", Unresolved::Failed("源目录不存在：/nope".into()));
        let plan = content_plan(vec![], &content, &Ledger::default());
        match &entry_of(&plan, "skill/foo").action {
            Action::Undeliverable { reason } => assert!(
                reason.contains("源目录不存在"),
                "理由要原样带到计划里，实际：{reason}"
            ),
            other => panic!("讲好要装却装不了，必须报 undeliverable，实际 {other:?}"),
        }
    }

    #[test]
    fn a_deferred_source_keeps_the_plan_a_verdict_and_only_adds_a_note() {
        // diff 不联网：动作照 Plan A 判（目标是空的 → install），行尾说明「这次判不了」。
        // **不能**报 undeliverable——apply 会去取，那是吓唬人。
        let content = content_with_unresolved(
            "foo",
            Unresolved::Deferred("diff 不联网；缓存里没有这个源的内容（apply 会去取）".into()),
        );
        let plan = content_plan(vec![], &content, &Ledger::default());
        let e = entry_of(&plan, "skill/foo");
        assert!(matches!(e.action, Action::Install), "实际 {:?}", e.action);
        assert!(
            e.note.as_deref().unwrap_or_default().contains("不联网"),
            "判不了的原因要写在行尾，实际 {:?}",
            e.note
        );
    }

    #[test]
    fn a_failed_source_does_not_override_a_verdict_that_asks_for_no_write() {
        // 源拿不到、两边又比不出高低 → unknown。它不是「要做而做不到」，
        // 只是「判不了」；把它换成 undeliverable 会把一个准确的判定改成一个吓人的名词。
        let content = content_with_unresolved("foo", Unresolved::Failed("取不到".into()));
        let plan = content_plan(vec![adopted_skill("foo")], &content, &Ledger::default());
        let e = entry_of(&plan, "skill/foo");
        assert!(
            !matches!(e.action, Action::Undeliverable { .. }),
            "不写盘的动作不该被改成 undeliverable，实际 {:?}",
            e.action
        );
        assert!(e.note.is_some(), "但「为什么判不了」要说出来");
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
            &no_source_harness(),
            &SnapshotTable::default(),
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

    /// 报告要能**预告** `--prune` 会做什么（§9.3 不变式二）：同样是 drift 行，
    /// 账本记着的那条写「--prune 可删」，用户手放的那条写「只报不删」——
    /// 两句用的是**同一个判据**（`ledger.find`），与 `prune_plan` 同源。
    #[test]
    fn a_drift_row_says_whether_prune_can_touch_it() {
        let t = TempHome::new();
        let actual = vec![actual_skill(&t, "owned"), actual_skill(&t, "handmade")];
        let plan = content_plan(
            actual,
            &ContentIndex::default(),
            &ledger_claiming("owned", "h"),
        );

        let note = |name: &str| {
            plan.harnesses[0]
                .entries
                .iter()
                .find(|e| e.id.name == name)
                .unwrap_or_else(|| panic!("{name} 该有一行 drift"))
                .note
                .clone()
                .unwrap_or_default()
        };
        assert!(note("owned").contains("--prune 可删"), "{}", note("owned"));
        assert!(
            note("handmade").contains("只报不删"),
            "{}",
            note("handmade")
        );
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

    /// §9.8：`Action::Remove` 是 `--prune` 的执行形状。`diff` 永不产它，
    /// 但 rank 必须先定好——渲染与排序都读它，插进来也不能打乱既有相对顺序。
    #[test]
    fn remove_ranks_after_conflict_and_before_install() {
        let update = Action::Update {
            from: "a".into(),
            to: "b".into(),
        };
        let undeliverable = Action::Undeliverable { reason: "r".into() };
        assert_eq!(Action::Conflict { reason: "x".into() }.rank(), 0);
        assert_eq!(Action::Remove.rank(), 1);
        assert!(Action::Remove.rank() < Action::Install.rank());
        assert!(Action::Install.rank() < update.rank());
        assert!(update.rank() < undeliverable.rank());
        assert!(undeliverable.rank() < Action::Drift.rank());
        assert!(Action::Drift.rank() < Action::Unknown.rank());
        assert!(Action::Unknown.rank() < Action::InSync.rank());
    }

    /// `remove` 今天恒为 0（`diff` 不产删除），但计数逻辑必须已经在，
    /// 且 `is_clean()` 认它——否则 `--prune` 的计划会被报成「干净」。
    #[test]
    fn a_remove_entry_counts_and_makes_the_plan_unclean() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![PlanEntry::new(
                    CapId::new(Kind::Skill, "gone"),
                    Action::Remove,
                )],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        assert_eq!(plan.counts().remove, 1);
        assert!(!plan.is_clean());
    }

    fn skill_entry(name: &str, action: Action) -> PlanEntry {
        PlanEntry::new(CapId::new(Kind::Skill, name), action)
    }

    fn ledger_entry(harness: &str, kind: Kind, name: &str) -> LedgerEntry {
        LedgerEntry {
            harness: harness.into(),
            kind,
            name: name.into(),
            path: PathBuf::from("/somewhere").join(name),
            source_id: "agents-dir".into(),
            content_hash: "h".into(),
            delivered_at_unix: 1_700_000_000,
        }
    }

    /// §9.8 的核心判决：**账本里记着的 drift 才是删除候选**。
    /// 非账本 drift（用户手放的）原样保留——`--prune` 不是「让目录等于清单」。
    #[test]
    fn prune_turns_only_ledger_recorded_drift_into_removals() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![
                    skill_entry("owned", Action::Drift),
                    skill_entry("handmade", Action::Drift),
                    skill_entry("wanted", Action::Install),
                ],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        let mut ledger = Ledger::default();
        ledger.record(ledger_entry("claude-code", Kind::Skill, "owned"));

        let pruned = prune_plan(&plan, &ledger);

        let action = |name: &str| {
            pruned.harnesses[0]
                .entries
                .iter()
                .find(|e| e.id.name == name)
                .unwrap()
                .action
                .clone()
        };
        assert_eq!(action("owned"), Action::Remove);
        assert_eq!(action("handmade"), Action::Drift, "非账本条目只报不删");
        assert_eq!(action("wanted"), Action::Install, "其余动作不受影响");

        // 原计划一个字节没动——`prune_plan` 是纯函数。
        assert_eq!(
            plan.harnesses[0].entries[0].action,
            Action::Drift,
            "不许改入参"
        );
    }

    /// 改判成 `Remove` 之后，「--prune 可删」必须消失——它已经不是「可删」而是
    /// 「即将删」。render 的 remove 行会把注记原样贴进括号后，留着就是一句反话。
    #[test]
    fn a_removal_drops_the_stale_prune_hint() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![
                    skill_entry("owned", Action::Drift)
                        .with_note(Some("未在清单中，--prune 可删".into())),
                ],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        let mut ledger = Ledger::default();
        ledger.record(ledger_entry("claude-code", Kind::Skill, "owned"));

        let pruned = prune_plan(&plan, &ledger);
        assert_eq!(pruned.harnesses[0].entries[0].action, Action::Remove);
        assert_eq!(
            pruned.harnesses[0].entries[0].note, None,
            "remove 行不能挂着「可删」"
        );
    }

    /// 账本键含 harness：`codex` 名下的一条记录不能把 `claude-code` 的 drift
    /// 变成删除，反过来也一样。
    #[test]
    fn a_ledger_entry_from_another_harness_does_not_authorize_a_removal() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![skill_entry("foo", Action::Drift)],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        let mut ledger = Ledger::default();
        ledger.record(ledger_entry("codex", Kind::Skill, "foo"));

        assert_eq!(
            prune_plan(&plan, &ledger).harnesses[0].entries[0].action,
            Action::Drift
        );
    }

    /// 空账本 = 一条都不删。这是「账本丢了只会退化成什么都不敢动」那条性质的
    /// 具体形态（§9.3）。
    #[test]
    fn an_empty_ledger_prunes_nothing() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![skill_entry("foo", Action::Drift)],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        assert_eq!(
            prune_plan(&plan, &Ledger::default()).harnesses[0].entries[0].action,
            Action::Drift
        );
    }

    /// 改判会改变 rank：删除条目必须重排到它该在的位置，
    /// 否则 `render` 的「同组连续」假设会破。
    #[test]
    fn prune_re_sorts_entries_after_re_judging() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![
                    skill_entry("a", Action::Drift),
                    skill_entry("b", Action::Install),
                ],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
            notes: BTreeMap::new(),
        };
        let mut ledger = Ledger::default();
        ledger.record(ledger_entry("claude-code", Kind::Skill, "a"));

        let entries = &prune_plan(&plan, &ledger).harnesses[0].entries;
        assert_eq!(entries[0].id.name, "a", "remove(1) 排在 install(2) 之前");
        assert_eq!(entries[0].action, Action::Remove);
    }
}
