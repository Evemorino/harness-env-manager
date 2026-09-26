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
use crate::manifest::Manifest;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// 清单的三个可收养段。段的顺序就是渲染顺序，也是 `section.as_str()` 的拼写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Skills,
    Mcps,
    Plugins,
}

impl Section {
    pub fn as_str(&self) -> &'static str {
        match self {
            Section::Skills => "skills",
            Section::Mcps => "mcps",
            Section::Plugins => "plugins",
        }
    }

    /// `Kind` → 它落在哪个段；表达不了的 kind 返回 `None`（§4 的 schema 只有三段）。
    fn of(kind: Kind) -> Option<Section> {
        match kind {
            Kind::Skill => Some(Section::Skills),
            Kind::Mcp => Some(Section::Mcps),
            Kind::Plugin => Some(Section::Plugins),
            Kind::Command | Kind::Agent | Kind::Hook => None,
        }
    }
}

/// 一条将要追加进清单的条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub section: Section,
    pub name: String,
    /// `kind/name`——与 `Skip.label` 同一套拼写，报告里两段的行首才读得一致。
    pub label: String,
    /// 条目**字段行**的 YAML 文本：每行 4 空格缩进（条目头下的一级）、以 `\n` 结尾，
    /// **不含** `name:` 那一行。插入是纯拼接，不需要再懂 YAML。
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
    let mut found: BTreeMap<(Kind, String), (BTreeSet<String>, Capability)> = BTreeMap::new();
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
                let reason = format!("随 plugin `{name}` 装进来的，不是散装条目");
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
            .map(|(harnesses, cap)| shape(&cap, &harnesses))
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
    for name in manifest.plugins.keys() {
        out.insert((Kind::Plugin, name.clone()));
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
                "plugin `{plugin}` 的分发产物，收养会把它降级成散装 skill"
            )),
            // 断链与指向库外的链接在这层分不开（`Spec::Skill` 只留 `content_path()`），
            // 所以理由写成一句覆盖两种情形的话——不为了好看的措辞去猜。
            SkillSource::Unknown => Err("来源不可判定（断链或指向共享库外），没有可记的源".into()),
            // 现状侧没有仓库与 ref 的信息（adapter 只读 harness 目录），记不出
            // `repo:`/`ref:` 两格——写一条空壳 `source: git` 比不写更坏。
            SkillSource::Git { .. } => {
                Err("来源是 git，但现状里读不到仓库与 ref，记不出 `repo:`/`ref:`".into())
            }
        },
        Spec::Mcp { .. } => Ok(()),
        Spec::Plugin {
            marketplace,
            enabled,
            ..
        } => {
            if !*enabled {
                return Err(
                    "已安装但被禁用（enabled = false），清单不该记一个用户关掉的东西".into(),
                );
            }
            if marketplace.is_none() {
                return Err(
                    "没有 marketplace，记不出 `source:`（§4 的 plugins 段只有三种 source）".into(),
                );
            }
            Ok(())
        }
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
    found: &mut BTreeMap<(Kind, String), (BTreeSet<String>, Capability)>,
    state: &HarnessState,
    cap: &Capability,
) {
    let entry = found
        .entry((cap.id.kind, cap.id.name.clone()))
        .or_insert_with(|| (BTreeSet::new(), cap.clone()));
    entry.0.insert(state.name.clone());
}

/// 一条能力 + 发现它的家 → 条目文本。
fn shape(cap: &Capability, harnesses: &BTreeSet<String>) -> Adoption {
    let kind = cap.id.kind;
    let section = Section::of(kind).expect("adoptable kinds all have a section");
    // 目标按 `KNOWN_HARNESSES` 的顺序写：清单是人读的，顺序不该随扫描顺序抖。
    let targets: Vec<String> = crate::manifest::KNOWN_HARNESSES
        .iter()
        .filter(|h| harnesses.contains(**h))
        .map(|h| (*h).to_string())
        .collect();
    let targets_line = format!("    targets: [{}]", targets.join(", "));
    let (mut body, summary) = match &cap.spec {
        Spec::Skill { source, .. } => match source {
            SkillSource::AgentsDir => (
                String::from("    source: agents-dir\n"),
                String::from("source=agents-dir"),
            ),
            SkillSource::Local { path: local } => {
                let p = local.display().to_string();
                (
                    format!("    source: local\n    path: {}\n", scalar(&p)),
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
            let _ = writeln!(out, "    command: {}", scalar(command));
            if !args.is_empty() {
                let items: Vec<String> = args.iter().map(|a| scalar(a)).collect();
                let _ = writeln!(out, "    args: [{}]", items.join(", "));
            }
            if !env.is_empty() {
                out.push_str("    env:\n");
                for (k, v) in env {
                    // 值永远是 `${VAR}` 引用，原样写回——**不展开**（§3 决策 3）。
                    let _ = writeln!(out, "      {}: {}", scalar(k), scalar(v));
                }
            }
            let summary = if args.is_empty() {
                format!("command={command}")
            } else {
                format!("command={command} args={}", args.join(" "))
            };
            (out, summary)
        }
        Spec::Plugin {
            marketplace,
            entry,
            version,
            ..
        } => {
            let marketplace = marketplace.clone().unwrap_or_default();
            let mut out = String::new();
            out.push_str("    source: claude-marketplace\n");
            let _ = writeln!(out, "    marketplace: {}", scalar(&marketplace));
            let _ = writeln!(out, "    entry: {}", scalar(entry));
            let mut summary =
                format!("source=claude-marketplace marketplace={marketplace} entry={entry}");
            // `ref:` 是这次收养的快照，不是判决。只有「version 形」才记（§2.1 第 3 条：
            // sha 形目录名与 `unknown` 与版本不可比，记进去就是把不可比当成了可比）。
            if let Some(v) = version.as_deref().and_then(version_shaped) {
                let _ = writeln!(out, "    ref: {}", scalar(&v));
                summary.push_str(&format!(" ref={v}"));
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

/// 「version 形」的判据只有一处定义（`capability::normalize_version`）：去前导 `v`/`V`
/// 后含小数点才算版本。sha 形目录名、分支名、`unknown` 都过不了。
fn version_shaped(value: &str) -> Option<String> {
    crate::capability::normalize_version(value)
}

/// YAML 标量渲染。安全就裸写，否则双引号包住并转义——**规则宁严勿宽**：多引一对引号
/// 只是不好看，少引一对可能让清单解不开或解成别的值。
///
/// 判「安全」的条件：非空、首字符是字母或 `_`（数字开头会被 YAML 读成整数）、
/// 其余字符属于 `[A-Za-z0-9_.\-/]`、且不是 `true/false/null/yes/no/on/off` 这类
/// 会被隐式解析成别的东西的词。
fn scalar(s: &str) -> String {
    let mut chars = s.chars();
    let plain = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        }
        _ => false,
    } && !matches!(
        s.to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off"
    );
    if plain {
        return s.to_string();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
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
        let text = format!(
            "version: 1\n{}:\n  {}:\n{body}",
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

    /// plugin：`ref:` 只在 version 形时记（sha 形与 `unknown` 与版本不可比）；
    /// 没有 marketplace 与「装了但禁用」都不收养。
    #[test]
    fn a_plugin_records_a_ref_only_when_the_version_is_comparable() {
        let manifest = Manifest::parse("version: 1\n").unwrap();
        let plugin =
            |entry: &str, marketplace: Option<&str>, version: Option<&str>, enabled| Spec::Plugin {
                marketplace: marketplace.map(str::to_string),
                entry: entry.into(),
                version: version.map(str::to_string),
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
                    plugin("superpowers", Some("official"), Some("v6.3.0"), true),
                ),
                cap(
                    "sha-only",
                    "claude-code",
                    plugin("sha-only", Some("official"), Some("b36e0829c6d0"), true),
                ),
                cap(
                    "no-marketplace",
                    "claude-code",
                    plugin("no-marketplace", None, None, true),
                ),
                cap(
                    "disabled",
                    "claude-code",
                    plugin("disabled", Some("official"), None, false),
                ),
            ],
        )];

        let report = adoptions(&manifest, &states);
        assert_eq!(report.adoptions.len(), 2, "{:?}", report.adoptions);
        // 按名字取，不按位置——BTreeMap 的顺序是名字升序（sha-only < superpowers）。
        let by_name = |name: &str| {
            report
                .adoptions
                .iter()
                .find(|a| a.name == name)
                .unwrap_or_else(|| panic!("no adoption named {name}"))
        };
        let superpowers = by_name("superpowers");
        // `scalar` 对数字开头的串一律引起来（YAML 会把 `6.3.0` 这类读成非字符串的
        // 隐式类型，多一对引号只是不好看）。往返断言在下面，那是真正的判据。
        assert!(
            superpowers.body.contains("ref: \"6.3.0\""),
            "{}",
            superpowers.body
        );
        let parsed = parse_with(superpowers.section, &superpowers.name, &superpowers.body);
        assert_eq!(
            parsed.plugins["superpowers"].git_ref.as_deref(),
            Some("6.3.0")
        );
        assert_eq!(
            parsed.plugins["superpowers"].marketplace.as_deref(),
            Some("official")
        );
        let sha_only = by_name("sha-only");
        assert!(
            !sha_only.body.contains("ref:"),
            "sha 形不可比，不该记 ref：{}",
            sha_only.body
        );

        assert_eq!(report.skips.len(), 2);
        assert!(
            report
                .skips
                .iter()
                .any(|s| s.reason.contains("marketplace"))
        );
        assert!(report.skips.iter().any(|s| s.reason.contains("禁用")));
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
