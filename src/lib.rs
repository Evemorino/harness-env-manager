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
use home::Home;
use manifest::Manifest;
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

pub mod adapters;
pub mod apply;
pub mod capability;
pub mod decompose;
pub mod diff;
pub mod doctor;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod render;
pub mod secrets;
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

    let registry = adapters::registry(home);
    // state 与它的 adapter 必须成对交给 compute，故先收进同一个 Vec 再借用。
    let mut pairs: Vec<(diff::HarnessState, &dyn HarnessAdapter)> = Vec::new();
    for a in &registry {
        if !is_enabled(&manifest, a.name()) {
            continue;
        }
        // §9.8 的 `--harness <name>`：**只过滤目标 pairs**。源扫描在下面，
        // 走的是 `scan_source_harnesses(&sources, &registry)`，不接收 `only`——
        // `source_harness:` 那家哪怕被排除也照样要被读，否则它的 bundle
        // 会凭空变成「定位不到」，用户得到一份假的 undeliverable。
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
        pairs.push((
            diff::HarnessState {
                name: a.name().to_string(),
                detected,
                actual,
            },
            &**a,
        ));
    }

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
    let plan = diff::compute(&desired, &inputs, &degrade, &content, &ledger, &sources);
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

/// `henv skills list`：给一个 git 地址，列出里面所有含 `SKILL.md` 的目录（§9.8）。
///
/// 只读入口：不读清单、不碰 harness 目录、不写账本。唯一的写入是 henv 自己
/// 缓存下的一个临时子目录（`store::git::list_skills` 自己负责删）。
/// 名字对齐到最长的那个：好看，且便于 `awk '{print $2}'` 直接取路径。
pub fn run_skills_list(
    repo: &str,
    git_ref: Option<&str>,
    sub: Option<&str>,
    home: &Home,
) -> anyhow::Result<String> {
    let found = store::git::list_skills(repo, sub, git_ref, &home.henv_git_cache())?;
    if found.is_empty() {
        // 空结果与「命令失败」是两回事：看过了，只是里面没有 skill。
        return Ok(format!("没有找到含 SKILL.md 的目录（看了 {repo}）\n"));
    }
    let width = found
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (name, rel) in found {
        let _ = writeln!(
            out,
            "{name}{}  {rel}",
            " ".repeat(width - name.chars().count())
        );
    }
    Ok(out)
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

    #[test]
    fn skills_list_prints_a_name_padded_to_the_longest_one() {
        let t = TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/foo/SKILL.md", "# foo");
        repo.write("skills/longer-name/SKILL.md", "# longer");
        repo.commit("init");

        let out = run_skills_list(&repo.url, None, None, &t.home).unwrap();
        assert_eq!(
            out,
            "foo          skills/foo\nlonger-name  skills/longer-name\n"
        );
    }

    #[test]
    fn skills_list_writes_nothing_outside_henv_cache() {
        // §9.8 说 `skills list` 只读。这里把「只读」钉在它最容易破的地方：
        // 扫完一个仓库就顺手建了某家的目录（或账本），用户下次 diff 会看到
        // 一个他没做过的改动。
        let t = TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/foo/SKILL.md", "# foo");
        repo.commit("init");

        run_skills_list(&repo.url, None, None, &t.home).unwrap();
        for rel in [".claude", ".codex", ".kimi", ".agents", ".local"] {
            assert!(!t.path(rel).exists(), "{rel} 不该被创建");
        }
    }

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
                "invalid manifest: {}: env.yaml invalid:\n  - mcps.c: env `K` must be a bare ${{VAR}} reference (value not shown)",
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

    #[test]
    fn apply_delivers_real_copies_and_records_them() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).expect("apply");
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
            "agents-dir"
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
        // codex 那边没有条目 → 照常交付真拷贝。
        assert!(
            std::fs::symlink_metadata(t.path(".codex/skills/foo"))
                .unwrap()
                .file_type()
                .is_dir()
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
    fn kimi_reports_a_loose_skill_as_undeliverable_and_apply_says_so() {
        // §9.1：kimi 的散装 skill 交付不了，报告要说清楚，且 apply 不许因此失败。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".kimi-code").mkdir(".claude").mkdir(".codex");
        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(out.contains("! undeliverable  skill/foo"), "{out}");
        assert!(out.contains("kimi-code 无用户级 skills 目录"), "{out}");
        assert!(!t.path(".kimi-code/skills").exists());
    }

    /// Review Focus 4：`--harness` 只收窄**目标**。没被点名的家一个字节都不写。
    /// （「源扫描不跟着关掉」那条在 `tests/bundle_end_to_end.rs` 里以公开入口的形态钉。）
    #[test]
    fn harness_narrows_the_targets_only() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");

        let out = apply_with(&t, one_skill_manifest(), false, Some("codex"));

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
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        apply_with(&t, one_skill_manifest(), false, Some("codex"));
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
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        t.write(".codex/skills/handmade/SKILL.md", "# handmade");
        apply_with(&t, one_skill_manifest(), false, Some("codex"));
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
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        apply_with(&t, one_skill_manifest(), false, Some("codex"));

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
}
