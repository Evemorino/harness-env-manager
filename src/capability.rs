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
        [Kind::Skill, Kind::Mcp, Kind::Command, Kind::Agent, Kind::Hook, Kind::Plugin]
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
        Self { kind, name: name.into() }
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
    Git { repo: String, path: Option<String>, git_ref: String },
    /// 本地开发中的目录
    Local { path: PathBuf },
    /// 由 plugin bundle 携带（不是散装 skill）
    PluginBundle { plugin: String },
    /// 来源无法判定（含断链）
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Skill { source: SkillSource, path: Option<PathBuf> },
    Mcp { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    Plugin {
        marketplace: Option<String>,
        entry: String,
        version: Option<String>,
        sha: Option<String>,
        install_path: Option<PathBuf>,
        enabled: bool,
    },
    Hook { event: String, command: String },
    Command { path: PathBuf },
    Agent { path: PathBuf },
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
        Self { id, origin: Origin::Manifest, spec, targets: Targets::All }
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
        assert_eq!(CapId::new(Kind::Skill, "dingtalk-mail").label(), "skill/dingtalk-mail");
        assert_eq!(CapId::new(Kind::Mcp, "context7").label(), "mcp/context7");
        assert_eq!(CapId::new(Kind::Plugin, "superpowers").label(), "plugin/superpowers");
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
}
