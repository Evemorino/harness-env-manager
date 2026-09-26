//! §9.5 的拆解一步：把「某家已安装的 plugin bundle」翻成一组**独立能力**。
//!
//! 这一步在 `compute` **之前**、`resolve_sources` **之前**跑，因为它是唯一
//! 知道「bundle 里有哪些 skill」的地方——而那正是内容层要去算哈希的路径。
//!
//! 为什么不是 `lower` 的一部分：`lower` 是纯函数，只看得见清单，够不着盘；
//! 而 skill 目录名只有读了 bundle 才知道。为什么不是 `resolve_sources` 的一部分：
//! 解析的职责是「给定一条能力，它的内容在哪」，而这一步**产出**能力。

use crate::adapters::HarnessAdapter;
use crate::capability::{
    CapId, Capability, Kind, Origin, SkillSource, Spec, Targets, normalize_locator,
};
use crate::home::Home;
use crate::lower::{SourceHarnessTable, plugin_identity};
use crate::manifest::{DegradeAction, KNOWN_HARNESSES, Manifest};
use crate::skills_dir::try_read_skill_entries;
use crate::store::skills::{BundleTable, LocatedBundle};
use anyhow::Context as _;
use std::collections::BTreeMap;
use std::path::Path;

/// bundle 自带 manifest 的四种形状（§9.5）。按顺序取**第一个存在且解析得开**的。
///
/// 四种都对，因为一个 bundle 是谁造的只有它自己知道：claude 的 marketplace 缓存、
/// codex、kimi 各写各的，而 kimi 还多一个根目录下的形状。
const BUNDLE_MANIFESTS: [&str; 4] = [
    ".claude-plugin/plugin.json",
    ".codex-plugin/plugin.json",
    ".kimi-plugin/plugin.json",
    "kimi.plugin.json",
];

/// 读 bundle 自带 manifest 里声明的**版本**。
///
/// - 四个位置一个都不存在 → `Ok(None)`：**不是错误**，不是每个 bundle 都自带 manifest。
/// - 存在但读不开 / 解不开 → `Err`：那份文件坏了，我们说不清它声明的是什么，
///   而「猜一个 None」会让一条坏 bundle 看起来和一条朴素 bundle 一样。
/// - 存在且解得开但没有 `version` → `Ok(None)`：**不再往下找**。同一个 bundle
///   里出现两份 manifest 本身就是怪事，第一份命中即止比「挑一份有版本号的」好解释。
///
/// 字段按宽松读（`get("version").and_then(Value::as_str)`）：这里只取一个装饰性的
/// 版本串，为它建一套形状原语是杀鸡用牛刀。归一化仍然过 [`normalize_locator`]，
/// 于是本机写着的字面量 `unknown` 不会变成一个假的版本段。
pub(crate) fn read_bundle_manifest(install_path: &Path) -> anyhow::Result<Option<String>> {
    for rel in BUNDLE_MANIFESTS {
        let p = install_path.join(rel);
        if !p.exists() {
            continue;
        }
        let Some(v) = crate::adapters::claude_code::read_json_if_present(&p)? else {
            continue; // 文件里写着 `null`：与缺段同义，继续找下一个形状
        };
        return Ok(normalize_locator(v.get("version").and_then(|x| x.as_str())));
    }
    Ok(None)
}

/// §9.5 的 `source_id`：`bundle@<source_harness>@<version>@<sha>`，缺段整段省略。
///
/// 纯展示串，**没有任何解析器**，也不参与任何判决（`compare_versions` 不看它）。
/// 它唯一的作用是让账本里的一行回答「这份拷贝是从哪来的」。
pub(crate) fn bundle_source_id(
    source_harness: &str,
    version: Option<&str>,
    sha: Option<&str>,
) -> String {
    let mut s = format!("bundle@{source_harness}");
    for seg in [version, sha].into_iter().flatten() {
        s.push('@');
        s.push_str(seg);
    }
    s
}

/// 一个源 harness 这次扫描的结果。
///
/// **两态而不是 `Option<Vec<_>>`**：「没装这家」与「装了但按它自己说的什么都没扫到」
/// 必须是两种状态——前者要报「`claude-code` 本机没装」，后者要报
/// 「`claude-code` 的安装记录里没有这个 plugin」。把两者压成空的 `Vec`，
/// 前者就会被报成后者，指着一个已经装好的 harness 让用户去重装。
///
/// `Clone` 是给 `scan_source_harnesses` 的复用路径用的：`plan_for` 已经读过的那份
/// 交进来，这里搬一份进结果表（搬能力清单比再走一遍目录便宜得多，见那里的注释）。
#[derive(Debug, Clone)]
pub enum SourceState {
    NotInstalled,
    Scanned(Vec<Capability>),
}

/// 按需扫描被声明为源的 harness（§9.5）。
///
/// **不参与 `targets`**：`source_harness:` 只请求**读**它，不请求往它写。
/// 把它收进 `plan_for` 的 `pairs` 会让它凭空多出一节 diff，而它在 `harnesses:`
/// 里很可能是关掉的——那正是「我只想拿它的内容」的写法。
///
/// 用的是同一个 `read_state()`：拆解不许有第二条读路径，否则
/// 「diff 里读到的」与「拆解取到的」能不一致，而消灭这两种不一致正是这个项目的存在理由。
///
/// `already` 是**调用方已经为启用的家读好的那份现状**（键是 harness 名，可能为空）。
/// 同一个家既是启用的目标又是清单声明的源时，直接搬用它——只调一次 `read_state()`。
/// 这比「省一次目录遍历」更要紧：两次 `read_state()` 之间盘面会变，共用一份才真正
/// 兑现上面那句「拆解不许有第二条读路径」。
pub fn scan_source_harnesses(
    sources: &SourceHarnessTable,
    registry: &[Box<dyn HarnessAdapter>],
    already: &BTreeMap<String, SourceState>,
) -> anyhow::Result<BTreeMap<String, SourceState>> {
    let mut out = BTreeMap::new();
    for name in sources.declared() {
        if let Some(state) = already.get(name) {
            out.insert(name.to_string(), state.clone());
            continue;
        }
        let Some(a) = crate::adapters::lookup(registry, name) else {
            // 校验期已保证值域在 KNOWN_HARNESSES 内，而 registry 与它逐个相等
            // （`src/adapters/mod.rs:173` 把两者钉成相等）。走到这里是代码 bug，
            // 但按「没装」处理比 panic 好：报告里会显式说出来，不是静默。
            out.insert(name.to_string(), SourceState::NotInstalled);
            continue;
        };
        if !a.detect() {
            out.insert(name.to_string(), SourceState::NotInstalled);
            continue;
        }
        // 上抛：这家扫不了是这台机器的问题，与 `plan_for` 里既有的几处同样处理。
        let caps = a
            .read_state()
            .with_context(|| format!("cannot scan source harness {name} state"))?;
        out.insert(name.to_string(), SourceState::Scanned(caps));
    }
    Ok(out)
}

/// 拆解这一步的全部产物。四张表，键统一是 **plugin 身份**。
///
/// 为什么不返回 `Result` 来表达「某一条坏了」：条目级失败是**正常结果**，
/// 不是异常（§9.7 失败隔离）。唯一的 `Err` 来自目录读不了——
/// 那是「我们说不清那里有什么」，不能假装成「那里什么都没有」。
#[derive(Debug, Default)]
pub struct Decomposition {
    /// 追加进 `desired` 的独立能力。**`lower` 的产物不含它们**——
    /// `lower` 是纯函数、够不着盘，而 skill 目录名只有读了 bundle 才知道。
    pub capabilities: Vec<Capability>,
    /// plugin 身份 → 报告注记（§9.5「装不下的部分」）。
    pub notes: BTreeMap<String, String>,
    /// plugin 身份 → 失败理由，成品的整句，可直接进 `undeliverable`。
    pub failures: BTreeMap<String, String>,
    /// plugin 身份 → 定位到的 bundle。
    pub locations: BundleTable,
}

/// §9.5 的拆解主体：清单里写了的每一条 plugin，去源家的已安装 bundle 里找东西。
///
/// `known` 是「有哪几家 harness」（`compute` 手里那几家的名字），
/// 用来把 `Targets::All` 具体化成「除源之外的几家」。
pub fn decompose_bundles(
    manifest: &Manifest,
    source_states: &BTreeMap<String, SourceState>,
    known: &[&str],
    home: &Home,
) -> anyhow::Result<Decomposition> {
    let mut out = Decomposition::default();

    // 「清单层面启用的家」。与本次的 `known` 刻意分开：`known` 里还叠着
    // `--harness` 的视图过滤，而「这条交付目标被 `harnesses:` 关掉了」是清单自己的
    // 问题，不该因为换个视图就时隐时现。
    let in_play: Vec<&str> = KNOWN_HARNESSES
        .iter()
        .copied()
        .filter(|h| crate::is_enabled(manifest, h))
        .collect();

    // §9.5：`CapId` 是身份——同名就是同一条能力，两条不可能都交付。
    // 先到先得，且**清单里手写的 `skills.*` 优先于 bundle 拆出来的**：前者是用户的
    // 显式声明，后者是附带发现。被挡下的那条不静默丢，记一条失败理由让那条 plugin
    // 出一行 undeliverable（见下面 ③）。bundle 与 bundle 之间按 `manifest.plugins`
    // 的顺序（`BTreeMap`，稳定）。
    let mut skill_owners: BTreeMap<String, String> = manifest
        .skills
        .keys()
        .map(|name| (name.clone(), format!("清单里手写的 `skill/{name}`")))
        .collect();

    for (key, entry) in &manifest.plugins {
        let Some(source) = entry.source_harness.as_deref() else {
            continue;
        };
        let identity = plugin_identity(key, entry);

        // ① 定位：源家的安装记录里那条 `Kind::Plugin` 的 `install_path`。
        //    消息三态必须分得开，因为三种失败要用户做三件不同的事。
        let located = match source_states.get(source) {
            Some(SourceState::Scanned(caps)) => {
                let actual = caps
                    .iter()
                    .find(|c| c.id == CapId::new(Kind::Plugin, identity.clone()));
                match actual {
                    None => Err(format!(
                        "plugin `{identity}`：`{source}` 的安装记录里没有这个 plugin（§9.5）"
                    )),
                    Some(c) => match &c.spec {
                        Spec::Plugin {
                            install_path: Some(p),
                            ..
                        } => Ok(p.clone()),
                        // codex 恒为这一支：它的 config.toml 不记路径。
                        _ => Err(format!(
                            "plugin `{identity}`：`{source}` 的安装记录里没有安装路径（§9.5）"
                        )),
                    },
                }
            }
            _ => Err(format!(
                "plugin `{identity}`：源 harness `{source}` 本机没装，取不到 bundle（§9.5）"
            )),
        };
        let install_path = match located {
            Ok(p) => p,
            Err(reason) => {
                out.failures.insert(identity, reason);
                continue;
            }
        };

        // ② 版本只进 `source_id`（§9.5）。manifest 解不开 = 这条坏了，
        //    不退化成「没有版本」——那会让一条坏 bundle 与一条朴素 bundle 长得一样。
        let version = match read_bundle_manifest(&install_path) {
            Ok(v) => v,
            Err(e) => {
                out.failures.insert(
                    identity.clone(),
                    format!("plugin `{identity}`：bundle manifest 解不开：{e:#}"),
                );
                continue;
            }
        };
        let sha = match source_states.get(source) {
            Some(SourceState::Scanned(caps)) => caps
                .iter()
                .find(|c| c.id == CapId::new(Kind::Plugin, identity.clone()))
                .and_then(|c| match &c.spec {
                    Spec::Plugin { sha, .. } => sha.clone(),
                    _ => None,
                }),
            _ => None,
        };
        out.locations.insert(
            identity.clone(),
            LocatedBundle {
                install_path: install_path.clone(),
                source_id: bundle_source_id(source, version.as_deref(), sha.as_deref()),
            },
        );

        // ③ 拆：`<install_path>/skills/<skill>/` 每个目录各是一条独立能力。
        //    targets 减去源家——「写了就拆，源不交付」（§9.5）。
        let targets = entry.targets.minus(source, known);
        // 交付目标一个都不在 `harnesses:` 启用的家里时，拆出来的每一条都永远不会有
        // 行（被关掉的家不规划），整条 plugin 于是静默消失。宁可报一句「这条无法满足」，
        // 也不让它消失：同样是「清单里有一条根本无法满足的要求」（§9.5）。
        if !targets.is_empty() && !targets.any_in(&in_play) {
            out.failures.insert(
                identity.clone(),
                format!(
                    "plugin `{identity}`：交付目标是 {}，都不在 `harnesses:` 启用的家里（§9.5）",
                    render_targets(&targets)
                ),
            );
            continue;
        }
        // 撞名的理由攒起来，一个 plugin 撞了多条也不丢信息（`failures` 是
        // 「plugin 身份 → 一句理由」，逐条 insert 会让前一条被后一条盖掉）。
        let mut collisions: Vec<String> = Vec::new();
        for e in try_read_skill_entries(&install_path.join("skills"), home)? {
            if let Some(owner) = skill_owners.get(&e.name) {
                collisions.push(format!(
                    "拆出的 `skill/{}` 与{owner} 身份冲突（`CapId` 同名），这一条不交付",
                    e.name
                ));
                continue;
            }
            skill_owners.insert(
                e.name.clone(),
                format!("plugin `{identity}` 拆出的 `skill/{}`", e.name),
            );
            out.capabilities.push(Capability {
                id: CapId::new(Kind::Skill, e.name),
                origin: Origin::Plugin {
                    name: identity.clone(),
                },
                spec: Spec::Skill {
                    source: SkillSource::PluginBundle {
                        plugin: identity.clone(),
                    },
                    // 清单侧声明的内容路径由内容层解析（表里那个 `install_path`），
                    // 与 `lower` 给 `AgentsDir`/`Git` 的 `None` 同一个道理。
                    path: None,
                },
                targets: targets.clone(),
            });
        }
        if !collisions.is_empty() {
            out.failures.insert(
                identity.clone(),
                format!("plugin `{identity}`：{}（§9.5）", collisions.join("；")),
            );
        }

        // ④ 「装不下的部分」：**只探存在性、不解析内容**（§9.5）。
        //    它们进的是这条目的报告说明，**不为它们新建能力身份**——
        //    建了就会出现一堆永远 undeliverable 的行，把报告读成噪音。
        //    `degrade.<kind> = skip` 的那一项一个字都不留。
        let mut leftovers: Vec<&str> = Vec::new();
        let probe: [(Kind, &str); 3] = [
            (Kind::Hook, "hooks"),
            (Kind::Command, "commands"),
            (Kind::Agent, "agents"),
        ];
        for (kind, dir) in probe {
            if install_path.join(dir).is_dir()
                && entry.degrade.action_for(kind) == DegradeAction::Report
            {
                leftovers.push(kind.as_str());
            }
        }
        if (install_path.join(".mcp.json").is_file() || install_path.join("mcp.json").is_file())
            && entry.degrade.action_for(Kind::Mcp) == DegradeAction::Report
        {
            leftovers.push(Kind::Mcp.as_str());
        }
        if !leftovers.is_empty() {
            out.notes.insert(
                identity,
                format!(
                    "bundle `{source}` 里还有 {}，未拆解（degrade.<kind> = report；要静音把它改成 skip）（§9.5）",
                    leftovers.join("、")
                ),
            );
        }
    }

    // 稳定的顺序：与 `lower` 一样按 label 排。同名碰撞已在 ③ 处解决（先到先得），
    // 到这里的 `CapId` 互不相同；排序只为让同一次输入两次运行一样（§9.10 的幂等）。
    out.capabilities.sort_by_key(|c| c.id.label());
    Ok(out)
}

/// 把一份目标集写成报告里那句人话：`` `kimi-code` ``、`` `codex`、`kimi-code` ``。
///
/// 只有 `Only` 会走到这里（`minus` 的产物恒是 `Only`）；`All` 给一句兜底，
/// 免得将来哪条路径把 `All` 递进来时得到一个空字符串。
fn render_targets(targets: &Targets) -> String {
    match targets {
        Targets::All => "全部".to_string(),
        Targets::Only(list) => list
            .iter()
            .map(|h| format!("`{h}`"))
            .collect::<Vec<_>>()
            .join("、"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::mcp::McpTarget;
    use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets};
    use crate::lower::SourceHarnessTable;
    use crate::manifest::Manifest;
    use crate::testutil::TempHome;
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::rc::Rc;

    #[test]
    fn source_id_drops_missing_segments() {
        assert_eq!(
            bundle_source_id("claude-code", Some("6.4.1"), Some("abc123")),
            "bundle@claude-code@6.4.1@abc123"
        );
        assert_eq!(
            bundle_source_id("claude-code", Some("6.4.1"), None),
            "bundle@claude-code@6.4.1"
        );
        assert_eq!(
            bundle_source_id("claude-code", None, None),
            "bundle@claude-code"
        );
        assert_eq!(
            bundle_source_id("codex", None, Some("abc123")),
            "bundle@codex@abc123"
        );
    }

    #[test]
    fn a_bundle_without_a_manifest_has_no_version() {
        let t = TempHome::new();
        t.mkdir("bundle/skills/foo");
        assert_eq!(
            read_bundle_manifest(&t.path("bundle")).expect("缺 manifest 不是错误"),
            None
        );
    }

    #[test]
    fn the_claude_manifest_shape_is_read() {
        let t = TempHome::new();
        t.write(
            "bundle/.claude-plugin/plugin.json",
            r#"{"name": "superpowers", "version": "6.4.1"}"#,
        );
        assert_eq!(
            read_bundle_manifest(&t.path("bundle")).unwrap().as_deref(),
            Some("6.4.1")
        );
    }

    /// kimi 的第二种形状：manifest 就躺在 bundle 根目录下。
    #[test]
    fn the_kimi_root_manifest_shape_is_read() {
        let t = TempHome::new();
        t.write("bundle/kimi.plugin.json", r#"{"version": "6.3.0"}"#);
        assert_eq!(
            read_bundle_manifest(&t.path("bundle")).unwrap().as_deref(),
            Some("6.3.0")
        );
    }

    /// 存在但解不开 = 报错，**不**退化成 `None`。半截 JSON 说明这份 bundle 是坏的，
    /// 而「坏」与「朴素」在报告里必须是两件事（Task 6 把它落成该条目的一条 undeliverable）。
    #[test]
    fn a_broken_manifest_is_an_error_not_a_missing_version() {
        let t = TempHome::new();
        t.write("bundle/.claude-plugin/plugin.json", r#"{"version": "6.4"#);
        let e = read_bundle_manifest(&t.path("bundle")).expect_err("半截 JSON 是坏的");
        assert!(format!("{e:#}").contains("plugin.json"), "{e:#}");
    }

    /// 本机在版本位写着的字面量 `unknown` 不变成假版本段（`normalize_locator` 的职责）。
    #[test]
    fn the_literal_unknown_is_not_a_version() {
        let t = TempHome::new();
        t.write(
            "bundle/.claude-plugin/plugin.json",
            r#"{"version": "unknown"}"#,
        );
        assert_eq!(read_bundle_manifest(&t.path("bundle")).unwrap(), None);
    }

    const KNOWN: [&str; 3] = ["claude-code", "codex", "kimi-code"];

    /// 一条「claude-code 装了名字叫 `name` 的 plugin、安装路径在 `install_path`」的能力。
    fn plugin_cap(name: &str, install_path: std::path::PathBuf, sha: Option<&str>) -> Capability {
        Capability {
            id: CapId::new(Kind::Plugin, name),
            origin: Origin::Adopted {
                harness: "claude-code".into(),
            },
            spec: Spec::Plugin {
                marketplace: Some("official".into()),
                entry: name.to_string(),
                version: Some("6.4.1".into()),
                sha: sha.map(str::to_string),
                install_path: Some(install_path),
                enabled: true,
            },
            targets: Targets::All,
        }
    }

    /// 一个「claude-code 装了 superpowers」的源状态：安装路径指向 `t` 下那个 bundle。
    fn claude_with(
        install_path: std::path::PathBuf,
        sha: Option<&str>,
    ) -> BTreeMap<String, SourceState> {
        let mut m = BTreeMap::new();
        m.insert(
            "claude-code".to_string(),
            SourceState::Scanned(vec![plugin_cap("superpowers", install_path, sha)]),
        );
        m
    }

    fn manifest_with_source_harness() -> Manifest {
        Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n",
        )
        .expect("清单合法")
    }

    /// 一个装了 superpowers 的假 HOME：`bundle/` 是安装目录，里面两个 skill。
    fn home_with_bundle() -> (TempHome, std::path::PathBuf) {
        let t = TempHome::new();
        let root = t.path("bundle");
        t.write(
            "bundle/skills/brainstorming/SKILL.md",
            "---\nname: b\n---\n",
        );
        t.write("bundle/skills/tdd/SKILL.md", "---\nname: t\n---\n");
        t.write(
            "bundle/.claude-plugin/plugin.json",
            r#"{"version": "6.4.1"}"#,
        );
        (t, root)
    }

    /// §9.5「交付什么」：`<install_path>/skills/<skill>/` 下每个目录各是一条
    /// 独立的 `Kind::Skill`，`Origin::Plugin`，`SkillSource::PluginBundle`。
    /// 它们**不是** `lower` 的产出——skill 目录名只有读了 bundle 才知道。
    #[test]
    fn each_skill_directory_becomes_a_capability() {
        let (t, root) = home_with_bundle();
        let m = manifest_with_source_harness();
        let d = decompose_bundles(&m, &claude_with(root.clone(), None), &KNOWN, &t.home).unwrap();

        let mut names: Vec<&str> = d.capabilities.iter().map(|c| c.id.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["brainstorming", "tdd"]);
        assert!(d.failures.is_empty(), "{:?}", d.failures);

        let cap = &d.capabilities[0];
        assert_eq!(cap.id.kind, Kind::Skill);
        assert_eq!(
            cap.origin,
            Origin::Plugin {
                name: "superpowers".into()
            }
        );
        match &cap.spec {
            Spec::Skill { source, path } => {
                assert_eq!(
                    source,
                    &SkillSource::PluginBundle {
                        plugin: "superpowers".into()
                    }
                );
                assert_eq!(*path, None, "清单侧声明的内容路径由内容层解析，不在这里填");
            }
            other => panic!("{other:?}"),
        }
    }

    /// §9.5「开关与目标」：交付目标 = `targets` − 源 harness。
    /// 清单没写 `targets`（默认 `All`），于是拆出来的 skill 目标是另外两家。
    #[test]
    fn the_source_harness_is_subtracted_from_targets() {
        let (t, root) = home_with_bundle();
        let m = manifest_with_source_harness();
        let d = decompose_bundles(&m, &claude_with(root, None), &KNOWN, &t.home).unwrap();
        assert_eq!(
            d.capabilities[0].targets,
            Targets::Only(vec!["codex".into(), "kimi-code".into()])
        );
    }

    /// §9.5：定位得到时报文取「安装记录里的路径」，并组出 `source_id`。
    #[test]
    fn the_bundle_is_located_and_gets_a_source_id() {
        let (t, root) = home_with_bundle();
        let m = manifest_with_source_harness();
        let d = decompose_bundles(
            &m,
            &claude_with(root.clone(), Some("abc123")),
            &KNOWN,
            &t.home,
        )
        .unwrap();
        let located = d.locations.get("superpowers").expect("定位到了");
        assert_eq!(located.install_path, root);
        assert_eq!(located.source_id, "bundle@claude-code@6.4.1@abc123");
    }

    /// 源 harness 本机没装：整条 undeliverable，报文点名是哪一家没装。
    #[test]
    fn an_uninstalled_source_harness_fails_the_whole_entry() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let mut states = BTreeMap::new();
        states.insert("claude-code".to_string(), SourceState::NotInstalled);

        let d = decompose_bundles(&m, &states, &KNOWN, &t.home).unwrap();
        assert!(d.capabilities.is_empty());
        let msg = d.failures.get("superpowers").expect("要报出来");
        assert!(msg.contains("claude-code"), "{msg}");
        assert!(msg.contains("没装"), "{msg}");
    }

    /// 源家装了别的 plugin、没有这一条：与「那家没装」是**两件事**——
    /// 前者要用户去装这个 plugin，后者要用户去装那家 harness。
    #[test]
    fn a_harness_without_this_plugin_says_so_distinctly() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let states = claude_with(t.path("does-not-matter"), None);
        // 把那条记录的名字改掉，模拟「装了 claude-code，但装的是别的 plugin」
        let mut states = states;
        if let Some(SourceState::Scanned(caps)) = states.get_mut("claude-code") {
            caps[0].id = CapId::new(Kind::Plugin, "something-else");
        }

        let d = decompose_bundles(&m, &states, &KNOWN, &t.home).unwrap();
        let msg = d.failures.get("superpowers").expect("要报出来");
        assert!(msg.contains("没有这个 plugin"), "{msg}");
    }

    /// **codex 恒为这一支**：它的安装记录里根本没有路径（`Spec::Plugin::install_path`
    /// 永远是 `None`，`src/adapters/codex.rs:113-119` 是那条已知局限）。
    /// 报文必须与「没装这个 plugin」分得开——否则用户会去重装一个已经装好的 plugin。
    #[test]
    fn a_record_without_an_install_path_says_so_distinctly() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let mut states = claude_with(t.path("irrelevant"), None);
        if let Some(SourceState::Scanned(caps)) = states.get_mut("claude-code") {
            if let Spec::Plugin { install_path, .. } = &mut caps[0].spec {
                *install_path = None;
            }
        }

        let d = decompose_bundles(&m, &states, &KNOWN, &t.home).unwrap();
        let msg = d.failures.get("superpowers").expect("要报出来");
        assert!(msg.contains("没有安装路径"), "{msg}");
        assert!(!msg.contains("没有这个 plugin"), "两句必须分得开：{msg}");
    }

    /// §9.5「装不下的部分」：`hooks/` 只探存在性、不解析内容，按 `degrade.<kind>`
    /// 逐 kind 决定进报告还是丢弃。默认是 `report`。
    #[test]
    fn leftover_parts_land_in_the_note_when_degrade_says_report() {
        let (t, root) = home_with_bundle();
        t.mkdir("bundle/hooks");
        let m = manifest_with_source_harness();
        let d = decompose_bundles(&m, &claude_with(root, None), &KNOWN, &t.home).unwrap();
        let note = d.notes.get("superpowers").expect("有装不下的部分就要留话");
        assert!(note.contains("hook"), "{note}");
        assert!(
            !note.contains("command"),
            "commands/ 不存在，不许凭空说：{note}"
        );
    }

    /// `degrade.hooks: skip` → 那一项一个字都不留。
    #[test]
    fn degrade_skip_drops_a_leftover_part_silently() {
        let (t, root) = home_with_bundle();
        t.mkdir("bundle/hooks");
        t.mkdir("bundle/commands");
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n    degrade:\n      hooks: skip\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &claude_with(root, None), &KNOWN, &t.home).unwrap();
        let note = d
            .notes
            .get("superpowers")
            .expect("commands/ 还在，注释仍要有");
        assert!(!note.contains("hook"), "skip 的必须消失：{note}");
        assert!(note.contains("command"), "{note}");
    }

    /// 没有 `source_harness:` 的条目**不进结果表**——它不是拆解请求。
    #[test]
    fn a_plugin_without_a_source_harness_is_not_decomposed() {
        let (t, _root) = home_with_bundle();
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: github\n    repo: x/y\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &BTreeMap::new(), &KNOWN, &t.home).unwrap();
        assert!(d.capabilities.is_empty());
        assert!(d.failures.is_empty());
        assert!(d.notes.is_empty());
        assert!(d.locations.is_empty());
    }

    /// 源家被 `harnesses:` 关掉也照样扫：`source_harness:` 只请求**读**它，
    /// 不请求往它写（§9.5）。这一条钉的是扫描那一步的选择，
    /// 「不因此多出一节 diff」由 Task 9 钉。
    #[test]
    fn a_disabled_harness_is_still_scanned_when_it_is_a_source() {
        let (t, root) = home_with_bundle();
        // claude-code 被关掉、codex 被关掉，只剩 kimi 是「开着」的目标
        t.write(
            "env.yaml",
            "version: 1\n\nharnesses:\n  claude-code:\n    enabled: false\n  codex:\n    enabled: false\n",
        );
        let manifest = Manifest::parse(
            "version: 1\n\n\
             harnesses:\n  claude-code:\n    enabled: false\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n",
        )
        .expect("清单合法");
        let table = SourceHarnessTable::new(&manifest);
        let registry = crate::adapters::registry(&t.home);

        // 造一个真实的 claude 安装记录，让 read_state 真的能扫出那条 plugin
        t.mkdir(".claude");
        t.write(
            ".claude/plugins/installed_plugins.json",
            &format!(
                r#"{{"plugins":{{"superpowers@official":[{{"installPath":"{}","version":"6.4.1"}}]}}}}"#,
                root.display()
            ),
        );
        t.write(".claude/settings.json", r#"{"enabledPlugins":{}}"#);

        let states = scan_source_harnesses(&table, &registry, &BTreeMap::new()).unwrap();
        match states.get("claude-code") {
            Some(SourceState::Scanned(caps)) => assert!(
                caps.iter()
                    .any(|c| c.id == CapId::new(Kind::Plugin, "superpowers")),
                "被关掉的家仍然要被读到"
            ),
            other => panic!("claude-code 应该被扫到：{other:?}"),
        }
    }

    /// 源家本机没装（`detect()` 假）→ `NotInstalled`，**不是**空表——
    /// 「没装」与「装了但什么都没扫到」必须是两种状态（否则「没装」会被报成
    /// 「没有这个 plugin」，指错方向）。
    #[test]
    fn a_missing_source_harness_scans_as_not_installed() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let table = SourceHarnessTable::new(&m);
        let registry = crate::adapters::registry(&t.home);
        let states = scan_source_harnesses(&table, &registry, &BTreeMap::new()).unwrap();
        assert!(matches!(
            states.get("claude-code"),
            Some(SourceState::NotInstalled)
        ));
    }

    /// **只扫被声明的那些家。** 没有任何 `source_harness:` 时一次 `read_state` 都不发。
    #[test]
    fn only_declared_harnesses_are_scanned() {
        let t = TempHome::new();
        let m =
            Manifest::parse("version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n")
                .expect("清单合法");
        let table = SourceHarnessTable::new(&m);
        assert_eq!(table.declared(), BTreeSet::new());
        let registry = crate::adapters::registry(&t.home);
        assert!(
            scan_source_harnesses(&table, &registry, &BTreeMap::new())
                .unwrap()
                .is_empty()
        );
    }

    /// `read_state` 每调一次记一笔的假 adapter。`detect` 恒真，好让「不读」这件事
    /// 只由 `read_state` 的计数说话。
    struct CountingAdapter {
        reads: Rc<Cell<usize>>,
    }

    impl HarnessAdapter for CountingAdapter {
        fn name(&self) -> &str {
            "claude-code"
        }
        fn detect(&self) -> bool {
            true
        }
        fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
            self.reads.set(self.reads.get() + 1);
            Ok(vec![plugin_cap(
                "superpowers",
                PathBuf::from("/nowhere"),
                None,
            )])
        }
        fn supports(&self, _cap: &Capability) -> Support {
            Support::Native
        }
        fn skills_target_dir(&self) -> Option<PathBuf> {
            None
        }
        fn mcp_target(&self) -> Option<McpTarget> {
            None
        }
    }

    /// B#3：同一个家既是启用的目标又是清单声明的源时，`plan_for` 读过的那份由
    /// `already` 交进来，扫描这一步**不该再读第二遍**。两次 `read_state()` 之间盘面
    /// 会变，共用一份才是「拆解不许有第二条读路径」的本意。
    #[test]
    fn an_already_scanned_source_is_not_read_again() {
        let table = SourceHarnessTable::new(&manifest_with_source_harness());
        let reads = Rc::new(Cell::new(0));
        let registry: Vec<Box<dyn HarnessAdapter>> = vec![Box::new(CountingAdapter {
            reads: reads.clone(),
        })];

        // 反面控制：不交 `already` 时确实会读一次——否则下面那个 0 可能恒真。
        let fresh = scan_source_harnesses(&table, &registry, &BTreeMap::new()).unwrap();
        assert_eq!(reads.get(), 1, "没有 already 时必须自己读");
        assert!(matches!(
            fresh.get("claude-code"),
            Some(SourceState::Scanned(_))
        ));

        let mut already = BTreeMap::new();
        already.insert(
            "claude-code".to_string(),
            SourceState::Scanned(vec![plugin_cap(
                "superpowers",
                PathBuf::from("/nowhere"),
                None,
            )]),
        );
        let reused = scan_source_harnesses(&table, &registry, &already).unwrap();
        assert_eq!(reads.get(), 1, "already 里有 → 不该再读第二遍");
        match reused.get("claude-code") {
            Some(SourceState::Scanned(caps)) => assert_eq!(caps.len(), 1, "原样搬用 already"),
            other => panic!("应当原样搬用 already：{other:?}"),
        }
    }

    /// `already` 说「没装」时也照搬：不再 `read_state()` 一次。
    #[test]
    fn an_already_known_missing_source_is_not_probed_again() {
        let table = SourceHarnessTable::new(&manifest_with_source_harness());
        let reads = Rc::new(Cell::new(0));
        let registry: Vec<Box<dyn HarnessAdapter>> = vec![Box::new(CountingAdapter {
            reads: reads.clone(),
        })];
        let mut already = BTreeMap::new();
        already.insert("claude-code".to_string(), SourceState::NotInstalled);

        let states = scan_source_harnesses(&table, &registry, &already).unwrap();
        assert!(matches!(
            states.get("claude-code"),
            Some(SourceState::NotInstalled)
        ));
        assert_eq!(reads.get(), 0, "already 里有 → 连读都不该发起");
    }

    /// B#4：清单里手写的 `skill/tdd` 与 bundle 拆出的同名 skill 是**同一条身份**，
    /// 不可能都交付。清单那条留下（用户显式写的优先），拆出来那条记一条失败理由——
    /// 不静默覆盖，也不静默丢。
    #[test]
    fn a_bundle_skill_colliding_with_a_manifest_skill_is_reported_not_delivered() {
        let (t, root) = home_with_bundle();
        let m = Manifest::parse(
            "version: 1\n\n\
             skills:\n  tdd:\n    source: agents-dir\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &claude_with(root.clone(), None), &KNOWN, &t.home).unwrap();

        let names: Vec<&str> = d.capabilities.iter().map(|c| c.id.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["brainstorming"],
            "撞名的 tdd 不该再产生第二条能力"
        );
        let reason = d.failures.get("superpowers").expect("撞名必须报出来");
        assert!(reason.contains("skill/tdd"), "{reason}");
        assert!(reason.contains("身份冲突"), "{reason}");
    }

    /// 一个 plugin 撞了多条时，理由要**全部**带上——`failures` 是「plugin 身份 →
    /// 一句理由」的位置，逐条 `insert` 会让前一条被后一条静默盖掉。
    #[test]
    fn several_collisions_in_one_bundle_are_all_in_the_reason() {
        let (t, root) = home_with_bundle();
        let m = Manifest::parse(
            "version: 1\n\n\
             skills:\n  tdd:\n    source: agents-dir\n  brainstorming:\n    source: agents-dir\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &claude_with(root.clone(), None), &KNOWN, &t.home).unwrap();

        assert!(d.capabilities.is_empty(), "两条都撞名 → 一条都不产出");
        let reason = d.failures.get("superpowers").expect("撞名必须报出来");
        assert!(reason.contains("skill/brainstorming"), "{reason}");
        assert!(reason.contains("skill/tdd"), "{reason}");
    }

    /// bundle 与 bundle 之间按同一套规则：先到先得（`manifest.plugins` 是 BTreeMap，
    /// 顺序稳定），后到的那条记失败理由。
    #[test]
    fn two_bundles_shipping_the_same_skill_collide_the_same_way() {
        let (t, root) = home_with_bundle();
        let second = t.path("bundle2");
        t.write(
            "bundle2/skills/brainstorming/SKILL.md",
            "---\nname: b\n---\n",
        );
        let mut states = BTreeMap::new();
        states.insert(
            "claude-code".to_string(),
            SourceState::Scanned(vec![
                plugin_cap("aaa", root, None),
                plugin_cap("bbb", second, None),
            ]),
        );
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  bbb:\n    source: claude-marketplace\n    marketplace: official\n    entry: bbb\n    source_harness: claude-code\n  aaa:\n    source: claude-marketplace\n    marketplace: official\n    entry: aaa\n    source_harness: claude-code\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &states, &KNOWN, &t.home).unwrap();

        let owners: Vec<&str> = d
            .capabilities
            .iter()
            .filter(|c| c.id.name == "brainstorming")
            .map(|c| match &c.origin {
                Origin::Plugin { name } => name.as_str(),
                other => panic!("拆出来的必须是 plugin 来源：{other:?}"),
            })
            .collect();
        assert_eq!(owners, vec!["aaa"], "aaa 在前，brainstorming 归它");
        let reason = d.failures.get("bbb").expect("bbb 那条要报出来");
        assert!(reason.contains("skill/brainstorming"), "{reason}");
        assert!(reason.contains("plugin `aaa`"), "{reason}");
    }

    /// B#5：交付目标全是**被 `harnesses:` 关掉的家**时，拆出来的每一条都永远不会有
    /// 行，整条 plugin 于是静默消失。现在报一条失败理由，不让它消失。
    #[test]
    fn a_bundle_targeting_only_disabled_harnesses_is_reported() {
        let (t, root) = home_with_bundle();
        let m = Manifest::parse(
            "version: 1\n\n\
             harnesses:\n  kimi-code:\n    enabled: false\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n    targets: [kimi-code]\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &claude_with(root.clone(), None), &KNOWN, &t.home).unwrap();

        assert!(d.capabilities.is_empty(), "一条都交付不出去，不该产生能力");
        let reason = d
            .failures
            .get("superpowers")
            .expect("要报出来，不能静默消失");
        assert!(reason.contains("kimi-code"), "{reason}");
        assert!(reason.contains("harnesses:"), "{reason}");
    }

    /// 反面控制：同一份 `targets:` 落在**被启用**的家上时照常拆解——
    /// 上面那条断言不是恒真。
    #[test]
    fn a_bundle_targeting_an_enabled_harness_is_decomposed_normally() {
        let (t, root) = home_with_bundle();
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n    targets: [kimi-code]\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &claude_with(root.clone(), None), &KNOWN, &t.home).unwrap();

        assert_eq!(d.capabilities.len(), 2);
        assert!(d.failures.is_empty(), "{:?}", d.failures);
    }
}
