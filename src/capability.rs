use std::collections::BTreeMap;
use std::path::PathBuf;

/// 能力种类。`Plugin` 是一等公民——spec §2.1 第 2 条：三家 harness 都有 plugin 机制，
/// v1 以 bundle manifest 对账为主，只在目标 harness 缺 manifest 时才降级拆解。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Skill,
    Mcp,
    Command,
    Agent,
    Hook,
    Plugin,
}

impl Kind {
    pub fn all() -> [Kind; 6] {
        [
            Kind::Skill,
            Kind::Mcp,
            Kind::Command,
            Kind::Agent,
            Kind::Hook,
            Kind::Plugin,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Mcp => "mcp",
            Kind::Command => "command",
            Kind::Agent => "agent",
            Kind::Hook => "hook",
            Kind::Plugin => "plugin",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::all().into_iter().find(|k| k.as_str() == s)
    }
}

/// 能力身份。跨 harness 匹配只认 (kind, name)；plugin 的 name 是 entry 名
/// （spec §2.1 第 3 条），marketplace 不参与身份。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId {
    pub kind: Kind,
    pub name: String,
}

impl CapId {
    pub fn new(kind: Kind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
        }
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.kind.as_str(), self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Manifest,
    Plugin { name: String },
    Adopted { harness: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// 内容已在 ~/.agents/skills 共享库
    AgentsDir,
    /// git 仓库，ref 就是锁（spec §4）
    Git {
        repo: String,
        path: Option<String>,
        git_ref: String,
    },
    /// 本地开发中的目录
    Local { path: PathBuf },
    /// 由 plugin bundle 携带（不是散装 skill）
    PluginBundle { plugin: String },
    /// 来源无法判定（含断链）
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Skill {
        source: SkillSource,
        path: Option<PathBuf>,
    },
    Mcp {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Plugin {
        marketplace: Option<String>,
        entry: String,
        version: Option<String>,
        sha: Option<String>,
        install_path: Option<PathBuf>,
        enabled: bool,
    },
    Hook {
        event: String,
        command: String,
    },
    Command {
        path: PathBuf,
    },
    Agent {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Targets {
    #[default]
    All,
    Only(Vec<String>),
}

impl Targets {
    pub fn includes(&self, harness: &str) -> bool {
        match self {
            Targets::All => true,
            Targets::Only(list) => list.iter().any(|h| h == harness),
        }
    }
}

/// 每家 harness 对一个能力的接纳度——「拆解降级」的判定点
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Native,
    Degraded { as_kind: Kind },
    Unsupported { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub id: CapId,
    pub origin: Origin,
    pub spec: Spec,
    pub targets: Targets,
}

impl Capability {
    pub fn new(id: CapId, spec: Spec) -> Self {
        Self {
            id,
            origin: Origin::Manifest,
            spec,
            targets: Targets::All,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionMatch {
    Same,
    /// from = 现状（actual），to = 期望（desired）
    Different {
        from: String,
        to: String,
    },
    /// 无法判定（一侧有 SHA 一侧只有 version、跨 kind、或信息不足）。
    /// 只读工具不该误报，diff 把 Unknown 当 in-sync。
    Unknown,
}

fn mcp_summary(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        command.to_string()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

/// SHA 优先、version 兜底（spec §2.1 第 3 条）。
pub fn compare_versions(desired: &Spec, actual: &Spec) -> VersionMatch {
    match (desired, actual) {
        (Spec::Plugin { sha: Some(d), .. }, Spec::Plugin { sha: Some(a), .. }) => {
            if d == a {
                VersionMatch::Same
            } else {
                VersionMatch::Different {
                    from: a.clone(),
                    to: d.clone(),
                }
            }
        }
        (
            Spec::Plugin {
                sha: None,
                version: Some(d),
                ..
            },
            Spec::Plugin {
                sha: None,
                version: Some(a),
                ..
            },
        ) => {
            if d == a {
                VersionMatch::Same
            } else {
                VersionMatch::Different {
                    from: a.clone(),
                    to: d.clone(),
                }
            }
        }
        (Spec::Plugin { .. }, Spec::Plugin { .. }) => VersionMatch::Unknown,
        (
            Spec::Skill {
                source: SkillSource::Git { git_ref: d, .. },
                ..
            },
            Spec::Skill {
                source: SkillSource::Git { git_ref: a, .. },
                ..
            },
        ) => {
            if d == a {
                VersionMatch::Same
            } else {
                VersionMatch::Different {
                    from: a.clone(),
                    to: d.clone(),
                }
            }
        }
        (
            Spec::Mcp {
                command: dc,
                args: da,
                env: de,
            },
            Spec::Mcp {
                command: ac,
                args: aa,
                env: ae,
            },
        ) => {
            if dc == ac && da == aa && de == ae {
                VersionMatch::Same
            } else {
                VersionMatch::Different {
                    from: mcp_summary(ac, aa),
                    to: mcp_summary(dc, da),
                }
            }
        }
        _ => VersionMatch::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrips_through_str() {
        for k in Kind::all() {
            assert_eq!(Kind::parse(k.as_str()), Some(k), "{k:?} roundtrip");
        }
        assert_eq!(Kind::parse("nope"), None);
    }

    /// `Kind::all()` 的**顺序**是契约的一部分，不只是集合：Task 14 的
    /// `render::summary()` 按它逐项输出 `skills:42 mcps:6 plugins:3` 计数行，
    /// 而那一行由 insta 快照逐字节锁定。换成别的排列会让 Task 14 的 golden 因为
    /// 一个和它自己的改动无关的原因变红，所以在这里把整条序列钉死。
    #[test]
    fn kind_all_order_is_pinned() {
        assert_eq!(
            Kind::all(),
            [
                Kind::Skill,
                Kind::Mcp,
                Kind::Command,
                Kind::Agent,
                Kind::Hook,
                Kind::Plugin
            ]
        );
    }

    #[test]
    fn cap_id_label_is_kind_slash_name() {
        assert_eq!(
            CapId::new(Kind::Skill, "dingtalk-mail").label(),
            "skill/dingtalk-mail"
        );
        assert_eq!(CapId::new(Kind::Mcp, "context7").label(), "mcp/context7");
        assert_eq!(
            CapId::new(Kind::Plugin, "superpowers").label(),
            "plugin/superpowers"
        );
    }

    #[test]
    fn targets_all_includes_any_harness() {
        assert!(Targets::All.includes("claude-code"));
        assert!(Targets::All.includes("anything"));
    }

    #[test]
    fn targets_only_includes_listed_harnesses() {
        let t = Targets::Only(vec!["claude-code".into(), "codex".into()]);
        assert!(t.includes("claude-code"));
        assert!(t.includes("codex"));
        assert!(!t.includes("kimi-code"));
    }

    #[test]
    fn targets_default_is_all() {
        assert_eq!(Targets::default(), Targets::All);
    }

    #[test]
    fn capability_new_defaults_to_manifest_origin_and_all_targets() {
        let c = Capability::new(
            CapId::new(Kind::Mcp, "context7"),
            Spec::Mcp {
                command: "npx".into(),
                args: vec!["-y".into(), "@upstash/context7-mcp".into()],
                env: BTreeMap::new(),
            },
        );
        assert_eq!(c.origin, Origin::Manifest);
        assert_eq!(c.targets, Targets::All);
    }

    fn plugin(version: Option<&str>, sha: Option<&str>) -> Spec {
        Spec::Plugin {
            marketplace: Some("m".into()),
            entry: "superpowers".into(),
            version: version.map(String::from),
            sha: sha.map(String::from),
            install_path: None,
            enabled: true,
        }
    }

    #[test]
    fn same_sha_wins_over_differing_version_strings() {
        // 本机实测场景：claude 6.4.1 vs kimi v6.3.0，SHA 相同
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        assert_eq!(
            compare_versions(
                &plugin(Some("6.4.1"), Some(sha)),
                &plugin(Some("v6.3.0"), Some(sha))
            ),
            VersionMatch::Same
        );
    }

    #[test]
    fn differing_sha_is_different_and_reports_actual_then_desired() {
        let desired = plugin(Some("6.4.1"), Some("aaa"));
        let actual = plugin(Some("6.3.0"), Some("bbb"));
        assert_eq!(
            compare_versions(&desired, &actual),
            VersionMatch::Different {
                from: "bbb".into(),
                to: "aaa".into()
            }
        );
    }

    #[test]
    fn version_fallback_when_neither_side_has_sha() {
        assert_eq!(
            compare_versions(&plugin(Some("1.2.0"), None), &plugin(Some("1.2.0"), None)),
            VersionMatch::Same
        );
        assert_eq!(
            compare_versions(&plugin(Some("1.3.0"), None), &plugin(Some("1.2.0"), None)),
            VersionMatch::Different {
                from: "1.2.0".into(),
                to: "1.3.0".into()
            }
        );
    }

    #[test]
    fn mixed_sha_and_version_only_is_unknown() {
        assert_eq!(
            compare_versions(&plugin(None, Some("aaa")), &plugin(Some("6.3.0"), None)),
            VersionMatch::Unknown
        );
        assert_eq!(
            compare_versions(&plugin(None, None), &plugin(None, None)),
            VersionMatch::Unknown
        );
    }

    #[test]
    fn git_skill_compares_by_ref() {
        let git = |r: &str| Spec::Skill {
            source: SkillSource::Git {
                repo: "obra/superpowers".into(),
                path: Some("skills".into()),
                git_ref: r.into(),
            },
            path: None,
        };
        assert_eq!(
            compare_versions(&git("v6.3.0"), &git("v6.3.0")),
            VersionMatch::Same
        );
        assert_eq!(
            compare_versions(&git("v2.2"), &git("v2.1")),
            VersionMatch::Different {
                from: "v2.1".into(),
                to: "v2.2".into()
            }
        );
    }

    #[test]
    fn mcp_compares_command_args_and_env() {
        let mcp = |cmd: &str, args: Vec<&str>, env: Vec<(&str, &str)>| Spec::Mcp {
            command: cmd.into(),
            args: args.into_iter().map(String::from).collect(),
            env: env
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        assert_eq!(
            compare_versions(
                &mcp("npx", vec!["-y", "pkg"], vec![]),
                &mcp("npx", vec!["-y", "pkg"], vec![])
            ),
            VersionMatch::Same
        );
        assert_ne!(
            compare_versions(
                &mcp("npx", vec!["-y", "pkg"], vec![]),
                &mcp("npx", vec!["-y", "other"], vec![])
            ),
            VersionMatch::Same
        );
        // ${VAR} 引用参与比较，但比的是引用串本身，不展开
        assert_eq!(
            compare_versions(
                &mcp("npx", vec![], vec![("K", "${K}")]),
                &mcp("npx", vec![], vec![("K", "${K}")])
            ),
            VersionMatch::Same
        );
    }

    /// 两条 `Different` 断言只检查「不等于 Same」，所以把 from/to 写反也照样绿；
    /// 这条钉死方向：`from` 是现状（actual），`to` 是期望（desired）。
    /// 顺带覆盖 `mcp_summary` 的空 args 分支。
    #[test]
    fn mcp_different_reports_actual_as_from_and_desired_as_to() {
        let mcp = |cmd: &str, args: Vec<&str>| Spec::Mcp {
            command: cmd.into(),
            args: args.into_iter().map(String::from).collect(),
            env: Default::default(),
        };
        assert_eq!(
            compare_versions(
                &mcp("npx", vec!["-y", "pkg-v2"]),
                &mcp("npx", vec!["-y", "pkg-v1"])
            ),
            VersionMatch::Different {
                from: "npx -y pkg-v1".into(),
                to: "npx -y pkg-v2".into(),
            }
        );
        // 无 args 时 summary 退化成 command 本身
        assert_eq!(
            compare_versions(&mcp("newbin", vec![]), &mcp("oldbin", vec![])),
            VersionMatch::Different {
                from: "oldbin".into(),
                to: "newbin".into()
            }
        );
    }

    /// 这条是本 Task 真正的护栏。version-fallback 分支上那两个 `sha: None` 守卫一旦被删，
    /// 上面 `mixed_sha_and_version_only_is_unknown` 仍全绿——因为它两边各缺一个字段，
    /// 无论有没有守卫都进不了第二分支。必须用「一边 sha+version 都有、另一边只有 version」
    /// 这个形状才检得出来——而这恰好就是本机真实数据形状：claude 记 `6.4.1` + `gitCommitSha`，
    /// codex/kimi 只记 version。少了守卫，第二分支会先撞上，把本该 Unknown（按 in-sync 处理）
    /// 的一对报成 `Different { from: "6.3.0", to: "6.4.1" }`，于是每次 diff 都误报 drift。
    #[test]
    fn sha_plus_version_against_version_only_is_unknown() {
        assert_eq!(
            compare_versions(
                &plugin(Some("6.4.1"), Some("abc123")),
                &plugin(Some("6.3.0"), None)
            ),
            VersionMatch::Unknown
        );
        // 反方向同理：谁的哪一边有 sha 都不该改变结论
        assert_eq!(
            compare_versions(
                &plugin(Some("6.3.0"), None),
                &plugin(Some("6.4.1"), Some("abc123"))
            ),
            VersionMatch::Unknown
        );
    }

    #[test]
    fn cross_kind_specs_are_unknown() {
        let skill = Spec::Skill {
            source: SkillSource::AgentsDir,
            path: None,
        };
        assert_eq!(
            compare_versions(&skill, &plugin(Some("1.0"), None)),
            VersionMatch::Unknown
        );
    }
}
