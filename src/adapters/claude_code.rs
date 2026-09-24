use crate::adapters::{CLAUDE_CODE, HarnessAdapter};
use crate::capability::{
    CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets, normalize_locator,
};
use crate::home::Home;
use crate::skills_dir::{SkillStore, plugin_from_path, try_read_skill_entries};
use anyhow::Context;
use anyhow::anyhow;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

    /// 用户级 MCP 在 ~/.claude.json 顶层 mcpServers（spec §2.1 第 1 条）。
    /// `projects.*.mcpServers` 是项目级，v1 不读（spec §1 非目标）。
    fn read_mcps(&self) -> anyhow::Result<Vec<Capability>> {
        let file = self.home.claude_json();
        let Some(root) = read_json_if_present(&file)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        // 「没有 mcpServers 这个键」和「它的值是显式 `null`」→ 空表（正常状态）；
        // 键在、值既不是 `null` 也不是对象 → 故障上抛。前者用户就是没配 MCP，后者是配置
        // 写坏了——报成「没配任何 MCP」正是「形状故障也是故障」那块点名的假同步。
        if let Some(servers) = optional_object(root.get("mcpServers"), &file, "key `mcpServers`")? {
            for (name, cfg) in servers {
                out.push(mcp_capability(name, cfg, CLAUDE_CODE, &file)?);
            }
        }
        Ok(out)
    }

    /// plugin 的**安装记录**在 `~/.claude/plugins/installed_plugins.json`（键是
    /// `name@marketplace`，值是安装数组），**门禁**在 settings.json 的 `enabledPlugins`。
    /// 一个键可能对应多条安装（实测就是数组），v1 只取第一条；空数组 = 没装成，跳过。
    /// `"plugins": null` 与缺键同为「一个都没装」（容器层 `null` 裁决，见 `optional_object`）。
    fn read_plugins(&self) -> anyhow::Result<Vec<Capability>> {
        let file = self.home.claude_installed_plugins();
        let Some(root) = read_json_if_present(&file)? else {
            return Ok(Vec::new());
        };
        // 门禁读不出来就是读不出来：退化成「全部 enabled」会把用户关掉的 plugin 报成
        // 已启用，正是失败隔离契约禁止的那种「看起来合理」的假同步。
        let enabled_map = self.read_enabled_plugins()?;

        let mut out = Vec::new();
        let Some(plugins) = optional_object(root.get("plugins"), &file, "key `plugins`")? else {
            return Ok(out);
        };
        for (key, installs) in plugins {
            let (entry, marketplace) = split_plugin_key(key);
            let context = format!("plugins entry `{key}`");
            // 「值不是数组」和「数组确实为空（没装成）」是两种事实：只有后者可以跳过。
            // 这里不用 optional_array：`plugins` 的每条键都**已经声明**「这个 plugin 有安装
            // 记录」，`{"p@mp": null}` 是写了键却没给记录，和 `{"p@mp": [null]}` 同族，
            // 都是坏值而不是「没配」。
            let installs = expect_array(installs, &file, &context)?;
            let Some(first) = installs.first() else {
                continue;
            };
            let install = expect_object(first, &file, &format!("{context}[0]"))?;
            let enabled = gate_enabled(&enabled_map, key, &entry);
            out.push(Capability {
                id: CapId::new(Kind::Plugin, entry.clone()),
                origin: Origin::Adopted {
                    harness: CLAUDE_CODE.into(),
                },
                spec: Spec::Plugin {
                    marketplace,
                    entry,
                    version: normalize_locator(optional_string(
                        install, "version", &file, &context,
                    )?),
                    sha: normalize_locator(optional_string(
                        install,
                        "gitCommitSha",
                        &file,
                        &context,
                    )?),
                    install_path: optional_string(install, "installPath", &file, &context)?
                        .map(PathBuf::from),
                    enabled,
                },
                targets: Targets::All,
            });
        }
        Ok(out)
    }

    /// settings.json 的 enabledPlugins 是门禁；installed_plugins.json 才是安装记录。
    /// 没列出的 key 默认启用（未列 = 未表态）。
    fn read_enabled_plugins(&self) -> anyhow::Result<BTreeMap<String, bool>> {
        let file = self.home.claude_settings();
        let Some(root) = read_json_if_present(&file)? else {
            return Ok(BTreeMap::new());
        };
        let mut out = BTreeMap::new();
        // `{"enabledPlugins": null}` = 一条门禁都没写 = 全部未表态 = 全部启用，与缺键同义；
        // 但只有「键缺失或值为 null」才走这条路，`true` / `[]` 这类坏值仍然上抛。
        if let Some(obj) =
            optional_object(root.get("enabledPlugins"), &file, "key `enabledPlugins`")?
        {
            for (k, v) in obj {
                // 非布尔的门禁值是坏数据，不是「没表态」：猜成 true 就是假同步。
                let Some(b) = v.as_bool() else {
                    anyhow::bail!(
                        "failed to parse enabledPlugins entry {k}: expected a boolean, found {}",
                        type_name(v)
                    );
                };
                out.insert(k.clone(), b);
            }
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
        let mut caps = self.read_skills()?;
        caps.extend(self.read_mcps()?);
        caps.extend(self.read_plugins()?);
        caps.sort_by_key(|a| a.id.label());
        // dedup_by 只去重相邻项，所以必须先 sort
        caps.dedup_by(|a, b| a.id == b.id);
        Ok(caps)
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}

/// 读 JSON；文件不存在返回 `Ok(None)`，语法错误返回带路径的 Err。
///
/// 「没有这个文件」和「文件在但读不出/解析不了」必须是两种返回——前者是正常状态，
/// 后者按计划里的失败隔离契约整次 diff 失败。
///
/// **共用范围是「走 JSON 的两家」：claude 自己 + Task 11 的 kimi。**
/// 说「Task 10/11 共用」是错的（Task 9 期间的旧措辞，Task 10 落地后一直没改）：
/// codex 读 TOML，`adapters::codex::read_config` 是它自己那份，这两个函数它一个都不用。
///
/// 返回的文档还保证是**对象**：走 JSON 的这几家 harness 的配置文件都是「顶层一个 map」
/// 的形状，而调用方一律用 `root.get("...")` 取键。顶层不是对象时 `get` 只会给 `None`，
/// 于是 `[]` / `"oops"` / `5` 这类坏文件会被读成「一个键都没有」= 「这个 harness 没配
/// 任何能力」，正是「形状故障也是故障」那块禁止的塌缩。守卫放在这里，复用本函数的调用方
/// （claude 与 kimi）自动带上，不必各家再写一遍；codex 不需要这条守卫——TOML 文档本身就是
/// 一个键值表，`toml::from_str::<TomlTable>` 给不出 `"oops"` 那种顶层（见 `read_config`）。
///
/// 唯一的例外是整个文档就是一个 `null`：那是容器层的「无」，与「没有这个文件」同义
/// （计划里 `null` 裁决那条：容器层 `null` = 没配，叶子层 `optional_string` 早已这么读），
/// 所以返回 `Ok(None)` 而不是故障。`false` / `[]` / `"oops"` / `5` 仍然上抛。
pub(crate) fn read_json_if_present(path: &Path) -> anyhow::Result<Option<Value>> {
    let Some(value) = read_json_value_if_present(path)? else {
        return Ok(None);
    };
    if value.is_object() {
        Ok(Some(value))
    } else if value.is_null() {
        Ok(None)
    } else {
        Err(shape_fault(&value, path, "top level", "an object"))
    }
}

/// 纯解析半边（不存在 → `Ok(None)`，语法错 → 带路径的 Err），不校验顶层形状。
/// 顶层形状单独一步，是为了让报错里的「top level must be an object」和解析失败
/// 的「failed to parse」是两条可分别看守的故障。
///
/// `with_context` 留下的 cause 为什么不会漏值：`serde_json::Error` 的 `Display` 实测
/// （1.0.151）只有「原因文本 + line/column」，不含源码，所以整条链渲染出来也没有原文。
/// 这与 `toml::de::Error` **相反**——后者逐字回显出错行，codex 侧因此只取
/// `Error::message()`（见 `adapters::codex::read_config`）。**不要在下面拼接任何文档原文**，
/// 门是 `a_json_parse_failure_never_echoes_the_offending_line`。
fn read_json_value_if_present(path: &Path) -> anyhow::Result<Option<Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let v = serde_json::from_str(&text)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            Ok(Some(v))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("failed to read {}", path.display()))),
    }
}

/// `"superpowers@claude-plugins-official"` → `("superpowers", Some("claude-plugins-official"))`
///
/// **claude 与 codex 两家的安装记录键都是 `name@marketplace`，都用这一个函数。**
/// 此前这里的文档写着「只有 claude 需要拆」，而 codex 侧因此另写了一份逐字相同的私有
/// `split_key`（Task 10 的派单边界，它当时不许改本文件）——两份无同步守卫的孪生实现是
/// 真实的漂移风险（拆分规则一改，一侧忘改就是 marketplace 静默变 `None`），所以现在合并成
/// 这一份，`adapters::codex::read_plugins` 直接调用它。
/// 签名与配置文件格式无关：收 `&str`、返回 owned 对，**不**碰 `serde_json::Value`，
/// 这也是它能同时服务 JSON 与 TOML 两家的原因（上面那两个 JSON helper 就做不到）。
/// kimi 不需要拆：它的 `installed.json` 是数组、身份直接躺在 `id` 字段里（Task 11）。
pub(crate) fn split_plugin_key(key: &str) -> (String, Option<String>) {
    match key.split_once('@') {
        Some((entry, mp)) => (entry.to_string(), Some(mp.to_string())),
        None => (key.to_string(), None),
    }
}

/// 查一个 plugin 的门禁。全文键 `name@marketplace` 优先（实测 settings.json 就写这个），
/// 没命中时退到裸 entry 名。
///
/// 退这一跳不是「宽容」，而是不退就又是一次假同步：手改 settings.json 常见的
/// `{"p": false}`（对安装记录键 `p@mp`）会被全文匹配整个忽略掉，于是**用户关掉的**
/// plugin 报成 `enabled: true`，diff 把「你禁用了它」念成「已同步」，报告里没有任
/// 何地方说明这个门禁键没被理解。
///
/// 反方向不成立：`enabledPlugins` 里有一个匹配不上任何安装记录的键，不能当故障上抛
/// ——卸载后残留的门禁键（或还没装的 plugin 预先写好开关）是正常状态，硬失败会把健康
/// 配置判成「读不出来」，那是另一种漏报。所以这里只在「同一条 plugin 上有两种写法」时
/// 定优先级：全文键更具体，赢裸名。
///
/// 两次查找都落空时的代价写在
/// `a_mis_typed_full_gate_key_silently_reads_as_enabled`：手抖少一个字母的全文键
/// （`superpower@mp` 对安装记录 `superpowers@mp`）谁也不匹配，于是静默 `enabled: true`。
/// 那是本裁决**已知**的残留盲点，那条测试钉住它，是为了让下一次改动必须红点什么，
/// 不是为了说它是对的。
fn gate_enabled(enabled_map: &BTreeMap<String, bool>, key: &str, entry: &str) -> bool {
    enabled_map
        .get(key)
        .or_else(|| enabled_map.get(entry))
        .copied()
        .unwrap_or(true)
}

/// 把一个 MCP 配置对象归一化。env 值保持 `${VAR}` 引用形态，绝不展开。
///
/// 只搬 `command` / `args` / `env` 三样：`type` 之类先不进 `Spec`（Task 3 的 `Spec::Mcp`
/// 里没有承载它的字段）。
/// **共用范围同 `read_json_if_present`：走 JSON 的两家（claude + Task 11 的 kimi）。**
/// codex 不复用它——它的 MCP 在 `config.toml` 里，`adapters::codex::read_mcps` 用
/// `toml::Value` 那一套原语读，类型域与这里的 `serde_json::Value` 不通。
///
/// 三样各自的「没配」和「配坏了」都不可合并，所以本函数是 `Result`：
/// `command` 缺键（或显式 `null`）才是 `""`——`Spec::Mcp.command` 是 `String`，
/// 「没有 command」和「command 是空串」在 v1 里同为「没配」；但 `command: 5` 是故障。
/// `args` / `env` 缺键（或显式 `null`，与缺键同义）→ 空集合；键在而类型不对、或元素/值
/// 不是字符串 → 故障上抛，否则一个写坏的 `args: "mcp"` 会连同内容一起被丢掉，只剩一个空
/// args 去看下一次比较。`args` 的元素类型是**强制**位置（`expect_string`），所以
/// `args: [null]` 是故障不是「没配」——这条分界见 `optional_object` 的文档。
/// `file` 只用于报错定位（Task 11 传它自己的 `~/.kimi-code/mcp.json`）。
pub(crate) fn mcp_capability(
    name: &str,
    cfg: &Value,
    harness: &str,
    file: &Path,
) -> anyhow::Result<Capability> {
    let context = format!("mcp server `{name}`");
    let cfg = expect_object(cfg, file, &context)?;
    let command = optional_string(cfg, "command", file, &context)?
        .map(String::from)
        .unwrap_or_default();

    let mut args = Vec::new();
    if let Some(list) = optional_array(cfg.get("args"), file, &format!("{context} field `args`"))? {
        for (i, element) in list.iter().enumerate() {
            args.push(
                expect_string(element, file, &format!("{context} field `args[{i}]`"))?.to_string(),
            );
        }
    }

    let mut env = BTreeMap::new();
    if let Some(fields) = optional_object(cfg.get("env"), file, &format!("{context} field `env`"))?
    {
        for (k, v) in fields {
            env.insert(
                k.clone(),
                expect_string(v, file, &format!("{context} field `env` key `{k}`"))?.to_string(),
            );
        }
    }

    Ok(Capability {
        id: CapId::new(Kind::Mcp, name.to_string()),
        origin: Origin::Adopted {
            harness: harness.into(),
        },
        spec: Spec::Mcp { command, args, env },
        targets: Targets::All,
    })
}

/// 值的类型名，只用于报错。**永远不返回值本身**——actual 侧的配置可能是明文密钥，
/// 而报错会进终端输出和 Task 14 的快照产物。
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// 「形状故障」的统一报错：文件路径 + 位置（只到键名为止）+ 期望类型 + 实际类型。
fn shape_fault(found: &Value, file: &Path, what: &str, expected: &str) -> anyhow::Error {
    anyhow!(
        "invalid shape in {}: {what} must be {expected}, found {}",
        file.display(),
        type_name(found)
    )
}

/// **强制**位置的形状裁决：「键缺失」由调用方的 `Option` 分支处理（那是正常状态），
/// 这里只裁决「类型不对」——包括显式 `null`，它在强制位置是一个写坏的值。
/// 可以缺位的容器用 `optional_object` / `optional_array`。
pub(crate) fn expect_object<'a>(
    value: &'a Value,
    file: &Path,
    what: &str,
) -> anyhow::Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| shape_fault(value, file, what, "an object"))
}

pub(crate) fn expect_array<'a>(
    value: &'a Value,
    file: &Path,
    what: &str,
) -> anyhow::Result<&'a Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| shape_fault(value, file, what, "an array"))
}

pub(crate) fn expect_string<'a>(
    value: &'a Value,
    file: &Path,
    what: &str,
) -> anyhow::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| shape_fault(value, file, what, "a string"))
}

/// 可选字符串字段：键缺失 → `Ok(None)`；显式 `null` → `Ok(None)`（JSON 里写「无」的
/// 常见形态，与缺键同义）；**有值但不是字符串** → 故障上抛，不塌成 `None`。
pub(crate) fn optional_string<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    file: &Path,
    context: &str,
) -> anyhow::Result<Option<&'a str>> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    expect_string(value, file, &format!("{context} field `{key}`")).map(Some)
}

/// **可缺位**的容器对象：键缺失**或**值是显式 `null` → `Ok(None)`（都是「没配」，正常状态）；
/// 值在且不是对象 → 故障上抛。
///
/// 为什么容器层的 `null` 算缺位而不是故障：`optional_string` 在叶子层已经把显式 `null`
/// 读成缺键（JSON 里写 `null` 常是「无」的表示法），容器层若判成故障，同一份配置就有两种
/// 互相矛盾的读法，而且是往「把没配的东西报成读不出来」那个方向错。
///
/// 分界靠**位置**而不是靠类型：`expect_object` / `expect_array` / `expect_string` 服务的是
/// 「值已经在那里了，必须是对的类型」的位置——数组元素（`args[0]`）、被选中的安装记录、
/// `mcpServers` 里一个已具名 server 的配置。那些位置的 `null` 是坏值，不是「没配」，
/// 所以 `type_name` 的 `"null"` 分支必须仍然可达。
///
/// 形状原语这一族（`expect_object` / `expect_array` / `expect_string` / `optional_string` /
/// 本函数 / `optional_array`）都是 `pub(crate)`：**走 JSON 的调用方**（claude 自己 +
/// Task 11 的 kimi）复用它们，不许另写一份。
/// 这里也曾经写着「Task 10/11 复用它们」，那是错的：codex 手里是 `toml::Value`，与本族收的
/// `serde_json::Value` 类型域不通，一个都喂不进去，所以它另写了 `toml_expect_*` /
/// `toml_optional_*` 那一套。两套只能各自被自己的测试钉住，改形状规则时**两处都要改**。
/// 「看见缺键」的那三个是 `optional_*`，它们收 `Option<&Value>`；`expect_*` 只收 `&Value`。
pub(crate) fn optional_object<'a>(
    value: Option<&'a Value>,
    file: &Path,
    what: &str,
) -> anyhow::Result<Option<&'a Map<String, Value>>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    expect_object(value, file, what).map(Some)
}

/// `optional_object` 的数组版，同一条分界。调用方是 `args` 这类「缺键 = 空集合」的字段；
/// 元素类型仍走 `expect_*`，所以 `[null]` 是故障不是空数组。
pub(crate) fn optional_array<'a>(
    value: Option<&'a Value>,
    file: &Path,
    what: &str,
) -> anyhow::Result<Option<&'a Vec<Value>>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    expect_array(value, file, what).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::KIMI_CODE;
    use crate::capability::{Kind, Origin, SkillSource, Spec, Targets};
    use crate::testutil::TempHome;
    use serde_json::json;
    use std::path::PathBuf;

    fn find<'a>(caps: &'a [Capability], kind: Kind, name: &str) -> Option<&'a Capability> {
        caps.iter().find(|c| c.id.kind == kind && c.id.name == name)
    }

    /// 断言失败时把整份输出的 id 列出来，省得只看到「2 != 0」却不知道多出来的是谁。
    fn labels(caps: &[Capability]) -> Vec<String> {
        caps.iter().map(|c| c.id.label()).collect()
    }

    /// 形状故障的报错文本按**整串**钉死（不 `contains`）：这四段——文件路径、位置（只到
    /// 键名）、期望类型、实际类型——就是那条规矩的全部内容，多一个字都可能是漏值。
    fn shape_msg(file: &Path, what: &str, expected: &str, found: &str) -> String {
        format!(
            "invalid shape in {}: {what} must be {expected}, found {found}",
            file.display()
        )
    }

    /// `read_state` 必须把形状故障上抛；返回报错文本供逐字比较。
    fn read_state_err(t: &TempHome) -> String {
        ClaudeCode::new(t.home.clone())
            .read_state()
            .expect_err("形状故障必须上抛，不能塌成空表")
            .to_string()
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

    #[test]
    fn reads_mcp_from_claude_json_not_settings_json() {
        let t = TempHome::new();
        t.mkdir(".claude")
            // settings.json 里放一个同名 MCP，若实现读错文件就会拿到 "wrong"
            .write(
                ".claude/settings.json",
                r#"{ "mcpServers": { "ctx": { "command": "wrong" } } }"#,
            )
            .write(
                ".claude.json",
                r#"{
                  "numStartups": 52,
                  "mcpServers": {
                    "kimi-cu": {
                      "type": "stdio",
                      "command": "/Applications/KimiCU.app/Contents/MacOS/kimi-cu",
                      "args": ["mcp", "-s", "user"],
                      "env": {}
                    },
                    "ctx": { "command": "npx", "args": ["-y", "@upstash/context7-mcp"],
                             "env": { "CONTEXT7_API_KEY": "${CONTEXT7_API_KEY}" } }
                  },
                  "projects": { "/some/path": { "trust_level": "trusted" } }
                }"#,
            );

        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let m = find(&caps, Kind::Mcp, "ctx").expect("mcp read from ~/.claude.json");
        assert_eq!(
            m.spec,
            Spec::Mcp {
                command: "npx".into(),
                args: vec!["-y".into(), "@upstash/context7-mcp".into()],
                env: [(
                    "CONTEXT7_API_KEY".to_string(),
                    "${CONTEXT7_API_KEY}".to_string()
                )]
                .into(),
            }
        );
        assert!(find(&caps, Kind::Mcp, "kimi-cu").is_some());
        assert_eq!(caps.iter().filter(|c| c.id.kind == Kind::Mcp).count(), 2);
        // helper 是三家共用的，归属和 targets 都得在真实调用路径上钉住：
        // `targets` 改成 `Only(["claude-code"])` 曾经在全绿套件下活下来（评审实测）。
        assert_eq!(
            m.origin,
            Origin::Adopted {
                harness: "claude-code".into()
            }
        );
        assert_eq!(
            m.targets,
            Targets::All,
            "actual 侧读出来的能力不该自带 targets 限制"
        );
    }

    #[test]
    fn project_level_mcp_servers_are_out_of_scope() {
        let t = TempHome::new();
        // 顶层 `mcpServers` **整个键都不写**（评审前这里写的是 `"mcpServers": {}`，
        // 于是只有「无条件合并项目级」那种变异会被杀掉；「用户级缺失时回落到项目级」
        // 照样活下来）。键缺失本身必须是正常的空状态，见
        // `mcp_servers_key_that_is_not_an_object_is_an_error_naming_the_key`。
        t.write(
            ".claude.json",
            r#"{ "projects": { "/p": { "mcpServers": { "proj-only": { "command": "c" } } } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(
            find(&caps, Kind::Mcp, "proj-only").is_none(),
            "v1 只读用户级 MCP"
        );
        assert_eq!(
            caps.iter().filter(|c| c.id.kind == Kind::Mcp).count(),
            0,
            "顶层 mcpServers 缺失 ⇒ 一个 MCP 都没有，项目级不得回填"
        );
    }

    #[test]
    fn env_reference_is_preserved_unexpanded() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "env": { "K": "${HENV_TEST_SECRET_XYZ}" } } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Mcp, "m").unwrap().spec {
            Spec::Mcp { env, .. } => {
                assert_eq!(env["K"], "${HENV_TEST_SECRET_XYZ}", "must stay a reference");
                assert!(!env.values().any(|v| v == "expanded-value"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unrelated_user_fields_do_not_break_parsing() {
        // merge 安全的前置条件：读侧必须对未知字段免疫
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "userID": "abc", "tipsHistory": {"x": 1}, "mcpServers": { "m": { "command": "c" } },
                 "someFutureField": [1,2,3] }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "theme": "auto", "tui": "fullscreen", "permissions": { "allow": ["x"] },
                 "enabledPlugins": {} }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Mcp, "m").is_some());
    }

    #[test]
    fn reads_plugin_with_sha_and_marketplace() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{
              "version": 2,
              "plugins": {
                "superpowers@claude-plugins-official": [
                  { "scope": "user",
                    "installPath": "/Users/x/.claude/plugins/cache/claude-plugins-official/superpowers/6.4.1",
                    "version": "6.4.1",
                    "installedAt": "2026-08-26T15:19:21.589Z",
                    "lastUpdated": "2026-09-23T00:37:36.131Z",
                    "gitCommitSha": "b36e0829c6d0140e93cfef2ca599b1b07d4a7797" }
                ],
                "context7@claude-plugins-official": [
                  { "scope": "user", "installPath": "/p/context7", "version": "db467cc56673" }
                ]
              }
            }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "enabledPlugins": { "context7@claude-plugins-official": false } }"#,
        );

        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let sp = find(&caps, Kind::Plugin, "superpowers").expect("identity is the entry name");
        // plugin 和 MCP 一样走的是三家共用的形状，origin/targets 也得钉住——
        // 评审实测：把 `targets: Targets::All` 换成 `Only(["claude-code"])` 全绿。
        assert_eq!(
            sp.origin,
            Origin::Adopted {
                harness: "claude-code".into()
            }
        );
        assert_eq!(
            sp.targets,
            Targets::All,
            "actual 侧读出来的能力不该自带 targets 限制"
        );
        assert_eq!(
            sp.spec,
            Spec::Plugin {
                marketplace: Some("claude-plugins-official".into()),
                entry: "superpowers".into(),
                version: Some("6.4.1".into()),
                sha: Some("b36e0829c6d0140e93cfef2ca599b1b07d4a7797".into()),
                install_path: Some(PathBuf::from(
                    "/Users/x/.claude/plugins/cache/claude-plugins-official/superpowers/6.4.1"
                )),
                enabled: true,
            }
        );

        let c7 = find(&caps, Kind::Plugin, "context7").unwrap();
        match &c7.spec {
            Spec::Plugin {
                enabled,
                sha,
                version,
                ..
            } => {
                assert!(!enabled, "settings.json enabledPlugins=false must win");
                assert_eq!(*sha, None);
                assert_eq!(
                    version.as_deref(),
                    Some("db467cc56673"),
                    "sha 形状的 version 目录名仍留在 version 字段，不猜测"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugin_without_sha_field_is_none_not_empty() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "x@mp": [ { "scope": "user", "version": "1.0.0" } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "x").unwrap().spec {
            Spec::Plugin { sha, version, .. } => {
                assert_eq!(*sha, None);
                assert_eq!(version.as_deref(), Some("1.0.0"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugin_key_without_marketplace_suffix_still_parses() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "solo": [ { "version": "1.0" } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "solo").unwrap().spec {
            Spec::Plugin {
                marketplace, entry, ..
            } => {
                assert_eq!(*marketplace, None);
                assert_eq!(entry, "solo");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_install_array_is_skipped() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "ghost@mp": [] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Plugin, "ghost").is_none());
    }

    /// 一条键多条安装时 v1 **只取第一条，其余整条丢掉**，两件事分开钉。
    /// 「取的是第一条」由本条钉：`installs.first()` 改成 `.last()`，实测只红本条。
    /// 「其余整条丢掉」由 `a_second_install_record_never_reaches_the_capability_list` 在
    /// dedup **之前**钉——本条看不出那个区别：`read_state` 的 `sort_by_key` + `dedup_by`
    /// 会把「逐条 push」压回同一个输出（实测改成 push-all 后本条仍绿，全套件只红那一条新
    /// 测试）。
    /// 残余：多条安装到底该取哪一条（越新越优先？）没在这里裁定，那是 Task 13/14 的裁决，
    /// 和「同名不同 marketplace 被 dedup 合并留哪条」是同一族问题。
    #[test]
    fn only_the_first_install_record_is_read_and_the_rest_are_dropped() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [
                  { "version": "1.0", "gitCommitSha": "aaa" },
                  { "version": "2.0", "gitCommitSha": "bbb" } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let same_id: Vec<&Capability> = caps
            .iter()
            .filter(|c| c.id == CapId::new(Kind::Plugin, "p"))
            .collect();
        assert_eq!(same_id.len(), 1, "一个 plugin 键只产出一条能力");
        match &same_id[0].spec {
            Spec::Plugin {
                version,
                sha,
                enabled,
                ..
            } => {
                assert_eq!(version.as_deref(), Some("1.0"), "取的是第一条");
                assert_eq!(sha.as_deref(), Some("aaa"), "locator 也来自第一条");
                assert!(*enabled);
            }
            other => panic!("{other:?}"),
        }
    }

    /// 「其余整条丢掉」这件事只能在 dedup 之前看守：`read_state` 会把重复 id 压回一条，
    /// 所以「每条安装都 push」和「只 push 第一条」在 `read_state` 的输出上**完全等价**
    /// （实测 push-all 变异下 `only_the_first_install_record_is_read_and_the_rest_are_dropped`
    /// 仍绿）。这里直接调 `read_plugins()`，多一条就红一条。
    #[test]
    fn a_second_install_record_never_reaches_the_capability_list() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [
                  { "version": "1.0", "gitCommitSha": "aaa" },
                  { "version": "2.0", "gitCommitSha": "bbb" } ] } }"#,
        );
        let plugins = ClaudeCode::new(t.home.clone())
            .read_plugins()
            .expect("多条安装记录本身不是故障");
        assert_eq!(
            labels(&plugins),
            vec!["plugin/p".to_string()],
            "其余记录必须在进列表之前就被丢掉，而不是留给 read_state 的 dedup 合并"
        );
    }

    /// 派单字面代码在 `read_plugins` 里写的是
    /// `self.read_enabled_plugins().unwrap_or_default()`（注释：「读失败不该拖垮 plugin
    /// 扫描，退化成全部视为 enabled」）。那正是失败隔离契约禁止的形状：一个语法坏掉的
    /// settings.json 会把用户**关掉**的 plugin 报成 `enabled: true`，diff 于是把
    /// 「你禁用了它」念成「已同步」，而报告里没有任何地方说明 settings.json 没读成。
    /// 所以这里上抛，并留测试看守。变异实测（第二轮重跑：把 `?` 换回
    /// `.unwrap_or_default()`，全套件 173 passed / **4 failed**）：红的是四条，不是本注释
    /// 先前说的两条——
    /// `a_malformed_settings_json_is_an_error_not_a_silent_enable_all`（语法坏）、
    /// `a_non_boolean_enable_gate_is_an_error_naming_the_plugin`（门禁值类型坏）、
    /// `enabled_plugins_that_is_not_an_object_is_an_error_naming_the_key`（门禁表自身类型坏）、
    /// `a_config_file_whose_top_level_is_not_an_object_is_an_error`（三条文件里坏的是
    /// settings.json 那一轮，实测报错里返回的正是那条 `enabled: true` 的 plugin）。
    /// 四条都走同一个 `?`，所以「谁在看守这个 `?`」的答案就是这四条。
    #[test]
    fn a_malformed_settings_json_is_an_error_not_a_silent_enable_all() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(".claude/settings.json", "{ enabledPlugins: ");
        let err = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect_err("settings.json 语法坏了不能当成「没有任何 disabled」");
        let msg = err.to_string();
        assert!(msg.contains("settings.json"), "{msg}");
    }

    /// `enabledPlugins` 的门禁值只认布尔。写成字符串 `"false"` 是手改 settings.json 的常见
    /// 笔误，猜成 `true` 又是一次假同步，所以带 plugin 键名上抛。
    /// 报错里出现的是**键名和值的类型**，不是值本身——「活配置里的值不进产物」这条规矩
    /// 对 settings.json 同样适用（MCP env 只是它最容易被踩的那个实例）。
    #[test]
    fn a_non_boolean_enable_gate_is_an_error_naming_the_plugin() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "enabledPlugins": { "p@mp": "false" } }"#,
        );
        let err = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect_err("非布尔门禁不能猜成启用")
            .to_string();
        assert!(err.contains("enabledPlugins entry p@mp"), "{err}");
        assert!(err.contains("a string"), "{err}");
        assert!(!err.contains("false"), "报错里不得出现原值：{err}");
    }

    /// version / sha 必须过 [`normalize_locator`]：本机 `installed_plugins.json` 里就躺着
    /// 字面量 `unknown`，不归一化的话两条 `Some("unknown")` 会命中 `compare_versions`
    /// 的「两边都有 SHA」分支，两个真不同的构建被报成 `Same`。派单 14 条没有这个形状。
    /// 变异实测：把 `normalize_locator(..)` 换成裸 `.and_then(Value::as_str)` 时只有这一条红
    /// （version 与 sha 两处是**两个**独立变异体，实测各红一次，都在同一条测试里）。
    #[test]
    fn sentinel_version_and_sha_become_none() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "unknown", "gitCommitSha": "" } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin { version, sha, .. } => {
                assert_eq!(*sha, None, "空串不是定位符");
                assert_eq!(*version, None, "`unknown` 不是版本号");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_files_are_not_errors() {
        let t = TempHome::new();
        t.mkdir(".claude");
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("no config != error");
        assert!(caps.is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_naming_the_file() {
        let t = TempHome::new();
        t.write(".claude.json", "{ not json");
        let err = ClaudeCode::new(t.home.clone())
            .read_state()
            .unwrap_err()
            .to_string();
        assert!(err.contains(".claude.json"), "{err}");
    }

    /// `toml::de::Error` 在 Task 10 实测**逐字回显出错那一行的源码**（`2 | TOKEN = "sk-live-…"`），
    /// 而本文件 `read_json_value_if_present` 的 `.with_context(..)` 把 `serde_json::Error`
    /// 原样留在 cause 链里、Task 15 的 CLI 又打算打印整条链——所以「JSON 侧不回显」在此之前
    /// 只是一条**没人测过的假设**。本条把它变成门。
    /// 实测结果（serde_json 1.0.151）：`Display` 只给「原因文本 + line/column」，不含源码，
    /// 所以本条**通过**；它把「不回显」钉成回归门槛——解析器哪天改口、或有人把出错行拼进
    /// 报错，这条就红。
    /// 通过**不等于**「JSON 路径一般安全」：真正撑着这件事的是本仓一律解成无类型 `Value`
    /// （带类型的 derive 反序列化会产出 serde 的 `invalid type: string "…"`，那里面就是值），
    /// 而不是 serde_json 客气。
    /// 断言渲染用 `{:#}` 与 `{:?}`：`to_string()` 只有首行，`with_context` 管不住 cause，
    /// 只看首行会漏掉整条链（同一条教训见 codex 的
    /// `a_toml_parse_failure_never_echoes_the_offending_line`）。
    #[test]
    fn a_json_parse_failure_never_echoes_the_offending_line() {
        let t = TempHome::new();
        // 密钥就写在**出错那一行**上：值没加引号，解析器停下来的位置正是 `sk-live-…`。
        t.write(
            ".claude.json",
            "{\n  \"mcpServers\": {\n    \"ctx\": { \"env\": { \"TOKEN\": sk-live-9f3a2b7c } }\n  }\n}\n",
        );
        let err = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect_err("语法坏的 .claude.json 必须是故障");
        // `TOKEN` 是那行上的键名：形状故障的报错允许带键名，但这条坏在**解析之前**，
        // 任何回显原文（整行还是半个 token 都一样）都会把它带出来。
        for rendered in [format!("{err:#}"), format!("{err:?}")] {
            assert!(
                !rendered.contains("sk-live-9f3a2b7c"),
                "整条报错链都不得带出原值：{rendered}"
            );
            assert!(
                !rendered.contains("TOKEN"),
                "报错不得回显出错那一行的原文：{rendered}"
            );
            // 路径与解析器自己的原因文本都还得在——不回显不能降级成一句「解析失败」。
            assert!(
                rendered.contains(&t.path(".claude.json").display().to_string()),
                "报错必须点明是哪个文件：{rendered}"
            );
            assert!(
                rendered.contains("expected value"),
                "报错必须留下解析器给的原因文本：{rendered}"
            );
        }
    }

    #[test]
    fn output_is_sorted_and_deduplicated_by_id() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/zzz")
            .mkdir(".claude/skills/aaa")
            .write(
                ".claude.json",
                r#"{ "mcpServers": { "z": { "command": "c" }, "a": { "command": "c" } } }"#,
            );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "{labels:?}");
        let mut dedup = labels.clone();
        dedup.dedup();
        assert_eq!(labels.len(), dedup.len(), "duplicate ids: {labels:?}");
    }

    /// 同名 plugin 来自两个 marketplace 时，`installed_plugins.json` 的两个键归一化成同一个
    /// `CapId`，派单要求的 dedup 于是**丢掉一条**。这条测试是钉住「确实只剩一条」这个事实，
    /// 不是认可合并是对的：合并留下哪条由 serde_json `Map` 的键序（BTreeMap，未开
    /// `preserve_order`）+ 稳定排序决定，所以是可复现的。真要区分两个 marketplace 得把
    /// 它纳入 `CapId`，那是 Task 13/14 的裁决，不在 Task 9。
    /// 变异实测（第二轮在 178 条套件上重跑 8 次，每次只红这一条）：删掉 `dedup_by` 那行，
    /// 红的是本条（`dups.len()` 变成 2）。`output_is_sorted_and_deduplicated_by_id` 不看守
    /// 这一步：它的 fixture 四条 id 互不相等，删掉 dedup 它照样绿——「去重真的发生了」
    /// 只有本条在钉。
    #[test]
    fn same_entry_name_from_two_marketplaces_collapses_to_one_capability() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": {
                 "dup@mp-b": [ { "version": "2.0" } ],
                 "dup@mp-a": [ { "version": "1.0" } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let dups: Vec<&Capability> = caps
            .iter()
            .filter(|c| c.id.kind == Kind::Plugin && c.id.name == "dup")
            .collect();
        assert_eq!(dups.len(), 1, "id 相同的多条会被 dedup 合并成一条");
        match &dups[0].spec {
            Spec::Plugin {
                marketplace,
                version,
                ..
            } => {
                assert_eq!(
                    marketplace.as_deref(),
                    Some("mp-a"),
                    "留下的是键序在前的那条"
                );
                assert_eq!(version.as_deref(), Some("1.0"));
            }
            other => panic!("{other:?}"),
        }
    }

    // ============ 形状故障：「键在但类型不对」不得塌成空扫描 ============

    #[test]
    fn mcp_servers_key_that_is_not_an_object_is_an_error_naming_the_key() {
        let t = TempHome::new();
        t.write(".claude.json", r#"{ "mcpServers": [] }"#);
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "key `mcpServers`",
                "an object",
                "an array"
            )
        );
    }

    #[test]
    fn plugins_key_that_is_not_an_object_is_an_error_naming_the_key() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": "oops" }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "key `plugins`",
                "an object",
                "a string"
            )
        );
    }

    /// `{"g@mp": "notarray"}` 和 `{"g@mp": []}` 曾经走同一条 `continue`：前者是配置写坏了，
    /// 后者才是「没装成」。空数组那条由 `empty_install_array_is_skipped` 守着，本条守着非数组。
    #[test]
    fn an_install_list_that_is_not_an_array_is_an_error_naming_the_entry() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "g@mp": "notarray" } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "plugins entry `g@mp`",
                "an array",
                "a string"
            )
        );
    }

    #[test]
    fn an_install_record_that_is_not_an_object_is_an_error_naming_the_entry() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "g@mp": [ "not-a-record" ] } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "plugins entry `g@mp`[0]",
                "an object",
                "a string"
            )
        );
    }

    /// 配置文件顶层不是对象时，`root.get(..)` 只会给 `None`，于是坏文件长得和
    /// 「一个键都没写」一模一样。守卫在 `read_json_if_present` 里，三个文件共用一处，
    /// 所以这里三家都点一遍（少一家就是少一次看守）。
    /// 门禁表本身也是同一个形状的一层：`"enabledPlugins": true` 过去走 `and_then(as_object)`
    /// 得到「一条门禁都没有」= 全部按 enabled 处理，正是被裁定为 Critical 的那种塌缩。
    #[test]
    fn enabled_plugins_that_is_not_an_object_is_an_error_naming_the_key() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(".claude/settings.json", r#"{ "enabledPlugins": true }"#);
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/settings.json"),
                "key `enabledPlugins`",
                "an object",
                "a boolean"
            )
        );
    }

    #[test]
    fn a_config_file_whose_top_level_is_not_an_object_is_an_error() {
        for rel in [
            ".claude.json",
            ".claude/settings.json",
            ".claude/plugins/installed_plugins.json",
        ] {
            let t = TempHome::new();
            // settings.json 只有「确实有 plugin 要看门禁」时才会被读到
            if rel == ".claude/settings.json" {
                t.write(
                    ".claude/plugins/installed_plugins.json",
                    r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
                );
            }
            t.write(rel, "[ 1, 2, 3 ]");
            assert_eq!(
                read_state_err(&t),
                shape_msg(&t.path(rel), "top level", "an object", "an array"),
                "{rel}"
            );
        }
    }

    #[test]
    fn an_mcp_entry_that_is_not_an_object_is_an_error_naming_the_server() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": "not-an-object" } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m`",
                "an object",
                "a string"
            )
        );
    }

    #[test]
    fn a_non_object_mcp_env_is_an_error_naming_the_server_and_field() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "env": [ [ "K", "V" ] ] } } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m` field `env`",
                "an object",
                "an array"
            )
        );
    }

    /// 非字符串的 env 值过去是 `filter_map` 直接丢掉：键名一起消失，于是「配坏了的 env」和
    /// 「没有 env」在 diff 里长得一样。现在上抛，且报错只到键名为止——值可能就是明文密钥。
    #[test]
    fn a_non_string_mcp_env_value_is_an_error_that_never_echoes_it() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c",
                  "env": { "TOKEN": { "nested": "sk-live-9f3a2b7c" } } } } }"#,
        );
        let err = read_state_err(&t);
        assert_eq!(
            err,
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m` field `env` key `TOKEN`",
                "a string",
                "an object"
            )
        );
        assert!(!err.contains("sk-live-9f3a2b7c"), "报错不得带出原值：{err}");
        assert!(!err.contains("nested"), "报错不得带出容器内容：{err}");
    }

    #[test]
    fn a_non_array_mcp_args_is_an_error_naming_the_server_and_field() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "args": "mcp -s user" } } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m` field `args`",
                "an array",
                "a string"
            )
        );
    }

    #[test]
    fn a_non_string_mcp_args_element_is_an_error_naming_the_index() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "args": ["-y", 7] } } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m` field `args[1]`",
                "a string",
                "a number"
            )
        );
    }

    #[test]
    fn a_non_string_mcp_command_is_an_error_naming_the_field() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": 5 } } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m` field `command`",
                "a string",
                "a number"
            )
        );
    }

    #[test]
    fn a_non_string_plugin_version_is_an_error_naming_the_field() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": 6.4 } ] } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "plugins entry `p@mp` field `version`",
                "a string",
                "a number"
            )
        );
    }

    /// 三个 locator 字段各是一个独立站点（`version` / `gitCommitSha` / `installPath`），
    /// 只测其中一个的话另外两个换回 `.and_then(Value::as_str)` 照样全绿。
    #[test]
    fn a_non_string_plugin_sha_is_an_error_naming_the_field() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "gitCommitSha": ["b36e082"] } ] } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "plugins entry `p@mp` field `gitCommitSha`",
                "a string",
                "an array"
            )
        );
    }

    #[test]
    fn a_non_string_plugin_install_path_is_an_error_naming_the_field() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "installPath": true } ] } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "plugins entry `p@mp` field `installPath`",
                "a string",
                "a boolean"
            )
        );
    }

    /// 显式 `null` 和缺键同为「没有这个 locator」（JSON 里写「无」的常见形态），不是故障。
    /// 类型不对的**有值**才是故障，见上面两条。
    #[test]
    fn explicit_null_locator_fields_read_as_absent_not_as_a_shape_fault() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": null } } }"#,
        );
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [
                  { "version": null, "gitCommitSha": null, "installPath": null } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("`null` 是「没有值」，不是形状故障");
        match &find(&caps, Kind::Mcp, "m").unwrap().spec {
            Spec::Mcp { command, .. } => assert_eq!(command, ""),
            other => panic!("{other:?}"),
        }
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin {
                version,
                sha,
                install_path,
                ..
            } => {
                assert_eq!(*version, None);
                assert_eq!(*sha, None);
                assert_eq!(*install_path, None);
            }
            other => panic!("{other:?}"),
        }
    }

    // ============ `null` 的两种读法：容器层 = 缺键，强制位置 = 坏值 ============

    /// `{"mcpServers": null}` 与缺键同为「没配 MCP」。叶子层的 `optional_string` 早就把显式
    /// `null` 读成缺键了，容器层若判故障，同一份配置就有两种互相矛盾的读法——而且错往
    /// 「把一件本来就没配的事报成整次 diff 失败」那个方向错，正是失败隔离契约禁的。
    #[test]
    fn a_null_mcp_servers_container_reads_as_no_servers_configured() {
        let t = TempHome::new();
        t.write(".claude.json", r#"{ "mcpServers": null }"#);
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("`mcpServers: null` 是没配，不是形状故障");
        assert!(caps.is_empty(), "{:?}", labels(&caps));
    }

    /// `{"plugins": null}` 同上：一个都没装，不是「读不出来」。
    #[test]
    fn a_null_plugins_container_reads_as_no_plugins_configured() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": null }"#,
        );
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("`plugins: null` 是没装，不是形状故障");
        assert!(caps.is_empty(), "{:?}", labels(&caps));
    }

    /// `{"enabledPlugins": null}` = 一条门禁都没表态 = 全部按启用读，与「没有 settings.json」
    /// 同义。注意方向和 `only_the_first_install_record...` 一致：这里没有新的信任被给出，
    /// 只是「没写」继续不等于「坏了」。
    #[test]
    fn a_null_enabled_plugins_container_reads_as_no_gates() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(".claude/settings.json", r#"{ "enabledPlugins": null }"#);
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("`enabledPlugins: null` 是没表态，不是形状故障");
        match &find(&caps, Kind::Plugin, "p")
            .expect("plugin 仍要读到")
            .spec
        {
            Spec::Plugin { enabled, .. } => assert!(*enabled, "空门禁表按未表态处理"),
            other => panic!("{other:?}"),
        }
    }

    /// `args` 是「缺键 = 空集合」的可选字段，所以 `args: null` 读成空 args。
    /// 与下一条是**两个**独立站点（keep-one 实测：本条只对 `args` 换回 `expect_array` 起反应，
    /// `env` 那侧的退化只红下一条），合并成一条就会让一次 `.expect` 同时替两个站点背书。
    #[test]
    fn a_null_mcp_args_field_reads_as_an_empty_args_list() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "args": null } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("`args: null` 是没配 args，不是形状故障");
        match &find(&caps, Kind::Mcp, "m").unwrap().spec {
            Spec::Mcp { args, .. } => assert!(args.is_empty(), "{args:?}"),
            other => panic!("{other:?}"),
        }
    }

    /// `env` 那一侧的同一分界：`env: null` = 没有环境变量，不是故障；
    /// `env: [ [ "K", "V" ] ]` 才是故障（`a_non_object_mcp_env_is_an_error_naming_the_server_and_field`）。
    #[test]
    fn a_null_mcp_env_field_reads_as_an_empty_env_map() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "env": null } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("`env: null` 是没配 env，不是形状故障");
        match &find(&caps, Kind::Mcp, "m").unwrap().spec {
            Spec::Mcp { env, .. } => assert!(env.is_empty(), "{env:?}"),
            other => panic!("{other:?}"),
        }
    }

    /// 整个文档就是一个 `null` = 「这个文件什么都没写」，与文件不存在同义（同一个守卫管三家
    /// 文件，所以逐一点一遍，少一家就是少一次看守）。`[]` / `"oops"` 那类坏文档仍上抛，
    /// 见 `a_config_file_whose_top_level_is_not_an_object_is_an_error`。
    #[test]
    fn a_config_file_that_is_just_null_reads_as_absent_not_as_a_shape_fault() {
        for rel in [
            ".claude.json",
            ".claude/settings.json",
            ".claude/plugins/installed_plugins.json",
        ] {
            let t = TempHome::new();
            // settings.json 只有「确实有 plugin 要看门禁」时才会被读到，那一轮因此
            // 应该读到那条 plugin；另外两轮没有别的输入 → 空表。
            if rel == ".claude/settings.json" {
                t.write(
                    ".claude/plugins/installed_plugins.json",
                    r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
                );
            }
            t.write(rel, "null");
            let caps = ClaudeCode::new(t.home.clone())
                .read_state()
                .unwrap_or_else(|e| panic!("{rel}: 一个 `null` 文档不是故障：{e}"));
            let expected = usize::from(rel == ".claude/settings.json");
            assert_eq!(caps.len(), expected, "{rel}: {:?}", labels(&caps));
        }
    }

    /// 反向的一半，也是 `type_name` 的 `"null"` 分支的看守：**数组元素**里的 `null` 不是
    /// 「没配」，是一个写坏的值。容器层改成「`null` 即缺键」之后，那个分支只剩「值必须
    /// 在那里」的位置可达（元素、已具名条目、门禁值……），实测把 `"null"` 这个字符串改掉
    /// 会红四条，全是下面这几类测试；把它当死代码删掉就等于让
    /// `args: ["-y", null]` 塌成 `["-y", ""]`。
    #[test]
    fn a_null_mcp_args_element_is_still_a_shape_fault() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "args": ["-y", null] } } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m` field `args[1]`",
                "a string",
                "null"
            )
        );
    }

    /// 被选中的那条安装记录同样是强制位置：`{"g@mp": [null]}` 是坏记录，不是「没装成」
    /// （后者是 `empty_install_array_is_skipped` 那个空数组）。
    #[test]
    fn a_null_install_record_is_still_a_shape_fault() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "g@mp": [null] } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "plugins entry `g@mp`[0]",
                "an object",
                "null"
            )
        );
    }

    /// `plugins` 的每条键都**已经声明**「这个 plugin 有安装记录」，所以 `"p@mp": null` 是
    /// 该给值没给，不是「没配」：容器层的宽容到 `plugins` 这一层为止。
    /// 这条与 `an_install_list_that_is_not_an_array_is_an_error_naming_the_entry` 同族。
    #[test]
    fn a_null_install_list_is_a_shape_fault_naming_the_entry() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "g@mp": null } }"#,
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude/plugins/installed_plugins.json"),
                "plugins entry `g@mp`",
                "an array",
                "null"
            )
        );
    }

    /// 同理，`mcpServers` 里已具名的 server 条目是强制位置：写了 `"m": null` 就是坏值。
    /// 「一个 server 都没列」和「列了却没给配置」不可混为一谈。
    #[test]
    fn a_null_named_mcp_server_is_a_shape_fault_naming_the_server() {
        let t = TempHome::new();
        t.write(".claude.json", r#"{ "mcpServers": { "m": null } }"#);
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".claude.json"),
                "mcp server `m`",
                "an object",
                "null"
            )
        );
    }

    /// 缺 `command` 键与 `command: null` 同为「没配 command」（`Spec::Mcp.command` 是
    /// `String`，Task 3 定的形状），但类型不对是故障。这条钉住那个映射，
    /// 免得下一次「顺手」把缺键也 bail 掉——那是另一种把正常状态报成坏配置。
    #[test]
    fn an_mcp_entry_without_a_command_reads_as_an_empty_command() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "bare": {}, "only-args": { "args": [] } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        for name in ["bare", "only-args"] {
            match &find(&caps, Kind::Mcp, name).unwrap().spec {
                Spec::Mcp { command, args, env } => {
                    assert_eq!(*command, "", "{name}");
                    assert!(args.is_empty(), "{name}");
                    assert!(env.is_empty(), "{name}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    // ============ 共享 helper 的跨 Task 字段（Task 10/11/13/14 的输入） ============

    /// Task 11 用 `KIMI_CODE` 走同一个 helper。评审实测：把 helper 里的
    /// `harness: harness.into()` 换成硬编码 `"claude-code"` 时，144 条全绿——
    /// 三家 MCP 会全部记到 claude 名下而无人看守。
    #[test]
    fn mcp_capability_attributes_the_harness_it_was_handed() {
        assert_eq!(KIMI_CODE, "kimi-code");
        let cfg = json!({ "command": "npx", "args": ["-y", "pkg"], "env": { "K": "${K}" } });
        let cap = mcp_capability("ctx", &cfg, KIMI_CODE, Path::new("mcp.json")).unwrap();
        assert_eq!(cap.id, CapId::new(Kind::Mcp, "ctx"));
        assert_eq!(
            cap.origin,
            Origin::Adopted {
                harness: "kimi-code".into()
            },
            "helper 必须照抄第三个参数，不是照抄 claude"
        );
        assert_eq!(cap.targets, Targets::All);
        assert_eq!(
            cap.spec,
            Spec::Mcp {
                command: "npx".into(),
                args: vec!["-y".into(), "pkg".into()],
                env: [("K".to_string(), "${K}".to_string())].into(),
            }
        );
    }

    /// helper 报错时得把**调用方**的文件路径带出来（Task 11 传的是它自己的 mcp.json），
    /// 否则报错只说 `mcp.json` 而用户不知道是哪一家的配置坏了。
    #[test]
    fn mcp_capability_names_the_file_it_was_handed() {
        let err = mcp_capability(
            "m",
            &json!("nope"),
            KIMI_CODE,
            &PathBuf::from("/x/mcp.json"),
        )
        .expect_err("非对象的 MCP 条目是形状故障");
        assert_eq!(
            err.to_string(),
            shape_msg(
                Path::new("/x/mcp.json"),
                "mcp server `m`",
                "an object",
                "a string"
            )
        );
    }

    /// 合并成一份实现之后（codex 那个逐字相同的私有 `split_key` 已删），这条把**两家共用**
    /// 的拆分语义钉住：按**第一个** `@` 拆，多出来的 `@` 留在 marketplace 段里。
    /// 派单点名的孪生风险是「一侧改了另一侧没改」，合并本身已经消掉它；但两侧原有 fixture
    /// 只有「单 `@`」与「无 `@`」两种，都判不出 `split_once` 与 `rsplit_once` 的差别。
    #[test]
    fn split_plugin_key_splits_on_the_first_at_sign_only() {
        assert_eq!(
            split_plugin_key("superpowers@claude-plugins-official"),
            (
                "superpowers".to_string(),
                Some("claude-plugins-official".to_string())
            )
        );
        assert_eq!(
            split_plugin_key("bare"),
            ("bare".to_string(), None),
            "没有 `@` 时 marketplace 是 None，不是空串"
        );
        assert_eq!(
            split_plugin_key("p@a@b"),
            ("p".to_string(), Some("a@b".to_string())),
            "两个 `@`：第一个是分隔符，其余算 marketplace 的一部分"
        );
        assert_eq!(
            split_plugin_key("@mp"),
            (String::new(), Some("mp".to_string())),
            "本函数不校验名字：空 entry 原样返回，裁决不在这里"
        );
    }

    // ============ enabledPlugins 门禁的键形状 ============

    /// 全文键 `name@marketplace` 没命中时退到裸 entry 名：不退就是又一次假同步——
    /// `{"p": false}` 配 `p@mp` 会被整个忽略，用户关掉的 plugin 报成 enabled=true。
    #[test]
    fn an_enable_gate_written_with_the_bare_entry_name_still_disables() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "enabledPlugins": { "p": false } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(!enabled, "裸 entry 名的门禁不能被忽略"),
            other => panic!("{other:?}"),
        }
    }

    /// 两种写法同时存在时全文键赢（它更具体）。把 `gate_enabled` 的查找顺序反过来就会红这里。
    #[test]
    fn the_full_plugin_key_gate_wins_over_a_bare_entry_name_gate() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "enabledPlugins": { "p@mp": true, "p": false } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(enabled, "全文键必须优先于裸名"),
            other => panic!("{other:?}"),
        }
    }

    /// 匹配不上任何安装记录的门禁键（卸载残留、预写的开关）是正常状态，不能上抛。
    /// 这条守的是**过度修复**的方向：把它改成 bail 会让健康配置报成「读不出来」。
    #[test]
    fn an_enable_gate_for_a_plugin_that_is_not_installed_is_ignored() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "p@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "enabledPlugins": { "gone@mp": false, "p@mp": false } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Plugin, "gone").is_none());
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(!enabled),
            other => panic!("{other:?}"),
        }
    }

    /// 门禁的**残留盲点，钉住现状不等于认可现状**：全文键写错了一个字母，于是它既匹配
    /// 不上安装记录键 `superpowers@mp`、也不是那条记录的裸名 `superpowers`，两次查找全部
    /// 落空 → `gate_enabled` 退到 `unwrap_or(true)` → 用户以为自己关掉了，diff 却念成
    /// 「已启用」。
    /// 为什么不改成上抛（上一轮的裁决，本轮只补名字）：卸载残留和预写的开关键在健康配置里
    /// 是正常状态，「匹配不上就失败」会把健康配置判成读不出来。实测把 `gate_enabled` 换成
    /// 「两次查找都落空就 bail」这种过度修复，红的正是本条和
    /// `reads_plugin_with_sha_and_marketplace`（`superpowers` 压根没被门禁列出）两条；
    /// `an_enable_gate_for_a_plugin_that_is_not_installed_is_ignored` 在那个变异下**仍绿**——
    /// 它钉的是「匹配不上的门禁键不产出一条能力」，那种键根本走不到 `gate_enabled`。
    /// 真正的治法是把「没被任何安装记录消费的门禁键」做成 Task 13/14 的**报告项**。
    #[test]
    fn a_mis_typed_full_gate_key_silently_reads_as_enabled() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "superpowers@mp": [ { "version": "1.0" } ] } }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "enabledPlugins": { "superpower@mp": false } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone())
            .read_state()
            .expect("匹配不上的门禁键不是故障（当前语义）");
        match &find(&caps, Kind::Plugin, "superpowers")
            .expect("plugin 本身仍要读到")
            .spec
        {
            Spec::Plugin { enabled, .. } => assert!(
                *enabled,
                "当前语义：两种写法都没命中的门禁 = 未表态 = enabled；改这一条要先过裁决"
            ),
            other => panic!("{other:?}"),
        }
    }
}
