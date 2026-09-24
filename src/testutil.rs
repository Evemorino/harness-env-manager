//! 测试专用：在 TempDir 里重建假 HOME。绝不触碰真实 ~。
#![allow(dead_code)]

use crate::adapters::HarnessAdapter;
use crate::capability::{CapId, Capability, Kind, Origin, Spec, Support, Targets};
use crate::home::Home;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct TempHome {
    // 必须保活：drop 即删除整棵目录树
    _dir: TempDir,
    pub home: Home,
    root: PathBuf,
}

impl TempHome {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("create tempdir");
        let home = Home::new(dir.path());
        // 用 canonicalize 后的 root 构造路径，才能与 adapter 输出（同样 canonicalize 过）直接比较
        let root = home.root().to_path_buf();
        Self {
            _dir: dir,
            home,
            root,
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// 写文件，自动创建父目录。
    pub fn write(&self, rel: &str, body: &str) -> &Self {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::fs::write(&p, body).expect("write file");
        self
    }

    pub fn mkdir(&self, rel: &str) -> &Self {
        std::fs::create_dir_all(self.path(rel)).expect("mkdir");
        self
    }

    /// 创建符号链接（Unix only；本项目只跑 macOS/Linux）。
    /// `target` 原样写入，相对路径由调用方负责正确性。
    pub fn symlink(&self, link: &str, target: &str) -> &Self {
        let l = self.path(link);
        if let Some(parent) = l.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::os::unix::fs::symlink(Path::new(target), &l).expect("symlink");
        self
    }
}

/// 测试替身：`read_state` 返回预置能力，`supports` 按 kind 查表。
///
/// Task 13/14 的 diff 与 golden 测试靠它喂进可控能力，不依赖任何真实配置树——
/// 也就不会去读 `~/.claude` 这类装着凭据的目录。
pub struct FakeAdapter {
    pub adapter_name: String,
    pub detected: bool,
    pub caps: Vec<Capability>,
    /// kind -> 拒绝理由。未列出的 kind 判 Native。
    pub unsupported: BTreeMap<Kind, String>,
}

impl FakeAdapter {
    pub fn native(name: &str) -> Self {
        Self {
            adapter_name: name.into(),
            detected: true,
            caps: Vec::new(),
            unsupported: BTreeMap::new(),
        }
    }

    pub fn with_caps(mut self, caps: Vec<Capability>) -> Self {
        self.caps = caps;
        self
    }

    pub fn with_detected(mut self, detected: bool) -> Self {
        self.detected = detected;
        self
    }

    pub fn refusing(mut self, kind: Kind, reason: &str) -> Self {
        self.unsupported.insert(kind, reason.into());
        self
    }

    /// 造一条「已安装」能力。
    pub fn cap(kind: Kind, name: &str, spec: Spec) -> Capability {
        Capability {
            id: CapId::new(kind, name),
            origin: Origin::Adopted {
                harness: String::new(),
            },
            spec,
            targets: Targets::All,
        }
    }

    /// 造一条由 plugin bundle 携带的 skill。
    pub fn bundled_skill(name: &str, plugin: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Skill, name),
            origin: Origin::Plugin {
                name: plugin.into(),
            },
            spec: Spec::Skill {
                source: crate::capability::SkillSource::PluginBundle {
                    plugin: plugin.into(),
                },
                path: None,
            },
            targets: Targets::All,
        }
    }

    /// 造一条 MCP 能力。
    pub fn mcp(name: &str, command: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Mcp, name),
            origin: Origin::Manifest,
            spec: Spec::Mcp {
                command: command.into(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
            targets: Targets::All,
        }
    }

    /// 造一条 plugin 能力。
    pub fn plugin(entry: &str, version: Option<&str>, sha: Option<&str>) -> Capability {
        Capability {
            id: CapId::new(Kind::Plugin, entry),
            origin: Origin::Manifest,
            spec: Spec::Plugin {
                marketplace: None,
                entry: entry.into(),
                version: version.map(String::from),
                sha: sha.map(String::from),
                install_path: None,
                enabled: true,
            },
            targets: Targets::All,
        }
    }
}

impl HarnessAdapter for FakeAdapter {
    fn name(&self) -> &str {
        &self.adapter_name
    }
    fn detect(&self) -> bool {
        self.detected
    }
    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(self.caps.clone())
    }
    fn supports(&self, cap: &Capability) -> Support {
        match self.unsupported.get(&cap.id.kind) {
            Some(reason) => Support::Unsupported {
                reason: reason.clone(),
            },
            None => Support::Native,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_files_and_symlinks_under_temp_root() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/real-one")
            .write(".agents/skills/real-one/SKILL.md", "# hi")
            .symlink(".claude/skills/real-one", "../../.agents/skills/real-one");

        assert!(t.path(".agents/skills/real-one/SKILL.md").exists());
        let link = t.path(".claude/skills/real-one");
        assert!(link.exists(), "symlink resolves through to the real dir");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../../.agents/skills/real-one")
        );
        assert!(t.home.root().is_absolute());
    }

    /// FakeAdapter 是 Task 13/14 唯一的 fixture 入口：它自己行为错了，那些 diff
    /// 断言就是在替一个不存在的双侧相符/拒绝说话。本 Task 里它**没有生产调用方**，
    /// 所以只有这条测试能证明它兑现了 `HarnessAdapter` 的语义。
    #[test]
    fn fake_adapter_answers_from_its_own_fields() {
        let mcp = FakeAdapter::mcp("context7", "npx");
        let plugin = FakeAdapter::plugin("superpowers", Some("6.4.1"), Some("abc"));
        let caps = vec![mcp.clone(), plugin.clone()];
        let fake = FakeAdapter::native("claude-code")
            .with_caps(caps.clone())
            .refusing(Kind::Plugin, "no plugins");

        assert_eq!(fake.name(), "claude-code");
        assert!(fake.detect(), "native() 默认已装");
        // read_state 原样返回预置能力，且每次调用给的是独立副本
        assert_eq!(fake.read_state().unwrap(), caps);
        // 查表按 kind：被拒的 plugin 带出理由，未列出的 mcp 判 Native
        assert_eq!(
            fake.supports(&plugin),
            Support::Unsupported {
                reason: "no plugins".into()
            }
        );
        assert_eq!(fake.supports(&mcp), Support::Native);
        assert_eq!(
            FakeAdapter::native("codex").supports(&plugin),
            Support::Native,
            "空表必须一律 Native"
        );
        // with_detected 是造出「本机没装这家」的唯一入口（Task 13 的跳过分支要用）
        assert!(
            !FakeAdapter::native("codex").with_detected(false).detect(),
            "with_detected(false) 必须盖过 native() 的 true"
        );
    }

    /// 四个 fixture builder 的产物形状也是契约：Task 13 的 diff 按 kind / origin /
    /// source 分派，一个标错 kind 的 builder 会把「plugin 携带的 skill」当成散装
    /// skill 来比，而 golden 依然自洽——只是钉住了一个错误的结论。
    #[test]
    fn fake_adapter_fixture_builders_tag_kind_origin_and_source() {
        let bare = FakeAdapter::cap(
            Kind::Hook,
            "pre",
            Spec::Hook {
                event: "PreToolUse".into(),
                command: "echo".into(),
            },
        );
        assert_eq!(bare.id, CapId::new(Kind::Hook, "pre"));
        assert_eq!(
            bare.origin,
            Origin::Adopted {
                harness: String::new()
            }
        );
        assert!(bare.targets.includes("kimi-code"));

        let bundled = FakeAdapter::bundled_skill("using-superpowers", "superpowers");
        assert_eq!(bundled.id, CapId::new(Kind::Skill, "using-superpowers"));
        assert_eq!(
            bundled.origin,
            Origin::Plugin {
                name: "superpowers".into()
            }
        );
        assert_eq!(
            bundled.spec,
            Spec::Skill {
                source: crate::capability::SkillSource::PluginBundle {
                    plugin: "superpowers".into()
                },
                path: None,
            }
        );

        let mcp = FakeAdapter::mcp("context7", "npx");
        assert_eq!(mcp.id, CapId::new(Kind::Mcp, "context7"));
        assert_eq!(mcp.origin, Origin::Manifest);

        let plugin = FakeAdapter::plugin("superpowers", Some("6.4.1"), Some("abc"));
        assert_eq!(plugin.id, CapId::new(Kind::Plugin, "superpowers"));
        assert_eq!(
            plugin.spec,
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("6.4.1".into()),
                sha: Some("abc".into()),
                install_path: None,
                enabled: true,
            },
            "version/sha 必须原样落到各自字段，串了位就等于给 diff 喂假 locator"
        );
    }
}
