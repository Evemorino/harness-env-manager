//! env.yaml 的类型化解析与校验。
//!
//! 三类条目（skills / mcps / plugins）是用户书写视角，解析后统一带 `targets` 与
//! `degrade` 语义（spec §4 决策 1）。校验错误**聚合**后一次性报出，避免用户改一条跑一次。

use crate::capability::{Kind, Targets};
use crate::secrets;
use serde::Deserialize;
use serde::de::{self, Deserializer, Unexpected};
use serde_yaml_ng::Value;
use std::collections::BTreeMap;

/// 清单支持的 schema 版本。
pub const SUPPORTED_VERSION: u32 = 1;

/// 已知 harness 名。新增 harness = 在此加一行 + adapters/ 新模块（spec §7）。
pub const KNOWN_HARNESSES: [&str; 3] = ["claude-code", "codex", "kimi-code"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub harnesses: BTreeMap<String, HarnessEntry>,
    #[serde(default)]
    pub skills: BTreeMap<String, SkillEntry>,
    #[serde(default)]
    pub mcps: BTreeMap<String, McpEntry>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessEntry {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillSourceKind {
    AgentsDir,
    Git,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    pub source: SkillSourceKind,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub targets: Targets,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, deserialize_with = "env_refs")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub targets: Targets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginSourceKind {
    ClaudeMarketplace,
    Github,
    ZipUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    pub source: PluginSourceKind,
    #[serde(default)]
    pub marketplace: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub degrade: DegradePolicy,
    #[serde(default)]
    pub targets: Targets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DegradeAction {
    /// 进 doctor / diff 的 `!` 行
    #[default]
    Report,
    /// 静默丢弃
    Skip,
}

impl<'de> Deserialize<'de> for DegradeAction {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "report" => Ok(DegradeAction::Report),
            "skip" => Ok(DegradeAction::Skip),
            other => Err(de::Error::invalid_value(
                Unexpected::Str(other),
                &"\"report\" or \"skip\"",
            )),
        }
    }
}

/// degrade 策略按 plugin 条目声明，不做全局开关（spec §4 决策 3）。
/// 未列出的 kind 默认 `report`——只读工具宁可多报也不静默丢能力。
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DegradePolicy {
    #[serde(default)]
    pub hooks: DegradeAction,
    #[serde(default)]
    pub commands: DegradeAction,
    #[serde(default)]
    pub skills: DegradeAction,
    #[serde(default)]
    pub mcps: DegradeAction,
    #[serde(default)]
    pub agents: DegradeAction,
}

impl DegradePolicy {
    pub fn action_for(&self, kind: Kind) -> DegradeAction {
        match kind {
            Kind::Hook => self.hooks,
            Kind::Command => self.commands,
            Kind::Skill => self.skills,
            Kind::Mcp => self.mcps,
            Kind::Agent => self.agents,
            // plugin 自身装不下不是 degrade 问题，交给 supports() 判 Unsupported
            Kind::Plugin => DegradeAction::Report,
        }
    }
}

/// `targets` 接受 `all` 或显式列表（spec §4 决策 4：`all` 是默认值）。
impl<'de> Deserialize<'de> for Targets {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Keyword(String),
            List(Vec<String>),
        }
        match Repr::deserialize(d)? {
            Repr::Keyword(k) if k == "all" => Ok(Targets::All),
            Repr::Keyword(other) => Err(de::Error::invalid_value(
                Unexpected::Str(&other),
                &"\"all\" or a list of harness names",
            )),
            Repr::List(list) => {
                for h in &list {
                    if !KNOWN_HARNESSES.contains(&h.as_str()) {
                        return Err(de::Error::invalid_value(
                            Unexpected::Str(h),
                            &"one of claude-code, codex, kimi-code",
                        ));
                    }
                }
                Ok(Targets::Only(list))
            }
        }
    }
}

/// `env` 是唯一「内容按定义就是密钥材料」的字段，所以它的形状错误不能由 serde 报：
/// serde 自己的类型错误会把标量原样渲染成 `invalid type: string "<值>"`，等于把密钥
/// 打进终端与日志（`mcps.a.env: invalid type: string "sk-live-…"` 是修前的实际输出）。
/// 这里把整个节点先取成 `Value`——它的 `Deserialize` 不会失败，serde 因此永远不会去
/// 格式化用户写的值——再由本函数转成 `BTreeMap`，进入消息的只有下面的静态词。
/// 代价：路径从 `mcps.a.env:` 降级为 `mcps.a:`，所以 expected 文本里保留 `env` 一词。
fn env_refs<'de, D>(d: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    /// 节点种类 → 静态描述，绝不带上内容。
    fn kind_of(v: &Value) -> Unexpected<'static> {
        match v {
            Value::Null => Unexpected::Unit,
            Value::Bool(_) => Unexpected::Other("a boolean"),
            Value::Number(_) => Unexpected::Other("a number"),
            Value::String(_) => Unexpected::Other("a string"),
            Value::Sequence(_) => Unexpected::Other("a sequence"),
            Value::Mapping(_) => Unexpected::Other("a map"),
            Value::Tagged(t) => kind_of(&t.value),
        }
    }

    /// 标量照旧强转成字符串（serde 本来就把 `command: 424242` 读成 `"424242"`），
    /// 这样 Number/Bool/Null 仍然走到 `validate_env`，而不是变成形状错误。
    fn scalar_of(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Null => Some(String::new()),
            Value::Tagged(t) => scalar_of(&t.value),
            Value::Sequence(_) | Value::Mapping(_) => None,
        }
    }

    let node = Value::deserialize(d)?;
    let mut out = BTreeMap::new();
    match node {
        // `env:` 写成空节点仍然是「没有变量」，与既有 `default` 行为一致。
        Value::Null => {}
        Value::Mapping(map) => {
            for (k, v) in map {
                let Some(key) = scalar_of(&k) else {
                    return Err(de::Error::invalid_type(kind_of(&k), &"an env name string"));
                };
                let Some(rendered) = scalar_of(&v) else {
                    return Err(de::Error::invalid_type(kind_of(&v), &"an env value string"));
                };
                out.insert(key, rendered);
            }
        }
        other => {
            return Err(de::Error::invalid_type(
                kind_of(&other),
                &"a mapping of env name to ${VAR}",
            ));
        }
    }
    Ok(out)
}

impl Manifest {
    /// 解析 + 校验。校验错误聚合成单个 anyhow 错误，每条带 `skills.a` 式路径。
    pub fn parse(text: &str) -> anyhow::Result<Manifest> {
        let m: Manifest = serde_yaml_ng::from_str(text)
            .map_err(|e| anyhow::anyhow!("failed to parse env.yaml: {e}"))?;
        let errors = m.validate();
        if errors.is_empty() {
            Ok(m)
        } else {
            anyhow::bail!("env.yaml invalid:\n  - {}", errors.join("\n  - "));
        }
    }

    fn validate(&self) -> Vec<String> {
        let mut e = Vec::new();

        if self.version != SUPPORTED_VERSION {
            e.push(format!(
                "unsupported version {} (henv understands version {SUPPORTED_VERSION})",
                self.version
            ));
        }

        for name in self.harnesses.keys() {
            if !KNOWN_HARNESSES.contains(&name.as_str()) {
                e.push(format!(
                    "harnesses.{name}: unknown harness (known: {})",
                    KNOWN_HARNESSES.join(", ")
                ));
            }
        }

        for (name, s) in &self.skills {
            let at = format!("skills.{name}");
            match s.source {
                SkillSourceKind::AgentsDir => {
                    if s.repo.is_some() || s.git_ref.is_some() {
                        e.push(format!("{at}: source agents-dir takes no repo/ref"));
                    }
                }
                SkillSourceKind::Git => {
                    if s.repo.is_none() {
                        e.push(format!("{at}: source git requires `repo`"));
                    }
                    if s.git_ref.is_none() {
                        e.push(format!("{at}: source git requires `ref` (ref is the lock)"));
                    }
                }
                SkillSourceKind::Local => {
                    if s.path.is_none() {
                        e.push(format!("{at}: source local requires `path`"));
                    }
                }
            }
        }

        for (name, m) in &self.mcps {
            let at = format!("mcps.{name}");
            if m.command.trim().is_empty() {
                e.push(format!("{at}: `command` must not be empty"));
            }
            for (k, v) in &m.env {
                if let Err(msg) = secrets::validate_env(k, v) {
                    e.push(format!("{at}: {msg}"));
                }
            }
        }

        for (name, p) in &self.plugins {
            let at = format!("plugins.{name}");
            match p.source {
                PluginSourceKind::ClaudeMarketplace => {
                    if p.marketplace.is_none() {
                        e.push(format!(
                            "{at}: source claude-marketplace requires `marketplace`"
                        ));
                    }
                    if p.entry.is_none() {
                        e.push(format!("{at}: source claude-marketplace requires `entry`"));
                    }
                }
                PluginSourceKind::Github => {
                    if p.repo.is_none() {
                        e.push(format!("{at}: source github requires `repo`"));
                    }
                }
                PluginSourceKind::ZipUrl => {}
            }
        }

        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
version: 1

harnesses:
  claude-code: { enabled: true }
  codex:       { enabled: true }
  kimi-code:   { enabled: true }

skills:
  dingtalk-mail:
    source: agents-dir
  superpowers:
    source: git
    repo: obra/superpowers
    path: skills
    ref: v6.3.0
  my-draft:
    source: local
    path: ~/dev/skills/my-draft

mcps:
  context7:
    command: npx
    args: ["-y", "@upstash/context7-mcp"]
    env:
      CONTEXT7_API_KEY: ${CONTEXT7_API_KEY}
    targets: all

plugins:
  superpowers-bundle:
    source: claude-marketplace
    marketplace: obra/superpowers-marketplace
    entry: superpowers
    degrade:
      hooks: report
      commands: report
"#;

    #[test]
    fn parses_spec_example_in_full() {
        let m = Manifest::parse(FULL).expect("spec §4 example must parse");
        assert_eq!(m.version, 1);
        assert_eq!(m.harnesses.len(), 3);
        assert!(m.harnesses["codex"].enabled);

        assert_eq!(m.skills.len(), 3);
        assert!(matches!(
            m.skills["dingtalk-mail"].source,
            SkillSourceKind::AgentsDir
        ));
        assert!(matches!(
            m.skills["superpowers"].source,
            SkillSourceKind::Git
        ));
        assert_eq!(
            m.skills["superpowers"].repo.as_deref(),
            Some("obra/superpowers")
        );
        assert_eq!(m.skills["superpowers"].path.as_deref(), Some("skills"));
        assert_eq!(m.skills["superpowers"].git_ref.as_deref(), Some("v6.3.0"));
        assert_eq!(
            m.skills["my-draft"].path.as_deref(),
            Some("~/dev/skills/my-draft")
        );

        let c = &m.mcps["context7"];
        assert_eq!(c.command, "npx");
        assert_eq!(c.args, vec!["-y", "@upstash/context7-mcp"]);
        assert_eq!(c.env["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}");
        assert_eq!(c.targets, Targets::All);

        let p = &m.plugins["superpowers-bundle"];
        assert!(matches!(p.source, PluginSourceKind::ClaudeMarketplace));
        assert_eq!(p.entry.as_deref(), Some("superpowers"));
        assert_eq!(p.degrade.hooks, DegradeAction::Report);
    }

    #[test]
    fn defaults_are_applied_when_sections_omitted() {
        let m = Manifest::parse("version: 1\n").expect("minimal manifest");
        assert!(m.harnesses.is_empty());
        assert!(m.skills.is_empty());
        assert!(m.mcps.is_empty());
        assert!(m.plugins.is_empty());
    }

    #[test]
    fn harness_entry_defaults_to_enabled() {
        let m = Manifest::parse("version: 1\nharnesses:\n  codex: {}\n").unwrap();
        assert!(m.harnesses["codex"].enabled);
    }

    #[test]
    fn degrade_defaults_to_report_for_unlisted_kinds() {
        let m = Manifest::parse(FULL).unwrap();
        let d = &m.plugins["superpowers-bundle"].degrade;
        assert_eq!(d.hooks, DegradeAction::Report);
        assert_eq!(
            d.skills,
            DegradeAction::Report,
            "unlisted kind defaults to report"
        );
        assert_eq!(d.action_for(Kind::Hook), DegradeAction::Report);
        assert_eq!(d.action_for(Kind::Mcp), DegradeAction::Report);
    }

    #[test]
    fn degrade_skip_is_honoured() {
        let yaml = "version: 1\nplugins:\n  p:\n    source: github\n    repo: a/b\n    degrade:\n      hooks: skip\n";
        let m = Manifest::parse(yaml).unwrap();
        assert_eq!(m.plugins["p"].degrade.hooks, DegradeAction::Skip);
        assert_eq!(
            m.plugins["p"].degrade.action_for(Kind::Hook),
            DegradeAction::Skip
        );
    }

    #[test]
    fn targets_accepts_all_or_explicit_list() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: all\n  b:\n    command: npx\n    targets: [claude-code, codex]\n";
        let m = Manifest::parse(yaml).unwrap();
        assert_eq!(m.mcps["a"].targets, Targets::All);
        assert_eq!(
            m.mcps["b"].targets,
            Targets::Only(vec!["claude-code".into(), "codex".into()])
        );
        assert!(m.mcps["b"].targets.includes("codex"));
        assert!(!m.mcps["b"].targets.includes("kimi-code"));
    }

    #[test]
    fn targets_rejects_unknown_harness_in_list() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: [cursor]\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("cursor"), "{err}");
    }

    #[test]
    fn targets_rejects_bare_keyword_that_is_not_all() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: none\n";
        assert!(Manifest::parse(yaml).is_err());
    }

    #[test]
    fn rejects_literal_secret_in_env_and_does_not_echo_it() {
        let yaml = "version: 1\nmcps:\n  context7:\n    command: npx\n    env:\n      CONTEXT7_API_KEY: sk-live-abc123\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("CONTEXT7_API_KEY"), "{err}");
        assert!(
            !err.contains("sk-live-abc123"),
            "must not echo secret: {err}"
        );
    }

    /// 修前的实际输出：`env: sk-live-abc123` → `mcps.a.env: invalid type: string
    /// "sk-live-abc123", expected a map`，64 字符也完整打印。这些是「该给映射却写了
    /// 标量」的手滑形态，所以断言的是**值不得出现**。`env` 一词必须由 expected 文本
    /// 带出来（路径已降级为 `mcps.a:`，没有它就没人知道是哪一写出错）——注意不能只
    /// 断言 `contains("env")`，外层的 `env.yaml` 会让它恒真。
    #[test]
    fn mistyped_env_node_does_not_echo_the_value() {
        for node in [
            "sk-live-abc123",
            "\"CONTEXT7_API_KEY=sk-live-abc123\"",
            "sk-live-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "424242",
        ] {
            let yaml = format!("version: 1\nmcps:\n  a:\n    command: npx\n    env: {node}\n");
            let err = Manifest::parse(&yaml).expect_err("{node} is not a mapping");
            let err = err.to_string();
            assert!(!err.contains("sk-live-abc123"), "must not echo: {err}");
            assert!(!err.contains("424242"), "must not echo: {err}");
            assert!(
                err.contains("expected a mapping of env"),
                "message must name the env node: {err}"
            );
        }
    }

    /// 键/值一侧的形状错误同样不能带出内容：非序列的键、序列或映射的值。
    #[test]
    fn mistyped_env_key_or_value_does_not_echo_the_value() {
        for (node, expected) in [
            ("AAA: [sk-live-abc123]", "expected an env value string"),
            ("AAA: {B: sk-live-abc123}", "expected an env value string"),
            ("[1, 2]: sk-live-abc123", "expected an env name string"),
        ] {
            let yaml =
                format!("version: 1\nmcps:\n  a:\n    command: npx\n    env:\n      {node}\n");
            let err = Manifest::parse(&yaml).expect_err("{node} is not a name/value pair");
            let err = err.to_string();
            assert!(!err.contains("sk-live-abc123"), "must not echo: {err}");
            assert!(err.contains(expected), "shape of {node}: {err}");
        }
    }

    /// 强转语义不能丢：serde 把 `command: 424242` 读成 `"424242"`，`env` 的标量值（含
    /// 带标签节点）同样要先变成字符串再交给 `validate_env`。判据是「没有出现形状错误」——
    /// 形状错误说明值根本没走到门禁，那才是这里要避免的语义漂移。
    #[test]
    fn env_scalar_value_still_reaches_the_secret_gate() {
        for value in ["424242", "42.5", "true", "", "!!str sk-live-abc123"] {
            let yaml = format!(
                "version: 1\nmcps:\n  a:\n    command: npx\n    env:\n      AAA: {value}\n"
            );
            let err = Manifest::parse(&yaml).expect_err("a non-reference value must be gated");
            let err = err.to_string();
            assert!(err.contains("AAA"), "gate must name the key: {err}");
            assert!(
                !err.contains("invalid type"),
                "value {value:?} must reach the gate, not a shape error: {err}"
            );
            assert!(!err.contains("424242"), "must not echo: {err}");
            assert!(!err.contains("sk-live-abc123"), "must not echo: {err}");
        }
    }

    #[test]
    fn rejects_unknown_version() {
        let err = Manifest::parse("version: 2\n").unwrap_err().to_string();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn rejects_missing_version() {
        assert!(Manifest::parse("skills: {}\n").is_err());
    }

    #[test]
    fn rejects_unknown_top_level_and_entry_fields() {
        assert!(Manifest::parse("version: 1\nskillz: {}\n").is_err());
        assert!(
            Manifest::parse("version: 1\nskills:\n  a:\n    source: local\n    pth: /x\n").is_err()
        );
    }

    #[test]
    fn rejects_git_skill_without_repo_or_ref() {
        let err = Manifest::parse("version: 1\nskills:\n  a:\n    source: git\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("repo"), "{err}");
        assert!(err.contains("ref"), "{err}");
    }

    #[test]
    fn rejects_local_skill_without_path() {
        let err = Manifest::parse("version: 1\nskills:\n  a:\n    source: local\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("path"), "{err}");
    }

    #[test]
    fn rejects_agents_dir_skill_with_repo() {
        let err =
            Manifest::parse("version: 1\nskills:\n  a:\n    source: agents-dir\n    repo: x/y\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("agents-dir"), "{err}");
    }

    #[test]
    fn rejects_claude_marketplace_plugin_without_marketplace_or_entry() {
        let yaml = "version: 1\nplugins:\n  a:\n    source: claude-marketplace\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("marketplace"), "{err}");
        assert!(err.contains("entry"), "{err}");
    }

    #[test]
    fn rejects_github_plugin_without_repo() {
        let err = Manifest::parse("version: 1\nplugins:\n  a:\n    source: github\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("repo"), "{err}");
    }

    #[test]
    fn rejects_unknown_harness_name() {
        let err = Manifest::parse("version: 1\nharnesses:\n  cursor: { enabled: true }\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("cursor"), "{err}");
    }

    #[test]
    fn aggregates_all_errors_in_one_message() {
        let yaml = "version: 1\nskills:\n  a:\n    source: git\n  b:\n    source: local\nmcps:\n  c:\n    command: npx\n    env:\n      K: literal\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("skills.a"), "{err}");
        assert!(err.contains("skills.b"), "{err}");
        assert!(err.contains("mcps.c"), "{err}");
    }

    #[test]
    fn yaml_syntax_error_surfaces_with_context() {
        let err = Manifest::parse("version: 1\nskills: [\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("env.yaml"), "{err}");
    }
}
