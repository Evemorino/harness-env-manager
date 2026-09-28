//! scan：收养（spec §5 的反向管道）。
//!
//! `diff`/`apply` 的方向是「清单 → 本机」；`scan` 是它的反向：读一遍本机现状，把**清单
//! 还描述不了、但本机确实装着**的能力变成 `env.yaml` 条目追加进去。方向反了，判据不反
//! ——现状与清单的比对仍是同一套 `Capability`，本模块只是把「actual 有、desired 无」的那
//! 一侧翻译成清单文本。
//!
//! 三条边界：
//!
//! - **只追加，绝不修改已有条目。** 清单里已有同名同 kind 的条目时静默跳过——这不是
//!   finding（`diff` 的 `drift`/`update` 才是），报出来只会在每台机器上刷满屏。
//! - **收养不扩范围。** `targets:` 记**发现它的那几家**，不写 `all`：收养的语义是「把现状
//!   记下来」，不是「顺手让它铺到另外两家」。
//! - **表达不了的必须说出来。** 清单 schema 表达不了的 kind、随 plugin 分发的内容、来源
//!   不可判定的条目，一律进 `skips` 并写明理由——静默丢弃会让人以为「本机没有」。

use crate::capability::{Capability, Kind, Origin, SkillSource, Spec};
use crate::diff::HarnessState;
use crate::home::Home;
use crate::manifest::Manifest;
use crate::manifest_edit::{self, EntryRef, scalar};
use anyhow::Context as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

/// 清单的三个可收养段。[`Section`] 与它的渲染规则住在 [`crate::manifest_edit`]——
/// 那是清单文本的概念，`scan` 只是它的一个调用方。
pub use crate::manifest_edit::Section;

/// 一条将要追加进清单的条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub section: Section,
    pub name: String,
    /// `kind/name`——与 `Skip.label` 同一套拼写，报告里两段的行首才读得一致。
    pub label: String,
    /// 条目**字段行**的 YAML 文本：每行 2 空格缩进（相对条目头，往下一级）、以 `\n`
    /// 结尾，**不含** `name:` 那一行。缩进是**相对**的：清单里已有的段用几个空格缩进，
    /// 插入时就补几个（`insert_entries` 负责），这样 `scan` 不会把用户的排版改掉。
    pub body: String,
    /// 报告里的一行人话（不含 `targets`，那是渲染器统一补的）。
    pub summary: String,
    /// 发现它的那些家，按 `KNOWN_HARNESSES` 顺序——报告与 `targets:` 行共用这一份。
    pub targets: Vec<String>,
}

/// 一条「本机有、但收不进清单」的条目，以及为什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    pub label: String,
    pub harness: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdoptionReport {
    pub adoptions: Vec<Adoption>,
    pub skips: Vec<Skip>,
}

/// 现状 → 收养清单。判定是纯函数：不读盘、不看进程环境。
pub fn adoptions(manifest: &Manifest, states: &[HarnessState]) -> AdoptionReport {
    let existing = declared_ids(manifest);
    // 同一个 `(kind, name)` 在多家扫到只产一条：`targets` 是发现它的那些家的并集。
    // `BTreeMap` 让输出稳定（skill → mcp → plugin，名字升序），不随 registry 顺序抖。
    //
    // 值是**逐家那一份能力**（不是并集 + 一份代表）：plugin 的载荷（§10.3 的版本）
    // 是**逐家**的——§2.2 第 6 条实测同一个 plugin 在本机可以有两个版本，
    // 记成一份就必须挑一个扔另一个。skill/mcp 只用到它的键（家名）。
    let mut found: BTreeMap<(Kind, String), BTreeMap<String, Capability>> = BTreeMap::new();
    let mut skips: BTreeMap<(String, String, String), Skip> = BTreeMap::new();

    for state in states {
        for cap in &state.actual {
            let key = (cap.id.kind, cap.id.name.clone());
            // 清单已经描述了它：directions 反了，判据不反——内容是否一致是 `diff` 的事，
            // `scan` 一个字节都不碰（Review Focus 5）。这里刻意不出 skip 行：正常情况。
            if existing.contains(&key) {
                continue;
            }
            // 随 plugin 装进来的内容不是散装条目（kimi 的 bundle skill 走这一支）：
            // 把它写成清单条目，等于把原生安装降级成散装 skill（§2.1 第 2 条）。
            if let Origin::Plugin { name } = &cap.origin {
                let reason = format!("随 plugin `{name}` 装进来（不是散装条目）");
                record_skip(&mut skips, cap, state, reason);
                continue;
            }
            match adoptable(cap) {
                Ok(()) => {
                    filled(&mut found, state, cap);
                }
                Err(reason) => record_skip(&mut skips, cap, state, reason),
            }
        }
    }

    AdoptionReport {
        adoptions: found
            .into_values()
            .map(|by_harness| {
                // 代表那份只用来取身份、kind 与**来源描述**；载荷（plugin 的版本）逐家从表里取。
                //
                // 挑法：**优先共享库来源**。同一条 skill 在几家可以各有各的描述——实测
                // `mcp-builder` 在 claude 是真目录（`Local`）、在 kimi 是共享库入口
                // （`AgentsDir`）。共享库是唯一跨家通用的那一份：写进清单后三家都读得到它，
                // 而写 `local` 会让本来读得到它的 kimi 变成 `undeliverable`（真机上就是这么
                // 冒出来的）。没有任何一家报共享库来源时，才退回先扫到的那份。
                let representative = by_harness
                    .values()
                    .find(|c| {
                        matches!(
                            &c.spec,
                            Spec::Skill {
                                source: SkillSource::AgentsDir,
                                ..
                            }
                        )
                    })
                    .unwrap_or_else(|| by_harness.values().next().expect("filled 至少插一条"));
                shape(representative, &by_harness)
            })
            .collect(),
        skips: skips.into_values().collect(),
    }
}

/// 清单里已经写了的 `(kind, name)`。段名 → kind 的映射只有这一处。
fn declared_ids(manifest: &Manifest) -> BTreeSet<(Kind, String)> {
    let mut out = BTreeSet::new();
    for name in manifest.skills.keys() {
        out.insert((Kind::Skill, name.clone()));
    }
    for name in manifest.mcps.keys() {
        out.insert((Kind::Mcp, name.clone()));
    }
    for (key, p) in &manifest.plugins {
        // §10.3：plugin 的身份是 `entry`（缺省才是 key，`plugin_identity` 是唯一
        // 定义）。按 key 判会把 `{key: superpowers-bundle, entry: superpowers}`
        // 与扫到的 `superpowers` 当成两条不同的东西，收养出一条重复条目。
        out.insert((Kind::Plugin, crate::lower::plugin_identity(key, p)));
    }
    out
}

/// 这一条能不能收进清单。`Err` 里的理由是给人读的，也是报告里那一行。
fn adoptable(cap: &Capability) -> Result<(), String> {
    match &cap.spec {
        Spec::Skill { source, .. } => match source {
            SkillSource::AgentsDir => Ok(()),
            SkillSource::Local { .. } => Ok(()),
            // plugin 缓存里的真目录：收养它就把「随 plugin 分发」写成了「散装 skill」，
            // 下次 apply 会往 harness 自己的目录里拷一份，plugin 升级后两边分叉。
            SkillSource::PluginBundle { plugin } => Err(format!(
                "随 plugin `{plugin}` 分发（收养会降级成散装 skill）"
            )),
            // 断链与指向库外的链接在这层分不开（`Spec::Skill` 只留 `content_path()`），
            // 所以理由写成一句覆盖两种情形的话——不为了好看的措辞去猜。
            SkillSource::Unknown => Err("来源不可判定（断链或指向共享库外）".into()),
            // 现状侧没有仓库与 ref 的信息（adapter 只读 harness 目录），记不出
            // `repo:`/`ref:` 两格——写一条空壳 `source: git` 比不写更坏。
            SkillSource::Git { .. } => Err("来源是 git，但读不到 repo/ref，记不出这两格".into()),
        },
        Spec::Mcp { .. } => Ok(()),
        // §10.3：plugin 收养成**记录型**条目（`source: snapshot`）——只记身份与逐家
        // 版本，不请求交付。它不再落进「交付请求」那套语义（`source_harness:` 拆解、
        // `targets` 决定送到哪几家），所以旧的那两难（恒 undeliverable / 静默扩范围）
        // 整类消失。
        Spec::Plugin { .. } => Ok(()),
        // §4 的清单只有 skills / mcps / plugins 三段，这三种 kind 表达不了。
        Spec::Command { .. } | Spec::Agent { .. } | Spec::Hook { .. } => Err(format!(
            "清单 schema 表达不了 `{}` 这个 kind",
            cap.id.kind.as_str()
        )),
    }
}

fn record_skip(
    skips: &mut BTreeMap<(String, String, String), Skip>,
    cap: &Capability,
    state: &HarnessState,
    reason: String,
) {
    let label = cap.id.label();
    skips.insert(
        (label.clone(), state.name.clone(), reason.clone()),
        Skip {
            label,
            harness: state.name.clone(),
            reason,
        },
    );
}

fn filled(
    found: &mut BTreeMap<(Kind, String), BTreeMap<String, Capability>>,
    state: &HarnessState,
    cap: &Capability,
) {
    found
        .entry((cap.id.kind, cap.id.name.clone()))
        .or_default()
        .insert(state.name.clone(), cap.clone());
}

/// 一条能力 + 发现它的家（家名 → 那一份能力）→ 条目文本。
///
/// 传整张表而不是一个家名集合，是因为 plugin 的载荷**逐家**不同（§10.3 的版本，
/// §2.2 第 6 条：同一个 plugin 在本机可以有两个版本）。skill/mcp 只用它的键。
fn shape(cap: &Capability, harnesses: &BTreeMap<String, Capability>) -> Adoption {
    let kind = cap.id.kind;
    let section = Section::of(kind).expect("adoptable kinds all have a section");
    // 目标按 `KNOWN_HARNESSES` 的顺序写：清单是人读的，顺序不该随扫描顺序抖。
    let targets: Vec<String> = crate::manifest::KNOWN_HARNESSES
        .iter()
        .filter(|h| harnesses.contains_key(**h))
        .map(|h| (*h).to_string())
        .collect();
    let targets_line = format!("  targets: [{}]", targets.join(", "));
    let (mut body, summary) = match &cap.spec {
        Spec::Skill { source, .. } => match source {
            SkillSource::AgentsDir => (
                String::from("  source: agents-dir\n"),
                String::from("source=agents-dir"),
            ),
            SkillSource::Local { path: local } => {
                let p = local.display().to_string();
                (
                    format!("  source: local\n  path: {}\n", scalar(&p)),
                    format!("source=local path={p}"),
                )
            }
            // `adoptable` 已经把它们挡在外面；这里给一个不会静默产出错误条目的兜底。
            SkillSource::PluginBundle { plugin } => {
                unreachable!("not adoptable: plugin `{plugin}` bundle")
            }
            SkillSource::Unknown => unreachable!("not adoptable: unknown skill source"),
            SkillSource::Git { .. } => unreachable!("not adoptable: git source"),
        },
        Spec::Mcp { command, args, env } => {
            let mut out = String::new();
            let _ = writeln!(out, "  command: {}", scalar(command));
            if !args.is_empty() {
                let items: Vec<String> = args.iter().map(|a| scalar(a)).collect();
                let _ = writeln!(out, "  args: [{}]", items.join(", "));
            }
            if !env.is_empty() {
                out.push_str("  env:\n");
                for (k, v) in env {
                    // 值永远是 `${VAR}` 引用，原样写回——**不展开**（§3 决策 3）。
                    let _ = writeln!(out, "    {}: {}", scalar(k), scalar(v));
                }
            }
            let summary = if args.is_empty() {
                format!("command={command}")
            } else {
                format!("command={command} args={}", args.join(" "))
            };
            (out, summary)
        }
        // §10.3：plugin 收养成记录型条目。**逐家**记版本，且**只记 version 形**的
        // （Review Focus 5）：sha 形的目录名（`db467cc56673`）记进去就是一条 diff
        // 说「不可比」的死值，比不记更坏——它看起来像是记下了什么。
        Spec::Plugin { .. } => {
            let mut recorded: Vec<(String, String)> = Vec::new();
            let mut unrecorded: Vec<String> = Vec::new();
            for h in crate::manifest::KNOWN_HARNESSES {
                let Some(c) = harnesses.get(h) else { continue };
                let Spec::Plugin { version, .. } = &c.spec else {
                    unreachable!("kind 与 spec 同源")
                };
                match version {
                    Some(v) if crate::capability::normalize_version(v).is_some() => {
                        recorded.push(((*h).to_string(), v.clone()));
                    }
                    // 观测到的不是 version 形（sha 形目录名、分支名），或本机读不出
                    // （codex 恒 `None`）：家仍然进 `targets`（它确实装着），版本不记。
                    _ => unrecorded.push((*h).to_string()),
                }
            }
            let mut out = String::from("  source: snapshot\n");
            if !recorded.is_empty() {
                out.push_str("  versions:\n");
                for (h, v) in &recorded {
                    let _ = writeln!(out, "    {h}: {}", scalar(v));
                }
            }
            let mut summary = String::from("source=snapshot");
            if !recorded.is_empty() {
                let list: Vec<String> = recorded.iter().map(|(h, v)| format!("{h} {v}")).collect();
                let _ = write!(summary, " versions=[{}]", list.join(", "));
            }
            if !unrecorded.is_empty() {
                let _ = write!(
                    summary,
                    "（{} 的版本不是 version 形，未记）",
                    unrecorded.join(", ")
                );
            }
            (out, summary)
        }
        Spec::Command { .. } | Spec::Agent { .. } | Spec::Hook { .. } => {
            unreachable!("not adoptable: {:?}", cap.id.kind)
        }
    };
    body.push_str(&targets_line);
    body.push('\n');
    Adoption {
        section,
        name: cap.id.name.clone(),
        label: cap.id.label(),
        body,
        summary,
        targets,
    }
}

/// 收养一次。I/O 只在两处：读现状（走与 `diff`/`apply`/`doctor` 共用的那一个
/// `read_harness_states`）与写回（复用 `apply::write::write_config` 的
/// temp → fsync → rename + `.henv.bak`）。
///
/// `dry_run` 时**只读**：不落 `.henv.bak`、不碰清单。
pub fn run_scan(
    manifest_path: &Path,
    home: &Home,
    dry_run: bool,
    out: &mut dyn Write,
) -> anyhow::Result<AdoptionReport> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("cannot read manifest: {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", manifest_path.display()))?;

    let registry = crate::adapters::registry(home);
    let states = crate::read_harness_states(&registry, &manifest, None)?;
    let report = adoptions(&manifest, &states);

    // **先落盘、后打印报告**：报告末行会说「已追加到 env.yaml」，那句话不能先于事实出现
    // （写失败时用户只看得到 `Error:`，看不到一句已经说出口的谎）。
    if !dry_run {
        if let Some(new_text) = insert_entries(&text, &report.adoptions)? {
            // 落盘前先自己解一遍：写出一份自己都读不回的清单，比不写坏得多。
            Manifest::parse(&new_text)
                .context("refusing to write a manifest that does not parse back")?;
            crate::apply::write::write_config(manifest_path, new_text.as_bytes())?;
        }
    }
    write!(out, "{}", render_scan(&report, dry_run))?;
    Ok(report)
}

/// 把条目插进清单文本的对应段，**已有字节一个不动**。返回 `None` = 没有条目可插
/// （空报告是空操作）。
///
/// 段定位、缩进推断与拒绝规则住在 [`crate::manifest_edit`]——`scan` 只是它的一个调用方，
/// 这里把 [`Adoption`] 翻译成通用的 [`EntryRef`]。
pub fn insert_entries(text: &str, adoptions: &[Adoption]) -> anyhow::Result<Option<String>> {
    let entries: Vec<EntryRef<'_>> = adoptions
        .iter()
        .map(|a| EntryRef {
            section: a.section,
            name: &a.name,
            body: &a.body,
        })
        .collect();
    manifest_edit::insert_entries(text, &entries)
}

/// 报告渲染。段名风格与 `doctor` 一致，但渲染器各是各的（`doctor` 的 golden 不该被
/// 收养报告的形状牵动）。`dry_run` 只影响末行那一句——它必须让读的人一眼看出
/// 「刚才那一下到底写没写盘」。
pub fn render_scan(report: &AdoptionReport, dry_run: bool) -> String {
    let width = report
        .adoptions
        .iter()
        .map(|a| a.label.len())
        .chain(
            report
                .skips
                .iter()
                .map(|s| s.label.len() + 1 + s.harness.len()),
        )
        .max()
        .unwrap_or(0)
        + 2;

    let mut out = String::new();
    let mut first = true;
    for a in &report.adoptions {
        let head = if first {
            first = false;
            format!("{:<width$}", "(adopt)")
        } else {
            " ".repeat(width)
        };
        let _ = writeln!(
            out,
            "{head}{:<width$} → {}: {} targets=[{}]",
            a.label,
            a.section.as_str(),
            a.summary,
            a.targets.join(", ")
        );
    }
    for s in &report.skips {
        let head = if first {
            first = false;
            format!("{:<width$}", "(skip)")
        } else {
            " ".repeat(width)
        };
        let label = format!("{}@{}", s.label, s.harness);
        let _ = writeln!(out, "{head}{label:<width$} {}", s.reason);
    }
    if !report.adoptions.is_empty() || !report.skips.is_empty() {
        out.push('\n');
    }

    let adopted = report.adoptions.len();
    let count = if adopted == 0 {
        "nothing to adopt".to_string()
    } else {
        format!("{adopted} to adopt")
    };
    let skipped = if report.skips.is_empty() {
        String::new()
    } else {
        format!(", {} skipped", report.skips.len())
    };
    let mode = if dry_run {
        "(dry run)"
    } else if adopted == 0 {
        // 没有条目可插时 `insert_entries` 直接返回 `None`：清单与 `.henv.bak` 都不动
        // （`run_scan` 那一支根本不会走到 `write_config`）。说「已追加」就是一句假话，
        // 而报告是唯一告诉用户「刚才到底发生了什么」的地方。
        "(nothing written)"
    } else {
        "(appended to env.yaml)"
    };
    let _ = writeln!(out, "Scan: {count}{skipped} {mode}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Targets};
    use std::path::PathBuf;

    fn cap(name: &str, harness: &str, spec: Spec) -> Capability {
        Capability {
            id: CapId::new(kind_of(&spec), name),
            origin: Origin::Adopted {
                harness: harness.into(),
            },
            spec,
            targets: Targets::All,
        }
    }

    fn kind_of(spec: &Spec) -> Kind {
        match spec {
            Spec::Skill { .. } => Kind::Skill,
            Spec::Mcp { .. } => Kind::Mcp,
            Spec::Plugin { .. } => Kind::Plugin,
            Spec::Hook { .. } => Kind::Hook,
            Spec::Command { .. } => Kind::Command,
            Spec::Agent { .. } => Kind::Agent,
        }
    }

    fn state(harness: &str, caps: Vec<Capability>) -> HarnessState {
        HarnessState {
            name: harness.into(),
            detected: true,
            actual: caps,
        }
    }

    /// 把一条生成的条目插进一份最小清单并解出来——**生成的文本必须自己解得出**，
    /// 这是「渲染器写错了」唯一有效的证据（比逐字符断言 body 强得多）。
    fn parse_with(section: Section, name: &str, body: &str) -> Manifest {
        // body 是**相对**缩进（2 空格 = 条目头下一级），拼进一份 2 空格缩进的清单。
        let indented: String = body.lines().map(|l| format!("  {l}\n")).collect();
        let text = format!(
            "version: 1\n{}:\n  {}:\n{indented}",
            section.as_str(),
            scalar(name)
        );
        Manifest::parse(&text).unwrap_or_else(|e| panic!("generated entry must parse: {e}\n{text}"))
    }

    fn agents_dir() -> Spec {
        Spec::Skill {
            source: SkillSource::AgentsDir,
            path: None,
        }
    }

    /// 两家都扫到同一条 `agents-dir` skill → 一条条目，`targets` 是那两家的并集，
    /// 且生成的文本解得出、字段逐一对得上。
    #[test]
    fn an_adopted_agents_dir_skill_remembers_only_the_harnesses_that_had_it() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let states = vec![
            state(
                "claude-code",
                vec![cap("trim", "claude-code", agents_dir())],
            ),
            state("codex", vec![cap("trim", "codex", agents_dir())]),
        ];

        let report = adoptions(&manifest, &states);
        assert_eq!(report.skips, Vec::new());
        assert_eq!(report.adoptions.len(), 1);
        let a = &report.adoptions[0];
        assert_eq!(a.section, Section::Skills);
        assert_eq!(a.name, "trim");
        assert_eq!(
            a.targets,
            vec!["claude-code".to_string(), "codex".to_string()]
        );

        let parsed = parse_with(a.section, &a.name, &a.body);
        let entry = &parsed.skills["trim"];
        assert_eq!(entry.source, crate::manifest::SkillSourceKind::AgentsDir);
        assert_eq!(
            entry.targets,
            Targets::Only(vec!["claude-code".into(), "codex".into()])
        );
    }

    /// 清单已经写了同名同 kind → 既不收养也不报（正常情况，报出来会在每台机器上刷满屏）。
    #[test]
    fn a_capability_already_in_the_manifest_is_never_adopted() {
        let manifest =
            Manifest::parse("version: 1\nskills:\n  foo:\n    source: agents-dir\n").unwrap();
        let states = vec![state(
            "claude-code",
            vec![cap("foo", "claude-code", agents_dir())],
        )];

        let report = adoptions(&manifest, &states);
        assert_eq!(report.adoptions, Vec::new());
        assert_eq!(report.skips, Vec::new(), "已声明的条目不是 finding");
    }

    /// §4 的清单只有三段：command / agent / hook 表达不了，必须逐条说出理由。
    #[test]
    fn kinds_the_manifest_cannot_express_are_skipped_with_a_reason() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let states = vec![state(
            "claude-code",
            vec![
                cap(
                    "pre-commit",
                    "claude-code",
                    Spec::Hook {
                        event: "PreToolUse".into(),
                        command: "x".into(),
                    },
                ),
                cap(
                    "ship",
                    "claude-code",
                    Spec::Command {
                        path: PathBuf::from("/x"),
                    },
                ),
                cap(
                    "reviewer",
                    "claude-code",
                    Spec::Agent {
                        path: PathBuf::from("/x"),
                    },
                ),
            ],
        )];

        let report = adoptions(&manifest, &states);
        assert!(report.adoptions.is_empty());
        assert_eq!(report.skips.len(), 3);
        for skip in &report.skips {
            assert!(
                skip.reason.contains("清单 schema 表达不了"),
                "{}",
                skip.reason
            );
            assert_eq!(skip.harness, "claude-code");
        }
    }

    /// plugin 分发的内容不是散装条目：`SkillSource::PluginBundle` 与 `Origin::Plugin`
    /// 两支都要挡（kimi 走前者在 `Origin::Plugin`，claude/codex 走后者）。
    #[test]
    fn plugin_distributed_content_is_skipped_because_adopting_would_downgrade_it() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let mut bundled = cap(
            "brainstorming",
            "codex",
            Spec::Skill {
                source: SkillSource::PluginBundle {
                    plugin: "superpowers".into(),
                },
                path: None,
            },
        );
        bundled.origin = Origin::Plugin {
            name: "superpowers".into(),
        };
        let states = vec![state(
            "codex",
            vec![
                bundled,
                cap(
                    "managed-skill",
                    "kimi-code",
                    Spec::Skill {
                        source: SkillSource::PluginBundle {
                            plugin: "kimi-cu".into(),
                        },
                        path: None,
                    },
                ),
            ],
        )];

        let report = adoptions(&manifest, &states);
        assert!(report.adoptions.is_empty());
        assert_eq!(report.skips.len(), 2);
        assert!(
            report.skips[0].reason.contains("superpowers"),
            "{:?}",
            report.skips[0]
        );
        assert!(
            report.skips[1].reason.contains("kimi-cu"),
            "{:?}",
            report.skips[1]
        );
    }

    /// MCP 的 `command`/`args`/`env` 全都要写回，且 `env` 值是 `${VAR}` 引用**原样**
    /// 保留（不展开，§3 决策 3）。
    #[test]
    fn an_mcp_is_adopted_with_its_command_args_and_unexpanded_refs() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "CONTEXT7_API_KEY".to_string(),
            "${CONTEXT7_API_KEY}".to_string(),
        );
        let states = vec![state(
            "codex",
            vec![cap(
                "context7",
                "codex",
                Spec::Mcp {
                    command: "npx".into(),
                    args: vec!["-y".into(), "@upstash/context7-mcp".into()],
                    env,
                },
            )],
        )];

        let report = adoptions(&manifest, &states);
        assert_eq!(report.adoptions.len(), 1);
        let a = &report.adoptions[0];
        assert_eq!(a.section, Section::Mcps);
        let parsed = parse_with(a.section, &a.name, &a.body);
        let entry = &parsed.mcps["context7"];
        assert_eq!(entry.command, "npx");
        assert_eq!(entry.args, vec!["-y", "@upstash/context7-mcp"]);
        assert_eq!(
            entry.env["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}",
            "引用必须原样写回，绝不展开"
        );
        assert_eq!(entry.targets, Targets::Only(vec!["codex".into()]));
    }

    /// §10.3：plugin 收养成**记录型**条目——`source: snapshot` + 逐家版本，
    /// **没有** `source_harness:`（它不再是一条交付请求）。三种形态都给同一份
    /// 记录：有没有 marketplace 与「记不记得住」无关，`enabled: false` 的也装着。
    ///
    /// 生成的文本必须自己解得出（与别的条目共用 `parse_with`）。
    #[test]
    fn marketplace_plugins_are_adopted_as_snapshot_records() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let plugin = |entry: &str, marketplace: Option<&str>, enabled| Spec::Plugin {
            marketplace: marketplace.map(str::to_string),
            entry: entry.into(),
            version: Some("v6.3.0".into()),
            sha: None,
            install_path: None,
            enabled,
        };
        let states = vec![state(
            "claude-code",
            vec![
                cap(
                    "superpowers",
                    "claude-code",
                    plugin("superpowers", Some("official"), true),
                ),
                cap(
                    "no-marketplace",
                    "claude-code",
                    plugin("no-marketplace", None, true),
                ),
                cap(
                    "disabled",
                    "claude-code",
                    plugin("disabled", Some("official"), false),
                ),
            ],
        )];

        let report = adoptions(&manifest, &states);
        assert!(report.skips.is_empty(), "{:?}", report.skips);
        assert_eq!(report.adoptions.len(), 3);
        for a in &report.adoptions {
            assert_eq!(a.section, Section::Plugins);
            assert_eq!(a.targets, vec!["claude-code".to_string()]);
            let parsed = parse_with(a.section, &a.name, &a.body);
            let entry = &parsed.plugins[&a.name];
            assert_eq!(
                entry.source,
                crate::manifest::PluginSourceKind::Snapshot,
                "收养来的 plugin 只能是记录型：{a:?}"
            );
            assert_eq!(entry.source_harness, None, "它不是交付请求：{a:?}");
            assert_eq!(
                entry.versions.as_ref().unwrap()["claude-code"],
                "v6.3.0",
                "原值记，不做归一化：{a:?}"
            );
        }
    }

    /// Review Focus 5：**只记 version 形**的版本。sha 形的目录名（`db467cc56673`）
    /// 记进去就是一条 diff 说「不可比」的死值——它看起来像记下了什么，实际不会
    /// 被对账。这台家仍然进 `targets`（它确实装着），版本不记。
    #[test]
    fn a_plugin_version_that_cannot_be_compared_is_not_recorded() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let plugin = |entry: &str, version: Option<&str>| Spec::Plugin {
            marketplace: None,
            entry: entry.into(),
            version: version.map(str::to_string),
            sha: None,
            install_path: None,
            enabled: true,
        };
        let states = vec![
            state(
                "claude-code",
                vec![cap(
                    "sha-shaped",
                    "claude-code",
                    plugin("sha-shaped", Some("db467cc56673")),
                )],
            ),
            state(
                "codex",
                vec![cap("sha-shaped", "codex", plugin("sha-shaped", None))],
            ),
        ];

        let report = adoptions(&manifest, &states);
        assert_eq!(report.adoptions.len(), 1);
        let a = &report.adoptions[0];
        let parsed = parse_with(a.section, &a.name, &a.body);
        let entry = &parsed.plugins["sha-shaped"];
        assert_eq!(
            entry.versions, None,
            "没有可比版本就不写 `versions:`：{a:?}"
        );
        assert_eq!(a.targets, vec!["claude-code", "codex"]);
        assert!(a.summary.contains("未记"), "不记也要说出来：{}", a.summary);
    }

    /// Review Focus 4：清单里已有同**身份**的条目（哪怕是交付型、哪怕 key 不一致）
    /// 就不收养——按 key 判会收养出一条重复条目。
    #[test]
    fn a_plugin_already_declared_under_another_key_is_not_adopted() {
        let manifest = Manifest::parse(
            "version: 1\nplugins:\n  superpowers-bundle:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n",
        )
        .unwrap();
        let states = vec![state(
            "claude-code",
            vec![cap(
                "superpowers",
                "claude-code",
                Spec::Plugin {
                    marketplace: Some("official".into()),
                    entry: "superpowers".into(),
                    version: Some("v6.3.0".into()),
                    sha: None,
                    install_path: None,
                    enabled: true,
                },
            )],
        )];
        let report = adoptions(&manifest, &states);
        assert!(
            report.adoptions.is_empty(),
            "identity 已在清单里就不收养：{:?}",
            report.adoptions
        );
        assert!(
            report.skips.is_empty(),
            "那是正常情况，不是 skip：{:?}",
            report.skips
        );
    }

    /// 名字里的 `:`、`#`、空格、前导数字都会让裸标量解成别的东西（或解不开）。
    /// 这条把 `scalar` 的判据钉死在**往返**上：解析回来的名字与原串逐字符相等。
    #[test]
    fn names_that_need_quoting_survive_the_round_trip() {
        let awkward = [
            "foo:bar",
            "#hash",
            "with space",
            "123",
            "true",
            "a\"b",
            "-dash",
            "trim_cot_leakage",
        ];
        for name in awkward {
            let manifest = Manifest::parse("version: 1\n").unwrap();
            let states = vec![state("codex", vec![cap(name, "codex", agents_dir())])];
            let report = adoptions(&manifest, &states);
            assert_eq!(report.adoptions.len(), 1, "{name}");
            let a = &report.adoptions[0];
            let parsed = parse_with(a.section, &a.name, &a.body);
            assert!(
                parsed.skills.contains_key(name),
                "{name:?} 没有往返回来，清单里是 {:?}",
                parsed.skills.keys().collect::<Vec<_>>()
            );
        }
    }

    /// 原串是新串的**子序列**：只做拼接的插入器，这条是「一个已有字节都没动」的判据
    /// （比逐行比对更强——它连行内字符的顺序漂移都能抓到）。
    fn is_subsequence(needle: &str, haystack: &str) -> bool {
        let mut it = haystack.chars();
        needle.chars().all(|c| it.any(|h| h == c))
    }

    fn skill_adoption(name: &str) -> Adoption {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let states = vec![state("codex", vec![cap(name, "codex", agents_dir())])];
        adoptions(&manifest, &states).adoptions.remove(0)
    }

    /// 追加进已存在的段：新条目落在那一段的末尾（下一个顶层键之前），原有字节一个不动。
    #[test]
    fn appending_into_an_existing_section_keeps_every_original_byte() {
        let text = "# 我的清单\nversion: 1\n\nskills:\n  foo:\n    source: agents-dir\n    targets: [claude-code]\n\nmcps:\n  m:\n    command: npx\n";
        let out = insert_entries(text, &[skill_adoption("bar")])
            .unwrap()
            .unwrap();

        assert!(is_subsequence(text, &out), "原有字节被动过了：\n{out}");
        let bar = out.find("  bar:").expect("新条目应在");
        assert!(
            out[..bar].ends_with("    targets: [claude-code]\n"),
            "新条目该接在 skills 段最后一个条目之后：\n{out}"
        );
        assert!(bar < out.find("mcps:").unwrap(), "不该插进别的段：\n{out}");
        // 解得出，且两边条目都在。
        let parsed = Manifest::parse(&out).unwrap();
        assert!(parsed.skills.contains_key("foo") && parsed.skills.contains_key("bar"));
        assert!(parsed.mcps.contains_key("m"), "后面那段不许被吃掉：\n{out}");
    }

    /// 段不存在 → 在文件末尾新建；与前文之间恰一个空行。
    #[test]
    fn a_missing_section_is_created_at_the_end() {
        let manifest =
            Manifest::parse("version: 1\nskills:\n  foo:\n    source: agents-dir\n").unwrap();
        let states = vec![state(
            "codex",
            vec![cap(
                "m",
                "codex",
                Spec::Mcp {
                    command: "npx".into(),
                    args: Vec::new(),
                    env: Default::default(),
                },
            )],
        )];
        let report = adoptions(&manifest, &states);
        let text = "version: 1\nskills:\n  foo:\n    source: agents-dir\n";
        let out = insert_entries(text, &report.adoptions).unwrap().unwrap();

        assert!(is_subsequence(text, &out), "原有字节被动过了：\n{out}");
        assert!(
            out.ends_with(
                "    source: agents-dir\n\nmcps:\n  m:\n    command: npx\n    targets: [codex]\n"
            ),
            "尾行长这样：\n{out:?}"
        );
        let parsed = Manifest::parse(&out).unwrap();
        assert!(parsed.skills.contains_key("foo") && parsed.mcps.contains_key("m"));
    }

    /// 不同段各自就位：skills 插段内、plugins 新建在末尾，一次调用同时发生。
    #[test]
    fn entries_land_in_their_own_sections() {
        let manifest =
            Manifest::parse("version: 1\nskills:\n  foo:\n    source: agents-dir\n").unwrap();
        let states = vec![state(
            "claude-code",
            vec![
                cap("bar", "claude-code", agents_dir()),
                cap(
                    "m",
                    "claude-code",
                    Spec::Mcp {
                        command: "npx".into(),
                        args: Vec::new(),
                        env: Default::default(),
                    },
                ),
            ],
        )];
        let report = adoptions(&manifest, &states);
        let out = insert_entries(
            "version: 1\nskills:\n  foo:\n    source: agents-dir\n",
            &report.adoptions,
        )
        .unwrap()
        .unwrap();

        let parsed = Manifest::parse(&out).unwrap();
        assert!(parsed.skills.contains_key("foo") && parsed.skills.contains_key("bar"));
        assert!(parsed.mcps.contains_key("m"), "新段该建在文件末尾：\n{out}");
        assert!(is_subsequence(
            "version: 1\nskills:\n  foo:\n    source: agents-dir\n",
            &out
        ));
    }

    /// 段头带内联值时**拒绝**，不试着去展开它——展开就要重写用户的排版。
    #[test]
    fn an_inline_section_header_is_refused_instead_of_corrupted() {
        for text in [
            "version: 1\nskills: {}\n",
            "version: 1\nskills: [a]\n",
            "version: 1\nskills: all\n",
        ] {
            let err = insert_entries(text, &[skill_adoption("bar")]).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("内联值"), "{text:?} → {msg}");
            assert!(msg.contains("skills"), "{text:?} → {msg}");
        }
    }

    /// 用户的段用 4 空格缩进条目 → 新条目也 4 空格，且整份清单仍解得出
    /// （YAML 的块缩进在同一层必须一致，插错缩进会让整份文件解不开）。
    #[test]
    fn entries_follow_the_existing_block_indentation() {
        let text = "version: 1\nskills:\n    foo:\n        source: agents-dir\n";
        let out = insert_entries(text, &[skill_adoption("bar")])
            .unwrap()
            .unwrap();
        // 条目头跟着旧条目的 4 空格；字段比它深一级（6 空格）——YAML 只要求更深。
        assert!(
            out.contains("\n    bar:\n      source: agents-dir\n"),
            "{out:?}"
        );
        let parsed = Manifest::parse(&out).unwrap();
        assert!(parsed.skills.contains_key("foo") && parsed.skills.contains_key("bar"));
    }

    /// 段里只有注释时，插在注释之后、段头之下（注释是用户的，不许被顶开或压住）。
    #[test]
    fn an_empty_but_commented_section_takes_entries_below_its_comment() {
        let text = "version: 1\nskills:\n  # 下面都是 skill\nmcps:\n  m:\n    command: npx\n";
        let out = insert_entries(text, &[skill_adoption("bar")])
            .unwrap()
            .unwrap();
        let bar = out.find("  bar:").unwrap();
        assert!(bar > out.find("# 下面都是 skill").unwrap(), "{out}");
        assert!(bar < out.find("mcps:").unwrap(), "{out}");
        assert!(Manifest::parse(&out).unwrap().skills.contains_key("bar"));
    }

    /// 文件末尾没有换行也要能追加（不然会与最后一行粘在一起）。
    #[test]
    fn a_file_without_a_trailing_newline_still_gets_a_clean_append() {
        let out = insert_entries("version: 1", &[skill_adoption("bar")])
            .unwrap()
            .unwrap();
        assert!(out.contains("version: 1\n\nskills:\n  bar:\n"), "{out:?}");
        assert!(Manifest::parse(&out).unwrap().skills.contains_key("bar"));
    }

    /// 空报告 = 一个字节都不写（返回 `None`，调用方因此不会碰盘）。
    #[test]
    fn an_empty_adoption_list_writes_nothing() {
        assert_eq!(insert_entries("version: 1\n", &[]).unwrap(), None);
    }

    /// 一个 `scalar` 的字面契约（往返之外的第二条证据：什么时候它才裸写）。
    #[test]
    fn scalar_only_leaves_plain_words_alone() {
        for plain in ["foo", "foo-bar", "foo_bar", "foo.bar", "a/b", "_x"] {
            assert_eq!(scalar(plain), plain, "{plain} 该裸写");
        }
        for quoted in [
            "", "1foo", "true", "null", "foo bar", "foo:bar", "#x", "-x", "a\"b",
        ] {
            assert!(scalar(quoted).starts_with('"'), "{quoted:?} 该引起来");
        }
    }

    /// 空报告是空操作：没有段、没有空行，只有末行。
    #[test]
    fn an_empty_report_is_only_the_footer() {
        assert_eq!(
            render_scan(&AdoptionReport::default(), true),
            "Scan: nothing to adopt (dry run)\n"
        );
    }

    /// 报告的 golden：收养与跳过两段、末行的模式词。
    #[test]
    fn a_scan_report_renders_both_sections() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("KIMI_API_KEY".to_string(), "${KIMI_API_KEY}".to_string());
        let states = vec![
            state(
                "claude-code",
                vec![cap(
                    "trim",
                    "claude-code",
                    Spec::Skill {
                        source: SkillSource::Local {
                            path: PathBuf::from("/home/u/.claude/skills/trim"),
                        },
                        path: None,
                    },
                )],
            ),
            state(
                "kimi-code",
                vec![
                    cap(
                        "kimi-cu",
                        "kimi-code",
                        Spec::Mcp {
                            command: "uvx".into(),
                            args: Vec::new(),
                            env,
                        },
                    ),
                    cap(
                        "ext",
                        "kimi-code",
                        Spec::Skill {
                            source: SkillSource::PluginBundle {
                                plugin: "kimi-cu".into(),
                            },
                            path: None,
                        },
                    ),
                ],
            ),
        ];

        let report = adoptions(&manifest, &states);
        insta::assert_snapshot!(render_scan(&report, true));
    }
}
