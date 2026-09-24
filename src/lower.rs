//! 三类清单条目 → 单一 Capability 流（spec §4 决策 1「一个出口」）。
//!
//! plugin 在此**不**被拆解：spec §2.1 第 2 条确认三家 harness 都有 plugin 机制且 bundle 自带
//! per-harness manifest，v1 以对账为主。拆解是 Plan B 在目标 harness 缺 manifest 时的降级路径。

use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, normalize_locator};
use crate::manifest::{DegradeAction, DegradePolicy, Manifest, PluginSourceKind, SkillSourceKind};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 把 `~/x` 展开为绝对路径。只处理前导 `~/` 与裸 `~`，不做 shell 语义。
fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_fallback();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_fallback().join(rest);
    }
    PathBuf::from(raw)
}

/// 只读 `HOME` 环境变量，不碰文件系统——`lower` 全程无 IO，所以这里不能走
/// `Home::resolve()`（它对 root 做 `canonicalize`，那是文件系统调用）。
fn home_fallback() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn lower(m: &Manifest) -> Vec<Capability> {
    let mut out: Vec<Capability> = Vec::new();

    for (name, s) in &m.skills {
        let source = match s.source {
            SkillSourceKind::AgentsDir => SkillSource::AgentsDir,
            SkillSourceKind::Git => SkillSource::Git {
                repo: s.repo.clone().unwrap_or_default(),
                path: s.path.clone(),
                // `manifest` 只保证 git 源带 `ref`，不保证它不是哨兵：`ref: unknown`
                // 会过关。这里过一遍 normalize_locator，否则两个各自记着 `unknown`
                // 的构建会被 compare_versions 判成 Same（假同步）。
                git_ref: normalize_locator(s.git_ref.as_deref()).unwrap_or_default(),
            },
            SkillSourceKind::Local => SkillSource::Local {
                path: expand_tilde(s.path.as_deref().unwrap_or_default()),
            },
        };
        out.push(Capability {
            id: CapId::new(Kind::Skill, name.clone()),
            origin: Origin::Manifest,
            spec: Spec::Skill { source, path: None },
            targets: s.targets.clone(),
        });
    }

    for (name, e) in &m.mcps {
        out.push(Capability {
            id: CapId::new(Kind::Mcp, name.clone()),
            origin: Origin::Manifest,
            spec: Spec::Mcp {
                command: e.command.clone(),
                args: e.args.clone(),
                // env 值已由 `secrets::validate_env` 保证是裸 `${VAR}` 引用：
                // 原样搬运，绝不展开、绝不重新引号、绝不剥掉花括号。
                env: e.env.clone(),
            },
            targets: e.targets.clone(),
        });
    }

    for (key, p) in &m.plugins {
        // 身份用 entry 名（spec §2.1 第 3 条），缺省回退到清单 key
        let entry = p.entry.clone().unwrap_or_else(|| key.clone());
        let marketplace = match p.source {
            PluginSourceKind::ClaudeMarketplace => p.marketplace.clone(),
            PluginSourceKind::Github => p.repo.clone(),
            PluginSourceKind::ZipUrl => None,
        };
        out.push(Capability {
            id: CapId::new(Kind::Plugin, entry.clone()),
            origin: Origin::Manifest,
            spec: Spec::Plugin {
                marketplace,
                entry,
                // 期望侧无 SHA：manifest 只有 ref（git 源）。ref 落在 version 位，
                // compare_versions 走 version 兜底；实际侧有 SHA 时判 Unknown → in-sync。
                // 哨兵（`unknown`/空白）在此归成 None → 落到 Unknown，而不是报假 drift。
                version: normalize_locator(p.git_ref.as_deref()),
                sha: None,
                install_path: None,
                enabled: true,
            },
            targets: p.targets.clone(),
        });
    }

    out.sort_by_key(|c| c.id.label());
    out
}

/// plugin 名 → degrade 策略。非 plugin 来源的能力一律 Report。
#[derive(Debug, Clone, Default)]
pub struct DegradeTable {
    by_plugin: BTreeMap<String, DegradePolicy>,
}

impl DegradeTable {
    pub fn new(m: &Manifest) -> Self {
        let mut by_plugin = BTreeMap::new();
        for (key, p) in &m.plugins {
            let entry = p.entry.clone().unwrap_or_else(|| key.clone());
            by_plugin.insert(entry, p.degrade.clone());
        }
        Self { by_plugin }
    }

    pub fn action(&self, cap: &Capability) -> DegradeAction {
        let policy = match &cap.origin {
            Origin::Plugin { name } => self.by_plugin.get(name),
            _ => None,
        };
        match policy {
            Some(p) => p.action_for(cap.id.kind),
            None => DegradeAction::Report,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Targets};
    use crate::manifest::{DegradeAction, Manifest};

    fn m(yaml: &str) -> Manifest {
        Manifest::parse(yaml).expect("test yaml must parse")
    }

    #[test]
    fn empty_manifest_lowers_to_nothing() {
        assert!(lower(&m("version: 1\n")).is_empty());
    }

    #[test]
    fn lowers_all_three_entry_kinds() {
        let yaml = r#"
version: 1
skills:
  dingtalk-mail: { source: agents-dir }
mcps:
  context7: { command: npx, args: ["-y", "@upstash/context7-mcp"] }
plugins:
  superpowers-bundle: { source: claude-marketplace, marketplace: obra/spm, entry: superpowers }
"#;
        let caps = lower(&m(yaml));
        assert_eq!(caps.len(), 3);

        let skill = caps.iter().find(|c| c.id.kind == Kind::Skill).unwrap();
        assert_eq!(skill.id.name, "dingtalk-mail");
        assert_eq!(
            skill.spec,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: None
            }
        );
        assert_eq!(skill.origin, Origin::Manifest);

        let mcp = caps.iter().find(|c| c.id.kind == Kind::Mcp).unwrap();
        assert_eq!(mcp.id.name, "context7");
        match &mcp.spec {
            Spec::Mcp { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &vec!["-y".to_string(), "@upstash/context7-mcp".to_string()]
                );
                assert!(env.is_empty());
            }
            other => panic!("expected Mcp, got {other:?}"),
        }

        let plugin = caps.iter().find(|c| c.id.kind == Kind::Plugin).unwrap();
        assert_eq!(
            plugin.id.name, "superpowers",
            "identity is the entry name, not the manifest key"
        );
    }

    #[test]
    fn plugin_entry_defaults_to_manifest_key() {
        let yaml = "version: 1\nplugins:\n  superpowers:\n    source: github\n    repo: obra/superpowers\n";
        let caps = lower(&m(yaml));
        assert_eq!(caps[0].id.name, "superpowers");
        match &caps[0].spec {
            Spec::Plugin {
                entry,
                marketplace,
                sha,
                version,
                enabled,
                ..
            } => {
                assert_eq!(entry, "superpowers");
                // github 源的 repo 落在 marketplace 位，作为来源标识
                assert_eq!(marketplace.as_deref(), Some("obra/superpowers"));
                assert_eq!(*sha, None);
                assert_eq!(*version, None);
                assert!(*enabled, "desired plugins are enabled by definition");
            }
            other => panic!("expected Plugin, got {other:?}"),
        }
    }

    #[test]
    fn git_skill_ref_becomes_the_lock() {
        let yaml = "version: 1\nskills:\n  superpowers:\n    source: git\n    repo: obra/superpowers\n    path: skills\n    ref: v6.3.0\n";
        let caps = lower(&m(yaml));
        assert_eq!(
            caps[0].spec,
            Spec::Skill {
                source: SkillSource::Git {
                    repo: "obra/superpowers".into(),
                    path: Some("skills".into()),
                    git_ref: "v6.3.0".into(),
                },
                path: None,
            }
        );
    }

    #[test]
    fn local_skill_path_is_expanded() {
        let yaml = "version: 1\nskills:\n  d:\n    source: local\n    path: ~/dev/skills/d\n";
        let caps = lower(&m(yaml));
        match &caps[0].spec {
            Spec::Skill {
                source: SkillSource::Local { path },
                ..
            } => {
                assert!(
                    !path.to_string_lossy().contains('~'),
                    "~ must be expanded: {path:?}"
                );
                assert!(path.is_absolute(), "{path:?}");
                assert!(path.ends_with("dev/skills/d"), "{path:?}");
            }
            other => panic!("expected Local skill, got {other:?}"),
        }
    }

    #[test]
    fn targets_propagate_onto_every_capability() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: [codex]\n  b:\n    command: npx\n";
        let caps = lower(&m(yaml));
        let a = caps.iter().find(|c| c.id.name == "a").unwrap();
        let b = caps.iter().find(|c| c.id.name == "b").unwrap();
        assert_eq!(a.targets, Targets::Only(vec!["codex".into()]));
        assert_eq!(b.targets, Targets::All);
    }

    /// 上面那条只经过 mcps 一段：把 skills/plugins 两处的 `targets` 换成
    /// `Targets::default()` 也照样全绿（实测该变异存活）。三类条目的传播得各有断言。
    #[test]
    fn targets_propagate_from_skills_and_plugins_too() {
        let yaml = "version: 1\nskills:\n  s: { source: agents-dir, targets: [kimi-code] }\nplugins:\n  p: { source: github, repo: x/y, targets: [codex] }\nmcps:\n  m: { command: npx }\n";
        let caps = lower(&m(yaml));
        let skill = caps.iter().find(|c| c.id.kind == Kind::Skill).unwrap();
        let plugin = caps.iter().find(|c| c.id.kind == Kind::Plugin).unwrap();
        let mcp = caps.iter().find(|c| c.id.kind == Kind::Mcp).unwrap();
        assert_eq!(
            skill.targets,
            Targets::Only(vec!["kimi-code".into()]),
            "skill targets 不传播就等于对三家都投"
        );
        assert_eq!(plugin.targets, Targets::Only(vec!["codex".into()]));
        assert_eq!(mcp.targets, Targets::All, "未写 targets 的条目仍是 all");
        assert!(
            !skill.targets.includes("codex"),
            "Only 列表不能把没点名的 harness 算进去"
        );
    }

    #[test]
    fn mcp_env_stays_as_unexpanded_reference() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    env:\n      K: ${K}\n";
        let caps = lower(&m(yaml));
        match &caps[0].spec {
            Spec::Mcp { env, .. } => {
                assert_eq!(env["K"], "${K}", "reference must not be expanded")
            }
            other => panic!("expected Mcp, got {other:?}"),
        }
    }

    #[test]
    fn output_is_deterministically_ordered() {
        let yaml = "version: 1\nmcps:\n  z: { command: a }\n  a: { command: a }\nskills:\n  y: { source: agents-dir }\n";
        let caps = lower(&m(yaml));
        let names: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "lowering must be deterministic: {names:?}");
    }

    #[test]
    fn degrade_table_routes_by_plugin_and_kind() {
        let yaml = "version: 1\nplugins:\n  a:\n    source: github\n    repo: x/y\n    entry: plug\n    degrade:\n      hooks: skip\n      commands: report\nmcps:\n  m:\n    command: npx\n";
        let man = m(yaml);
        let caps = lower(&man);
        let table = DegradeTable::new(&man);

        let hook = Capability {
            id: CapId::new(Kind::Hook, "plug:pre-commit"),
            origin: Origin::Plugin {
                name: "plug".into(),
            },
            spec: Spec::Hook {
                event: "SessionStart".into(),
                command: "x".into(),
            },
            targets: Targets::All,
        };
        assert_eq!(
            table.action(&hook),
            DegradeAction::Skip,
            "policy comes from the owning plugin entry"
        );

        let plugin = caps.iter().find(|c| c.id.kind == Kind::Plugin).unwrap();
        assert_eq!(table.action(plugin), DegradeAction::Report);

        let mcp = caps.iter().find(|c| c.id.kind == Kind::Mcp).unwrap();
        assert_eq!(
            table.action(mcp),
            DegradeAction::Report,
            "non-plugin capabilities default to report"
        );
    }

    #[test]
    fn degrade_table_tolerates_unknown_plugin_origin() {
        let man = m("version: 1\n");
        let table = DegradeTable::new(&man);
        let orphan = Capability {
            id: CapId::new(Kind::Hook, "x"),
            origin: Origin::Plugin {
                name: "not-in-manifest".into(),
            },
            spec: Spec::Hook {
                event: "E".into(),
                command: "c".into(),
            },
            targets: Targets::All,
        };
        assert_eq!(table.action(&orphan), DegradeAction::Report);
    }

    /// `manifest::validate` 只保证 git 源**带** `ref`，不保证它不是哨兵，所以
    /// `ref: unknown` 会一路走到这里。不过 `normalize_locator` 的话两个各自记着
    /// `unknown` 的构建会被 `compare_versions` 判成 `Same`（假同步）——把实现里的
    /// `normalize_locator` 删掉，本测试第一组断言立刻变红。
    #[test]
    fn sentinel_git_ref_does_not_become_a_lock() {
        for raw in ["unknown", "UNKNOWN", " unknown ", "\\t "] {
            let yaml = format!(
                "version: 1\nskills:\n  a:\n    source: git\n    repo: x/y\n    ref: \"{raw}\"\n"
            );
            let caps = lower(&m(&yaml));
            match &caps[0].spec {
                Spec::Skill {
                    source:
                        SkillSource::Git {
                            git_ref,
                            repo,
                            path,
                        },
                    ..
                } => {
                    assert!(
                        git_ref.is_empty(),
                        "sentinel ref {raw:?} must not survive as a lock: {git_ref:?}"
                    );
                    assert_eq!(repo, "x/y", "repo 是来源信息，不能跟着哨兵一起丢");
                    assert_eq!(path, &None);
                }
                other => panic!("expected Git skill, got {other:?}"),
            }
        }
        // 真 ref 原样保留，只 trim 掉两侧空白
        let yaml =
            "version: 1\nskills:\n  a:\n    source: git\n    repo: x/y\n    ref: \" v6.3.0 \"\n";
        match &lower(&m(yaml))[0].spec {
            Spec::Skill {
                source: SkillSource::Git { git_ref, .. },
                ..
            } => assert_eq!(git_ref, "v6.3.0", "real ref must pass through trimmed"),
            other => panic!("expected Git skill, got {other:?}"),
        }
    }

    /// plugin 侧同一个哨兵落在 `Option<String>` 位：归成 `None` 之后 `compare_versions`
    /// 走 `Unknown`（按 in-sync 处理），而不是拿 `Some("unknown")` 去和实际侧比 version。
    #[test]
    fn sentinel_plugin_ref_is_not_a_version() {
        for raw in ["unknown", "UNKNOWN"] {
            let yaml = format!(
                "version: 1\nplugins:\n  p:\n    source: github\n    repo: x/y\n    ref: \"{raw}\"\n"
            );
            let caps = lower(&m(&yaml));
            match &caps[0].spec {
                Spec::Plugin {
                    version,
                    sha,
                    entry,
                    ..
                } => {
                    assert_eq!(version, &None, "sentinel ref {raw:?} must not be a version");
                    assert_eq!(sha, &None, "期望侧永远没有 SHA");
                    assert_eq!(entry, "p");
                }
                other => panic!("expected Plugin, got {other:?}"),
            }
        }
        // 真 ref 仍然按设计落在 version 位（version 兜底比对）
        let yaml =
            "version: 1\nplugins:\n  p:\n    source: github\n    repo: x/y\n    ref: v6.3.0\n";
        match &lower(&m(yaml))[0].spec {
            Spec::Plugin { version, .. } => {
                assert_eq!(version.as_deref(), Some("v6.3.0"), "真 ref 不能被清掉")
            }
            other => panic!("expected Plugin, got {other:?}"),
        }
    }

    /// brief 的 local 用例只走 `~/x` 分支。删掉裸 `~` 分支后 `~` 会原样留下一个相对
    /// 字面量（`is_absolute` 变红）；把所有路径一律拼到 HOME 底下的实现则会让
    /// `/abs/skills/d` 那条断言变红。
    #[test]
    fn tilde_expansion_covers_bare_home_and_plain_paths() {
        let local = |raw: &str| {
            let yaml =
                format!("version: 1\nskills:\n  d:\n    source: local\n    path: \"{raw}\"\n");
            match &lower(&m(&yaml))[0].spec {
                Spec::Skill {
                    source: SkillSource::Local { path },
                    ..
                } => path.clone(),
                other => panic!("expected Local skill, got {other:?}"),
            }
        };
        let home = local("~");
        assert!(home.is_absolute(), "bare ~ must expand, got {home:?}");
        assert!(!home.to_string_lossy().contains('~'), "{home:?}");
        assert_eq!(local("~/x"), home.join("x"), "`~/x` 必须挂在同一个 root 下");
        assert_eq!(local("/abs/skills/d"), PathBuf::from("/abs/skills/d"));
    }
}
