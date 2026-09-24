use crate::adapters::{CLAUDE_CODE, HarnessAdapter};
use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets};
use crate::home::Home;
use crate::skills_dir::{SkillStore, plugin_from_path, try_read_skill_entries};

pub struct ClaudeCode {
    home: Home,
}

impl ClaudeCode {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    /// 扫 `~/.claude/skills`。**扫描失败一律上抛**：把「读不出这个目录」呈现成
    /// 「这家一个 skill 都没装」是只读对账工具最坏的漏报方向（计划里「一个 harness
    /// 扫描失败 = 整次 diff 失败」那条契约对 Task 9 的硬要求）。
    /// 只有「目录不存在」才是空的，那由 `try_read_skill_entries` 自己判。
    fn read_skills(&self) -> anyhow::Result<Vec<Capability>> {
        let mut out = Vec::new();
        for e in try_read_skill_entries(&self.home.claude_skills(), &self.home)? {
            let source = match e.store {
                SkillStore::Shared => SkillSource::AgentsDir,
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
                    harness: CLAUDE_CODE.into(),
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

impl HarnessAdapter for ClaudeCode {
    fn name(&self) -> &str {
        CLAUDE_CODE
    }

    fn detect(&self) -> bool {
        self.home.claude_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        let caps = self.read_skills()?;
        Ok(caps)
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
    fn reads_skill_symlink_from_shared_store() {
        let t = TempHome::new();
        t.mkdir(".claude")
            .mkdir(".agents/skills/dingtalk-mail")
            .symlink(
                ".claude/skills/dingtalk-mail",
                "../../.agents/skills/dingtalk-mail",
            );

        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "dingtalk-mail").expect("skill present");
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".agents/skills/dingtalk-mail"))
            }
        );
        assert_eq!(
            s.origin,
            Origin::Adopted {
                harness: "claude-code".into()
            }
        );
        assert_eq!(s.targets, Targets::All);
    }

    #[test]
    fn reads_real_dir_skill_as_harness_local() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/trim-cot-leakage");
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "trim-cot-leakage").unwrap();
        match &s.spec {
            Spec::Skill {
                source: SkillSource::Local { path },
                ..
            } => {
                assert!(
                    path.ends_with(".claude/skills/trim-cot-leakage"),
                    "{path:?}"
                );
            }
            other => panic!("expected Local, got {other:?}"),
        }
    }

    #[test]
    fn broken_skill_link_still_surfaces() {
        let t = TempHome::new();
        t.mkdir(".claude/skills")
            .symlink(".claude/skills/gone", "../../.agents/skills/nope");
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "gone").expect("broken link must be reported");
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::Unknown,
                path: None
            }
        );
    }

    /// `SkillStore::PluginCache` 那条 arm 派单的 14 条里没有看守（三条 skill 测试只覆盖
    /// Shared / HarnessLocal / Other），而它是 Task 13 判 drift 豁免的输入：这一格判错，
    /// plugin 自带的 skill 会被报成「你手工装的散装 skill」→ 假 drift。
    #[test]
    fn reads_skill_inside_plugin_cache_as_plugin_bundle() {
        let t = TempHome::new();
        let cache = ".claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming";
        t.mkdir(cache);
        t.symlink(
            ".claude/skills/brainstorming",
            &t.path(cache).to_string_lossy(),
        );

        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "brainstorming").expect("plugin-cache skill present");
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::PluginBundle {
                    plugin: "superpowers".into()
                },
                path: Some(t.path(cache)),
            }
        );
    }

    /// 失败隔离契约在 adapter 这一侧的另一半：skills 目录**读不出**（这里 `.claude/skills`
    /// 是个普通文件 → `read_dir` 报 ENOTDIR）必须是 `Err`，不是空表。
    /// 变异实测：把 `read_skills` 里的 `?` 换成 `.unwrap_or_default()`（= 误用 lossy 的
    /// `read_skill_entries`）时，派单那 14 条一条都不会红，只有这条会。
    #[test]
    fn an_unreadable_skills_dir_is_an_error_not_an_empty_scan() {
        let t = TempHome::new();
        t.mkdir(".claude")
            .write(".claude/skills", "not a directory");
        let err = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect_err("读不出的 skills 目录必须上抛，不能吞成空表");
        let msg = err.to_string();
        assert!(msg.starts_with("failed to read skills dir "), "{msg}");
    }
}
