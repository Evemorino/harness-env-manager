use crate::adapters::{CODEX, HarnessAdapter};
use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets};
use crate::home::Home;
use crate::skills_dir::{SkillStore, plugin_from_path, try_read_skill_entries};
use anyhow::anyhow;
use std::collections::BTreeMap;
use std::path::Path;
use toml::Table as TomlTable;
use toml::Value as TomlValue;

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

    /// 读 `~/.codex/config.toml`：**文件不存在 → `Ok(None)`（正常状态）**，语法错 →
    /// 带路径的 `Err`。顶层直接收成 `toml::Table`：TOML 文档本身就是一个键值表，
    /// 「顶层不是表」在这个格式里没有对应输入（JSON 能写出 `"oops"` / `5`，TOML 不能），
    /// 所以 claude 那条顶层守卫在这里由解析器的类型免费提供，不需要一条走不到的分支。
    ///
    /// 用动态导航而不是 typed struct：codex 的 config.toml 里有大量用户手写的无关字段
    /// （`model_providers`、`projects`、`tui`、`hooks.state`），读侧必须对它们免疫。
    fn read_config(&self, file: &Path) -> anyhow::Result<Option<TomlTable>> {
        match std::fs::read_to_string(file) {
            Ok(text) => match toml::from_str::<TomlTable>(&text) {
                Ok(root) => Ok(Some(root)),
                // **不**用 `.with_context(..)` 包住原 error：`toml::de::Error` 的 `Display`
                // 逐字回显出错那一行的源码（`2 | TOKEN = "..."`），而 `main.rs` 打印的是整条
                // cause 链——那行可能就是 `[mcp_servers.x.env]` 里的明文密钥。
                // `Error::message()` 只给原因文本、不含源码，路径由我们自己带上。
                Err(e) => Err(anyhow!(
                    "failed to parse {}: {}",
                    file.display(),
                    e.message()
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                Err(anyhow::Error::new(e).context(format!("failed to read {}", file.display())))
            }
        }
    }

    /// 用户级 MCP 在 `config.toml` 的 `[mcp_servers.<name>]`。
    fn read_mcps(&self, root: &TomlTable, file: &Path) -> anyhow::Result<Vec<Capability>> {
        let mut out = Vec::new();
        // 「没有 mcp_servers 这个键」→ 空表（正常状态）；键在、值不是表 → 故障上抛。
        // TOML 侧的坏值是 `mcp_servers = "..."`（少了 `[mcp_servers]` 那层表头）。
        // `[mcp_servers]` 写了头但一条没配 → 类型对且为空，仍是合法空结果。
        let Some(servers) =
            toml_optional_table(root.get("mcp_servers"), file, "key `mcp_servers`")?
        else {
            return Ok(out);
        };
        for (name, cfg) in servers {
            let context = format!("mcp server `{name}`");
            let cfg = toml_expect_table(cfg, file, &context)?;
            // `command` 缺键才是 `""`（`Spec::Mcp.command` 是 `String`，v1 里「没有
            // command」和「command 是空串」同为没配）；`command = 5` 是故障。
            let command = toml_optional_string(cfg, "command", file, &context)?
                .unwrap_or_default()
                .to_string();

            let mut args = Vec::new();
            if let Some(list) =
                toml_optional_array(cfg.get("args"), file, &format!("{context} field `args`"))?
            {
                for (i, element) in list.iter().enumerate() {
                    args.push(
                        toml_expect_string(element, file, &format!("{context} field `args[{i}]`"))?
                            .to_string(),
                    );
                }
            }

            let mut env = BTreeMap::new();
            if let Some(fields) =
                toml_optional_table(cfg.get("env"), file, &format!("{context} field `env`"))?
            {
                for (k, v) in fields {
                    // 保持 `${VAR}` 引用形态，绝不展开。报错只到键名为止——actual 侧的
                    // 值可能就是明文密钥，而这条消息会进终端输出和 Task 14 的快照。
                    env.insert(
                        k.clone(),
                        toml_expect_string(v, file, &format!("{context} field `env` key `{k}`"))?
                            .to_string(),
                    );
                }
            }

            out.push(Capability {
                id: CapId::new(Kind::Mcp, name.clone()),
                origin: Origin::Adopted {
                    harness: CODEX.into(),
                },
                spec: Spec::Mcp { command, args, env },
                targets: Targets::All,
            });
        }
        Ok(out)
    }

    /// plugin 记录就是 `config.toml` 里的 `[plugins."<entry>@<marketplace>"]`：键本身即身份。
    ///
    /// **已知局限**（Plan C 的 doctor 要报出来）：codex 的 `config.toml` 不记录 plugin 的
    /// 版本或 SHA（缓存目录名可能是 semver、git sha 或字面量 `unknown`），所以 `version`
    /// 与 `sha` 恒为 `None` → `compare_versions` 判 `Unknown` → diff 按 in-sync 处理，
    /// 不误报 update。这也是本文件不 import `normalize_locator` 的原因：codex 一个 locator
    /// 字段都不读，没有需要归一化的东西。
    fn read_plugins(&self, root: &TomlTable, file: &Path) -> anyhow::Result<Vec<Capability>> {
        let mut out = Vec::new();
        // 与 `mcp_servers` 同一条分界：缺键 = 没配（正常）；`plugins = "oops"` = 写坏了，
        // 报成「这家一个 plugin 都没装」就是假同步。
        let Some(plugins) = toml_optional_table(root.get("plugins"), file, "key `plugins`")? else {
            return Ok(out);
        };
        for (key, cfg) in plugins {
            let context = format!("plugins entry `{key}`");
            // 「值不是表」和「表确实为空」是两种事实：`plugins = { p = true }` 是键写了却没给
            // 记录（坏值），而 `[plugins.p]`（空表）才是「装了但一条配置都没写」。
            let cfg = toml_expect_table(cfg, file, &context)?;
            let (entry, marketplace) = split_key(key);
            out.push(Capability {
                id: CapId::new(Kind::Plugin, entry.clone()),
                origin: Origin::Adopted {
                    harness: CODEX.into(),
                },
                spec: Spec::Plugin {
                    marketplace,
                    entry,
                    version: None,
                    sha: None,
                    install_path: None,
                    // 缺 `enabled` = 未表态 = 启用（与 claude 的门禁默认一致）；
                    // `enabled = "yes"` 是坏值，由 `toml_optional_bool` 上抛而不是猜 true。
                    enabled: toml_optional_bool(cfg, "enabled", file, &context)?.unwrap_or(true),
                },
                targets: Targets::All,
            });
        }
        Ok(out)
    }

    /// 扫 `~/.codex/skills`。**扫描失败一律上抛**：把「读不出这个目录」呈现成
    /// 「这家一个 skill 都没装」是只读对账工具最坏的漏报方向（失败隔离契约）。
    /// 只有「目录不存在」才是空的，那由 `try_read_skill_entries` 自己判。
    ///
    /// 签名返回 `Result` 而不是计划示例里的 `Vec`：`try_read_skill_entries` 本身可失败，
    /// 示例那句 `for e in try_read_skill_entries(..)` 连编译都过不去（`Result` 不是
    /// `IntoIterator`），而改用 lossy 的 `read_skill_entries` 就把「读不了」重新吞成空表。
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
        let mut caps = self.read_skills()?;
        let file = self.home.codex_config();
        if let Some(root) = self.read_config(&file)? {
            caps.extend(self.read_mcps(&root, &file)?);
            caps.extend(self.read_plugins(&root, &file)?);
        }
        caps.sort_by_key(|a| a.id.label());
        // dedup_by 只去重相邻项，所以必须先 sort
        caps.dedup_by(|a, b| a.id == b.id);
        Ok(caps)
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}

/// TOML 侧的形状原语一族。它们与 claude 的 `expect_*` / `optional_*` **不是**重复建设：
/// 那六个收 `serde_json::Value`，这一族收 `toml::Value`，两个类型系统之间没有可复用的
/// 入口（`read_json_if_present` / `mcp_capability` 同理不适用）。报错文案逐字沿用 claude 的
/// `invalid shape in {file}: {position} must be {expected}, found {type name}`，
/// 这样 Task 14 的 golden 里三家长得一样。
/// 与 claude 唯一的用词差别：TOML 的映射叫 table，所以是 `a table` / `an integer`
/// / `a float` / `a datetime` 这几个 JSON 里没有的类型名。
fn toml_type_name(value: &TomlValue) -> &'static str {
    match value {
        TomlValue::String(_) => "a string",
        TomlValue::Integer(_) => "an integer",
        TomlValue::Float(_) => "a float",
        TomlValue::Boolean(_) => "a boolean",
        TomlValue::Datetime(_) => "a datetime",
        TomlValue::Array(_) => "an array",
        TomlValue::Table(_) => "a table",
        // 这里**没有** `null` 分支，也不是漏写：TOML 没有 `null` 字面量，`toml::Value`
        // 也就没有 `Null` 变体。claude 那条「容器层 `null` 与缺键同义」的裁决在 codex 侧
        // 不可达，所以不为它造任何兜底分支（连匹配臂都构造不出来）。
    }
}

/// 「形状故障」的统一报错：文件路径 + 位置（只到键名为止）+ 期望类型 + 实际类型。
/// **永远不带上值本身**——`~/.codex/config.toml` 里的 env 值可能是明文密钥，而这条消息
/// 会进终端输出和 Task 14 的 insta 快照。
fn shape_fault(found: &TomlValue, file: &Path, what: &str, expected: &str) -> anyhow::Error {
    anyhow!(
        "invalid shape in {}: {what} must be {expected}, found {}",
        file.display(),
        toml_type_name(found)
    )
}

/// **强制**位置的形状裁决：值已经在那里了，就必须是对的类型。
fn toml_expect_table<'a>(
    value: &'a TomlValue,
    file: &Path,
    what: &str,
) -> anyhow::Result<&'a TomlTable> {
    value
        .as_table()
        .ok_or_else(|| shape_fault(value, file, what, "a table"))
}

fn toml_expect_array<'a>(
    value: &'a TomlValue,
    file: &Path,
    what: &str,
) -> anyhow::Result<&'a Vec<TomlValue>> {
    value
        .as_array()
        .ok_or_else(|| shape_fault(value, file, what, "an array"))
}

fn toml_expect_string<'a>(
    value: &'a TomlValue,
    file: &Path,
    what: &str,
) -> anyhow::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| shape_fault(value, file, what, "a string"))
}

/// **可缺位**的容器表：键缺失 → `Ok(None)`（正常状态）；键在而类型不对 → 故障上抛。
/// 只有「类型对且为空」才降级成一个空的 `Some`，那种空由调用方按正常空结果处理。
fn toml_optional_table<'a>(
    value: Option<&'a TomlValue>,
    file: &Path,
    what: &str,
) -> anyhow::Result<Option<&'a TomlTable>> {
    let Some(value) = value else {
        return Ok(None);
    };
    toml_expect_table(value, file, what).map(Some)
}

/// `toml_optional_table` 的数组版，同一条分界；元素类型仍走 `toml_expect_string`，
/// 所以 `args = [7]` 是故障不是「没配 args」。
fn toml_optional_array<'a>(
    value: Option<&'a TomlValue>,
    file: &Path,
    what: &str,
) -> anyhow::Result<Option<&'a Vec<TomlValue>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    toml_expect_array(value, file, what).map(Some)
}

/// 可选字符串字段：键缺失 → `Ok(None)`；**有值但不是字符串** → 故障上抛，不塌成 `None`。
fn toml_optional_string<'a>(
    table: &'a TomlTable,
    key: &str,
    file: &Path,
    context: &str,
) -> anyhow::Result<Option<&'a str>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    toml_expect_string(value, file, &format!("{context} field `{key}`")).map(Some)
}

/// 布尔门禁的强制版。claude 那六个原语里**没有**布尔版（Task 11 要补的 `expect_bool`
/// 就是它），codex 侧 `enabled` 是这个形状唯一的读者，所以本文件自带一对。
/// `.and_then(toml::Value::as_bool).unwrap_or(true)` 就是被裁定为 Critical 的塌缩：
/// `enabled = "yes"` 会被读成「已启用」。
fn toml_expect_bool(value: &TomlValue, file: &Path, what: &str) -> anyhow::Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| shape_fault(value, file, what, "a boolean"))
}

/// 可选布尔字段：键缺失 → `Ok(None)`（未表态，由调用方定默认值）；类型不对 → 故障。
fn toml_optional_bool(
    table: &TomlTable,
    key: &str,
    file: &Path,
    context: &str,
) -> anyhow::Result<Option<bool>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    toml_expect_bool(value, file, &format!("{context} field `{key}`")).map(Some)
}

/// `"superpowers@openai-api-curated"` → `("superpowers", Some("openai-api-curated"))`
///
/// 与 claude 的 `split_plugin_key` 逐字同形，这里是第二份而不是复用，是派单划的边界：
/// Task 10 不得改 `claude_code.rs`，而那份的文档目前写着「只有 claude 需要拆」——实测
/// codex 的 plugin 键同样是 `name@marketplace`（Task 10 评审要向计划提的修正点）。
/// **拆分规则若变动，两处必须同步改。**
fn split_key(key: &str) -> (String, Option<String>) {
    match key.split_once('@') {
        Some((entry, mp)) => (entry.to_string(), Some(mp.to_string())),
        None => (key.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec, Targets};
    use crate::testutil::TempHome;
    use std::path::{Path, PathBuf};

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

    // ============ config.toml：MCP ============

    /// 形状故障的报错文本按**整串**钉死（不 `contains`）：文件路径、位置（只到键名为止）、
    /// 期望类型、实际类型这四段就是全部内容，多一个字都可能是漏值。格式与 claude 的
    /// `shape_msg` 一字不差，Task 14 的 golden 才不会两家两种样子。
    fn shape_msg(file: &Path, what: &str, expected: &str, found: &str) -> String {
        format!(
            "invalid shape in {}: {what} must be {expected}, found {}",
            file.display(),
            found
        )
    }

    /// `read_state` 必须把形状故障上抛；返回报错文本供逐字比较。
    fn read_state_err(t: &TempHome) -> String {
        Codex::new(t.home.clone())
            .read_state()
            .expect_err("形状故障必须上抛，不能塌成空表")
            .to_string()
    }

    /// 只有一家的 config.toml 会写：坏值一律塞进这个文件。
    fn write_config(t: &TempHome, body: &str) {
        t.write(".codex/config.toml", body);
    }

    fn config_file(t: &TempHome) -> PathBuf {
        t.path(".codex/config.toml")
    }

    #[test]
    fn reads_mcp_servers_from_config_toml() {
        let t = TempHome::new();
        write_config(
            &t,
            r#"model = "deepseek-v4-flash"
model_reasoning_effort = "high"

[model_providers.custom]
name = "deepseek"
base_url = "https://api.deepseek.com"

[projects."/Users/x/Code"]
trust_level = "trusted"

[mcp_servers]

[mcp_servers.aiflowdesign]
type = "stdio"
command = "/abs/Electron"
args = ["/abs/app", "--mcp-stdio", "--app", "desktop"]

[mcp_servers.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp"]

[mcp_servers.context7.env]
CONTEXT7_API_KEY = "${CONTEXT7_API_KEY}"
"#,
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();

        let a = find(&caps, Kind::Mcp, "aiflowdesign").unwrap();
        assert_eq!(
            a.spec,
            Spec::Mcp {
                command: "/abs/Electron".into(),
                args: vec![
                    "/abs/app".into(),
                    "--mcp-stdio".into(),
                    "--app".into(),
                    "desktop".into()
                ],
                env: Default::default(),
            }
        );
        assert_eq!(
            a.origin,
            Origin::Adopted {
                harness: "codex".into()
            }
        );
        assert_eq!(
            a.targets,
            Targets::All,
            "actual 侧读出来的能力不该自带 targets 限制"
        );

        let c = find(&caps, Kind::Mcp, "context7").unwrap();
        match &c.spec {
            Spec::Mcp { env, .. } => {
                // 原样保留引用形态，绝不展开成真实密钥
                assert_eq!(env["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}");
            }
            other => panic!("{other:?}"),
        }
        // 无关字段不得被当成 MCP：`model_providers.custom` 与 `projects."..."` 都不算
        assert!(find(&caps, Kind::Mcp, "custom").is_none());
        assert_eq!(
            caps.iter().filter(|c| c.id.kind == Kind::Mcp).count(),
            2,
            "{:?}",
            caps.iter().map(|c| c.id.label()).collect::<Vec<_>>()
        );
    }

    /// `command` 缺键 = 「没配」，在 v1 里读成空串（与 claude 同一裁决）；
    /// `args` / `env` 缺键 = 空集合。而「类型对且为空」（`args = []`、`env = {}`）也是
    /// 同一种合法空——只有「类型不对」才是故障，见下面那六条。
    #[test]
    fn absent_and_empty_command_args_env_all_read_as_configured_nothing() {
        let t = TempHome::new();
        write_config(
            &t,
            r#"[mcp_servers.bare]
[mcp_servers.empty]
args = []
env = {}
"#,
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        for name in ["bare", "empty"] {
            let m = find(&caps, Kind::Mcp, name)
                .unwrap_or_else(|| panic!("{name} 这个 server 必须出现在报告里"));
            assert_eq!(
                m.spec,
                Spec::Mcp {
                    command: String::new(),
                    args: Vec::new(),
                    env: Default::default(),
                },
                "`{name}`：缺键与「类型对且为空」同为正常空值"
            );
        }
    }

    /// `[mcp_servers]` 写了头却没配任何 server：类型对且为空 = 合法空结果，不是故障。
    /// 与 `mcp_servers_key_that_is_not_a_table_...` 一起把「空」和「坏」分开钉。
    #[test]
    fn an_empty_mcp_servers_table_is_not_an_error() {
        let t = TempHome::new();
        write_config(&t, "mcp_servers = {}\n");
        let caps = Codex::new(t.home.clone())
            .read_state()
            .expect("空的 mcp_servers 不是故障");
        assert_eq!(caps.iter().filter(|c| c.id.kind == Kind::Mcp).count(), 0);
    }

    #[test]
    fn mcp_servers_key_that_is_not_a_table_is_an_error_naming_the_key() {
        let t = TempHome::new();
        write_config(&t, "mcp_servers = \"oops\"\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(&config_file(&t), "key `mcp_servers`", "a table", "a string")
        );
    }

    #[test]
    fn an_mcp_server_entry_that_is_not_a_table_is_an_error_naming_the_server() {
        let t = TempHome::new();
        write_config(&t, "mcp_servers = { m = \"not-a-table\" }\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(&config_file(&t), "mcp server `m`", "a table", "a string")
        );
    }

    #[test]
    fn a_non_string_mcp_command_is_an_error_naming_the_field() {
        let t = TempHome::new();
        write_config(&t, "[mcp_servers.m]\ncommand = 5\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &config_file(&t),
                "mcp server `m` field `command`",
                "a string",
                "an integer"
            )
        );
    }

    #[test]
    fn a_non_array_mcp_args_is_an_error_naming_the_server_and_field() {
        let t = TempHome::new();
        write_config(
            &t,
            "[mcp_servers.m]\ncommand = \"c\"\nargs = \"mcp -s user\"\n",
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &config_file(&t),
                "mcp server `m` field `args`",
                "an array",
                "a string"
            )
        );
    }

    #[test]
    fn a_non_string_mcp_args_element_is_an_error_naming_the_index() {
        let t = TempHome::new();
        write_config(&t, "[mcp_servers.m]\ncommand = \"c\"\nargs = [\"-y\", 7]\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &config_file(&t),
                "mcp server `m` field `args[1]`",
                "a string",
                "an integer"
            )
        );
    }

    #[test]
    fn a_non_table_mcp_env_is_an_error_naming_the_server_and_field() {
        let t = TempHome::new();
        write_config(&t, "[mcp_servers.m]\ncommand = \"c\"\nenv = [\"K\"]\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &config_file(&t),
                "mcp server `m` field `env`",
                "a table",
                "an array"
            )
        );
    }

    /// 非字符串的 env 值过去是 `filter_map` 直接丢掉：键名一起消失，于是「配坏了的 env」和
    /// 「没有 env」在 diff 里长得一样。现在上抛，且报错只到键名为止——值可能就是明文密钥。
    #[test]
    fn a_non_string_mcp_env_value_is_an_error_that_never_echoes_it() {
        let t = TempHome::new();
        write_config(
            &t,
            "[mcp_servers.m]\ncommand = \"c\"\nenv = { TOKEN = { nested = \"sk-live-9f3a2b7c\" } }\n",
        );
        let err = read_state_err(&t);
        assert_eq!(
            err,
            shape_msg(
                &config_file(&t),
                "mcp server `m` field `env` key `TOKEN`",
                "a string",
                "a table"
            )
        );
        assert!(!err.contains("sk-live-9f3a2b7c"), "报错不得带出原值：{err}");
        assert!(!err.contains("nested"), "报错不得带出容器内容：{err}");
    }

    /// `toml::de::Error` 的 `Display` 会**逐字回显出错那一行的源码**（`2 | TOKEN = "..."`），
    /// 而 main.rs 用 `{err:?}` 打印整条 cause 链——配坏的往往正是 `[mcp_servers.x.env]`
    /// 里那行明文密钥。`with_context` 只包在顶层，管不住 cause 的渲染，所以解析失败只取
    /// `Error::message()`（纯原因文本，不含源码）。
    /// 断言用 `{err:?}`（= 真实打印形态）而不是 `to_string()`：只看首行会漏掉整条链。
    #[test]
    fn a_toml_parse_failure_never_echoes_the_offending_line() {
        let t = TempHome::new();
        write_config(
            &t,
            "[mcp_servers.x.env]\nTOKEN = \"sk-live-9f3a2b7c\ncontext = \n",
        );
        let err = Codex::new(t.home.clone())
            .read_state()
            .expect_err("语法错的 config.toml 必须是故障");
        let rendered = format!("{err:?}");
        assert!(
            !rendered.contains("sk-live-9f3a2b7c"),
            "整条报错链都不得带出原值：{rendered}"
        );
        // 路径与原因还得在——去掉回显不能把报错降级成一句「解析失败」
        assert!(
            rendered.contains(&config_file(&t).display().to_string()),
            "报错必须点明是哪个文件：{rendered}"
        );
        assert!(
            rendered.contains("invalid basic string"),
            "报错必须留下解析器给的原因文本：{rendered}"
        );
    }

    /// 整串钉死（不 `contains`）：这条消息**只有**「路径 + 解析器的原因文本」两段，
    /// 没有第三个成分。`a_toml_parse_failure_never_echoes_the_offending_line` 管的是
    /// 「不得回显源码」，本条管的是「也不得少说路径」。
    /// 原因文本是 toml 1.1.6 的实测输出，换解析器版本会红——那是有意的锁。
    #[test]
    fn malformed_toml_is_an_error_naming_the_file() {
        let t = TempHome::new();
        write_config(&t, "model = \n[[[");
        let err = Codex::new(t.home.clone())
            .read_state()
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "failed to parse {}: string values must be quoted, expected literal string",
                config_file(&t).display()
            )
        );
    }

    #[test]
    fn missing_config_is_not_an_error() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let caps = Codex::new(t.home.clone())
            .read_state()
            .expect("no config != error");
        assert!(caps.is_empty());
    }

    // ============ config.toml：plugin ============

    #[test]
    fn reads_plugins_from_config_toml() {
        let t = TempHome::new();
        write_config(
            &t,
            r#"[plugins."superpowers@openai-api-curated"]
enabled = true

[plugins."coderabbit@openai-api-curated"]
enabled = false
"#,
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let sp = find(&caps, Kind::Plugin, "superpowers").unwrap();
        assert_eq!(
            sp.spec,
            Spec::Plugin {
                marketplace: Some("openai-api-curated".into()),
                entry: "superpowers".into(),
                // codex 的 config.toml 不记录版本/SHA（Task 10「已知局限」），两家都是 None
                version: None,
                sha: None,
                install_path: None,
                enabled: true,
            }
        );
        assert_eq!(
            sp.origin,
            Origin::Adopted {
                harness: "codex".into()
            }
        );
        assert_eq!(sp.targets, Targets::All);
        match &find(&caps, Kind::Plugin, "coderabbit").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(!enabled),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugin_key_without_marketplace_still_parses() {
        let t = TempHome::new();
        write_config(&t, "[plugins.local-thing]\nenabled = true\n");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "local-thing").unwrap().spec {
            Spec::Plugin {
                marketplace, entry, ..
            } => {
                assert_eq!(*marketplace, None);
                assert_eq!(entry, "local-thing");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 计划示例这里写的是 `[plugins.bare@mp]`——**那不是合法 TOML**（裸键只允许
    /// A-Za-z0-9`_`-，`@` 必须引号包起来），实测解析器直接报 `invalid unquoted key`。
    /// 真实 config.toml 写的就是 `[plugins."superpowers@openai-api-curated"]，
    /// 所以这里按真实形状加引号。
    #[test]
    fn plugin_enabled_defaults_true_when_field_absent() {
        let t = TempHome::new();
        write_config(&t, "[plugins.\"bare@mp\"]\n");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "bare").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(*enabled),
            other => panic!("{other:?}"),
        }
    }

    /// `[plugins]` 写了头但一条没记：类型对且为空 = 合法空结果。
    /// 与下面 `plugins_key_that_is_not_a_table_...` 一起把「空」和「坏」分开钉。
    #[test]
    fn an_empty_plugins_table_is_not_an_error() {
        let t = TempHome::new();
        write_config(&t, "plugins = {}\n");
        let caps = Codex::new(t.home.clone())
            .read_state()
            .expect("空的 plugins 不是故障");
        assert_eq!(caps.iter().filter(|c| c.id.kind == Kind::Plugin).count(), 0);
    }

    #[test]
    fn plugins_key_that_is_not_a_table_is_an_error_naming_the_key() {
        let t = TempHome::new();
        write_config(&t, "plugins = \"oops\"\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(&config_file(&t), "key `plugins`", "a table", "a string")
        );
    }

    #[test]
    fn a_plugin_entry_that_is_not_a_table_is_an_error_naming_the_key() {
        let t = TempHome::new();
        // `p = true` 而不是 `[plugins.p]`：键写了却没给记录，和 claude 侧
        // `{"p@mp": "notarray"}` 同族，是坏值而不是「没配」。
        write_config(&t, "plugins = { p = true }\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &config_file(&t),
                "plugins entry `p`",
                "a table",
                "a boolean"
            )
        );
    }

    /// `enabled = "yes"` 过去走 `.and_then(as_bool).unwrap_or(true)`：非布尔的门禁值
    /// 被读成「已启用」，于是**用户关掉/写坏了的** plugin 报成 enabled，正是被裁定为
    /// Critical 的那种塌缩。现在上抛，且整串相等证明报错没带值。
    #[test]
    fn a_non_boolean_plugin_enabled_is_an_error_naming_the_field() {
        let t = TempHome::new();
        write_config(&t, "[plugins.\"p@mp\"]\nenabled = \"yes\"\n");
        let err = read_state_err(&t);
        assert_eq!(
            err,
            shape_msg(
                &config_file(&t),
                "plugins entry `p@mp` field `enabled`",
                "a boolean",
                "a string"
            )
        );
        assert!(!err.contains("yes"), "报错不得带出原值：{err}");
    }

    // ============ 汇总：排序 / 去重 ============

    #[test]
    fn output_is_sorted_and_deduplicated() {
        let t = TempHome::new();
        t.mkdir(".codex/skills/zzz")
            .mkdir(".codex/skills/aaa")
            .write(
                ".codex/config.toml",
                "[mcp_servers.z]\ncommand = \"c\"\n[mcp_servers.a]\ncommand = \"c\"\n",
            );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "{labels:?}");
        let mut dedup = labels.clone();
        dedup.dedup();
        assert_eq!(labels.len(), dedup.len(), "duplicate ids: {labels:?}");
    }

    /// 身份是 `kind + name` 两半，不只是名字：同名的一条 skill 和一条 MCP 必须**都**留下。
    /// 本条存在的理由是那条 `dedup_by(|a, b| a.id == b.id)`——把判据换成只看名字的
    /// `a.id.name == b.id.name` 时，只有本条会红（`output_is_sorted_and_deduplicated` 的
    /// fixture 名字互不相同，另两条测试的 skill/MCP 不落在相邻位置），所以「去重按 id 而不是
    /// 按名字」这件事必须有这一条来钉，否则派单的 id 语义在 codex 侧无人看守。
    #[test]
    fn a_skill_and_an_mcp_sharing_a_name_are_both_reported() {
        let t = TempHome::new();
        t.mkdir(".codex/skills/dup");
        write_config(&t, "[mcp_servers.dup]\ncommand = \"c\"\n");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        assert_eq!(
            labels,
            ["mcp/dup".to_string(), "skill/dup".to_string()],
            "{labels:?}"
        );
        assert_eq!(
            find(&caps, Kind::Skill, "dup").map(|c| c.id.kind),
            Some(Kind::Skill)
        );
        assert_eq!(
            find(&caps, Kind::Mcp, "dup").map(|c| c.id.kind),
            Some(Kind::Mcp)
        );
    }

    /// 同名 plugin 来自两个 marketplace 时，两个键归一化成同一个 `CapId`，派单要求的
    /// dedup 于是**丢掉一条**。这条钉住「确实只剩一条、而且留下的是哪一条」：
    /// `output_is_sorted_and_deduplicated` 的 fixture 四条 id 互不相等，删掉 `dedup_by`
    /// 它照样绿，所以「去重真的发生了」只有本条在守。
    /// 「留下的是哪条」按 `enabled` 这一**读取级**字段判，不按条数判：条数只由 dedup 那一步
    /// 决定，读错了哪条它照样成立。
    #[test]
    fn same_entry_name_from_two_marketplaces_collapses_to_one_capability() {
        let t = TempHome::new();
        write_config(
            &t,
            r#"[plugins."dup@mp-a"]
enabled = true

[plugins."dup@mp-b"]
enabled = false
"#,
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let dups: Vec<&Capability> = caps
            .iter()
            .filter(|c| c.id.kind == Kind::Plugin && c.id.name == "dup")
            .collect();
        assert_eq!(dups.len(), 1, "id 相同的多条会被 dedup 合并成一条");
        match &dups[0].spec {
            Spec::Plugin {
                marketplace,
                enabled,
                ..
            } => {
                assert_eq!(
                    marketplace.as_deref(),
                    Some("mp-a"),
                    "留下的是键序在前的那条"
                );
                assert!(
                    *enabled,
                    "留下必须是 mp-a 那条记录本身（enabled = true），不是 mp-b 的 false"
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
