//! 三类清单条目 → 单一 Capability 流（spec §4 决策 1「一个出口」）。
//!
//! plugin 在此**不**被拆解：spec §2.1 第 2 条确认三家 harness 都有 plugin 机制且 bundle 自带
//! per-harness manifest，v1 以对账为主。拆解是 Plan B 在目标 harness 缺 manifest 时的降级路径。

use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, normalize_locator};
use crate::manifest::{
    DegradeAction, DegradePolicy, Manifest, PluginEntry, PluginSourceKind, SkillSourceKind,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// tilde 展开的**纯**内核：`root` 就是 HOME，`None` 表示环境里没有 HOME。
///
/// 拆出来只为了让「HOME 缺失」那一支可测：走 `home_from_env()` 的那条路要读进程
/// 环境，而测试并行跑、`set_var` 会互相踩（report C7 / 审计 U10）。分支顺序与原实现
/// 逐字对应，只是 root 从「读环境」变成「传进来」。
fn expand_tilde_in(root: Option<&Path>, raw: &str) -> PathBuf {
    let base = || {
        root.map(PathBuf::from)
            // 没有 HOME 时 `~` 展开成文件系统根：留下字面量 `~` 会变成一个「看着像路径」
            // 的相对串，比一个绝对的 `/` 更难解释，也永远不会是 CWD 无关的位置。
            .unwrap_or_else(|| PathBuf::from("/"))
    };
    if raw == "~" {
        return base();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return base().join(rest);
    }
    PathBuf::from(raw)
}

/// 把 `~/x` 展开为绝对路径。只处理前导 `~/` 与裸 `~`，不做 shell 语义。
fn expand_tilde(raw: &str) -> PathBuf {
    expand_tilde_in(home_from_env().as_deref(), raw)
}

/// 只读 `HENV_HOME` / `HOME` 环境变量，不碰文件系统——`lower` 全程无 IO，所以这里不能走
/// `Home::resolve()`（它对 root 做 `canonicalize`，那是文件系统调用）。优先级与
/// `Home::resolve()` 一致：`HENV_HOME` 优先，其次 `HOME`；空的 `HENV_HOME` 落到 `HOME`
/// 而不是顶掉它（`lower` 不返回 `Result`，没法照搬 `Home::resolve()` 的硬报错，退化成
/// 下一个来源）。这是全局约束「一律经 `Home`」的唯一例外，且是刻意的：Plan B 若给
/// `lower` 引入 IO，届时改为接收已解析的 root。
fn home_from_env() -> Option<PathBuf> {
    std::env::var_os("HENV_HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// plugin 条目的**身份**：`entry` 名，缺省回退到清单 key（spec §2.1 第 3 条）。
///
/// `lower` 用它发能力，`DegradeTable` 用它做键——两处必须是同一个函数：分开两份
/// 实现时（审计 U11），表可以按 entry 键、`lower` 按 key 发，degrade 策略就此静默
/// 失效，而两边的单测各自都还是绿的。
fn plugin_identity(key: &str, p: &PluginEntry) -> String {
    p.entry.clone().unwrap_or_else(|| key.to_string())
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
        // 身份只有一份定义：`plugin_identity`（spec §2.1 第 3 条 + 审计 U11），
        // 表侧与这里必须键到同一个串，否则策略静默失效。
        let entry = plugin_identity(key, p);
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
                // compare_versions 走 version 形比较（spec §2.1 第 3 条）；判不了则 Unknown。
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
///
/// 键就是 [`plugin_identity`]，与 `lower` 发能力用的是同一个函数（审计 U11）。
/// 查表只认 `Origin::Plugin`，而 Plan A 两侧各差一半、谁也吃不到它：**desired 侧**（`lower`
/// 产出的）全是 `Origin::Manifest`，没有可命中的键；**actual 侧**的 bundle 能力带
/// `Origin::Plugin`，但 Task 13 只对 desired 侧查表，没人来问。所以这张表在 Plan A 里
/// **两侧都是惰性的**，`degrade: {hooks: skip}` 目前不产生任何可观察差异（计划交接决策 7）。
/// 让策略生效是 Plan B 拆解 plugin 时的事——那是设计后果，不是这里要“顺手修掉”的 bug。
#[derive(Debug, Clone, Default)]
pub struct DegradeTable {
    by_plugin: BTreeMap<String, DegradePolicy>,
}

impl DegradeTable {
    pub fn new(m: &Manifest) -> Self {
        let mut by_plugin = BTreeMap::new();
        for (key, p) in &m.plugins {
            by_plugin.insert(plugin_identity(key, p), p.degrade.clone());
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

    /// 按 kind 造一个占位 spec。`DegradeTable::action` 只读 `id.kind` 与 `origin`，
    /// 所以「某个 kind 其实永远不会带 Origin::Plugin」这种可能性要在**这里**堵掉，
    /// 不能由 spec 的形状隐式决定（那会让将来任何按 spec 分支的实现悄悄跑偏）。
    fn spec_of(kind: Kind) -> Spec {
        match kind {
            Kind::Skill => Spec::Skill {
                source: SkillSource::AgentsDir,
                path: None,
            },
            Kind::Mcp => Spec::Mcp {
                command: "npx".into(),
                args: vec![],
                env: Default::default(),
            },
            Kind::Command => Spec::Command {
                path: PathBuf::from("/p"),
            },
            Kind::Agent => Spec::Agent {
                path: PathBuf::from("/p"),
            },
            Kind::Hook => Spec::Hook {
                event: "SessionStart".into(),
                command: "x".into(),
            },
            Kind::Plugin => Spec::Plugin {
                marketplace: None,
                entry: "plug".into(),
                version: None,
                sha: None,
                install_path: None,
                enabled: true,
            },
        }
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
        // 只断 id.name 的话，`Spec::Plugin` 里那三个字段（entry / marketplace /
        // install_path）写了什么没人看守——审计 U3/U6/U8 的变异体（`entry: key.clone()`、
        // claude 源的 `marketplace => None`、`install_path: Some(..)`）全都存活。
        // 整个 spec 一比一对上，才真的钉住「期望侧 plugin 长什么样」。
        assert_eq!(
            plugin.spec,
            Spec::Plugin {
                marketplace: Some("obra/spm".into()),
                entry: "superpowers".into(),
                version: None,
                sha: None,
                install_path: None,
                enabled: true,
            },
            "key 是 superpowers-bundle：spec 里任何一处都不许留着它"
        );
        assert_eq!(plugin.origin, Origin::Manifest);
        assert_eq!(mcp.origin, Origin::Manifest);
        assert_eq!(skill.origin, Origin::Manifest);
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

    /// zip-url 源今天降成什么形状，这条就是那个形状的看守者：清单里没有任何可当**来源
    /// 标识**的位置字段（`marketplace` / `repo` 都没有），所以 `marketplace` 是 `None`、
    /// `entry` 回退到清单 key。
    ///
    /// 「zip-url 缺 url 字段」是**计划**层面的缺口（Task 5 追加已记为留给 Plan B/C），
    /// 但缺口的存在不等于形状不可测：不钉住的话，将来补 `url` 时把 `marketplace` 从
    /// `None` 改成 `Some(..)` 是无声漂移——golden 自洽、没有别的测试会红。
    /// 实测变异体：`PluginSourceKind::ZipUrl => Some(entry.clone())`（给该源补一个「拿身份
    /// 当来源标识」的 locator）→ 整套 121 个里**只有**这条报红，其余测试没有一个用 zip-url 源。
    /// 反过来说，`=> p.entry.clone()` 是个**假变异体**：本 fixture 没写 `entry`，它照样是
    /// `None`，两种实现下测试同色，所以不能拿它当证据。
    /// 信号的**唯一**来源是 `marketplace` 那一行：只留 `entry` 那条断言时，上面的变异体存活。
    /// `entry` 那一行与 `degrade_table_and_lower_share_one_plugin_identity` 的第一组重复，
    /// 留在这里只是为了把「该源不许另起身份」写进同一个 fixture，它不额外杀变异体。
    #[test]
    fn zip_url_plugin_lowers_with_no_marketplace() {
        let caps = lower(&m("version: 1\nplugins:\n  zp: { source: zip-url }\n"));
        match &caps[0].spec {
            Spec::Plugin {
                marketplace, entry, ..
            } => {
                assert_eq!(marketplace, &None, "zip-url 没有可当来源标识的位置字段");
                assert_eq!(entry, "zp", "身份仍是清单 key，不该被换成别的东西");
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
    /// 字面量（`is_absolute` 变红）。
    ///
    /// **更正（审计 CB/U9）**：这一版文档曾声称「把所有路径一律拼到 HOME 底下的实现会
    /// 让 `/abs/skills/d` 那条断言变红」——**测过了，不会**：`PathBuf::join` 遇到绝对参数
    /// 是**替换**而不是拼接，所以 `home.join("/abs/skills/d") == /abs/skills/d`，那条变异
    /// 体存活。真正能钉住它的是下面这条 `dev/skills` 相对路径断言：一律挂到 HOME 底下会
    /// 把它变成 `<HOME>/dev/skills`，于是变红（同时保住 C5 记的「相对路径保持相对」）。
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
        // U9：非 tilde 的相对路径原样穿过——不展开、不挂 root、不加 `./`。
        assert_eq!(
            local("dev/skills"),
            PathBuf::from("dev/skills"),
            "相对路径必须保持相对（C5），否则上面那条绝对路径断言谁也分不清两种实现"
        );
        assert!(!local("dev/skills").is_absolute());
    }

    /// U10：`HOME` 缺失那一支以前无法测——`home_fallback()` 要读进程环境，而 `set_var`
    /// 会被并行的测试踩到（report C7）。现在展开逻辑的纯内核 `expand_tilde_in` 收一个
    /// `Option<&Path>`，`None` 就是「环境里没有 HOME」，可以直接钉。
    /// 剩下的 `home_from_env()`（一次 `var_os` 调用）仍然没有测试，也不该有。
    #[test]
    fn tilde_expansion_without_home_stays_absolute_and_never_keeps_the_tilde() {
        assert_eq!(expand_tilde_in(None, "~"), PathBuf::from("/"));
        assert_eq!(
            expand_tilde_in(None, "~/dev/skills"),
            PathBuf::from("/dev/skills")
        );
        // 与 Some 分支同一条规则：绝对路径原样、相对路径保持相对。
        let root = PathBuf::from("/root");
        assert_eq!(expand_tilde_in(Some(&root), "~"), root);
        assert_eq!(
            expand_tilde_in(Some(&root), "~/dev/skills"),
            root.join("dev/skills")
        );
        assert_eq!(
            expand_tilde_in(Some(&root), "/abs/d"),
            PathBuf::from("/abs/d")
        );
        assert_eq!(
            expand_tilde_in(Some(&root), "dev/d"),
            PathBuf::from("dev/d")
        );
        assert_eq!(expand_tilde_in(None, "dev/d"), PathBuf::from("dev/d"));
        // `~user` 这种 shell 语义明确不做：只有裸 `~` 与 `~/` 前缀会被展开。
        assert_eq!(
            expand_tilde_in(Some(&root), "~other/x"),
            PathBuf::from("~other/x")
        );
    }

    /// U1/U4/U5：origin 是 `DegradeTable` **唯一**的分支依据，而 `lower` 在三处设置它。
    /// 之前只有 skill 那一处被钉住（审计：mcp 与 plugin 两处各写一个别的 origin 都全绿），
    /// 配合 U1 就是「一条 manifest 里的 mcp 悄悄继承了某个 plugin 的 degrade 策略」。
    /// `Origin::Plugin` 在实际侧由 adapter 产出（Task 8-11），期望侧不该有。
    #[test]
    fn lowered_capabilities_never_claim_a_plugin_origin() {
        let yaml = "version: 1\nskills:\n  s: { source: agents-dir }\nmcps:\n  m: { command: npx }\nplugins:\n  k: { source: github, repo: x/y, entry: plug }\n";
        let caps = lower(&m(yaml));
        assert_eq!(caps.len(), 3);
        for c in &caps {
            assert_eq!(
                c.origin,
                Origin::Manifest,
                "{} 的 origin 被改写过：degrade 表就是按 origin 分支的",
                c.id.label()
            );
        }
        // 名字与某个 plugin 的 entry 相同也不能改变结论（mcp/mcp 同名是最常见的撞车）。
        let by_name = |kind: Kind, name: &str| {
            caps.iter()
                .find(|c| c.id.kind == kind && c.id.name == name)
                .expect("capability")
        };
        assert_eq!(by_name(Kind::Plugin, "plug").origin, Origin::Manifest);
        assert_eq!(by_name(Kind::Mcp, "m").origin, Origin::Manifest);
    }

    /// U1：只有 `Origin::Plugin` 会去查表。审计的变异体是 `_ => self.by_plugin
    /// .values().next()`——「拿第一条策略凑数」，它存活过整轮 42 个变异。
    /// 之所以此前测不出来：fixture 的策略只写了 `hooks`，而 `action_for(Kind::Plugin)`
    /// 又硬编码 `Report`，于是所有非 plugin 断言都是**因为错的原因**通过的。
    #[test]
    fn only_plugin_origin_ever_consults_the_degrade_table() {
        let man = m(
            "version: 1\nplugins:\n  k:\n    source: github\n    repo: x/y\n    entry: plug\n    degrade:\n      hooks: skip\n      skills: skip\n",
        );
        let table = DegradeTable::new(&man);
        let cap = |kind: Kind, origin: Origin| Capability {
            id: CapId::new(kind, "whatever"),
            origin,
            spec: spec_of(kind),
            targets: Targets::All,
        };
        let adopted = || Origin::Adopted {
            harness: "claude-code".into(),
        };
        // 命中表里的 plugin 名，但 origin 不是 Plugin：一条策略都不许继承。
        assert_eq!(
            table.action(&cap(Kind::Hook, adopted())),
            DegradeAction::Report,
            "Adopted 的 hook 名字再像，也不属于任何 plugin"
        );
        assert_eq!(
            table.action(&cap(Kind::Skill, adopted())),
            DegradeAction::Report,
            "表里确实写着 skills: skip，非 plugin 来源也不能吃到它"
        );
        assert_eq!(
            table.action(&cap(Kind::Skill, Origin::Manifest)),
            DegradeAction::Report,
            "期望侧（lower 产出的全是 Manifest）不查表"
        );
        // 对照组：同样的 kind、origin 换成 Plugin{plug} → 必须吃到 skip。
        // 没有这一半，「一律 Report」的实现（`_ => None` 写成 `None`）也能全绿。
        let plug = || Origin::Plugin {
            name: "plug".into(),
        };
        assert_eq!(table.action(&cap(Kind::Hook, plug())), DegradeAction::Skip);
        assert_eq!(table.action(&cap(Kind::Skill, plug())), DegradeAction::Skip);
    }

    /// U2：命中之后由 `cap.id.kind` 决定读策略的哪一行。审计的变异体是
    /// `Some(p) => p.hooks`（永远读 hooks 那行），此前存活——因为只有 Hook 被
    /// 在「有命中」的情况下探过。每行一个 kind、并且故意让各行的值互不相同。
    #[test]
    fn a_policy_hit_still_routes_by_the_capability_kind() {
        let man = m(
            "version: 1\nplugins:\n  k:\n    source: github\n    repo: x/y\n    entry: plug\n    degrade:\n      hooks: skip\n",
        );
        let table = DegradeTable::new(&man);
        let from_plugin = |kind: Kind| Capability {
            id: CapId::new(kind, "plug:x"),
            origin: Origin::Plugin {
                name: "plug".into(),
            },
            spec: spec_of(kind),
            targets: Targets::All,
        };
        assert_eq!(
            table.action(&from_plugin(Kind::Hook)),
            DegradeAction::Skip,
            "声明了的那一行必须命中"
        );
        for kind in [Kind::Command, Kind::Skill, Kind::Mcp, Kind::Agent] {
            assert_eq!(
                table.action(&from_plugin(kind)),
                DegradeAction::Report,
                "degrade 只写了 hooks: skip，{kind:?} 不能跟着吃 skip"
            );
        }
        // `Kind::Plugin` 走 action_for 的硬编码分支，同样不许读到别的行。
        assert_eq!(
            table.action(&from_plugin(Kind::Plugin)),
            DegradeAction::Report
        );
    }

    /// U11 的另一半：表的键与 `lower` 发出的 plugin 身份必须是**同一个**字符串。
    /// 以前这条规则在两个函数里各写一遍（`p.entry.clone().unwrap_or_else(..)`），
    /// 只有 `lower` 那一份被钉住——表侧改成 `unwrap_or_default()` 仍全绿，于是
    /// 「没写 entry 的 plugin」的 degrade 策略静默失效。
    #[test]
    fn degrade_table_and_lower_share_one_plugin_identity() {
        // 两组只差 `entry` 这一行：省掉它，身份就回退到清单 key。
        for (yaml, expected) in [
            (
                "version: 1\nplugins:\n  the-key:\n    source: github\n    repo: x/y\n    degrade:\n      hooks: skip\n",
                "the-key",
            ),
            (
                "version: 1\nplugins:\n  the-key:\n    source: github\n    repo: x/y\n    entry: the-entry\n    degrade:\n      hooks: skip\n",
                "the-entry",
            ),
        ] {
            let man = m(yaml);
            let caps = lower(&man);
            let plugin = caps.iter().find(|c| c.id.kind == Kind::Plugin).unwrap();
            assert_eq!(plugin.id.name, expected);
            // 关键：hook 的 origin 取的是 **lower 自己发出来的**那个身份。
            // 两处身份一旦分叉（任何一个被改掉），这里就会从 Skip 翻成 Report。
            let hook = Capability {
                id: CapId::new(Kind::Hook, format!("{}:pre-commit", plugin.id.name)),
                origin: Origin::Plugin {
                    name: plugin.id.name.clone(),
                },
                spec: Spec::Hook {
                    event: "SessionStart".into(),
                    command: "x".into(),
                },
                targets: Targets::All,
            };
            assert_eq!(
                DegradeTable::new(&man).action(&hook),
                DegradeAction::Skip,
                "{expected}：表的键必须认得 lower 发出的 plugin 身份"
            );
        }
    }

    /// 策略按 plugin 各存一份：`by_plugin` 是按键索引的表，不是「第一条策略」。
    /// fixture 里被点名的 plugin 是 **B**，A 才是 BTreeMap 序里的第一条——
    /// 任何「拿 values().next() 凑数」的实现都会在 A 那一行变红。
    #[test]
    fn a_degrade_policy_never_leaks_across_plugins() {
        let man = m(
            "version: 1\nplugins:\n  a-plain:\n    source: github\n    repo: x/a\n  z-bundler:\n    source: github\n    repo: x/z\n    degrade:\n      hooks: skip\n",
        );
        let table = DegradeTable::new(&man);
        let hook = |name: &str| Capability {
            id: CapId::new(Kind::Hook, format!("{name}:pre-commit")),
            origin: Origin::Plugin { name: name.into() },
            spec: Spec::Hook {
                event: "SessionStart".into(),
                command: "x".into(),
            },
            targets: Targets::All,
        };
        assert_eq!(
            table.action(&hook("a-plain")),
            DegradeAction::Report,
            "没写 degrade 的 plugin 不能被邻居的策略覆盖"
        );
        assert_eq!(table.action(&hook("z-bundler")), DegradeAction::Skip);
    }

    /// 本 Task 的 API 契约：查表不许 panic、不许向上抛错（Task 13 是对整条流 map 的）。
    #[test]
    fn degrade_table_covers_every_kind_without_a_hit() {
        let table = DegradeTable::new(&m("version: 1\n"));
        let origin = Origin::Plugin {
            name: "not-in-manifest".into(),
        };
        for kind in Kind::all() {
            let cap = Capability {
                id: CapId::new(kind, "x"),
                origin: origin.clone(),
                spec: spec_of(kind),
                targets: Targets::All,
            };
            assert_eq!(
                table.action(&cap),
                DegradeAction::Report,
                "{kind:?}：空表上任何 kind 都必须走默认值，不能 panic 也不能漏"
            );
        }
    }

    /// 计划里的硬约束：`Targets::Only` **不排序不去重**，`Only(vec![])` 与 All 不是一回事。
    /// 这条以前没人钉：`targets` 只做了一次 clone，但谁都能在 clone 前后加一道
    /// `sort_unstable` / `dedup`，而渲染侧（Task 14 的 golden）就会跟着变。
    #[test]
    fn explicit_targets_stay_verbatim_and_empty_only_stays_empty() {
        let yaml = "version: 1\nmcps:\n  dup:\n    command: npx\n    targets: [codex, claude-code, codex]\n  empty: { command: npx, targets: [] }\n";
        let caps = lower(&m(yaml));
        let dup = caps.iter().find(|c| c.id.name == "dup").unwrap();
        assert_eq!(
            dup.targets,
            Targets::Only(vec!["codex".into(), "claude-code".into(), "codex".into()]),
            "顺序与重复项都是原样"
        );
        let empty = caps.iter().find(|c| c.id.name == "empty").unwrap();
        assert_eq!(
            empty.targets,
            Targets::Only(vec![]),
            "Only([]) 不能被归成 All：它是「一家都不投」，includes 恒 false"
        );
        assert!(
            !empty.targets.includes("codex"),
            "Only([]) 若被当成 All，这里就会是 true"
        );
    }

    /// 「plugin 不会被自己的 degrade 策略卸掉」是 Task 13 直接依赖的一条独立结论：
    /// `action_for(Kind::Plugin)` 走 manifest.rs 里那条硬编码 Report。
    /// **如实说明覆盖关系**：这条不是唯一的看守——把 `action()` 改成固定读某一行时，
    /// `a_policy_hit_still_routes_by_the_capability_kind` 也会一起报红。单独钉它是因为
    /// 规则本身与「按 kind 路由」不是一回事（前者管 plugin 自身可不可见，后者管 bundle
    /// 内容怎么处理），报红的测试名应当说出 broken 的是哪一条。
    #[test]
    fn a_plugin_never_degrades_itself_even_when_its_policy_skips_hooks() {
        let man = m(
            "version: 1\nplugins:\n  k:\n    source: github\n    repo: x/y\n    entry: plug\n    degrade:\n      hooks: skip\n",
        );
        let table = DegradeTable::new(&man);
        let cap = |kind: Kind| Capability {
            id: CapId::new(kind, "plug"),
            origin: Origin::Plugin {
                name: "plug".into(),
            },
            spec: spec_of(kind),
            targets: Targets::All,
        };
        assert_eq!(
            table.action(&cap(Kind::Hook)),
            DegradeAction::Skip,
            "对照组：bundle 里的 hook 才是策略要管的东西"
        );
        assert_eq!(
            table.action(&cap(Kind::Plugin)),
            DegradeAction::Report,
            "plugin 自身的安装/缺失不是 degrade 问题，交给 supports() 判 Unsupported"
        );
    }

    /// 查表**只**认 (origin, id.kind)，spec 及其形状一律不参与。为什么要在意：实际侧的
    /// spec 由各家 adapter 造、字段齐全度不同；一旦 `action()` 里出现按 spec 分支的写法
    /// （`Spec::Hook{..} => p.hooks`），策略就按「spec 长得像什么」而不是按 id 生效——
    /// 而按 kind 路由那条测试分辨不出这种写法（它两边 spec 与 kind 本来就一致）。
    #[test]
    fn degrade_lookup_reads_kind_and_origin_never_the_spec() {
        let man = m(
            "version: 1\nplugins:\n  k:\n    source: github\n    repo: x/y\n    entry: plug\n    degrade:\n      hooks: skip\n",
        );
        let table = DegradeTable::new(&man);
        let with = |kind: Kind, spec: Spec| Capability {
            id: CapId::new(kind, "plug:thing"),
            origin: Origin::Plugin {
                name: "plug".into(),
            },
            spec,
            targets: Targets::All,
        };
        // kind 固定 Hook，spec 换成六种形状：结论不许变。
        for shape in Kind::all() {
            assert_eq!(
                table.action(&with(Kind::Hook, spec_of(shape))),
                DegradeAction::Skip,
                "spec 换成 {shape:?} 的形状就改变结论 = 在按 spec 分支"
            );
        }
        // 反过来：spec 固定 Hook，kind 逐个换——只有 Hook 那一行吃到 skip。
        for kind in Kind::all() {
            let want = if kind == Kind::Hook {
                DegradeAction::Skip
            } else {
                DegradeAction::Report
            };
            assert_eq!(
                table.action(&with(kind, spec_of(Kind::Hook))),
                want,
                "{kind:?} 必须读自己那一行"
            );
        }
    }

    /// `CapId` 是 (kind, name)：**同名可以跨段共存，一条都不许被合并**。本机实测
    /// `superpowers` 既是 plugin entry 也是 skill 名；`lower` 一旦改成按 name 去重
    /// （或中途把它塞进 `BTreeMap<String, _>`），少的那一条就从 diff 里彻底消失——
    /// 与 skills_dir D4 同方向的漏报。审计那 42 个变异里没有这一类。
    #[test]
    fn same_name_across_sections_stays_three_separate_capabilities() {
        let yaml = "version: 1\nskills:\n  superpowers: { source: agents-dir }\nmcps:\n  superpowers: { command: npx }\nplugins:\n  anything: { source: github, repo: x/y, entry: superpowers }\n";
        let caps = lower(&m(yaml));
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        assert_eq!(
            labels,
            vec!["mcp/superpowers", "plugin/superpowers", "skill/superpowers"],
            "三条都得在，并且按 label 排序"
        );
    }
}
