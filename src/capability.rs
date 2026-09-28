use std::collections::BTreeMap;
use std::path::PathBuf;

/// 能力种类。`Plugin` 是一等公民——spec §2.1 第 2 条：三家 harness 都有 plugin 机制，
/// v1 以 bundle manifest 对账为主，只在目标 harness 缺 manifest 时才降级拆解。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
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

/// 一个 MCP 的两种传输形态。**恰好一种**——这是清单校验与三家适配器读写的共同契约。
///
/// 为什么值得单独一个类型：这两种形态**没有公共字段**（stdio 有命令、没有 URL；http 反之），
/// 用一个「可有可无的 command」去表示会让每个读取点都得自己判空——而漏判一次就是一个
/// 静默的错判。类型在这里替所有下游把关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// 本地进程：起一个命令，走 stdio 通信。
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    /// 远端：一个 URL，可选的请求头（放凭据的地方，所以值同样必须是 `${VAR}` 引用）。
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl McpTransport {
    /// 人读的一行摘要。**只渲染引用，字面量一律 `***`**——`actual` 侧的值是从活配置里
    /// 读回来的，可能就是明文密钥（§3 决策 3）。
    ///
    /// 引用本身没有秘密可泄露（`${CONTEXT7_API_KEY}` 只是个变量名），而且**必须**渲染出来：
    /// 只显示键名时，把一个 header 从 `${A}` 改成 `${B}` 会让 `diff` 的 update 行左右
    /// 一模一样（`[headers: Authorization] → [headers: Authorization]`），等于没说改了什么。
    pub(crate) fn summary(&self) -> String {
        match self {
            McpTransport::Stdio { command, args, env } => {
                let mut s = command.clone();
                for a in args {
                    s.push(' ');
                    s.push_str(a);
                }
                if !env.is_empty() {
                    s.push_str(&format!(" [env: {}]", pairs(env)));
                }
                s
            }
            McpTransport::Http { url, headers } => {
                let mut s = format!("http {url}");
                if !headers.is_empty() {
                    s.push_str(&format!(" [headers: {}]", pairs(headers)));
                }
                s
            }
        }
    }
}

/// 一串 env / header 渲染成 `K=${K},W=***`：键序由 `BTreeMap` 保证稳定，值那一侧见
/// [`shown_value`]。
fn pairs(map: &std::collections::BTreeMap<String, String>) -> String {
    map.iter()
        .map(|(k, v)| format!("{k}={}", shown_value(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// 一个 env / header 值渲染成什么：**认得出的引用照原样**（那只是个变量名，`${K:-}` 的
/// `:-` 与 `Bearer ` 前缀也是要看见的信息），其余一律 `***`。
///
/// 判据借清单门禁那条（[`crate::secrets::is_manifest_value`]）而不是「看起来像不像」的
/// 启发式：门禁收什么，这里就显示什么，两份规则不会漂。
fn shown_value(value: &str) -> &str {
    if crate::secrets::is_manifest_value(value) {
        value
    } else {
        "***"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Skill {
        source: SkillSource,
        path: Option<PathBuf>,
    },
    Mcp {
        transport: McpTransport,
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

    /// §9.5：交付目标 = `targets` − `source_harness`。
    ///
    /// `known` 是「有哪几家 harness」——`All` 不知道这件事，只有调用方知道
    /// （`compute` 手里那几家的名字，与 `KNOWN_HARNESSES` 相等）。当参数传而不是
    /// 从 `manifest` 读，是为了让本模块继续不认识「清单」这个概念。
    pub fn minus(&self, harness: &str, known: &[&str]) -> Targets {
        match self {
            Targets::All => Targets::Only(
                known
                    .iter()
                    .filter(|h| **h != harness)
                    .map(|h| (*h).to_string())
                    .collect(),
            ),
            Targets::Only(list) => Targets::Only(
                list.iter()
                    .filter(|h| h.as_str() != harness)
                    .cloned()
                    .collect(),
            ),
        }
    }

    /// 空目标集：这个能力谁都不交付。
    pub fn is_empty(&self) -> bool {
        matches!(self, Targets::Only(list) if list.is_empty())
    }

    /// 目标集里有没有 `in_play` 里的任何一家。
    ///
    /// `All` 恒真——它包含所有家。`Only` 则可能整份名单都点名了没被启用的家
    /// （`targets: [kimi-code]` 而 `harnesses.kimi-code.enabled = false`），
    /// 那种条目一条行都产生不出来，§9.5 要把它当「无法满足」报出来。
    pub fn any_in(&self, in_play: &[&str]) -> bool {
        match self {
            Targets::All => true,
            Targets::Only(list) => list.iter().any(|h| in_play.contains(&h.as_str())),
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
    /// 无法判定：跨 kind、信息不足、或 version 不是「version 形」（sha 形目录名 / 分支名）。
    /// diff 不再把 Unknown 折成 in-sync，而是报出可见的 `? unknown`（spec §2.1 第 3 条）。
    Unknown,
}

/// 把「没有值」和「有值但是哨兵」都收敛成 None，交给 compare_versions 的 `sha: None`
/// 守卫去降级到 version 分支或 Unknown。实测哨兵：`unknown`（大小写不一）、空串、纯空白。
pub fn normalize_locator(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("unknown") {
        return None;
    }
    Some(v.to_string())
}

/// MCP 的人类可读摘要。**只渲染键名，绝不渲染 env 值**——desired 侧的值永远是 `${VAR}`
/// 引用，但 actual 侧是 adapter 从 `~/.claude.json` 这类活配置读出来的，可能就是明文真密钥。
/// spec §3 决策 3：密钥永不进任何产物。
///
/// 可见性提成 `pub(crate)` 是因为 `diff` 要拿它渲染 `update` 的 from/to——
/// 再写一个同样的渲染函数就等于有了两份「什么算同一个 MCP」的定义。
pub(crate) fn mcp_summary(transport: &McpTransport) -> String {
    transport.summary()
}

/// 判据（spec §2.1 第 3 条，2026-09-25 修订）：plugin 用 entry 名作身份、版本比较见
/// [`compare_plugins`]；skill git 源比 `ref`；mcp 比 command/args/env；其余 Unknown。
pub fn compare_versions(desired: &Spec, actual: &Spec) -> VersionMatch {
    match (desired, actual) {
        (Spec::Plugin { .. }, Spec::Plugin { .. }) => compare_plugins(desired, actual),
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
        (Spec::Mcp { transport: dt }, Spec::Mcp { transport: at }) => {
            if dt == at {
                VersionMatch::Same
            } else {
                VersionMatch::Different {
                    from: mcp_summary(at),
                    to: mcp_summary(dt),
                }
            }
        }
        _ => VersionMatch::Unknown,
    }
}

/// plugin 的版本判据（spec §2.1 第 3 条，2026-09-25 修订）：
/// 1. 两边 SHA 都有且**不相等** ⇒ Different（内容不同，最硬的一刀）；
/// 2. 否则（都 None、或相等——相等不再直接判 Same，见 §2.2 第 7 条）比 version 形：
///    两边都能归一化出「version 形」才可比，归一化后相等 ⇒ Same、不等 ⇒ Different
///    （from = 现状原值、to = 期望原值）；
/// 3. 其余 ⇒ Unknown。
fn compare_plugins(desired: &Spec, actual: &Spec) -> VersionMatch {
    let (
        Spec::Plugin {
            sha: d_sha,
            version: d_ver,
            ..
        },
        Spec::Plugin {
            sha: a_sha,
            version: a_ver,
            ..
        },
    ) = (desired, actual)
    else {
        unreachable!("compare_plugins 只在两边都是 Plugin 时调用");
    };

    if let (Some(d), Some(a)) = (d_sha, a_sha) {
        if d != a {
            return VersionMatch::Different {
                from: a.clone(),
                to: d.clone(),
            };
        }
    }

    match (d_ver, a_ver) {
        (Some(d), Some(a)) => match (normalize_version(d), normalize_version(a)) {
            (Some(dn), Some(an)) => {
                if dn == an {
                    VersionMatch::Same
                } else {
                    VersionMatch::Different {
                        from: a.clone(),
                        to: d.clone(),
                    }
                }
            }
            _ => VersionMatch::Unknown,
        },
        _ => VersionMatch::Unknown,
    }
}

/// 记录型 plugin（§10.3）的版本对账结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordedMatch {
    Same,
    Different,
    /// 有一边不是 version 形（sha 形目录名、分支名、哨兵、本机版本读不出）——
    /// 按 §2.1 第 3 条**不报假 drift**。
    Incomparable,
}

/// 清单记的版本（`source: snapshot` 的 `versions.<harness>`）vs 本机 `version` 位。
///
/// 与 [`compare_plugins`] 共用 [`normalize_version`]：**可比性只有一份定义**。
/// 分开写两份必然出现「scan 记了、diff 说不可比」的两套话——`normalize_version`
/// 的 rustdoc 里就是这么约定的。
pub(crate) fn compare_recorded_version(recorded: &str, actual: Option<&str>) -> RecordedMatch {
    match (
        normalize_version(recorded),
        actual.and_then(normalize_version),
    ) {
        (Some(r), Some(a)) if r == a => RecordedMatch::Same,
        (Some(_), Some(_)) => RecordedMatch::Different,
        _ => RecordedMatch::Incomparable,
    }
}

/// 归一化 version 串：去前导 `v`/`V`；仅当结果含小数点（`6.3.0` 形）才返回 Some，
/// sha 形目录名（`db467cc56673`）、分支名（`main`）等不含小数点 → None（不可比，落 Unknown）。
///
/// `pub(crate)` 是因为 `scan` 收养 plugin 时要判「这个 version 值不值得记进 `ref:`」——
/// 那必须与 diff 的「可比」判据同源，否则会出现「scan 记了、diff 说不可比」的两套话。
pub(crate) fn normalize_version(value: &str) -> Option<String> {
    let v = value.trim();
    let v = v
        .strip_prefix('v')
        .or_else(|| v.strip_prefix('V'))
        .unwrap_or(v);
    if v.contains('.') {
        Some(v.to_string())
    } else {
        None
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

    /// 账本把 `Kind` 序列化成 `as_str()` 的那个串（§9.3）。serde 的 rename 与
    /// `as_str()`/`parse()` 是**独立的第三处拼写**，必须钉成相等——否则账本里
    /// 会出现一个 `parse()` 认不出、而 serde 认得出的 kind。
    #[test]
    fn kind_serde_uses_the_same_spelling_as_as_str() {
        for k in Kind::all() {
            let json = serde_json::to_string(&k).expect("serialize");
            assert_eq!(json, format!("\"{}\"", k.as_str()), "{k:?}");
            let back: Kind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, k);
        }
        assert!(serde_json::from_str::<Kind>("\"nonsense\"").is_err());
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
                transport: McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec!["-y".into(), "@upstash/context7-mcp".into()],
                    env: BTreeMap::new(),
                },
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
    fn same_sha_across_differing_version_strings_is_different() {
        // 修订核心：sha 相同不再判 Same（claude 的 gitCommitSha 陈旧，§2.2 第 7 条），
        // version 6.4.1 vs v6.3.0 归一化后不同 → Different。
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        assert_eq!(
            compare_versions(
                &plugin(Some("6.4.1"), Some(sha)),
                &plugin(Some("v6.3.0"), Some(sha))
            ),
            VersionMatch::Different {
                from: "v6.3.0".into(),
                to: "6.4.1".into()
            }
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
    fn leading_v_is_normalized_away() {
        // claude 写 `6.4.1`，kimi 写 `v6.4.1`，去前导 v 后应判 Same（spec §2.1 第 3 条规则 2）。
        assert_eq!(
            compare_versions(&plugin(Some("6.4.1"), None), &plugin(Some("v6.4.1"), None)),
            VersionMatch::Same
        );
    }

    #[test]
    fn non_version_form_locators_are_unknown() {
        // sha 形目录名（claude 8/19 实测）与分支名（`main`）不含小数点 → 不是 version 形，
        // 判不了 → Unknown，绝不报假 drift（spec §2.1 第 3 条规则 3、§2.2 第 7 条覆盖面）。
        assert_eq!(
            compare_versions(
                &plugin(Some("db467cc56673"), None),
                &plugin(Some("v6.3.0"), None)
            ),
            VersionMatch::Unknown
        );
        assert_eq!(
            compare_versions(&plugin(Some("main"), None), &plugin(Some("main"), None)),
            VersionMatch::Unknown
        );
    }

    #[test]
    fn equal_sha_without_version_is_unknown_not_same() {
        // 「sha 相等 ⇒ Same」的废除核心：sha 相同但两边都没 version，无法确认内容一致
        // （gitCommitSha 可能陈旧，§2.2 第 7 条），只能 Unknown，不能撒谎 Same。
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        assert_eq!(
            compare_versions(&plugin(None, Some(sha)), &plugin(None, Some(sha))),
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
            transport: McpTransport::Stdio {
                command: cmd.into(),
                args: args.into_iter().map(String::from).collect(),
                env: env
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            },
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
        // 上面三对 fixture 全都有 command 或 args 上的差异，`de == ae` 那一段其实没被
        // 看过一眼：把它删掉测试仍全绿。这一对 command+args 逐字相同、**只差 env 值**，
        // 是唯一能杀掉该变异的形状。引用照原样渲染，所以这次 from/to 是**看得出差别**的。
        assert_eq!(
            compare_versions(
                &mcp("npx", vec!["-y", "pkg"], vec![("K", "${B}")]),
                &mcp("npx", vec!["-y", "pkg"], vec![("K", "${A}")])
            ),
            VersionMatch::Different {
                from: "npx -y pkg [env: K=${A}]".into(),
                to: "npx -y pkg [env: K=${B}]".into(),
            }
        );
        // env 逐字相同（键名与值都相同）才算 Same：这一对与上一对只在 env 的值上有差别
        assert_eq!(
            compare_versions(
                &mcp("npx", vec!["-y", "pkg"], vec![("K", "${A}")]),
                &mcp("npx", vec!["-y", "pkg"], vec![("K", "${A}")])
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
            transport: McpTransport::Stdio {
                command: cmd.into(),
                args: args.into_iter().map(String::from).collect(),
                env: Default::default(),
            },
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

    /// 摘要的渲染规则：**引用照原样，其余一律 `***`**。这一条的两头都重要——
    /// `actual` 侧的 env 值由 adapter 从活配置里读出来，可能就是明文真密钥（安全约束），
    /// 而只渲染键名会让 `${A}` → `${B}` 这种改动在报告里看不见（可用性约束，2026-09-28）。
    /// 所以每条渲染串都逐字钉死：键名顺序是契约的一部分（BTreeMap 字节升序 + `,` 分隔，
    /// 插入顺序不参与），值那一侧既不许漏出字面量，也不许把引用抹成 `***`。
    #[test]
    fn mcp_summary_shows_references_and_redacts_everything_else() {
        let mcp = |cmd: &str, args: Vec<&str>, env: Vec<(&str, &str)>| Spec::Mcp {
            transport: McpTransport::Stdio {
                command: cmd.into(),
                args: args.into_iter().map(String::from).collect(),
                env: env
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            },
        };
        // actual 侧是字面量（手写的配置里会有）→ 只给 `***`，值一个字都不许露面；
        // desired 侧是引用 → 照原样。两侧因此**看得出差别**。
        assert_eq!(
            compare_versions(
                &mcp("npx", vec!["-y", "pkg"], vec![("K", "${K}")]),
                &mcp("npx", vec!["-y", "pkg"], vec![("K", "sk-live-9f3a")])
            ),
            VersionMatch::Different {
                from: "npx -y pkg [env: K=***]".into(),
                to: "npx -y pkg [env: K=${K}]".into(),
            }
        );
        // 空默认值（`${K:-}`）也是要看见的信息：它与 `${K}` 不是同一件事，
        // 而两个形状在清单里都合法。
        assert_eq!(
            compare_versions(
                &mcp("npx", vec![], vec![("K", "${K:-}")]),
                &mcp("npx", vec![], vec![("K", "${K}")])
            ),
            VersionMatch::Different {
                from: "npx [env: K=${K}]".into(),
                to: "npx [env: K=${K:-}]".into(),
            }
        );
        // 多键：字节升序、`,` 分隔、无空格；desired 侧的插入顺序是 Z 然后 A
        assert_eq!(
            compare_versions(
                &mcp("npx", vec!["x"], vec![("A", "${A}"), ("Z", "${Z}")]),
                &mcp("npx", vec!["x"], vec![("Z", "sk-live-9f3a"), ("A", "${A}")])
            ),
            VersionMatch::Different {
                from: "npx x [env: A=${A},Z=***]".into(),
                to: "npx x [env: A=${A},Z=${Z}]".into(),
            }
        );
        // 只有一侧有 env → 另一侧不渲染 `[env: ]` 段，方向仍是 from=actual、to=desired
        assert_eq!(
            compare_versions(
                &mcp("npx", vec!["-y", "pkg"], vec![("TOKEN", "sk-live-9f3a")]),
                &mcp("npx", vec!["-y", "pkg"], vec![])
            ),
            VersionMatch::Different {
                from: "npx -y pkg".into(),
                to: "npx -y pkg [env: TOKEN=***]".into(),
            }
        );
        // 无 args 时 `[env: ]` 段直接跟在 command 后面（那个前导空格是它自己的）
        assert_eq!(
            compare_versions(
                &mcp("newbin", vec![], vec![("K", "${K}")]),
                &mcp("newbin", vec![], vec![])
            ),
            VersionMatch::Different {
                from: "newbin".into(),
                to: "newbin [env: K=${K}]".into(),
            }
        );
    }

    /// 修订前这条是护栏：一边 sha+version 都有、另一边只有 version，旧实现判 Unknown。
    /// 修订后 sha 只在「两边都有且不等」时定 Different，其余 fall through 到 version 比较，
    /// 所以这一对（version 6.4.1 vs 6.3.0 归一化后不同）必须判 Different——sha 在一边缺失
    /// 不再阻断 version 比较。
    #[test]
    fn sha_on_one_side_does_not_block_version_comparison() {
        assert_eq!(
            compare_versions(
                &plugin(Some("6.4.1"), Some("abc123")),
                &plugin(Some("6.3.0"), None)
            ),
            VersionMatch::Different {
                from: "6.3.0".into(),
                to: "6.4.1".into()
            }
        );
        // 反方向同理
        assert_eq!(
            compare_versions(
                &plugin(Some("6.3.0"), None),
                &plugin(Some("6.4.1"), Some("abc123"))
            ),
            VersionMatch::Different {
                from: "6.4.1".into(),
                to: "6.3.0".into()
            }
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

    /// 实测本机 locator 有五种形态，其中一种是字面量 `unknown`（大小写不一）、还有空串与
    /// 纯空白。这些哨兵必须在进 `compare_versions` 之前收敛成 `None`，否则两条
    /// `sha: Some("unknown")` 会命中「两边都有 SHA」分支判成 `Same`——两个真不同的构建
    /// 被报成同步，正是本 Task 要消灭的那类假阴性。
    #[test]
    fn normalize_locator_collapses_absent_and_sentinel_values() {
        assert_eq!(normalize_locator(None), None);
        assert_eq!(normalize_locator(Some("")), None);
        assert_eq!(normalize_locator(Some("  ")), None);
        assert_eq!(normalize_locator(Some("\t\n ")), None);
        assert_eq!(normalize_locator(Some("unknown")), None);
        assert_eq!(normalize_locator(Some("UNKNOWN")), None);
        assert_eq!(normalize_locator(Some("UnKnOwN")), None);
        assert_eq!(normalize_locator(Some(" unknown ")), None);
        // 真 locator 原样返回（两侧空白 trim 掉）
        assert_eq!(
            normalize_locator(Some("6.3.0")).as_deref(),
            Some("6.3.0"),
            "version 串不该被哨兵规则吃掉"
        );
        assert_eq!(
            normalize_locator(Some("a1b2c3")).as_deref(),
            Some("a1b2c3"),
            "短 sha 串同样原样返回"
        );
        assert_eq!(
            normalize_locator(Some("  b36e0829c6d0140e93cfef2ca599b1b07d4a7797  ")).as_deref(),
            Some("b36e0829c6d0140e93cfef2ca599b1b07d4a7797")
        );
    }

    /// 端到端：归一化 + 比较串成一条流水线，也就是下游（Task 5 的 `ref`、Task 9/10/11 的
    /// `read_state`）真正要跑的形态。少了归一化，第一对会判成 `Same`（假同步）、第二对会
    /// 判成 `Different`（假 drift）；两条都必须是 `Unknown`。
    #[test]
    fn sentinel_shas_normalize_to_unknown_not_a_false_same() {
        let normalized = |version: Option<&str>, sha: Option<&str>| Spec::Plugin {
            marketplace: Some("m".into()),
            entry: "superpowers".into(),
            version: normalize_locator(version),
            sha: normalize_locator(sha),
            install_path: None,
            enabled: true,
        };
        // 两边都把「没有 SHA」写成同一个哨兵：不归一化 = Same
        assert_eq!(
            compare_versions(
                &normalized(None, Some("unknown")),
                &normalized(None, Some("unknown"))
            ),
            VersionMatch::Unknown
        );
        // 哨兵大小写/空白不同就不算「有 SHA」，也不能因为两串不相等就报成 drift
        assert_eq!(
            compare_versions(
                &normalized(None, Some("unknown")),
                &normalized(None, Some(" UNKNOWN "))
            ),
            VersionMatch::Unknown
        );
        // 归一化把哨兵清掉之后，version 兜底分支照常工作
        assert_eq!(
            compare_versions(
                &normalized(Some("6.4.1"), Some("unknown")),
                &normalized(Some("6.4.1"), Some(""))
            ),
            VersionMatch::Same
        );
        assert_eq!(
            compare_versions(
                &normalized(Some("6.4.1"), Some("unknown")),
                &normalized(Some("6.3.0"), Some("UNKNOWN"))
            ),
            VersionMatch::Different {
                from: "6.3.0".into(),
                to: "6.4.1".into()
            }
        );
    }

    /// §9.5 的交付目标 = `targets` − `source_harness`。
    /// `All` 是默认值，它必须能**具体化**成「除这一家之外的三家」——
    /// 否则「默认三家全要」的清单写上一个源 harness 之后就哪家都不交付了。
    #[test]
    fn minus_a_harness_from_all_names_the_rest() {
        let known = ["claude-code", "codex", "kimi-code"];
        assert_eq!(
            Targets::All.minus("claude-code", &known),
            Targets::Only(vec!["codex".into(), "kimi-code".into()])
        );
    }

    #[test]
    fn minus_a_harness_from_an_explicit_list_keeps_the_order() {
        let known = ["claude-code", "codex", "kimi-code"];
        let t = Targets::Only(vec!["kimi-code".into(), "claude-code".into()]);
        assert_eq!(
            t.minus("claude-code", &known),
            Targets::Only(vec!["kimi-code".into()])
        );
    }

    /// 减掉一家不在名单里的 harness 是恒等变换——`targets` 里本来就没它。
    #[test]
    fn minus_something_that_is_not_there_changes_nothing() {
        let known = ["claude-code", "codex", "kimi-code"];
        let t = Targets::Only(vec!["codex".into()]);
        assert_eq!(t.minus("claude-code", &known), t);
    }

    #[test]
    fn any_in_answers_whether_a_target_is_in_play() {
        assert!(Targets::All.any_in(&["codex"]), "All 包含所有家");
        assert!(Targets::Only(vec!["codex".into()]).any_in(&["codex", "kimi-code"]));
        assert!(
            !Targets::Only(vec!["kimi-code".into()]).any_in(&["claude-code", "codex"]),
            "整份名单都没被启用 → 不在盘面上"
        );
        assert!(
            !Targets::Only(vec![]).any_in(&["codex"]),
            "空集当然不在盘面上"
        );
    }

    /// `All` 减去唯一已知的一家 = 空集。这正是 §9.5 「目标只有源 harness 自己」
    /// 那个要报 undeliverable 的情形，所以「空」必须判得出来。
    #[test]
    fn only_and_all_cannot_be_empty_until_they_are() {
        assert!(!Targets::All.is_empty(), "All 永远不空");
        assert!(!Targets::Only(vec!["codex".into()]).is_empty());
        assert!(Targets::Only(vec![]).is_empty());
        assert!(
            Targets::All
                .minus("claude-code", &["claude-code"])
                .is_empty()
        );
    }
}
