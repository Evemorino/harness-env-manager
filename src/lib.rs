//! henv：跨 harness 能力环境管理器。
//!
//! `diff` 只读：解析 `env.yaml` → 归一化能力 → 扫本机三家现状 → 纯函数 diff → 渲染。
//! 它全链路不写 `Home` 下的任何文件，也不联网：git 源只读 henv 自己的缓存
//! （`~/.cache/henv/git`），缓存里没有就如实报「这次判不了」（§9.4）。
//! `apply`（Plan B1）在此基础上多走一步落盘——把 skills 真拷贝进各家、并把「放了什么」
//! 记进账本；它同样从 `plan_for` 的同一份计划出发，且只经由 `apply::execute` 写盘。
//! 唯一会改盘的是 `apply`（写 harness 目录、写账本、刷新 git 缓存）。
//!
//! Plan A 的「不写 `Home`」不变量由 `tests/read_only_guard.rs` 静态守卫，Plan B1 把它
//! 收窄成**写入白名单**：只有 `src/apply/` 与 `src/store/` 下的代码允许出现写盘调用。

use adapters::HarnessAdapter;
use anyhow::Context as _;
use capability::McpTransport;
use home::Home;
use manifest::Manifest;
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

pub mod adapters;
pub mod apply;
pub mod browse;
pub mod capability;
pub mod decompose;
pub mod diff;
pub mod doctor;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod manifest_edit;
pub mod render;
pub mod scan;
pub mod secrets;
pub mod show;
pub mod skills_dir;
pub mod store;

// 只有测试引用这三个常量；不加 cfg 会让 `cargo build` 报 unused import。
#[cfg(test)]
use adapters::{CLAUDE_CODE, CODEX, KIMI_CODE};

// `tests/` 下的集成测试是独立 crate，看不到 `#[cfg(test)]` 模块；`testutil`
// feature（非默认）把它们需要的夹具（`TempHome`/`GitFixture`）放出来。
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

/// 交互闸门（§9.8）：`apply` 是 henv 唯一会问人一句话的地方。
/// 抽成 trait 是为了让 `run_apply` 能被测——否则测一次 apply 就要喂真 stdin。
pub trait Confirm {
    /// `prompt` 已含问句本身。返回 true 才继续。
    fn ask(&mut self, prompt: &str) -> bool;
}

/// `--yes`：不提问，恒同意（§9.8）。
pub struct AssumeYes;

impl Confirm for AssumeYes {
    fn ask(&mut self, _prompt: &str) -> bool {
        true
    }
}

/// 生产默认：问题写 stderr，读 stdin 一行，`y`/`Y` 开头算同意。
/// 默认是「不同意」——`--yes` 之外的任何输入都必须指向「什么都没发生」。
pub struct StdinConfirm;

impl Confirm for StdinConfirm {
    fn ask(&mut self, prompt: &str) -> bool {
        use std::io::BufRead as _;
        eprint!("{prompt} [y/N] ");
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        line.trim_start().starts_with(['y', 'Y'])
    }
}

/// 逐家读现状。**只在这里发生**：`diff`/`apply`/`doctor`/`scan` 共用同一次观察——
/// 两次 `read_state()` 之间盘面会变，共用一份才让各命令看到的「现状」是同一个东西。
///
/// `registry` 由调用方传进来：它是有序表，顺序决定 `pairs` 的配对，不要在函数里重建
/// 第二份。未安装的家不 `read_state`（对不存在的目录做无谓 IO），现状记空。
///
/// `only` 是 §9.8 的 `--harness <name>`：**只过滤目标**。源扫描不走这里。
pub(crate) fn read_harness_states(
    registry: &[Box<dyn HarnessAdapter>],
    manifest: &Manifest,
    only: Option<&str>,
) -> anyhow::Result<Vec<diff::HarnessState>> {
    let mut states = Vec::new();
    for a in registry {
        if !is_enabled(manifest, a.name()) {
            continue;
        }
        if let Some(only) = only {
            if a.name() != only {
                continue;
            }
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
        states.push(diff::HarnessState {
            name: a.name().to_string(),
            detected,
            actual,
        });
    }
    Ok(states)
}

/// 一次规划的全部产物。`diff` 与 `apply` 共用**同一个**规划函数——
/// 两个规划器能互相不一致，正是这个项目存在要消灭的「假同步」。
pub(crate) struct Planned {
    pub plan: diff::Plan,
    /// 清单 lower 后的能力表。MCP 的交付要拿它的三段载荷——`plan` 里只有 id 与动作。
    /// `diff` 不看它（它只读），但两个入口共用 `plan_for`，所以它跟着一起返回。
    desired: Vec<capability::Capability>,
    /// 解析后的清单本体。`doctor` 要在规划之外看一眼 `${VAR}` 引用（§10.1 的 `(env)` 段），
    /// 但**不许**再读一次盘：这一份就是本次规划用的那一份，两次读之间清单可能被改。
    pub manifest: Manifest,
    content: store::skills::ContentIndex,
    ledger: store::provenance::Ledger,
}

pub(crate) fn plan_for(
    manifest_path: &Path,
    home: &Home,
    mode: store::skills::SourceMode,
    only: Option<&str>,
) -> anyhow::Result<Planned> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("cannot read manifest: {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", manifest_path.display()))?;

    let mut desired = lower::lower(&manifest);
    let degrade = lower::DegradeTable::new(&manifest);
    let sources = lower::SourceHarnessTable::new(&manifest);
    // §10.3：记录型 plugin 的逐家版本。与上面两张表同源、同一次清单解析。
    let snapshots = lower::SnapshotTable::new(&manifest);

    let registry = adapters::registry(home);
    // §9.8 的 `--harness <name>`：**只过滤目标 pairs**。源扫描在下面，
    // 走的是 `scan_source_harnesses(&sources, &registry)`，不接收 `only`——
    // `source_harness:` 那家哪怕被排除也照样要被读，否则它的 bundle
    // 会凭空变成「定位不到」，用户得到一份假的 undeliverable。
    let states = read_harness_states(&registry, &manifest, only)?;
    // state 与它的 adapter 必须成对交给 compute，故先收进同一个 Vec 再借用。
    let pairs: Vec<(diff::HarnessState, &dyn HarnessAdapter)> = states
        .into_iter()
        .map(|state| {
            let adapter = adapters::lookup(&registry, &state.name);
            (
                state,
                adapter.expect("read_harness_states only yields registered names"),
            )
        })
        .collect();

    // 源 harness 按需扫（§9.5）：**不进 `pairs`**——`source_harness:` 只请求读它，
    // 不请求往它写，也不请求它多出一节 diff。它在 `harnesses:` 里很可能是关掉的，
    // 那正是「我只想拿它的内容」的写法。
    //
    // 已经为启用的家读过的那份现状直接交出去：同一个家既是目标又是源时不该再读第二遍。
    // 两次 `read_state()` 之间盘面会变，共用一份才让「diff 里读到的」与「拆解取到的」
    // 真正是同一次观察（`decompose` 那句「不许有第二条读路径」原本只做到了同函数，
    // 没做到同一次）。
    let declared = sources.declared();
    let already: std::collections::BTreeMap<String, decompose::SourceState> = pairs
        .iter()
        .filter(|(s, _)| declared.contains(s.name.as_str()))
        .map(|(s, _)| {
            let state = if s.detected {
                decompose::SourceState::Scanned(s.actual.clone())
            } else {
                decompose::SourceState::NotInstalled
            };
            (s.name.clone(), state)
        })
        .collect();
    let known: Vec<&str> = pairs.iter().map(|(s, _)| s.name.as_str()).collect();
    let source_states = decompose::scan_source_harnesses(&sources, &registry, &already)
        .context("cannot scan source harness state")?;

    // §9.5 的拆解：把 bundle 里能拆的能力变成独立能力，追加进清单侧。
    // 必须早于 `resolve_sources`——拆出来的 skill 也要解析内容（它们的源是 bundle）。
    let decomposed = decompose::decompose_bundles(&manifest, &source_states, &known, home)?;
    desired.extend(decomposed.capabilities.iter().cloned());
    desired.sort_by_key(|c| c.id.label());

    // 目标侧内容扫描：逐家算，按 harness 名收进一张表。
    let mut targets: std::collections::BTreeMap<String, _> = Default::default();
    for (state, _) in &pairs {
        targets.insert(
            state.name.clone(),
            store::skills::target_content(&state.actual)?,
        );
    }

    let inputs: Vec<diff::HarnessInput> = pairs
        .iter()
        .map(|(state, adapter)| diff::HarnessInput {
            state,
            adapter: *adapter,
        })
        .collect();

    // §9.2 第 1 步：来源解析。唯一在 compute 之前的 I/O。
    // `mode` 在这里分岔成全链路唯一的两种行为：`Cached` 不联网（`diff`），
    // `Fetch` 会去取（`apply`）。模式必须一路传到这一层——分类「延期还是失败」
    // 是它的责任（§9.4），而这一层是唯一知道「源到底取到没有」的地方。
    let mut content = store::skills::resolve_sources(&desired, home, mode, &decomposed.locations)?;
    content.targets = targets;
    content.notes = decomposed.notes.clone();
    // 定位失败的 plugin 条目：理由由内容层带进来（`compute` 按 `CapId` 取回，
    // 与普通 skill 源失败走的是同一个 `unresolved` 表）。
    for (plugin, reason) in &decomposed.failures {
        content.unresolved.insert(
            capability::CapId::new(capability::Kind::Plugin, plugin.clone()),
            store::skills::Unresolved::Failed(reason.clone()),
        );
    }

    let ledger = store::provenance::Ledger::load(home)?;
    let plan = diff::compute(
        &desired, &inputs, &degrade, &content, &ledger, &sources, &snapshots,
    );
    Ok(Planned {
        plan,
        desired,
        manifest,
        content,
        ledger,
    })
}

/// 只读比对：清单为期望，本机为现状，输出「切 harness 时什么会失效」。
///
/// 恒以 `Ok(())` 结束（有 drift 也是 `Ok`）——`diff` 是报告不是断言，
/// 门禁语义留给 Plan C 的 `doctor`。
///
/// 注：`degrade:` 从 B3b 起真的参与判决——它决定「目标 harness 吃不下、
/// 但内容已经拆出来了」的那些能力是进报告还是静默丢弃（§9.5）。
pub fn run_diff(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<()> {
    let planned = plan_for(manifest_path, home, store::skills::SourceMode::Cached, None)?;
    write!(out, "{}", render::render(&planned.plan))?;
    Ok(())
}

/// 扫三家的现状，**不看清单**。`status` 与 `browse` 共用：它们回答的都是「本机事实」，
/// 与某份期望无关，所以 `read_harness_states`（那个要 manifest 判 `enabled`）在这里不适用。
pub(crate) fn read_states(
    home: &Home,
    only: Option<&str>,
) -> anyhow::Result<Vec<diff::HarnessState>> {
    let registry = adapters::registry(home);
    let mut states = Vec::new();
    for adapter in &registry {
        let name = adapter.name();
        if only.is_some_and(|want| want != name) {
            continue;
        }
        let detected = adapter.detect();
        // 未安装的不去 `read_state`：对一个不存在的目录做 IO 没有意义，也让 `detected=false`
        // 这个事实更干净。
        let actual = if detected {
            adapter
                .read_state()
                .with_context(|| format!("cannot scan {name} state"))?
        } else {
            Vec::new()
        };
        states.push(diff::HarnessState {
            name: name.to_string(),
            detected,
            actual,
        });
    }
    Ok(states)
}

/// `henv status`：列出本机每一家**当前装着的**能力（§9.8）。
///
/// 与 `diff` 的分工：`diff` 回答「清单要的和本机差在哪」，`status` 回答「本机现在有什么」。
/// 所以它**不读清单**——没有清单也能用，被 `enabled: false` 排除的家照样列出来。排查
/// 「codex 怎么会有这个 skill」这类问题时，人要的是现状，而不是现状与某份期望的差。
///
/// 只读：不写任何文件，连缓存都不碰（`read_state` 全程只读）。
pub fn run_status(
    home: &Home,
    out: &mut dyn Write,
    only: Option<&str>,
    kind: Option<capability::Kind>,
) -> anyhow::Result<()> {
    // 账本只用来标 `[henv]`：这条是 henv 交付的，还是本机原生的/手放的。
    let ledger = store::provenance::Ledger::load(home)?;
    for state in read_states(home, only)? {
        let name = state.name.as_str();
        if !state.detected {
            let _ = writeln!(out, "{name}  (not installed)");
            continue;
        }
        let caps = &state.actual;
        let _ = writeln!(out, "{name}");
        for k in capability::Kind::all() {
            if kind.is_some_and(|want| want != k) {
                continue;
            }
            let rows: Vec<&capability::Capability> =
                caps.iter().filter(|c| c.id.kind == k).collect();
            if rows.is_empty() {
                continue;
            }
            let _ = writeln!(out, "  {} ({})", plural(k), rows.len());
            // 名字列对齐到最长的那条——与 `skills list` 同一个排版习惯。
            let width = rows
                .iter()
                .map(|c| c.id.name.chars().count())
                .max()
                .unwrap_or(0);
            for c in rows {
                let managed = if ledger.find(name, c.id.kind, &c.id.name).is_some() {
                    "  [henv]"
                } else {
                    ""
                };
                let _ = writeln!(
                    out,
                    "    {:<width$}  {}{managed}",
                    c.id.name,
                    source_summary(&c.spec, home.root()),
                );
            }
        }
    }
    Ok(())
}

/// 分组标题的复数拼写。`Kind::as_str` 是单数（`skill`），这里只加 `s`——
/// 六个词恰好都是规则复数，不值得为它建一张表。
fn plural(kind: capability::Kind) -> String {
    format!("{}s", kind.as_str())
}

/// 一条能力的来源摘要——**这是 `status` 存在的理由之一**：`diff` 只在你和清单不一致时
/// 才说话，而「这条到底哪来的」是排查时最先要问的问题。
fn source_summary(spec: &capability::Spec, home: &std::path::Path) -> String {
    use capability::{SkillSource, Spec};
    match spec {
        Spec::Skill { source, .. } => match source {
            SkillSource::AgentsDir => "agents-dir".to_string(),
            SkillSource::Local { path } => format!("local  {}", tidy(path, home)),
            SkillSource::Git { repo, git_ref, .. } => format!("git    {repo}@{git_ref}"),
            SkillSource::PluginBundle { plugin } => format!("plugin {plugin}"),
            SkillSource::Unknown => "unknown".to_string(),
        },
        // MCP 的摘要只渲染键名与命令/URL，绝不渲染值——那是 `mcp_summary` 的既有契约
        // （actual 侧的值可能是明文密钥）。
        Spec::Mcp { transport } => capability::mcp_summary(transport),
        Spec::Plugin {
            marketplace,
            version,
            ..
        } => match (marketplace.as_deref(), version.as_deref()) {
            (Some(m), Some(v)) => format!("{m} {v}"),
            (Some(m), None) => m.to_string(),
            (None, Some(v)) => v.to_string(),
            (None, None) => "—".to_string(),
        },
        Spec::Command { path } | Spec::Agent { path } => tidy(path, home),
        Spec::Hook { event, command } => format!("{event}  {command}"),
    }
}

/// 路径里的家目录前缀写成 `~`：`status` 是给人看的，一行里出现四遍 `/Users/nava` 只会挤压
/// 真正有信息的那部分。
fn tidy(path: &std::path::Path, home: &std::path::Path) -> String {
    let raw = path.display().to_string();
    let prefix = home.display().to_string();
    match raw.strip_prefix(&prefix) {
        Some(rest) if !rest.is_empty() => format!("~{rest}"),
        _ => raw,
    }
}

/// 交付：把清单要的 skill 真拷贝到各家，并把「放了什么」记进账本（§9.2/§9.3）。
///
/// 顺序是刻意的：先算计划 → 冲突检查 → 渲染 → 问人 → 才落盘。
/// 冲突在问人之前就返回错误——让人对着一个不可能执行的计划回答 y 没有意义。
///
/// `apply` 是 henv 唯一的写路径；它不做计划以外的事（§9.1 的范围）。
///
/// `prune` 只撤销**账本里记着**的条目——henv 自己交付过的 skill 拷贝与 MCP 键；
/// 用户手放的 drift 一律只报不删（§9.8）。`only` 把一次运行限定在一家，
/// 且**只收窄目标**：源扫描不受它影响。
pub fn run_apply(
    manifest_path: &Path,
    home: &Home,
    out: &mut dyn Write,
    confirm: &mut dyn Confirm,
    prune: bool,
    only: Option<&str>,
) -> anyhow::Result<()> {
    let mut planned = plan_for(manifest_path, home, store::skills::SourceMode::Fetch, only)?;

    // §9.8：`--prune` 只在这里改计划——`diff::compute` 与 `diff` 都不产删除动作，
    // 于是「删什么」是一件可测的纯函数，而 `diff` 的输出逐字节不变。
    // 改判必须在渲染之前：用户要看到的就是「这次会删什么」。
    if prune {
        planned.plan = diff::prune_plan(&planned.plan, &planned.ledger);
    }
    write!(out, "{}", render::render(&planned.plan))?;

    let conflicts = planned.plan.conflicts();
    anyhow::ensure!(
        conflicts.is_empty(),
        "refusing to apply, {} name conflict(s) found:\n{}",
        conflicts.len(),
        conflicts
            .iter()
            .map(|e| {
                format!(
                    "  {} ({})",
                    e.id.label(),
                    match &e.action {
                        diff::Action::Conflict { reason } => reason.as_str(),
                        _ => "",
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    // 「无事可做」的判定**不在这里**：`execute` 遍历计划，对没有 install/update
    // 条目的 harness 返回空报告，`delivered_total() == 0` 时既不写账本也不打印。
    // 这里再判一次就多出一份会与它漂移的判定。
    //
    // 只在已安装的家里问：一个都没装的话，问「要不要向 (空) 交付」没有意义。
    let pending: Vec<&str> = planned
        .plan
        .harnesses
        .iter()
        .filter(|h| h.detected)
        .map(|h| h.harness.as_str())
        .collect();
    if pending.is_empty() {
        let _ = writeln!(out, "没有已安装的 harness，无事可做。");
        return Ok(());
    }

    // 确认文案要说清这次**不只是交付**：删除不可逆（§9.7 没有回滚命令）。
    // 删除条数为 0 时文案与从前一字不差。
    let removals = planned.plan.counts().remove;
    let mut what = format!("将向 {} 交付", pending.join(", "));
    if removals > 0 {
        // 「最多」：这是**计划**里的删除条数，个别条目可能在执行时失败（§9.7 不进
        // 事务、失败进报告）。数字偏高比偏低好，但话得说明白。
        let _ = write!(what, "，最多删除 {removals} 项");
    }
    if !confirm.ask(&format!("{what}。继续？")) {
        let _ = writeln!(out, "cancelled");
        return Ok(());
    }

    let report = apply::execute(
        &planned.plan,
        &planned.desired,
        &planned.content,
        &mut planned.ledger,
        home,
    )?;
    for h in &report.harnesses {
        for id in &h.delivered {
            let _ = writeln!(out, "{:<13} delivered {}", h.harness, id);
        }
        for id in &h.removed {
            let _ = writeln!(out, "{:<13} removed {}", h.harness, id);
        }
    }
    // 账本只在真的动过之后写：一次空跑不该改动 henv 的状态文件。
    // 「删成功」也算动过——不落盘的话，下一次 diff 会把已删的条目再报一遍 drift，
    // 而账本里那份记录还在（§9.7 的修复路径依赖它）。
    if report.delivered_total() > 0 || report.removed_total() > 0 {
        planned.ledger.save(home)?;
    }
    let partial = report.partial();
    if !partial.is_empty() {
        let _ = writeln!(out, "partial:");
        for (harness, why) in partial {
            let _ = writeln!(out, "  {harness}: {why}");
        }
    }
    Ok(())
}

/// `add` 要加的东西：一条 skill，或者一条 MCP。
///
/// 两类共用同一个命令、同一套「改清单 → 交付」的流程，只有载荷不同——所以是 enum 而不是
/// 两个平行的请求类型（否则「改清单」「验证」「dry-run」那几段会各写两遍）。
pub enum AddKind<'a> {
    Skill {
        source: manifest::SkillSourceKind,
        repo: Option<&'a str>,
        path: Option<&'a str>,
        git_ref: Option<&'a str>,
    },
    Mcp {
        /// 已按 CLI 参数拼好的传输形态。stdio 与 http 的字段没有交集，所以在 CLI 层
        /// 就把它们收敛成一个值——这里拿到的已经是**恰好一种**。
        transport: &'a McpTransport,
    },
}

impl AddKind<'_> {
    fn section(&self) -> manifest_edit::Section {
        match self {
            AddKind::Skill { .. } => manifest_edit::Section::Skills,
            AddKind::Mcp { .. } => manifest_edit::Section::Mcps,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self {
            AddKind::Skill { .. } => "skill",
            AddKind::Mcp { .. } => "mcp",
        }
    }
}

/// `henv add` 的入参。
///
/// `targets` 为 `None` 时**不写** `targets:` 行——清单的默认值是 `all`，写与不写语义
/// 相同，但让文件保持用户自己的样子。
pub struct AddRequest<'a> {
    pub name: &'a str,
    pub kind: AddKind<'a>,
    pub targets: Option<Vec<String>>,
    pub manifest: &'a Path,
    pub dry_run: bool,
    pub no_apply: bool,
}

/// 往清单里插一条 skill，可选接着交付。
///
/// 顺序是刻意的：**先验证源真的取得到，再改清单**。验证失败就一个字节都不写——否则会留下
/// 一条指向不存在内容的条目，之后每次 `diff` 都报错，而用户不知道为什么。
///
/// 重名不静默覆盖：清单是事实源，`add` 只负责「这里还没有这条」。
pub fn run_add(
    req: &AddRequest<'_>,
    home: &Home,
    out: &mut dyn Write,
    confirm: &mut dyn Confirm,
    only: Option<&str>,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(req.manifest)
        .with_context(|| format!("cannot read manifest: {}", req.manifest.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", req.manifest.display()))?;
    let section = req.kind.section();
    let kind_name = req.kind.kind_name();
    // 重名只在**同一段**里判：`skills.foo` 与 `mcps.foo` 可以共存（它们是两种能力）。
    let taken = match section {
        manifest_edit::Section::Skills => manifest.skills.contains_key(req.name),
        manifest_edit::Section::Mcps => manifest.mcps.contains_key(req.name),
        manifest_edit::Section::Plugins => manifest.plugins.contains_key(req.name),
    };
    if taken {
        anyhow::bail!(
            "清单里已经有 {kind_name} `{}`：要重来先 `henv remove {}`",
            req.name,
            req.name
        );
    }

    let body = entry_body(req)?;
    verify_source(req, home)?;

    if req.dry_run {
        let _ = writeln!(out, "(dry-run) 会新增 {kind_name}/{}：", req.name);
        let _ = writeln!(out, "  {}:", req.name);
        for line in body.lines() {
            let _ = writeln!(out, "  {line}");
        }
        return Ok(());
    }

    let entry = manifest_edit::EntryRef {
        section,
        name: req.name,
        body: &body,
    };
    let new_text =
        manifest_edit::insert_entries(&text, &[entry])?.expect("非空条目列表不会返回 None");
    // 落盘前先自己解一遍：写出一份自己都读不回的清单，比不写坏得多。
    Manifest::parse(&new_text).context("refusing to write a manifest that does not parse back")?;
    apply::write::write_config(req.manifest, new_text.as_bytes())?;
    let _ = writeln!(
        out,
        "已写入 {}：新增 {kind_name}/{}",
        req.manifest.display(),
        req.name
    );

    if req.no_apply {
        let _ = writeln!(out, "（--no-apply：这次只改清单，交付留给 `henv apply`。）");
        return Ok(());
    }
    run_apply(req.manifest, home, out, confirm, false, only)
}

/// 条目字段行的 YAML 文本（2 空格相对缩进），与 `scan` 收养出来的条目同形。
fn entry_body(req: &AddRequest<'_>) -> anyhow::Result<String> {
    let mut body = match &req.kind {
        AddKind::Skill {
            source,
            repo,
            path,
            git_ref,
        } => skill_body(source, *repo, *path, *git_ref)?,
        AddKind::Mcp { transport } => mcp_body(transport),
    };
    // `targets` 是两类共有的：不写就是 `all`，写就落到同一行上。
    if let Some(targets) = &req.targets {
        let _ = writeln!(body, "  targets: [{}]", targets.join(", "));
    }
    Ok(body)
}

/// skill 条目的字段行。
fn skill_body(
    source: &manifest::SkillSourceKind,
    repo: Option<&str>,
    path: Option<&str>,
    git_ref: Option<&str>,
) -> anyhow::Result<String> {
    use manifest::SkillSourceKind as Source;
    let mut body = String::new();
    match source {
        Source::Git => {
            let repo = repo.context("source git requires `--repo`")?;
            let git_ref = git_ref.context("source git requires `--ref` (ref is the lock)")?;
            let _ = writeln!(body, "  source: git");
            let _ = writeln!(body, "  repo: {}", manifest_edit::scalar(repo));
            if let Some(path) = path {
                let _ = writeln!(body, "  path: {}", manifest_edit::scalar(path));
            }
            let _ = writeln!(body, "  ref: {}", manifest_edit::scalar(git_ref));
        }
        Source::Local => {
            let path = path.context("source local requires `--path`")?;
            let _ = writeln!(body, "  source: local");
            let _ = writeln!(body, "  path: {}", manifest_edit::scalar(path));
        }
        Source::AgentsDir => {
            let _ = writeln!(body, "  source: agents-dir");
        }
    }
    Ok(body)
}

/// MCP 条目的字段行。`env`/`headers` 的值在 CLI 层就要求写成 `${VAR}`（见 `parse_env`），
/// `scalar()` 会把它加上引号——那正好是合法的 YAML，解出来还是 `${VAR}`。
fn mcp_body(transport: &McpTransport) -> String {
    let mut body = String::new();
    match transport {
        McpTransport::Stdio { command, args, env } => {
            let _ = writeln!(body, "  command: {}", manifest_edit::scalar(command));
            if !args.is_empty() {
                let items: Vec<String> = args.iter().map(|a| manifest_edit::scalar(a)).collect();
                let _ = writeln!(body, "  args: [{}]", items.join(", "));
            }
            write_pairs(&mut body, "env", env);
        }
        McpTransport::Http { url, headers } => {
            let _ = writeln!(body, "  url: {}", manifest_edit::scalar(url));
            write_pairs(&mut body, "headers", headers);
        }
    }
    body
}

/// `env:` / `headers:` 那样的一小段嵌套映射。
fn write_pairs(
    body: &mut String,
    field: &str,
    pairs: &std::collections::BTreeMap<String, capability::McpValue>,
) {
    if pairs.is_empty() {
        return;
    }
    let _ = writeln!(body, "  {field}:");
    for (key, value) in pairs {
        let _ = writeln!(
            body,
            "    {}: {}",
            manifest_edit::scalar(key),
            manifest_edit::scalar(&value.to_text())
        );
    }
}

/// 源现在取得到吗？取不到就不该写进清单。
///
/// **只对 skill 生效**：MCP 没有「源目录」可验证——它的 `--command` 多半由 `npx`/`uvx`
/// 在运行时解析，而交付本来也不检查可执行性（claude 走它自己的 CLI，codex/kimi 只写配置）。
/// 假装校验一遍只会给假信心。
fn verify_source(req: &AddRequest<'_>, home: &Home) -> anyhow::Result<()> {
    use manifest::SkillSourceKind as Source;
    let AddKind::Skill {
        source,
        repo,
        path,
        git_ref,
    } = &req.kind
    else {
        return Ok(());
    };
    match source {
        Source::Git => {
            let repo = repo.context("source git requires `--repo`")?;
            let git_ref = git_ref.context("source git requires `--ref`")?;
            let got = store::git::resolve(
                repo,
                *path,
                git_ref,
                &home.henv_git_cache(),
                store::skills::SourceMode::Fetch,
            )?;
            anyhow::ensure!(
                got.is_some(),
                "取不到 {repo} 的 {git_ref}（ref 或 path 对不对？先跑 \
                 `henv show {repo}`）"
            );
        }
        Source::Local => {
            let raw = path.context("source local requires `--path`")?;
            let path = lower::expand_tilde_in(Some(home.root()), raw);
            anyhow::ensure!(path.is_dir(), "本地源不是目录：{}", path.display());
        }
        Source::AgentsDir => {
            let path = home.agents_skills().join(req.name);
            anyhow::ensure!(path.is_dir(), "共享库里没有这个 skill：{}", path.display());
        }
    }
    Ok(())
}

/// `henv remove` 的入参。
pub struct RemoveRequest<'a> {
    pub names: Vec<String>,
    pub manifest: &'a Path,
    pub dry_run: bool,
    /// `true` = 只改清单，不动本机副本。
    pub keep: bool,
}

/// 从清单删条目，可选接着撤销本机副本。
///
/// 撤销复用 `apply --prune` 的路径——它只删**账本里记着是 henv 放的**那些；用户手放的
/// 副本照旧只报不删（§9.8）。顺序不能反：prune 的判据是「账本里有、清单里不要」。
pub fn run_remove(
    req: &RemoveRequest<'_>,
    home: &Home,
    out: &mut dyn Write,
    confirm: &mut dyn Confirm,
    only: Option<&str>,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(req.manifest)
        .with_context(|| format!("cannot read manifest: {}", req.manifest.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", req.manifest.display()))?;
    // 名字落在哪一段：**三段都找**（skill / MCP / plugin 的条目都该删得掉）。
    //
    // plugin 按 **key** 匹配，不按它的身份（`entry`）：删条目删的是 YAML 的键，而
    // `{key: superpowers-bundle, entry: superpowers}` 这种写法里两者不同（§10.3）。
    // 缺一个就整次不动：半删的清单比不删更难解释。
    let mut found: Vec<(String, manifest_edit::Section)> = Vec::new();
    for name in &req.names {
        let section = if manifest.skills.contains_key(name) {
            manifest_edit::Section::Skills
        } else if manifest.mcps.contains_key(name) {
            manifest_edit::Section::Mcps
        } else if manifest.plugins.contains_key(name) {
            manifest_edit::Section::Plugins
        } else {
            anyhow::bail!("清单里没有 `{name}`");
        };
        found.push((name.clone(), section));
    }

    if req.dry_run {
        for (name, section) in &found {
            let _ = writeln!(out, "(dry-run) 会从清单删掉 {}/{name}", section.as_str());
        }
        if !req.keep {
            let _ = writeln!(out, "(dry-run) 并撤销本机由 henv 交付的副本");
        }
        return Ok(());
    }

    let mut new_text = text;
    for (name, section) in &found {
        let Some(next) = manifest_edit::remove_entry(&new_text, *section, name)? else {
            anyhow::bail!("解析出了 `{name}`，文本里却找不到它的条目——请检查清单排版");
        };
        new_text = next;
    }
    Manifest::parse(&new_text).context("refusing to write a manifest that does not parse back")?;
    apply::write::write_config(req.manifest, new_text.as_bytes())?;
    let _ = writeln!(
        out,
        "已写入 {}：删掉 {}",
        req.manifest.display(),
        found
            .iter()
            .map(|(n, section)| format!("{}/{n}", section.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if req.keep {
        let _ = writeln!(
            out,
            "（--keep：本机副本没动，要清就再跑 `henv apply --prune`。）"
        );
        return Ok(());
    }
    run_apply(req.manifest, home, out, confirm, true, only)
}

/// `henv upgrade` 的入参。
pub struct UpgradeRequest<'a> {
    /// 空 = 清单里所有 `source: git` 的条目。
    pub names: Vec<String>,
    pub manifest: &'a Path,
    /// 指定目标 ref；给了就不查上游。
    pub to: Option<&'a str>,
    pub dry_run: bool,
    pub no_apply: bool,
}

/// 把清单里 git 源的 `ref:` 移到上游最新的 version 形 tag，可选接着交付。
///
/// 只认 version 形 tag：一个都没有时报错并提示 `--to`，**不猜**默认分支——「ref 就是锁」
/// 的前提是在两个明确的点之间移动。每个条目独立判定（失败隔离）：一个仓库取不到不该
/// 拦住别的条目。
///
/// 先确认新 ref 的内容真的取得到，再改清单：留一条指向不存在 ref 的条目比升级失败更糟。
pub fn run_upgrade(
    req: &UpgradeRequest<'_>,
    home: &Home,
    out: &mut dyn Write,
    confirm: &mut dyn Confirm,
    only: Option<&str>,
) -> anyhow::Result<()> {
    use manifest::SkillSourceKind as Source;
    let text = std::fs::read_to_string(req.manifest)
        .with_context(|| format!("cannot read manifest: {}", req.manifest.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", req.manifest.display()))?;

    let selected: Vec<String> = if req.names.is_empty() {
        manifest
            .skills
            .iter()
            .filter(|(_, e)| e.source == Source::Git)
            .map(|(k, _)| k.clone())
            .collect()
    } else {
        for name in &req.names {
            let Some(entry) = manifest.skills.get(name) else {
                anyhow::bail!("清单里没有 skill `{name}`");
            };
            anyhow::ensure!(
                entry.source == Source::Git,
                "`{name}` 不是 git 源，没有可升级的 ref"
            );
        }
        req.names.clone()
    };
    if selected.is_empty() {
        let _ = writeln!(out, "清单里没有 git 源的 skill，无事可做。");
        return Ok(());
    }

    let mut new_text = text;
    let mut changed: Vec<(String, String, String)> = Vec::new();
    let mut problems: Vec<(String, String)> = Vec::new();
    for name in &selected {
        let entry = &manifest.skills[name];
        let repo = entry.repo.as_deref().unwrap_or_default();
        let path = entry.path.as_deref();
        let old = entry.git_ref.as_deref().unwrap_or_default();
        let target = match req.to {
            Some(to) => to.to_string(),
            None => match store::git::latest_version_tag(repo, &home.henv_git_cache()) {
                Ok(Some(tag)) => tag,
                Ok(None) => {
                    problems.push((
                        name.clone(),
                        format!("{repo} 没有 version 形 tag；用 `--to <ref>` 指定目标"),
                    ));
                    continue;
                }
                Err(e) => {
                    problems.push((name.clone(), format!("{e:#}")));
                    continue;
                }
            },
        };
        if target == old {
            continue; // 已经在这一版上：幂等，不写清单也不留备份。
        }
        match store::git::resolve(
            repo,
            path,
            &target,
            &home.henv_git_cache(),
            store::skills::SourceMode::Fetch,
        ) {
            Ok(Some(_)) => {}
            Ok(None) => {
                problems.push((name.clone(), format!("取不到 {repo} 的 {target}")));
                continue;
            }
            Err(e) => {
                problems.push((name.clone(), format!("{e:#}")));
                continue;
            }
        }
        match manifest_edit::set_entry_field(
            &new_text,
            manifest_edit::Section::Skills,
            name,
            "ref",
            &target,
        )? {
            Some(next) => {
                new_text = next;
                changed.push((name.clone(), old.to_string(), target));
            }
            None => problems.push((
                name.clone(),
                "文本里找不到这条条目的 `ref:` 字段".to_string(),
            )),
        }
    }

    for (name, from, to) in &changed {
        let _ = writeln!(out, "skill/{name}: {from} → {to}");
    }
    if changed.is_empty() {
        let _ = writeln!(out, "没有需要升级的条目。");
    }
    if req.dry_run {
        return Ok(());
    }
    if !changed.is_empty() {
        Manifest::parse(&new_text)
            .context("refusing to write a manifest that does not parse back")?;
        apply::write::write_config(req.manifest, new_text.as_bytes())?;
        let _ = writeln!(
            out,
            "已写入 {}：{} 条 ref 更新",
            req.manifest.display(),
            changed.len()
        );
    }
    if !problems.is_empty() {
        let _ = writeln!(out, "partial:");
        for (name, why) in &problems {
            let _ = writeln!(out, "  skill/{name}: {why}");
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    if req.no_apply {
        let _ = writeln!(out, "（--no-apply：这次只改清单，交付留给 `henv apply`。）");
        return Ok(());
    }
    run_apply(req.manifest, home, out, confirm, false, only)
}

/// `harnesses` 是 opt-out 列表：未列出 = 启用，只有显式 `enabled: false` 排除。
/// 清单里这家开着没有。没写就是开（spec §4 harnesses 注释）。
///
/// `pub(crate)`：`decompose` 也要用同一条判据（§9.5 的「交付目标被关掉」），
/// 复制一份 `unwrap_or(true)` 迟早在某一边走样。
pub(crate) fn is_enabled(m: &Manifest, name: &str) -> bool {
    m.harnesses.get(name).map(|h| h.enabled).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Kind;
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
    fn diff_plans_without_network_and_apply_plans_with_it() {
        // 模式穿线的端到端证据：同一份清单、同一份缓存状态，两个入口必须得到
        // 两种结果——否则 §9.4 的「diff 不联网」只是文档里的一句话。
        let t = TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/foo/SKILL.md", "# foo");
        repo.commit("init");
        t.write(
            "env.yaml",
            &format!(
                "version: 1\nskills:\n  foo:\n    source: git\n    repo: {}\n    ref: main\n    path: skills/foo\n    targets: [claude-code]\n",
                repo.url
            ),
        );
        let m = t.path("env.yaml");

        let cached = plan_for(&m, &t.home, store::skills::SourceMode::Cached, None).unwrap();
        assert!(
            cached
                .content
                .unresolved
                .contains_key(&capability::CapId::new(capability::Kind::Skill, "foo")),
            "diff 不联网：第一次跑时缓存是空的，源应当报「这次判不了」"
        );

        let fetched = plan_for(&m, &t.home, store::skills::SourceMode::Fetch, None).unwrap();
        assert!(
            fetched
                .content
                .sources
                .contains_key(&capability::CapId::new(capability::Kind::Skill, "foo")),
            "apply 会去取，所以同样一份清单在 Fetch 模式下应当解析出内容"
        );
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
            out.ends_with(
                "Plan: 0 conflict, 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"
            ),
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
            out.ends_with(
                "Plan: 0 conflict, 1 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"
            ),
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
        // 密钥门禁：本计划要求回声断言按**整串**相等，不用 `contains`——`contains` 挡不住
        // 「只回显了值的前缀」的实现（`secrets::validate_env` 的
        // `validate_env_message_is_pinned_and_never_carries_the_value` 同一条规矩）。
        // 路径由 tempdir 派生，所以用 `format!` 拼出期望值，本机与 CI 都对得上。
        assert_eq!(
            err,
            format!(
                "invalid manifest: {}: env.yaml invalid:\n  - mcps.c: env `K` must be a ${{VAR}} reference, optionally prefixed with `Bearer `; `${{VAR:-}}` with an empty default is allowed too (value not shown)",
                t.path("env.yaml").display()
            )
        );
        assert!(out.is_empty(), "出错时不输出半截报告");
    }

    use crate::testutil::{AssumeYes, RefuseAll};

    /// 在 TempHome 上跑一次 run_apply，返回渲染文本。
    fn apply(t: &TempHome, manifest: &str, confirm: &mut dyn Confirm) -> anyhow::Result<String> {
        t.write("env.yaml", manifest);
        let mut out = Vec::new();
        run_apply(&t.path("env.yaml"), &t.home, &mut out, confirm, false, None)?;
        Ok(String::from_utf8(out).expect("utf8"))
    }

    /// 同上，但能指定 `--prune` 与 `--harness`。
    fn apply_with(t: &TempHome, manifest: &str, prune: bool, only: Option<&str>) -> String {
        t.write("env.yaml", manifest);
        let mut out = Vec::new();
        run_apply(
            &t.path("env.yaml"),
            &t.home,
            &mut out,
            &mut AssumeYes,
            prune,
            only,
        )
        .expect("run_apply");
        String::from_utf8(out).expect("utf8")
    }

    /// 最省事的「一个 agents-dir skill」清单。
    fn one_skill_manifest() -> &'static str {
        "version: 1\nskills:\n  foo:\n    source: agents-dir\n"
    }

    /// 「一个 local 源的 skill」清单：内容放在 `src/foo`，**不落在任何 harness 的扫描面里**，
    /// 所以三家都得真交付。
    ///
    /// 交付/撤销机制的那些测试用它而不是 `one_skill_manifest`：agents-dir 的语义现在是
    /// 「共享库——读得到的不拷」，codex 与 kimi 都会判 in-sync，于是「拷了没有」这类断言
    /// 不再说明机制是否正常。
    fn local_skill_manifest(t: &TempHome) -> String {
        format!(
            "version: 1\nskills:\n  foo:\n    source: local\n    path: {}\n",
            t.path("src/foo").display()
        )
    }

    #[test]
    fn apply_delivers_real_copies_and_records_them() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");
        let manifest = local_skill_manifest(&t);

        let out = apply(&t, &manifest, &mut AssumeYes).expect("apply");
        assert!(
            out.contains("~ update") || out.contains("+ install"),
            "{out}"
        );

        for dir in [".claude/skills/foo", ".codex/skills/foo"] {
            let p = t.path(dir);
            let meta = std::fs::symlink_metadata(&p).expect("delivered");
            assert!(
                meta.file_type().is_dir() && !meta.file_type().is_symlink(),
                "{dir} 必须是真拷贝"
            );
            assert_eq!(
                std::fs::read_to_string(p.join("SKILL.md")).unwrap(),
                "# foo"
            );
        }

        // §9.3：账本落在任何内容目录之外。
        let ledger = crate::store::provenance::Ledger::load(&t.home).unwrap();
        assert!(ledger.find(CLAUDE_CODE, Kind::Skill, "foo").is_some());
        assert!(ledger.find(CODEX, Kind::Skill, "foo").is_some());
        assert_eq!(
            ledger
                .find(CLAUDE_CODE, Kind::Skill, "foo")
                .unwrap()
                .source_id,
            format!("local:{}", t.path("src/foo").display())
        );
        assert!(t.path(".local/state/henv/provenance.json").exists());
    }

    #[test]
    fn a_second_apply_changes_nothing() {
        // §9.10 的幂等验收：重跑是空操作，且报告说得出这句话。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");
        apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();

        let before = std::fs::read_to_string(t.path(".local/state/henv/provenance.json")).unwrap();
        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        let after = std::fs::read_to_string(t.path(".local/state/henv/provenance.json")).unwrap();

        assert!(out.contains("✓ in-sync"), "{out}");
        assert!(
            out.ends_with("Plan: 0 conflict, 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"),
            "{out}"
        );
        assert_eq!(before, after, "没有交付就不该改账本");
    }

    #[test]
    fn a_form_different_but_content_equal_target_is_left_alone() {
        // §9.3 + §9.10：本机那 14 条软链的形态。内容相同 → in-sync，
        // 而且**形态不许被迁移**——apply 不能把它"修"成拷贝。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../.agents/skills/foo");
        t.mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(
            !out.contains("~ update") && !out.contains("! conflict"),
            "内容相同就不该有任何动作：{out}"
        );
        let link = t.path(".claude/skills/foo");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "内容相同就不该动它——形态无关的判定单位正是为了这个"
        );
        // codex 那边也不交付：它原生读 `~/.agents/skills`，这条内容就在它眼前
        // （2026-09-28 的修正）。只扫 `~/.codex/skills` 时这里会多拷一份。
        assert!(
            !t.path(".codex/skills/foo").exists(),
            "共享库来源的能力对 codex 是 in-sync，不该重拷"
        );
    }

    #[test]
    fn a_dangling_link_is_healed_into_a_real_copy() {
        // §2.2 第 1 条 + §9.3 的自愈：断链没有内容，apply 把它换成真拷贝。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../.agents/skills/gone");
        t.mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(out.contains("(断链 →"), "{out}");
        let target = t.path(".claude/skills/foo");
        let meta = std::fs::symlink_metadata(&target).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "# foo"
        );
    }

    #[test]
    fn a_foreign_copy_with_different_content_stops_the_run() {
        // §9.3 不变式二：不是 henv 放的东西，绝不覆盖——而且整次 apply 一个字节都不写。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# mine")
            .write(".claude/skills/foo/SKILL.md", "# theirs")
            .mkdir(".codex");

        let err = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap_err();
        assert!(format!("{err:#}").contains("skill/foo"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
            "# theirs",
            "别人的东西一个字节都不许动"
        );
        assert!(
            !t.path(".codex/skills/foo").exists(),
            "冲突阻断全部交付，包括别家"
        );
        assert!(!t.path(".local/state/henv/provenance.json").exists());
    }

    #[test]
    fn refusing_the_confirmation_writes_nothing() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut RefuseAll).unwrap();
        assert!(out.contains("cancelled"), "{out}");
        assert!(!t.path(".claude/skills/foo").exists());
        assert!(!t.path(".codex/skills/foo").exists());
        assert!(!t.path(".local/state/henv/provenance.json").exists());
    }

    #[test]
    fn a_hand_modified_copy_is_overwritten_and_reported() {
        // §9.3：账本记着它是我们放的，盘上哈希与账本不符 → 覆盖，但要说出来。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# v2");
        t.mkdir(".claude").mkdir(".codex");
        apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();

        t.write(".claude/skills/foo/SKILL.md", "# hand edited");
        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(out.contains("[副本被手改过]"), "{out}");
        assert_eq!(
            std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
            "# v2",
            "被手改的副本要恢复成清单要的内容"
        );
    }

    #[test]
    fn run_diff_still_reports_the_unknown_for_an_unresolvable_source() {
        // 接线之后 diff 的既有行为不变：local 源指向不存在的目录 → 落回 Plan A。
        //
        // 目标侧那个 `foo` 是这条断言的前提，不是装饰：源解析不出来 → content 层
        // 给不出判定 → compute 落回 compare_versions；而 `compare_versions` 只在
        // 「目标侧已有同名条目」时才被问到。目标侧空着的话走的是
        // `state.actual.iter().find(...) == None` 那一支，只能给 `+ install`
        // ——「已知目标有、但判不了新旧」才是 `? unknown` 想说的那件事。
        // 夹具与断言不符时改夹具，不改断言：断言原文来自 brief。
        let t = TempHome::new();
        t.write(".claude/skills/foo/SKILL.md", "# foo");
        let out = diff(
            &t,
            "version: 1\nskills:\n  foo:\n    source: local\n    path: /tmp/definitely-not-here\n",
        );
        assert!(out.contains("? unknown  skill/foo"), "{out}");
    }

    #[test]
    fn a_manifest_name_that_escapes_the_skills_dir_is_refused_before_any_write() {
        // 清单里的名字会直接拼进 `target_dir.join(name)`：`../../evil` 于是指向
        // `<home>/evil`，`/etc/foo` 直接**替换**基路径。这条测试钉的是**用户可见的那一半**
        // ——不能只在清单层拒绝（manifest::tests 里那三条），还得证明 `apply` 真的
        // 连一个字节都没写。错误消息里的 `single path component` 同时是自检：
        // 若 apply 是为别的理由失败的，这条断言会拆穿它。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");

        let err = apply(
            &t,
            "version: 1\nskills:\n  \"../../evil\":\n    source: agents-dir\n",
            &mut AssumeYes,
        )
        .expect_err("逃逸的名字必须在落盘前被拒");
        // `{:#}` 取整条 cause 链（与 invalid_manifest_… 那条同一规矩）：
        // `to_string()` 只有最外层那句 `invalid manifest: <path>`，细节在 cause 里。
        let err = format!("{err:#}");
        assert!(err.contains("single path component"), "{err}");
        assert!(!t.path(".claude/skills").exists(), "不该建出 skills 目录");
        assert!(!t.path("evil").exists(), "绝不写到 skills 目录之外");
    }

    #[test]
    fn kimi_reads_a_shared_skill_in_sync_and_never_copies_it() {
        // 2026-09-28 修正：kimi 原生读 `~/.agents/skills`（它的 `USER_GENERIC_DIRS`），
        // 所以共享库来源的 skill 在它这儿就是 in-sync——不是「交付不了」，也不该往
        // `~/.kimi-code/skills` 拷一份。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".kimi-code").mkdir(".claude").mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        // 只看行首那行；Plan 汇总里永远有 `0 undeliverable` 这个字样。
        assert!(!out.contains("! undeliverable"), "{out}");
        assert!(!t.path(".kimi-code/skills").exists(), "共享库的东西不用搬");
    }

    #[test]
    fn kimi_reports_a_local_only_skill_as_undeliverable() {
        // kimi 没有 henv 写得了的用户级 skills 目录：非共享库来源的能力确实交付不了，
        // 报告要说清楚，且 apply 不许因此失败（§9.1）。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".kimi-code").mkdir(".claude").mkdir(".codex");
        let manifest = local_skill_manifest(&t);

        let out = apply(&t, &manifest, &mut AssumeYes).unwrap();
        assert!(out.contains("! undeliverable  skill/foo"), "{out}");
        assert!(out.contains("kimi-code 无用户级 skills 目录"), "{out}");
        assert!(!t.path(".kimi-code/skills").exists());
    }

    /// Review Focus 4：`--harness` 只收窄**目标**。没被点名的家一个字节都不写。
    /// （「源扫描不跟着关掉」那条在 `tests/bundle_end_to_end.rs` 里以公开入口的形态钉。）
    #[test]
    fn harness_narrows_the_targets_only() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");
        let manifest = local_skill_manifest(&t);

        let out = apply_with(&t, &manifest, false, Some("codex"));

        assert!(t.path(".codex/skills/foo").exists(), "{out}");
        assert!(
            !t.path(".claude/skills/foo").exists(),
            "没被点名的家一个字节都不该写：\n{out}"
        );
    }

    /// Review Focus 2：没给 `--prune` 时，清单里已经不要的条目**一个都不删**，
    /// 账本也不动——这是不变式，不是默认值。
    #[test]
    fn apply_without_prune_never_deletes_a_drifted_copy() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        let manifest = local_skill_manifest(&t);
        apply_with(&t, &manifest, false, Some("codex"));
        assert!(t.path(".codex/skills/foo").exists(), "先交付到盘上");

        // 清单里去掉 foo，再跑一次**不带** `--prune` 的 apply。
        let out = apply_with(&t, "version: 1\n", false, Some("codex"));

        assert!(
            t.path(".codex/skills/foo").exists(),
            "没给 --prune 就不许删：\n{out}"
        );
        assert!(!out.contains("removed"), "{out}");
        let ledger = crate::store::provenance::Ledger::load(&t.home).unwrap();
        assert!(
            ledger.find(CODEX, Kind::Skill, "foo").is_some(),
            "账本也不许动"
        );
    }

    /// 正路：`--prune` 删掉账本记着的那一条，非账本的 drift 逐字节不动。
    /// 端到端那条在 `tests/prune_end_to_end.rs` 里再走一遍公开入口。
    #[test]
    fn prune_deletes_only_what_the_ledger_records() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        t.write(".codex/skills/handmade/SKILL.md", "# handmade");
        let manifest = local_skill_manifest(&t);
        apply_with(&t, &manifest, false, Some("codex"));
        let handmade = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();

        let out = apply_with(&t, "version: 1\n", true, Some("codex"));

        assert!(out.contains("removed skill/foo"), "{out}");
        assert!(!t.path(".codex/skills/foo").exists(), "账本记着的要删掉");
        assert!(
            t.path(".codex/skills/handmade").exists(),
            "非账本的 drift 不许动"
        );
        assert_eq!(
            std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
            handmade,
            "逐字节不变"
        );
        let ledger = crate::store::provenance::Ledger::load(&t.home).unwrap();
        assert!(
            ledger.find(CODEX, Kind::Skill, "foo").is_none(),
            "删成功要把账本落盘"
        );
    }

    /// 确认文案必须说清这次会删东西——删除不可逆（§9.7 没有回滚命令）。
    /// 这里顺手把「拒绝之后什么都不发生」也钉住。
    #[test]
    fn the_prompt_says_how_many_items_will_be_deleted() {
        struct Recorder(Vec<String>);
        impl Confirm for Recorder {
            fn ask(&mut self, prompt: &str) -> bool {
                self.0.push(prompt.to_string());
                false
            }
        }

        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        let manifest = local_skill_manifest(&t);
        apply_with(&t, &manifest, false, Some("codex"));

        t.write("env.yaml", "version: 1\n");
        let mut out = Vec::new();
        let mut rec = Recorder(Vec::new());
        run_apply(
            &t.path("env.yaml"),
            &t.home,
            &mut out,
            &mut rec,
            true,
            Some("codex"),
        )
        .unwrap();

        assert_eq!(rec.0, vec!["将向 codex 交付，最多删除 1 项。继续？"]);
        assert!(
            t.path(".codex/skills/foo").exists(),
            "拒绝之后什么都不该发生"
        );
    }

    /// `status` 回答的是「本机现在有什么」——与清单无关，被禁用的家照列，
    /// 而且每条要能看出「是不是 henv 放的」。
    #[test]
    fn status_lists_every_harness_and_marks_what_henv_delivered() {
        let t = TempHome::new();
        t.mkdir(".claude").mkdir(".codex");
        t.write(".claude/skills/foo/SKILL.md", "# foo");
        t.write(".codex/skills/bar/SKILL.md", "# bar");
        // 账本里只记 claude 的 foo：它就是「henv 交付的」那一条。
        let mut ledger = crate::store::provenance::Ledger::default();
        ledger.record(crate::store::provenance::LedgerEntry {
            harness: CLAUDE_CODE.to_string(),
            kind: Kind::Skill,
            name: "foo".to_string(),
            path: t.path(".claude/skills/foo"),
            source_id: "agents-dir".to_string(),
            content_hash: "deadbeef".to_string(),
            delivered_at_unix: 1,
        });
        ledger.save(&t.home).unwrap();

        let mut out = Vec::new();
        run_status(&t.home, &mut out, None, None).unwrap();
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("claude-code"), "{s}");
        assert!(s.contains("  skills (1)"), "按 kind 分组要带计数：{s}");
        let foo = s.lines().find(|l| l.contains("foo")).expect("foo 要列出来");
        assert!(foo.contains("[henv]"), "账本里的那条要标出来：{foo}");
        let bar = s.lines().find(|l| l.contains("bar")).expect("bar 要列出来");
        assert!(!bar.contains("[henv]"), "不在账本里的不该标：{bar}");
        assert!(s.contains("kimi-code  (not installed)"), "{s}");
    }

    #[test]
    fn status_can_be_narrowed_by_harness_and_kind() {
        let t = TempHome::new();
        t.mkdir(".claude").mkdir(".codex");
        t.write(".claude/skills/foo/SKILL.md", "# foo");
        t.write(".codex/skills/bar/SKILL.md", "# bar");

        let mut out = Vec::new();
        run_status(&t.home, &mut out, Some(CODEX), Some(Kind::Skill)).unwrap();
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("codex"), "{s}");
        assert!(!s.contains("claude-code"), "点名一家就只看那一家：{s}");
        assert!(s.contains("  skills ("), "{s}");
        assert!(!s.contains("  plugins ("), "点名 kind 就只看那一类：{s}");
        assert!(!s.contains("not installed"), "点名了就只列那一家：{s}");
    }
}
