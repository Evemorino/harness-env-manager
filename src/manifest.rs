//! env.yaml 的类型化解析与校验。
//!
//! 三类条目（skills / mcps / plugins）是用户书写视角，解析后统一带 `targets` 与
//! `degrade` 语义（spec §4 决策 1）。校验错误**聚合**后一次性报出，避免用户改一条跑一次。

use crate::capability::{Kind, Targets};
use crate::secrets;
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Unexpected, Visitor};
use serde_yaml_ng::Value;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::{Component, Path};

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
    #[serde(default, deserialize_with = "deserialize_unique_map")]
    pub skills: BTreeMap<String, SkillEntry>,
    #[serde(default, deserialize_with = "deserialize_unique_map")]
    pub mcps: BTreeMap<String, McpEntry>,
    #[serde(default, deserialize_with = "deserialize_unique_map")]
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

/// 会带出**用户写的值**的三种 serde 消息标记（`unknown variant` 的内容在反引号里）。
const VALUE_MARKERS: [&str; 3] = ["invalid type: ", "invalid value: ", "unknown variant "];
/// 只带**键名**的三种标记，见 `redact_serde_message` 文档第 3 条。
const KEY_ONLY_MARKERS: [&str; 3] = ["unknown field ", "missing field ", "duplicate field "];
/// `Unexpected` 的渲染里带内容的那几种前缀 → 静态类型词（与 `env_refs` 里给节点种类的那批
/// 静态词同一批，所以 `env` 那处的既有消息一个字都不变）。
const CONTENT_ACTUALS: [(&str, &str); 5] = [
    ("string \"", "a string"),
    ("integer `", "a number"),
    ("floating point `", "a number"),
    ("boolean `", "a boolean"),
    ("character `", "a character"),
];
/// 解析器把「实际值」与「模式要求」分开的固定串。
const EXPECTED_SEP: &str = ", expected ";
const REDACTED_ACTUAL: &str = "a value (not shown)";
const REDACTED_VARIANT: &str = "(value not shown)";
const REDACTED_SHAPE: &str = "unrecognized parse problem (parser text not shown)";

/// 反序列化一张**键必须唯一**的表。
///
/// `BTreeMap` 的 `Deserialize` 就是 `entry.insert(value)`：后一条**静默顶掉**前一条，而
/// `MapAccess` 本身会把重复键老老实实都吐出来。所以能发现重复的地方只有这里——换成自己收，
/// 撞键就报。
///
/// 措辞**故意**沿用 serde 自己的 `duplicate field`：它是 `KEY_ONLY_MARKERS` 里「只带键名」
/// 的三种形状之一，会**原样**穿过 `redact_serde_message`。自创任何一种别的说法都会被那道
/// 门禁换成静态理由，用户就拿不到出错的那个键名。
///
/// `visit_unit` 是必需的：`skills:`（空值）在 YAML 里解成 unit，而 `BTreeMap` 自己的实现
/// 把它当空表——少了这一格，「写了 `skills:` 但没写内容」会从「空表」变成形状错误。
fn deserialize_unique_map<'de, D, V>(d: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct UniqueMap<V>(PhantomData<V>);

    impl<'de, V: Deserialize<'de>> Visitor<'de> for UniqueMap<V> {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a mapping")
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(BTreeMap::new())
        }

        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, V>()? {
                if out.contains_key(&key) {
                    return Err(de::Error::custom(format!("duplicate field `{key}`")));
                }
                out.insert(key, value);
            }
            Ok(out)
        }
    }

    d.deserialize_map(UniqueMap::<V>(PhantomData))
}

/// 能力的名字（`skills`/`mcps`/`plugins` 三个映射的**键**）会直接参与路径拼接：
/// `store::skills` 拿它找源（`agents_skills().join(name)`），`apply` 拿它定位目标
/// （`target_dir.join(name)`），B3 的 plugin 安装目录与 `claude mcp add-json <name>`
/// 同理。`Path::join` 的语义让两类名字能逃出去：**绝对路径替换基路径**、`..` 逐级上溯
/// ——于是 henv 会去读/写目标目录**之外**，静默覆盖用户没让它碰的东西，正是不变式二
/// 要消灭的形态。故名字必须是单一路径分量。
///
/// 「名字里真有斜杠」不是需要支持的情形：身份由 `marketplace`/`entry`/`repo` 那些字段
/// 承载，键里出现分隔符只说明清单层次写错了。
///
/// `\` 单列一条：它在 Unix 上是普通字符、在 Windows 上是分隔符，而清单是可移植的——
/// 今天在 macOS 上它只是个怪名字，换台机器就是第二处逃逸。
///
/// **本函数只按 POSIX 语义判「是不是单一路径分量」，这不是疏忽而是有意的范围**：henv
/// 目前只跑 macOS/Linux，且**编译期就锁死**了这一点——`src/apply/write.rs` 直接调
/// `std::os::unix::fs::symlink`，`std::os::unix` 在 Windows 上不存在，所以本 crate 今天
/// 根本构建不出 Windows 产物。因此 `:`（盘符/NTFS ADS）与尾部空格/点（Windows 会自行
/// 归一化掉）在唯一支持的平台上都是**普通文件名字符**，逃不出基路径。
/// 哪天要支持 Windows，这里**必须**一起补：拒 `:`、拒 `trim_end_matches([' ', '.'])`
/// 之后为空的或落到 `.`/`..` 的名字——那两处归一化是 Windows 独有的逃逸面。
fn name_is_a_single_component(name: &str) -> bool {
    if name.contains('\\') {
        return false;
    }
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}

/// git 源的 `path` 是**仓库内**的子目录：相对、不带 `..`、不以 `/` 开头。
///
/// 这不是防越权（读哪个目录用户在本机本来就有权限决定），是防清单自相矛盾：
/// `source: git` 说「内容在这个仓库里」，`path: ../../x` 说的是「内容在这个仓库外」。
/// 后者应当写成 `source: local`。另一半在 `git::resolve` 里：那边是「查不到就报错」。
fn git_sub_path_is_safe(s: &str) -> bool {
    if s.starts_with('/') {
        return false;
    }
    Path::new(s)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

/// 认得的 serde 标记里取**最靠前**的那个（值里可以自带标记词，实测
/// `version: "invalid type: sk-live-…"`，从后往前找会把值的尾巴当成消息头留下）。
fn value_marker(msg: &str) -> Option<(usize, &'static str)> {
    VALUE_MARKERS
        .into_iter()
        .filter_map(|marker| msg.find(marker).map(|at| (at, marker)))
        .min()
}

/// 引号与反引号是 serde 包住「用户写的内容」唯二的两种方式。
fn may_carry_content(msg: &str) -> bool {
    msg.contains('"') || msg.contains('`')
}

/// 认不出的消息里唯一还能安全保留的成分是点号路径前缀：`": "` 之前、且它自己不含引号
/// （含引号说明那段已经可能是值的一截，整段丢掉）。
fn safe_path_prefix(msg: &str) -> Option<&str> {
    let (prefix, _) = msg.split_once(": ")?;
    (!may_carry_content(prefix)).then_some(prefix)
}

/// actual 段（`invalid type:` / `invalid value:` 后面那截）→ 静态词。
fn redact_actual(actual: &str) -> &str {
    for (prefix, word) in CONTENT_ACTUALS {
        if actual.starts_with(prefix) {
            return word;
        }
    }
    // 走到这里要么是解析器自己的静态词（`sequence` / `map` / `unit value`，以及 `env_refs`
    // 递进来的 `a string` 这类 `Unexpected::Other`），要么是不认识的**带内容形态**。
    if may_carry_content(actual) {
        REDACTED_ACTUAL
    } else {
        actual
    }
}

/// serde 渲染形状错误时会把**用户写的标量原样**打印进消息：`invalid type: string "<值>"` /
/// `invalid value: string "<值>"` / ```unknown variant `<值>````。`env.yaml` 解成 typed struct，
/// 所以任何字段只要「值不满足期望类型」就走这条路——实测六种形态（`version`、
/// `mcps.*.args`、`degrade.*`、`targets`、两个 `source` 枚举、`harnesses.*.enabled`）都会
/// 把值带出来，多行块标量也整段带出。而这件事发生在 `m.validate()` 那道 `${VAR}` 门禁**之前**：
/// 一条误粘的明文密钥如果同时不满足期望类型，就是被门禁自己打印出来的。
///
/// 本函数只做**这一条解析路径**上的遮蔽（`Manifest::parse` 是唯一调用方；`env_refs` 与 codex
/// 的 TOML 侧同样是单点修法，全局 scrubbing 会破坏别的输出）。白名单的是**serde 的消息形状**，
/// 不是「看起来像密钥的子串」——后者一定漏掉下一种格式，而且漏掉之后测试仍然全绿。三条规则：
///
/// 1. `invalid type:` / `invalid value:`：内容只可能待在 actual 段里，所以整段换成静态类型词；
///    `expected …` 段是模式文本（来自 struct / enum 定义与我们的 `&"…"` 字面量），原样保留。
///    actual 与 expected 按**最后一次** `", expected "` 切：值里可以自带这个子串。
/// 2. ```unknown variant `X` ```：X 是用户写的值 → 整个名字丢掉，不去猜它从哪结束。
/// 3. 只带**键名**的三种（`unknown field` / `missing field` / `duplicate field`）原样保留。
///    键名不是 `${VAR}` 规则管的对象，而点号路径前缀本来就由同一个文件的键组成
///    （`mcps.<条目名>.args`）——遮掉键只会让人找不到出错行，却挡不住值。
///
/// 三种都不认得的形状（解析器换了措辞、或我们没建模过的第三种内容载体）：**宁可少说也不回显**，
/// 换成一句静态理由，再补上前缀与 `location` 给的 `at line L column C`。`location` 是解析器的
/// 结构化输出，不是文档内容，所以重建位置不会把值带回来。
fn redact_serde_message(raw: &str, location: Option<(usize, usize)>) -> String {
    if let Some((at, marker)) = value_marker(raw) {
        let body = &raw[at + marker.len()..];
        let (actual, expected) = match body.rsplit_once(EXPECTED_SEP) {
            Some((actual, expected)) => (actual, Some(expected)),
            None => (body, None),
        };
        let mut out = String::with_capacity(raw.len());
        out.push_str(&raw[..at]);
        out.push_str(marker);
        // `unknown variant ` 的措辞里「实际值」就是那个名字本身，没有类型词可留。
        if marker == "unknown variant " {
            out.push_str(REDACTED_VARIANT);
        } else {
            out.push_str(redact_actual(actual));
        }
        if let Some(expected) = expected {
            out.push_str(EXPECTED_SEP);
            out.push_str(expected);
        }
        return out;
    }
    if KEY_ONLY_MARKERS
        .into_iter()
        .any(|marker| raw.contains(marker))
        || !may_carry_content(raw)
    {
        return raw.to_string();
    }
    let mut out = String::new();
    if let Some(prefix) = safe_path_prefix(raw) {
        out.push_str(prefix);
        out.push_str(": ");
    }
    out.push_str(REDACTED_SHAPE);
    if let Some((line, column)) = location {
        out.push_str(&format!(" at line {line} column {column}"));
    }
    out
}

/// 「字段在但空白」与「字段缺失」是两种不同的失败：`requires_x` 那条管缺失，这里管空白。
/// 两者都必须拦下来，因为空白值会原样进 `lower`：`entry: ""` 降出 `CapId{Plugin, ""}`
/// （label `plugin/`）、并把 degrade 表键到 `""`——于是任何报不出 plugin 身份的东西都会
/// 吃到这个条目的策略。`None`（没写）在这里放过，交给各 source 的 requires 规则。
///
/// **消息里只有 `路径.字段` 这一串**，被拒的值绝不出现：这些是用户写的自由文本，而且
/// 与 `env` 相邻（同一个文件里可能就有密钥），回显等于把它们打进终端与日志。
fn reject_blank(at: &str, field: &str, value: Option<&str>, e: &mut Vec<String>) {
    if value.is_some_and(|v| v.trim().is_empty()) {
        e.push(format!("{at}.{field}: must not be empty"));
    }
}

impl Manifest {
    /// 解析 + 校验。校验错误聚合成单个 anyhow 错误，每条带 `skills.a` 式路径。
    ///
    /// 解析失败那一支**必须**经 `redact_serde_message` 再拼消息：typed 反序列化的形状错误
    /// 自带用户写的标量，而这里是 `${VAR}` 门禁之前唯一会打印该文件内容的地方。
    pub fn parse(text: &str) -> anyhow::Result<Manifest> {
        let m: Manifest = serde_yaml_ng::from_str(text).map_err(|e| {
            // 位置取解析器的结构化输出，不取消息文本里的 `at line …`——最后一道分支要用
            // 它重建定位，而那条分支的整串消息都已经被换掉了。
            let location = e.location().map(|l| (l.line(), l.column()));
            anyhow::anyhow!(
                "failed to parse env.yaml: {}",
                redact_serde_message(&e.to_string(), location)
            )
        })?;
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
            if !name_is_a_single_component(name) {
                e.push(format!(
                    "{at}: name must be a single path component (no `/`, no `\\`, not `.`/`..`, not empty)"
                ));
            }
            reject_blank(&at, "repo", s.repo.as_deref(), &mut e);
            reject_blank(&at, "path", s.path.as_deref(), &mut e);
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
                    // `repo` 原样进 `git clone` 的 argv（`repo_url` 只展开 owner/repo
                    // 简写）。以 `-` 开头的值于是会被 git 当成**选项**：实测
                    // `repo: --upload-pack=/tmp/x.sh` 能让 clone 把那个脚本跑起来。
                    // 清单是最容易从别处复制来的文件，这道闸就断在值进来的地方。
                    if s.repo.as_deref().is_some_and(|r| r.starts_with('-')) {
                        e.push(format!(
                            "{at}: repo must not start with `-` (git would read it as an option)"
                        ));
                    }
                    if s.git_ref.is_none() {
                        e.push(format!("{at}: source git requires `ref` (ref is the lock)"));
                    }
                    if let Some(p) = s.path.as_deref() {
                        if !git_sub_path_is_safe(p) {
                            e.push(format!(
                                "{at}: path must be a relative path inside the repository \
                                 (no leading `/`, no `..`)"
                            ));
                        }
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
            if !name_is_a_single_component(name) {
                e.push(format!(
                    "{at}: name must be a single path component (no `/`, no `\\`, not `.`/`..`, not empty)"
                ));
            }
            // `command` 是 serde 必填字段，所以这里永远是 `Some`：缺失由反序列化挡，
            // 空白由这条挡（与上面两条同一个规则、同一个消息形状）。
            reject_blank(&at, "command", Some(&m.command), &mut e);
            for (k, v) in &m.env {
                if let Err(msg) = secrets::validate_env(k, v) {
                    e.push(format!("{at}: {msg}"));
                }
            }
        }

        for (name, p) in &self.plugins {
            let at = format!("plugins.{name}");
            if !name_is_a_single_component(name) {
                e.push(format!(
                    "{at}: name must be a single path component (no `/`, no `\\`, not `.`/`..`, not empty)"
                ));
            }
            reject_blank(&at, "marketplace", p.marketplace.as_deref(), &mut e);
            reject_blank(&at, "entry", p.entry.as_deref(), &mut e);
            reject_blank(&at, "repo", p.repo.as_deref(), &mut e);
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

    /// 这两条原本断言 `err.contains("cursor")`——而被断言的那个词正是 serde 回显的**值**，
    /// 与 `mistyped_scalars_never_reach_the_rendered_parse_error` 要挡掉的是同一件事（值写成
    /// `sk-live-…` 时它就是被同一条消息打印出来的）。现在整串钉死遮蔽后的形态：真正的牙齿是
    /// 「这条输入必须报错」＋「报的是哪一个条目、哪一行」，harness 名由用户自己看着那行改。
    #[test]
    fn targets_rejects_unknown_harness_in_list() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: [cursor]\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert_eq!(
            err,
            "failed to parse env.yaml: mcps.a: invalid value: a string, expected one of claude-code, codex, kimi-code at line 4 column 5"
        );
    }

    /// 列表校验的「每一个元素都查」：现有 fixture 是单元素，所以只查首元素的实现
    /// 在这里会**接受** `[codex, cursor]`，未知 harness 静默过关——所以本条与上面那条
    /// 消息逐字相同并不冗余：判据是「它照样必须报错」。
    #[test]
    fn targets_rejects_unknown_harness_after_the_first() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: [codex, cursor]\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert_eq!(
            err,
            "failed to parse env.yaml: mcps.a: invalid value: a string, expected one of claude-code, codex, kimi-code at line 4 column 5"
        );
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

    /// 六个 struct 都要「未知键 = 错误」，而上面那条只钉住了 `Manifest`（它的 SkillEntry
    /// 断言即使去掉 `deny_unknown_fields` 也照样红，因为同一条目上还带着 local-requires-
    /// path 那条规则）。这里每行一个 struct、一个拼错的键，并且挑**没有必填字段**的形态，
    /// 于是 `is_err()` 只能来自 `deny_unknown_fields` 本身。
    #[test]
    fn every_struct_rejects_an_unknown_field() {
        assert!(Manifest::parse("version: 1\nharnesses:\n  codex:\n    enbled: true\n").is_err());
        assert!(
            Manifest::parse("version: 1\nskills:\n  a:\n    source: agents-dir\n    pth: /x\n")
                .is_err()
        );
        assert!(
            Manifest::parse("version: 1\nmcps:\n  a:\n    command: npx\n    cmd: npx\n").is_err()
        );
        assert!(
            Manifest::parse(
                "version: 1\nplugins:\n  a:\n    source: github\n    repo: x/y\n    repos: z\n"
            )
            .is_err()
        );
        assert!(Manifest::parse(
            "version: 1\nplugins:\n  a:\n    source: github\n    repo: x/y\n    degrade:\n      hookz: report\n"
        )
        .is_err());
    }

    #[test]
    fn rejects_git_skill_without_repo() {
        // 只有 `repo` 缺失：`path` 在场，所以「requires ref」不可能来自读错字段。
        let err = Manifest::parse(
            "version: 1\nskills:\n  a:\n    source: git\n    ref: v1\n    path: skills\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("requires `repo`"), "{err}");
        assert!(!err.contains("requires `ref`"), "{err}");
    }

    #[test]
    fn rejects_git_skill_without_ref() {
        // 与上面那条只差被测字段：`path` 在场，所以把谓词改成读 `s.path` 的实现
        // 这里会直接**接受**，而 `repo` 规则不会替它把消息凑出来。
        let err = Manifest::parse(
            "version: 1\nskills:\n  a:\n    source: git\n    repo: a/b\n    path: skills\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("requires `ref`"), "{err}");
        assert!(!err.contains("requires `repo`"), "{err}");
    }

    /// 位置字段写成空白（`""` / `"   "`）与写成真值不是一回事，而**两者都不许当成真值**：
    /// `entry: ""` 会降出 `CapId{Plugin, ""}`、并把 degrade 表键到 `""`（实测：一个
    /// `Origin::Plugin{name: ""}` 的 hook 会吃到那个条目的 `hooks: skip`）。
    ///
    /// 每行只把**被测字段**换成空白，其余保持合法；同一行的正例（真值）必须通过——
    /// 否则「只要写了就拒」的变异体（`value.is_some()`）照样全绿。空串与纯空白是两个
    /// 变异体：前者盯「只查 in_none」，后者专盯 `.trim()` 被删。
    ///
    /// 断言用**整条消息逐字相等**：路径写错、字段名写错、多出一条兄弟消息、或把用户写的
    /// 值回显出来（这些字段与 `env` 同文件相邻），都会立刻不等。
    #[test]
    fn blank_location_fields_are_rejected_by_dotted_path() {
        for (template, path) in [
            (
                "version: 1\nskills:\n  a:\n    source: git\n    repo: {v}\n    ref: v1\n",
                "skills.a.repo",
            ),
            (
                "version: 1\nskills:\n  a:\n    source: local\n    path: {v}\n",
                "skills.a.path",
            ),
            (
                "version: 1\nmcps:\n  a:\n    command: {v}\n",
                "mcps.a.command",
            ),
            (
                "version: 1\nplugins:\n  a:\n    source: claude-marketplace\n    marketplace: {v}\n    entry: e\n",
                "plugins.a.marketplace",
            ),
            (
                "version: 1\nplugins:\n  a:\n    source: claude-marketplace\n    marketplace: m\n    entry: {v}\n",
                "plugins.a.entry",
            ),
            (
                "version: 1\nplugins:\n  a:\n    source: github\n    repo: {v}\n",
                "plugins.a.repo",
            ),
        ] {
            for blank in ["\"\"", "\"   \""] {
                let yaml = template.replace("{v}", blank);
                let Err(err) = Manifest::parse(&yaml) else {
                    panic!("{path} 写成空白必须被拒，实际被接受");
                };
                let err = err.to_string();
                assert_eq!(
                    err,
                    format!("env.yaml invalid:\n  - {path}: must not be empty"),
                    "唯一的错误必须是这一条，并且不回显用户写的值"
                );
            }
            let yaml = template.replace("{v}", "real-value");
            Manifest::parse(&yaml).unwrap_or_else(|e| panic!("{path}：真值不该报错，实际：{e}"));
        }
    }

    /// 空白检查**不看 source**：可选位置字段写成空白时，任何 `requires` 规则都不会报，
    /// 于是只有这条规则挡得住——`entry` 是 plugin 身份的来源，github 源上一个
    /// `entry: "  "` 就足以让 `lower` 发出一条 `CapId{Plugin, "  "}`。
    ///
    /// 上面那组的 fixture 每个字段都恰好是「该源必填」的那一个，所以「把空白检查挪进
    /// `match s.source` 里、只查该源必填字段」的写法在那一组里全绿。这一组就是它的对照。
    #[test]
    fn blank_is_rejected_even_in_fields_the_source_does_not_require() {
        for (yaml, path) in [
            (
                "version: 1\nskills:\n  a:\n    source: git\n    repo: x/y\n    ref: v1\n    path: \"  \"\n",
                "skills.a.path",
            ),
            (
                "version: 1\nskills:\n  a:\n    source: local\n    path: p\n    repo: \"\"\n",
                "skills.a.repo",
            ),
            (
                "version: 1\nplugins:\n  a:\n    source: github\n    repo: x/y\n    entry: \"  \"\n",
                "plugins.a.entry",
            ),
            (
                "version: 1\nplugins:\n  a:\n    source: claude-marketplace\n    marketplace: m\n    entry: e\n    repo: \"\"\n",
                "plugins.a.repo",
            ),
        ] {
            let Err(err) = Manifest::parse(yaml) else {
                panic!("{path} 写成空白必须被拒，实际被接受");
            };
            assert_eq!(
                err.to_string(),
                format!("env.yaml invalid:\n  - {path}: must not be empty"),
                "{path}：唯一的错误必须是这一条"
            );
        }
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

    /// 同一条规则的第二个 disjunct：`ref` 是 git skill 的锁，agents-dir 带它说明用户
    /// 以为自己在配 git。去掉 `|| s.git_ref.is_some()` 后这条 fixture 会被**接受**。
    #[test]
    fn rejects_agents_dir_skill_with_ref() {
        let err =
            Manifest::parse("version: 1\nskills:\n  a:\n    source: agents-dir\n    ref: v1\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("agents-dir"), "{err}");
    }

    /// `repo` 会被原样交给 `git clone`（`repo_url` 只展开 owner/repo 简写）。以 `-`
    /// 开头的值于是变成 git 的**选项**：实测 `repo: --upload-pack=/tmp/x.sh` 会让
    /// clone 真的把那个脚本跑起来。清单是最容易被复制粘贴的东西（项目 README、
    /// 同事的 env.yaml），所以这条必须在清单层断掉。
    #[test]
    fn rejects_repo_that_starts_with_a_dash() {
        let err = Manifest::parse(
            "version: 1\nskills:\n  a:\n    source: git\n    repo: --upload-pack=/tmp/x.sh\n    ref: main\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("repo"), "{err}");
        assert!(err.contains("option"), "报错要说清为什么：{err}");
    }

    #[test]
    fn rejects_claude_marketplace_plugin_without_marketplace_or_entry() {
        let yaml = "version: 1\nplugins:\n  a:\n    source: claude-marketplace\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        // 必须带上 `requires` 前缀：source 的值本身就是 `claude-marketplace`，
        // 只断言 contains("marketplace") 的话兄弟消息会替被测那条规则作证。
        assert!(err.contains("requires `marketplace`"), "{err}");
        assert!(err.contains("requires `entry`"), "{err}");
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
        // env 两个键，坏的那个排在后面（BTreeMap 序 = AAA, BBB）：只有一个键时
        // 「每个键都查」这条规则没人钉住，take(1) 会让第二个键上的明文密钥过关。
        let yaml = "version: 1\nskills:\n  a:\n    source: git\n  b:\n    source: local\nmcps:\n  c:\n    command: npx\n    env:\n      AAA: ${OK}\n      BBB: sk-live-abc123\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("skills.a"), "{err}");
        assert!(err.contains("skills.b"), "{err}");
        assert!(err.contains("mcps.c"), "{err}");
        assert!(err.contains("BBB"), "second env key must be checked: {err}");
        assert!(!err.contains("sk-live-abc123"), "must not echo: {err}");
    }

    #[test]
    fn yaml_syntax_error_surfaces_with_context() {
        let err = Manifest::parse("version: 1\nskills: [\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("env.yaml"), "{err}");
    }

    /// 五类字段各埋一个假密钥，断言**渲染出来的报错逐字等于遮蔽后的形态**。
    ///
    /// 修前的实际输出（`{:#}` 与 `{:?}` 一模一样——`anyhow!` 拼的是字符串，没有 cause 链，
    /// 所以两种打印形态都带值，这是与 codex TOML 那处**不同的机制**）：
    /// ```text
    /// failed to parse env.yaml: version: invalid type: string "sk-live-9f3a2b7c", expected u32 at line 1 column 10
    /// failed to parse env.yaml: mcps.a.args: invalid type: string "sk-live-9f3a2b7c", expected a sequence at line 5 column 11
    /// failed to parse env.yaml: plugins.p.degrade: invalid value: string "sk-live-9f3a2b7c", expected "report" or "skip" at line 7 column 7
    /// failed to parse env.yaml: mcps.a: invalid value: string "sk-live-9f3a2b7c", expected "all" or a list of harness names at line 4 column 5
    /// failed to parse env.yaml: skills.a.source: unknown variant `sk-live-9f3a2b7c`, expected one of `agents-dir`, `git`, `local` at line 4 column 13
    /// ```
    /// 这条路**在 `m.validate()` 那道 `${VAR}` 门禁之前**，所以一条误粘的明文密钥只要同时
    /// 不满足期望类型，就是被门禁自己打印出来的。断言用整串相等（计划 Global Constraints ①）：
    /// 路径、位置、`expected` 文本、遮蔽词任何一处变形都会立刻红，「值没出现」也因此不是靠
    /// `contains` 凑出来的。`want` 自身还要过一遍「不含植进去的值」，防止把修前的输出抄进表里。
    #[test]
    fn mistyped_scalars_never_reach_the_rendered_parse_error() {
        const SECRET: &str = "sk-live-9f3a2b7c";
        for (yaml, want) in [
            // version（u32）：标量、带引号的标量、浮点、布尔、以及装不下的整数。
            (
                "version: sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: version: invalid type: a string, expected u32 at line 1 column 10",
            ),
            (
                "version: \"sk-live-9f3a2b7c\"\n",
                "failed to parse env.yaml: version: invalid type: a string, expected u32 at line 1 column 10",
            ),
            (
                "version: 1.5\n",
                "failed to parse env.yaml: version: invalid type: a number, expected u32 at line 1 column 10",
            ),
            (
                "version: true\n",
                "failed to parse env.yaml: version: invalid type: a boolean, expected u32 at line 1 column 10",
            ),
            (
                "version: 12345678901234567890\n",
                "failed to parse env.yaml: version: invalid value: a number, expected u32 at line 1 column 10",
            ),
            // mcps.*.args（Vec<String>）：整个条目被写成块标量时同理，值带换行也只算一段。
            (
                "version: 1\nmcps:\n  a:\n    command: npx\n    args: sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: mcps.a.args: invalid type: a string, expected a sequence at line 5 column 11",
            ),
            (
                "version: 1\nmcps:\n  a: |\n    sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: mcps.a: invalid type: a string, expected struct McpEntry at line 3 column 6",
            ),
            // degrade.*（手写 invalid_value）与 targets（同）：`expected` 文本自带引号，
            // 遮蔽只准动 actual 那一段。
            (
                "version: 1\nplugins:\n  p:\n    source: github\n    repo: a/b\n    degrade:\n      hooks: sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: plugins.p.degrade: invalid value: a string, expected \"report\" or \"skip\" at line 7 column 7",
            ),
            (
                "version: 1\nmcps:\n  a:\n    command: npx\n    targets: sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: mcps.a: invalid value: a string, expected \"all\" or a list of harness names at line 4 column 5",
            ),
            (
                "version: 1\nmcps:\n  a:\n    command: npx\n    targets: [sk-live-9f3a2b7c]\n",
                "failed to parse env.yaml: mcps.a: invalid value: a string, expected one of claude-code, codex, kimi-code at line 4 column 5",
            ),
            // source 枚举（serde 派生的 unknown variant，两个 enum 各一条）。
            (
                "version: 1\nskills:\n  a:\n    source: sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: skills.a.source: unknown variant (value not shown), expected one of `agents-dir`, `git`, `local` at line 4 column 13",
            ),
            (
                "version: 1\nplugins:\n  a:\n    source: sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: plugins.a.source: unknown variant (value not shown), expected one of `claude-marketplace`, `github`, `zip-url` at line 4 column 13",
            ),
            // 派单没点名的第 6 处同类：harnesses.*.enabled（bool），以及值里自带标记词的形态。
            (
                "version: 1\nharnesses:\n  codex:\n    enabled: sk-live-9f3a2b7c\n",
                "failed to parse env.yaml: harnesses.codex.enabled: invalid type: a string, expected a boolean at line 4 column 14",
            ),
            (
                "version: \"invalid type: sk-live-9f3a2b7c\"\n",
                "failed to parse env.yaml: version: invalid type: a string, expected u32 at line 1 column 10",
            ),
            // 值里自带的是**另一种**标记词：取标记必须按最靠前的算，取到值里那个会把值的
            // 前半截留在消息头上（`… string "invalid value: a …`）。
            (
                "version: \"invalid value: sk-live-9f3a2b7c\"\n",
                "failed to parse env.yaml: version: invalid type: a string, expected u32 at line 1 column 10",
            ),
            // 值里自带 `, expected `：必须按**最后一次**出现切，否则值的后半截会留在消息里。
            (
                "version: \"a, expected sk-live-9f3a2b7c\"\n",
                "failed to parse env.yaml: version: invalid type: a string, expected u32 at line 1 column 10",
            ),
        ] {
            assert!(
                !want.contains(SECRET),
                "fixture 写错了：期望的消息里还带着植进去的值"
            );
            let err = Manifest::parse(yaml).expect_err("坏值必须在解析期就报出来");
            // `{:#}` 与 `{:?}` 是 Task 15 计划打印的两种形态，两种都得过。
            assert_eq!(format!("{err:#}"), want, "yaml = {yaml:?}");
            assert_eq!(format!("{err:?}"), want, "yaml = {yaml:?}");
        }
    }

    /// `redact_serde_message` 的分支表。这些形态**没法**只靠 env.yaml fixture 从外面打到
    /// （尤其是「解析器换了措辞」那条），所以直接测函数本身——生产侧只有一个调用方
    /// （`Manifest::parse`），遮蔽范围仍然是这一条解析路径。
    #[test]
    fn the_sanitizer_whitelists_message_shapes_and_fails_closed_on_the_rest() {
        for (raw, location, want) in [
            // 认不出、且自带引号：整条换成静态理由，位置用解析器给的结构化值重建。
            (
                "mcps.a: some brand new parser complaint about \"sk-live-9f3a2b7c\"",
                Some((4, 5)),
                "mcps.a: unrecognized parse problem (parser text not shown) at line 4 column 5",
            ),
            // 同上，但引号出现在 `": "` 之前——这时连前缀都不能留（它可能已经是值的一截）。
            (
                "string \"x\": some brand new parser complaint",
                Some((1, 2)),
                "unrecognized parse problem (parser text not shown) at line 1 column 2",
            ),
            // 认不出、没有 `at line`（`duplicate field` 那类会给出 None）：不编造位置。
            (
                "brand new complaint about \"sk-live-9f3a2b7c\"",
                None,
                "unrecognized parse problem (parser text not shown)",
            ),
            // 认不出但不含引号 = libyaml 自己的措辞（实测：found a tab character …），
            // 原样放过——语法错是最高频的报错，把它降级成一句「解析失败」就是拿可用性换安全。
            (
                "found a tab character that violates indentation at line 2 column 1, while scanning a plain scalar at line 1 column 10",
                Some((2, 1)),
                "found a tab character that violates indentation at line 2 column 1, while scanning a plain scalar at line 1 column 10",
            ),
            // 只带键名的三种原样放过（裁决见 `redact_serde_message` 文档第 3 条）。
            (
                "skills.a: unknown field `pth`, expected one of `source`, `repo`, `path`, `ref`, `targets`",
                Some((5, 5)),
                "skills.a: unknown field `pth`, expected one of `source`, `repo`, `path`, `ref`, `targets`",
            ),
            (
                "mcps.a: missing field `command`",
                Some((3, 1)),
                "mcps.a: missing field `command`",
            ),
            (
                "mcps.a: duplicate field `command`",
                Some((3, 6)),
                "mcps.a: duplicate field `command`",
            ),
            // 下一种「带内容的 actual 形态」：前缀不认识又含引号 → 丢内容，但 `expected` 留住。
            (
                "version: invalid type: byte string \"736b2d6c697665\", expected u32",
                Some((1, 10)),
                "version: invalid type: a value (not shown), expected u32",
            ),
            // actual 段本身是解析器的静态词 → 一个字都不动。
            (
                "skills.a.source: invalid type: map, expected a YAML tag starting with '!'",
                Some((4, 13)),
                "skills.a.source: invalid type: map, expected a YAML tag starting with '!'",
            ),
            // 值里自带反引号：整个名字丢掉，不去猜它到哪结束。
            (
                "skills.a.source: unknown variant `a`b`, expected one of `git`",
                Some((4, 13)),
                "skills.a.source: unknown variant (value not shown), expected one of `git`",
            ),
            // 值里自带 `, expected `（单元级，配合上面那条端到端的）。
            (
                "version: invalid type: string \"a, expected b\", expected u32",
                Some((1, 10)),
                "version: invalid type: a string, expected u32",
            ),
            // 没有 `, expected ` 收尾的形态（截断/未来措辞）：actual 照样丢。
            (
                "invalid value: string \"sk-live-9f3a2b7c\"",
                None,
                "invalid value: a string",
            ),
        ] {
            assert_eq!(
                redact_serde_message(raw, location),
                want,
                "raw = {raw:?} location = {location:?}"
            );
        }
    }

    #[test]
    fn a_skill_name_that_escapes_the_skills_dir_is_rejected() {
        // 名字会直接参与路径拼接：`agents_skills().join(name)`（读源）与
        // `target_dir.join(name)`（写目标）。`Path::join` 遇到绝对路径**替换**基路径、
        // 遇到 `..` **逐级逃逸**——两种都能把 henv 带出 skills 目录，去静默覆盖用户
        // 没让它碰的东西，正是不变式二要消灭的形态。故名字必须是单一路径分量。
        for name in ["../../evil", "a/b", "/etc/foo", "..", ".", ""] {
            let text = format!("version: 1\nskills:\n  \"{name}\":\n    source: agents-dir\n");
            let err = Manifest::parse(&text)
                .expect_err(&format!("name {name:?} 会被 Path::join 带出 skills 目录"))
                .to_string();
            assert!(err.contains("single path component"), "{name:?} → {err}");
        }
    }

    #[test]
    fn a_git_sub_path_may_not_escape_the_checkout() {
        // 逃出去的 path 会把缓存目录以外的内容当成 skill 内容读到、并拷进 harness。
        // 用户的清单里写 `local` 源是明说「读这个目录」，写 `git` 源却指定 `../..`
        // 就是清单自相矛盾——拒绝它，而不是照做。
        for bad in ["../other", "skills/../../escape", "/etc", "a/../b"] {
            let text = format!(
                "version: 1\nskills:\n  s1:\n    source: git\n    repo: obra/superpowers\n    ref: main\n    path: {bad}\n    targets: [claude-code]\n"
            );
            let Err(err) = Manifest::parse(&text) else {
                panic!("path = {bad:?} 应当被拒，却被接受");
            };
            assert!(
                err.to_string().contains("path"),
                "报错要点名 path，实际：{err}"
            );
        }
    }

    #[test]
    fn a_plain_git_sub_path_is_accepted() {
        let text = "version: 1\nskills:\n  s1:\n    source: git\n    repo: obra/superpowers\n    ref: main\n    path: skills/foo\n    targets: [claude-code]\n";
        assert!(Manifest::parse(text).is_ok());
    }

    #[test]
    fn every_capability_key_is_a_single_path_component() {
        // skill 一直有这个约束（B1 加的），mcp 与 plugin 没有——而它们的名字同样
        // 会被 henv 拿去拼路径（plugin 的安装目录、`claude mcp add-json <name>`）。
        // 键里带 `/` 是清单层次写错了，不是「名字里就有斜杠」：身份由
        // `marketplace`/`entry`/`repo` 那些字段承载，不靠键。
        for bad in ["a/b", "..", ".", "", "a\\b", "/etc"] {
            // 单引号标量：双引号里 `\b` 是 YAML 的转义（退格），那样 `a\b` 这条
            // 根本没把反斜杠喂给校验函数，测试会假绿。
            let mcp = format!("version: 1\nmcps:\n  '{bad}':\n    command: npx\n");
            let plugin = format!(
                "version: 1\nplugins:\n  '{bad}':\n    source: claude-marketplace\n    marketplace: obra/superpowers\n    entry: superpowers\n"
            );
            for (kind, text) in [("mcps", mcp), ("plugins", plugin)] {
                assert!(
                    Manifest::parse(&text).is_err(),
                    "{kind} 的键 {bad:?} 应当被拒"
                );
            }
        }
        // 反面的绊线：约束不能宽到把正常名字也拒了。
        let ok = "version: 1\nmcps:\n  context7:\n    command: npx\nplugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: obra/superpowers\n    entry: superpowers\n";
        assert!(Manifest::parse(ok).is_ok(), "正常名字要照收");
    }

    #[test]
    fn a_skill_name_with_a_backslash_is_rejected() {
        // `\` 在 Unix 上是普通字符、在 Windows 上是分隔符。清单是可移植的：
        // 今天在 macOS 上它只是个怪名字，换台机器就变成第二处逃逸。
        let text = "version: 1\nskills:\n  'a\\\\b':\n    source: agents-dir\n";
        let err = Manifest::parse(text).unwrap_err().to_string();
        assert!(err.contains("single path component"), "{err}");
    }

    #[test]
    fn duplicate_keys_are_rejected_in_all_three_entry_maps() {
        // 同一个 helper 挂在三张表上，所以三张都要有证据——只钉 skills 的话，
        // 以后有人把 mcps/plugins 的 `deserialize_with` 删掉，测试照样全绿。
        for text in [
            "version: 1\nmcps:\n  a: { command: x }\n  a: { command: y }\n",
            "version: 1\nplugins:\n  a: { source: github, repo: r }\n  a: { source: github, repo: s }\n",
        ] {
            let err = Manifest::parse(text)
                .expect_err(&format!("重复键必须被拒：{text}"))
                .to_string();
            assert!(err.contains("duplicate field `a`"), "{err}");
        }
    }

    #[test]
    fn a_bare_section_key_is_still_an_empty_table() {
        // `deserialize_unique_map` 的 `visit_unit` 那一格：YAML 里 `skills:` 是 unit，
        // `BTreeMap` 自己的实现把它当空表。少了那一格，这样写会从「空表」变成形状错误
        // ——而「写了段落名、还没写内容」正是手写清单时最常见的中间状态。
        let m = Manifest::parse("version: 1\nskills:\nmcps:\nplugins:\n")
            .expect("空段落应解成空表，不是形状错误");
        assert!(m.skills.is_empty() && m.mcps.is_empty() && m.plugins.is_empty());
    }

    #[test]
    fn ordinary_skill_names_still_pass() {
        // 反面：这条守卫只该拒绝「不是单一路径分量」的名字。空格与非 ASCII 是合法目录名
        // （Review Focus 3 要求交付后逐字节不变），带点的名字是既有惯用法（如 `kimi-cu`）。
        for name in ["foo", "my skill", "技能", "kimi-cu", "a.b", "_x", "-y"] {
            let text = format!("version: 1\nskills:\n  \"{name}\":\n    source: agents-dir\n");
            let m = Manifest::parse(&text).unwrap_or_else(|e| panic!("{name:?} 应合法：{e}"));
            assert!(m.skills.contains_key(name), "{name:?}");
        }
    }

    #[test]
    fn a_duplicate_skill_name_is_rejected_not_silently_dropped() {
        // Review Focus 4 的另一半：同一个名字写两次。json/yaml 的 map 反序列化默认是
        // last-wins，也就是后一条**静默吞掉**前一条——用户以为配了两个源，实际只交付了一个。
        // 使用者期望：要么清单层拒绝，要么报一个看得见的冲突；静默丢不是其中任何一种。
        let text = "version: 1\nskills:\n  foo:\n    source: agents-dir\n  foo:\n    source: local\n    path: /tmp/x\n";
        let err = Manifest::parse(text)
            .expect_err("同名两次必须被拒绝，不能静默取后者")
            .to_string();
        assert!(err.contains("duplicate field"), "{err}");
        assert!(err.contains("foo"), "消息要给出出错的键名：{err}");
    }
}
