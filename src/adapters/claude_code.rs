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
        // 「没有 mcpServers 这个键」→ 空表（正常状态）；「有键但不是对象」→ 故障上抛。
        // 前者用户就是没配 MCP，后者是配置写坏了——报成「没配任何 MCP」正是
        // 「形状故障也是故障」那块点名的假同步。
        if let Some(servers) = root.get("mcpServers") {
            for (name, cfg) in expect_object(servers, &file, "key `mcpServers`")? {
                out.push(mcp_capability(name, cfg, CLAUDE_CODE, &file)?);
            }
        }
        Ok(out)
    }

    /// plugin 的**安装记录**在 `~/.claude/plugins/installed_plugins.json`（键是
    /// `name@marketplace`，值是安装数组），**门禁**在 settings.json 的 `enabledPlugins`。
    /// 一个键可能对应多条安装（实测就是数组），v1 只取第一条；空数组 = 没装成，跳过。
    fn read_plugins(&self) -> anyhow::Result<Vec<Capability>> {
        let file = self.home.claude_installed_plugins();
        let Some(root) = read_json_if_present(&file)? else {
            return Ok(Vec::new());
        };
        // 门禁读不出来就是读不出来：退化成「全部 enabled」会把用户关掉的 plugin 报成
        // 已启用，正是失败隔离契约禁止的那种「看起来合理」的假同步。
        let enabled_map = self.read_enabled_plugins()?;

        let mut out = Vec::new();
        let Some(plugins) = root.get("plugins") else {
            return Ok(out);
        };
        for (key, installs) in expect_object(plugins, &file, "key `plugins`")? {
            let (entry, marketplace) = split_plugin_key(key);
            let context = format!("plugins entry `{key}`");
            // 「值不是数组」和「数组确实为空（没装成）」是两种事实：只有后者可以跳过。
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
        if let Some(obj) = root.get("enabledPlugins") {
            for (k, v) in expect_object(obj, &file, "key `enabledPlugins`")? {
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
/// 后者按计划里的失败隔离契约整次 diff 失败。Task 10/11 共用。
///
/// 返回的文档还保证是**对象**：三家 harness 的这几个配置文件都是「顶层一个 map」的
/// 形状，而调用方一律用 `root.get("...")` 取键。顶层不是对象时 `get` 只会给 `None`，
/// 于是 `[]` / `"oops"` / `5` 这类坏文件会被读成「一个键都没有」= 「这个 harness 没配
/// 任何能力」，正是「形状故障也是故障」那块禁止的塌缩。守卫放在这里，Task 10/11 复用
/// 本函数就自动带上，不必各家再写一遍。
pub(crate) fn read_json_if_present(path: &Path) -> anyhow::Result<Option<Value>> {
    let Some(value) = read_json_value_if_present(path)? else {
        return Ok(None);
    };
    if value.is_object() {
        Ok(Some(value))
    } else {
        Err(shape_fault(&value, path, "top level", "an object"))
    }
}

/// 纯解析半边（不存在 → `Ok(None)`，语法错 → 带路径的 Err），不校验顶层形状。
/// 顶层形状单独一步，是为了让报错里的「top level must be an object」和解析失败
/// 的「failed to parse」是两条可分别看守的故障。
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
/// 只有 claude 需要拆：kimi 的 `installed.json` 是数组、身份直接躺在 `id` 字段里（Task 11）。
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
/// 里没有承载它的字段）。Task 10/11 共用。
///
/// 三样各自的「没配」和「配坏了」都不可合并，所以本函数是 `Result`：
/// `command` 缺键（或显式 `null`）才是 `""`——`Spec::Mcp.command` 是 `String`，
/// 「没有 command」和「command 是空串」在 v1 里同为「没配」；但 `command: 5` 是故障。
/// `args` / `env` 缺键 → 空集合；键在而类型不对、或元素/值不是字符串 → 故障上抛，
/// 否则一个写坏的 `args: "mcp"` 会连同内容一起被丢掉，只剩一个空 args 去看下一次比较。
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
    if let Some(value) = cfg.get("args") {
        for (i, element) in expect_array(value, file, &format!("{context} field `args`"))?
            .iter()
            .enumerate()
        {
            args.push(
                expect_string(element, file, &format!("{context} field `args[{i}]`"))?.to_string(),
            );
        }
    }

    let mut env = BTreeMap::new();
    if let Some(value) = cfg.get("env") {
        for (k, v) in expect_object(value, file, &format!("{context} field `env`"))? {
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

/// 「键缺失」由调用方的 `Option` 分支处理（那是正常状态），这里只裁决「键在但类型不对」。
fn expect_object<'a>(
    value: &'a Value,
    file: &Path,
    what: &str,
) -> anyhow::Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| shape_fault(value, file, what, "an object"))
}

fn expect_array<'a>(value: &'a Value, file: &Path, what: &str) -> anyhow::Result<&'a Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| shape_fault(value, file, what, "an array"))
}

fn expect_string<'a>(value: &'a Value, file: &Path, what: &str) -> anyhow::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| shape_fault(value, file, what, "a string"))
}

/// 可选字符串字段：键缺失 → `Ok(None)`；显式 `null` → `Ok(None)`（JSON 里写「无」的
/// 常见形态，与缺键同义）；**有值但不是字符串** → 故障上抛，不塌成 `None`。
fn optional_string<'a>(
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

    /// 派单字面代码在 `read_plugins` 里写的是
    /// `self.read_enabled_plugins().unwrap_or_default()`（注释：「读失败不该拖垮 plugin
    /// 扫描，退化成全部视为 enabled」）。那正是失败隔离契约禁止的形状：一个语法坏掉的
    /// settings.json 会把用户**关掉**的 plugin 报成 `enabled: true`，diff 于是把
    /// 「你禁用了它」念成「已同步」，而报告里没有任何地方说明 settings.json 没读成。
    /// 所以这里上抛，并留测试看守。变异实测（评审修正过本注释原先的过度声明）：把 `?` 换回
    /// `.unwrap_or_default()` 会红**两条**——下面这两条各自走的就是同一个 `?`：
    /// `a_malformed_settings_json_is_an_error_not_a_silent_enable_all`（语法坏）与
    /// `a_non_boolean_enable_gate_is_an_error_naming_the_plugin`（门禁值类型坏）。
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
    /// 变异实测：删掉 `dedup_by` 那行，红的只有这一条（`dups.len()` 变成 2）。
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
}
