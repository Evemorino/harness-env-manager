use crate::adapters::{CODEX, HarnessAdapter};
use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets};
use crate::home::Home;
use crate::skills_dir::{SkillStore, plugin_from_path, try_read_skill_entries};

pub struct Codex {
    home: Home,
}

impl Codex {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    /// 扫 `~/.codex/skills`。**扫描失败一律上抛**：把「读不出这个目录」呈现成
    /// 「这家一个 skill 都没装」是只读对账工具最坏的漏报方向（失败隔离契约）。
    /// 只有「目录不存在」才是空的，那由 `try_read_skill_entries` 自己判。
    ///
    /// 签名与 claude 的同名方法一致返回 `Result`，不是计划示例里的 `Vec`：
    /// `try_read_skill_entries` 本身可失败，示例那句 `for e in try_read_skill_entries(..)`
    /// 连编译都过不去（`Result` 不是 `IntoIterator`），而用 lossy 的 `read_skill_entries`
    /// 就把「读不了」重新吞成空表了。
    fn read_skills(&self) -> anyhow::Result<Vec<Capability>> {
        let mut out = Vec::new();
        for e in try_read_skill_entries(&self.home.codex_skills(), &self.home)? {
            let source = match e.store {
                SkillStore::Shared => SkillSource::AgentsDir,
                // 实测（spec §2.1 第 4 条）：codex 的 skills 是绝对 symlink，直接指向
                // `~/.claude/plugins/cache/<mp>/<name>/<sha>/skills/<skill>`。
                SkillStore::PluginCache => {
                    let resolved = e.resolved.clone().unwrap_or_default();
                    match plugin_from_path(&resolved) {
                        Some(plugin) => SkillSource::PluginBundle { plugin },
                        None => SkillSource::Unknown,
                    }
                }
                SkillStore::HarnessLocal => SkillSource::Local {
                    path: e.resolved.clone().unwrap_or_else(|| e.path.clone()),
                },
                // 断链或指向库外：来源不可判定，交给 doctor 报告
                SkillStore::Other => SkillSource::Unknown,
            };
            out.push(Capability {
                id: CapId::new(Kind::Skill, e.name),
                origin: Origin::Adopted {
                    harness: CODEX.into(),
                },
                spec: Spec::Skill {
                    source,
                    path: e.resolved,
                },
                targets: Targets::All,
            });
        }
        Ok(out)
    }
}

impl HarnessAdapter for Codex {
    fn name(&self) -> &str {
        CODEX
    }

    fn detect(&self) -> bool {
        self.home.codex_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        self.read_skills()
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec, Targets};
    use crate::testutil::TempHome;

    fn find<'a>(caps: &'a [Capability], kind: Kind, name: &str) -> Option<&'a Capability> {
        caps.iter().find(|c| c.id.kind == kind && c.id.name == name)
    }

    #[test]
    fn reads_skills_that_symlink_into_claude_plugin_cache() {
        // 实测 codex 直接链进 claude 插件缓存，完全绕过 ~/.agents
        let t = TempHome::new();
        let rel = ".claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming";
        t.mkdir(rel);
        let cached = t.path(rel);
        t.symlink(".codex/skills/brainstorming", &cached.to_string_lossy());

        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "brainstorming").unwrap();
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::PluginBundle {
                    plugin: "superpowers".into()
                },
                path: Some(cached)
            }
        );
        assert_eq!(
            s.origin,
            Origin::Adopted {
                harness: "codex".into()
            }
        );
        assert_eq!(
            s.targets,
            Targets::All,
            "actual 侧读出来的能力不该自带 targets 限制"
        );
    }

    #[test]
    fn reads_skill_symlinked_into_shared_agents_store() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/dingtalk-mail");
        let target = t.path(".agents/skills/dingtalk-mail");
        t.symlink(".codex/skills/dingtalk-mail", &target.to_string_lossy());

        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "dingtalk-mail").unwrap();
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(target)
            }
        );
        assert_eq!(
            s.origin,
            Origin::Adopted {
                harness: "codex".into()
            }
        );
    }

    #[test]
    fn reads_real_dir_skill_as_local() {
        let t = TempHome::new();
        t.mkdir(".codex/skills/my-own");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Skill, "my-own").unwrap().spec {
            Spec::Skill {
                source: SkillSource::Local { path },
                ..
            } => {
                assert!(path.ends_with(".codex/skills/my-own"), "{path:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 断链不是「没装」，也不该被吞掉：它得作为 `Unknown` 出现在报告里，交给 doctor。
    #[test]
    fn broken_skill_link_still_surfaces() {
        let t = TempHome::new();
        t.mkdir(".codex/skills")
            .symlink(".codex/skills/gone", "../../.agents/skills/nope");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "gone").expect("broken link must be reported");
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::Unknown,
                path: None
            }
        );
    }

    /// 失败隔离契约在 codex 这一侧的另一半：skills 目录**读不出**（这里 `.codex/skills`
    /// 是个普通文件 → `read_dir` 报 ENOTDIR）必须是 `Err`，不是空表。
    #[test]
    fn an_unreadable_skills_dir_is_an_error_not_an_empty_scan() {
        let t = TempHome::new();
        t.mkdir(".codex").write(".codex/skills", "not a directory");
        let err = Codex::new(t.home.clone())
            .read_state()
            .expect_err("读不出的 skills 目录必须上抛，不能吞成空表");
        let msg = err.to_string();
        assert!(msg.starts_with("failed to read skills dir "), "{msg}");
    }
}
