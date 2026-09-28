use crate::adapters::claude_code::split_plugin_key;
use crate::adapters::{CODEX, HarnessAdapter};
use crate::apply::mcp::{McpMechanism, McpTarget};
use crate::capability::{
    CapId, Capability, Kind, McpTransport, McpValue, Origin, SkillSource, Spec, Support, Targets,
};
use crate::home::Home;
use crate::skills_dir::try_read_skill_entries;
use anyhow::anyhow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
            // 两种传输**恰好一种**：有 `url` 就是 http（请求头在 `http_headers`），
            // 否则按 stdio 读 `command` / `args` / `env`。
            //
            // `command` 缺键才是 `""`——「没有 command」和「command 是空串」同为没配；
            // `command = 5` 是故障。
            let transport = match toml_optional_string(cfg, "url", file, &context)? {
                Some(url) => {
                    // `http_headers` 的值是**字面量**（codex 原样发出去）：照抄成 `Literal`，
                    // **哪怕它长得像 `${VAR}`**——那正是这个模型要挡住的东西（§9.6）。
                    let mut headers =
                        toml_literal_map(cfg.get("http_headers"), file, &context, "http_headers")?;
                    // `env_http_headers` 是「头名 → 变量名」——那才是**从环境取值**。
                    for (header, var) in toml_string_map(
                        cfg.get("env_http_headers"),
                        file,
                        &context,
                        "env_http_headers",
                    )? {
                        headers.insert(
                            header,
                            McpValue::Ref {
                                var,
                                bearer: false,
                                optional: false,
                            },
                        );
                    }
                    // `bearer_token_env_var` 是 codex 那条 bearer 通道——它固定送
                    // `Authorization: Bearer <token>`，所以读回成清单里的一等写法。
                    // 写路径就是从它来的（见 `mcp_toml`），读回来不是同一个形状就会
                    // 每轮都报 update。
                    if let Some(var) =
                        toml_optional_string(cfg, "bearer_token_env_var", file, &context)?
                    {
                        headers.insert(
                            "Authorization".to_string(),
                            McpValue::Ref {
                                var: var.to_string(),
                                bearer: true,
                                optional: false,
                            },
                        );
                    }
                    McpTransport::Http {
                        url: url.to_string(),
                        headers,
                    }
                }
                None => {
                    // `env` 与 `http_headers` 同理：**字面量**。
                    let mut env = toml_literal_map(cfg.get("env"), file, &context, "env")?;
                    // `env_vars` 是 codex 的**同名转发**：清单侧的等价写法就是 `K: ${K}`。
                    for var in toml_string_array(cfg.get("env_vars"), file, &context, "env_vars")? {
                        env.insert(
                            var.clone(),
                            McpValue::Ref {
                                var,
                                bearer: false,
                                optional: false,
                            },
                        );
                    }
                    McpTransport::Stdio {
                        command: toml_optional_string(cfg, "command", file, &context)?
                            .unwrap_or_default()
                            .to_string(),
                        args: toml_string_array(cfg.get("args"), file, &context, "args")?,
                        // 保持 `${VAR}` 引用形态，绝不展开。报错只到键名为止——actual 侧的
                        // 值可能就是明文密钥，而这条消息会进终端输出和 Task 14 的快照。
                        env,
                    }
                }
            };

            out.push(Capability {
                id: CapId::new(Kind::Mcp, name.clone()),
                origin: Origin::Adopted {
                    harness: CODEX.into(),
                },
                spec: Spec::Mcp { transport },
                targets: Targets::All,
            });
        }
        Ok(out)
    }

    /// plugin 记录就是 `config.toml` 里的 `[plugins."<entry>@<marketplace>"]`：键本身即身份。
    ///
    /// **已知局限**（Plan C 的 doctor 要报出来）：codex 的 `config.toml` 不记录 plugin 的
    /// 版本或 SHA（缓存目录名可能是 semver、git sha 或字面量 `unknown`），所以 `version`
    /// 与 `sha` 恒为 `None` → `compare_versions` 判 `Unknown` → diff 报可见的 `? unknown`，
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
            // 安装记录键的拆分复用 claude 那一份：`name@marketplace` 这个形状两家都有，
            // 而 `split_plugin_key` 只碰 `&str`，与 TOML/JSON 的值类型无关，所以能共用。
            // 本文件**曾经**为此另写过一个逐字相同的私有 `split_key`（Task 10 的派单边界，
            // 当时 `claude_code.rs` 的文档还写着「只有 claude 需要拆」）——两份无同步守卫的
            // 实现是真实的漂移风险，边界解除后合并成现在这一处。
            // 除它以外本文件不从 `claude_code` 拿任何东西：那批 helper 收
            // `serde_json::Value`，与本文件的 `toml::Value` 类型域不通。
            let (entry, marketplace) = split_plugin_key(key);
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
        // 两个目录：harness 自己的，以及共享库。**顺序有意义**——`read_state` 的 `dedup_by`
        // 保留先扫到的那一条，所以同名时赢的是 `~/.codex/skills` 里的那份（它就是交付目标）。
        //
        // 为什么也要扫共享库：codex 原生读 `~/.agents/skills`（与 kimi 共用的 `.agents`
        // 约定，2026-09-28 实测）。只扫自己那个目录，共享库里的 skill 会被判 `install`——
        // 对一个已经能用的能力重拷一份，报告和盘上都多出一次无意义的动作。
        let shared_root = self.home.agents_skills();
        for dir in [self.home.codex_skills(), shared_root.clone()] {
            for e in try_read_skill_entries(&dir, &self.home)? {
                // 来源说的是**入口**在哪，不是真身在哪：`~/.agents/skills/<name>` 本身是一条
                // 软链时（实测就这么几条），codex 是从那个入口读到它的，与它最终指向谁的目录
                // 无关。按真身判定会把「共享库来源」误记成「某个 harness 的本地目录」，
                // 而 kimi 的 `supports` 正是按这个来源判它读不读得到。
                let source = if dir == shared_root {
                    SkillSource::AgentsDir
                } else {
                    crate::adapters::skill_source_of(&e)
                };
                // §9.3：断链没有内容但有路径，自愈要能看见它（见 `SkillEntry::content_path`）。
                // 先取再搬 `e.name`——顺序反了就是对一个被部分移动的值借方法。
                let path = e.content_path();
                out.push(Capability {
                    id: CapId::new(Kind::Skill, e.name),
                    origin: Origin::Adopted {
                        harness: CODEX.into(),
                    },
                    spec: Spec::Skill { source, path },
                    targets: Targets::All,
                });
            }
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

    fn skills_target_dir(&self) -> Option<PathBuf> {
        Some(self.home.codex_skills())
    }

    fn mcp_target(&self) -> Option<McpTarget> {
        Some(McpTarget {
            path: self.home.codex_config(),
            mechanism: McpMechanism::CodexToml,
        })
    }

    fn supports(&self, _cap: &Capability) -> Support {
        // codex 原生支持全部六种能力
        Support::Native
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

/// `args` 一类的字符串数组。缺键或 `null` 都是空表；形状不对照常报错。
fn toml_string_array(
    value: Option<&TomlValue>,
    file: &Path,
    context: &str,
    field: &str,
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    let Some(list) = toml_optional_array(value, file, &format!("{context} field `{field}`"))?
    else {
        return Ok(out);
    };
    for (i, element) in list.iter().enumerate() {
        out.push(
            toml_expect_string(element, file, &format!("{context} field `{field}[{i}]`"))?
                .to_string(),
        );
    }
    Ok(out)
}

/// `env` / `http_headers` → 值：这两格是**字面量**（codex 原样发出去，不展开 `${VAR}`），
/// 所以一律照抄成 `Literal`，**哪怕它长得像引用**。`env_vars` / `env_http_headers` /
/// `bearer_token_env_var` 才是「从环境取值」的那几格（见 `read_mcps`）。
fn toml_literal_map(
    value: Option<&TomlValue>,
    file: &Path,
    context: &str,
    field: &str,
) -> anyhow::Result<BTreeMap<String, McpValue>> {
    Ok(toml_string_map(value, file, context, field)?
        .into_iter()
        .map(|(key, text)| (key, McpValue::Literal(text)))
        .collect())
}

/// `env` / `http_headers` 一类的字符串映射。报错只到键名为止——值可能是明文密钥。
fn toml_string_map(
    value: Option<&TomlValue>,
    file: &Path,
    context: &str,
    field: &str,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let Some(fields) = toml_optional_table(value, file, &format!("{context} field `{field}`"))?
    else {
        return Ok(out);
    };
    for (k, v) in fields {
        out.insert(
            k.clone(),
            toml_expect_string(v, file, &format!("{context} field `{field}` key `{k}`"))?
                .to_string(),
        );
    }
    Ok(out)
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

/// 布尔门禁的强制版。claude 那一族形状原语**原本**没有布尔版（Task 11 预备派单已给它补上
/// `expect_bool`，见 [crate::adapters::claude_code::expect_bool]），但那一族收
/// `serde_json::Value`，与本文件的 `toml::Value` 类型域不通，合并不了，所以本文件自带一对。
/// codex 侧 `enabled` 是这个形状唯一的读者。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec, Targets};
    use crate::skills_dir::plugin_from_path;
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
        // 来源仍不可判定（doctor 的事），但 `path` 要给得出**链接本身**：§9.3 的断链自愈
        // 靠内容层看得见这个条目才成立，`None` 会让那条承诺在真实运行里永远走不到。
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::Unknown,
                path: Some(t.path(".codex/skills/gone"))
            }
        );
    }

    /// `SkillStore::PluginCache` arm 里 `plugin_from_path(..) => None` 这一支**今天不可达**，
    /// 所以这条测试不假装覆盖它，而是把围住它的恒等式钉在 codex 的入口上：
    /// `classify` 判 `PluginCache` 的判据就是 `plugin_from_path(path).is_some()`
    /// （`skills_dir::is_in_plugin_cache` 的定义），于是「归了缓存却没有 plugin 身份」这个
    /// 输入构造不出来——两条臂里只有 `Some` → `PluginBundle` 可达。下面两半是那条恒等式的
    /// 两个方向：缓存形态 ⇒ 拿得出身份；拿不出身份 ⇒ 根本不是缓存（落 `HarnessLocal`，不是
    /// `Unknown`）。谁把缓存判定放宽成「任意位置出现 `cache`」（skills_dir 文档里记的那次 D1
    /// 回归），第二半立刻红，而那也正是那条 `None` 臂变成真 Unknown 通道的时刻。
    #[test]
    fn a_plugin_cache_path_always_carries_a_plugin_identity() {
        // 方向一：`plugins` 在 `cache` **之前** ⇒ 身份拿得出来。
        let t = TempHome::new();
        let cache = ".claude/plugins/cache/mp/superpowers/b36e0829/skills/brainstorming";
        t.mkdir(cache);
        assert_eq!(
            plugin_from_path(&t.path(cache)).as_deref(),
            Some("superpowers")
        );

        // 方向二：同样的 `cache/<mp>/<name>/<version>/skills/<skill>` 尾巴，但**没有**
        // `plugins` 段 ⇒ `plugin_from_path` 给 `None`，它也就不再是 PluginCache 臂的输入。
        let t = TempHome::new();
        let lookalike = ".codex/skills/mine/cache/mp/superpowers/b36e0829/skills/leaf";
        t.mkdir(lookalike);
        assert_eq!(plugin_from_path(&t.path(lookalike)), None);
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Skill, "mine").unwrap().spec {
            Spec::Skill {
                source: SkillSource::Local { .. },
                ..
            } => {}
            other => panic!("无 `plugins` 段的 cache 尾巴必须落 HarnessLocal，不是 {other:?}"),
        }
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
                transport: McpTransport::Stdio {
                    command: "/abs/Electron".into(),
                    args: vec![
                        "/abs/app".into(),
                        "--mcp-stdio".into(),
                        "--app".into(),
                        "desktop".into()
                    ],
                    env: Default::default(),
                },
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
            Spec::Mcp {
                transport: McpTransport::Stdio { env, .. },
                ..
            } => {
                // 原样保留引用形态，绝不展开成真实密钥
                assert_eq!(env["CONTEXT7_API_KEY"].to_text(), "${CONTEXT7_API_KEY}");
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
                    transport: McpTransport::Stdio {
                        command: String::new(),
                        args: Vec::new(),
                        env: Default::default(),
                    },
                },
                "`{name}`：缺键与「类型对且为空」同为正常空值"
            );
        }
    }

    /// `[mcp_servers]` 写了头却没配任何 server：类型对且为空 = 合法空结果，不是故障。
    /// 与 `mcp_servers_key_that_is_not_a_table_...` 一起把「空」和「坏」分开钉。
    /// 远端 MCP：codex 用 `url`；请求头有两种写法——`http_headers` 是**字面量**，
    /// `env_http_headers` 才是「从环境取值」（值与写路径对称，见下一条测试）。
    #[test]
    fn reads_an_http_mcp_server() {
        let t = TempHome::new();
        write_config(
            &t,
            "[mcp_servers.ctx]\nurl = \"https://mcp.context7.com/mcp\"\n\
             [mcp_servers.ctx.http_headers]\nAuthorization = \"${K}\"\n",
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Mcp, "ctx").unwrap().spec {
            Spec::Mcp {
                transport: McpTransport::Http { url, headers },
                ..
            } => {
                assert_eq!(url, "https://mcp.context7.com/mcp");
                assert_eq!(headers["Authorization"].to_text(), "${K}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// `env_http_headers` 是「头名 → 变量名」：读回来必须是 `${VAR}` 引用，否则
    /// 「henv 自己按这个写法交付的条目」在下一轮 diff 会被报成 update（写下去的和读回来
    /// 的不是一个形状），或者更糟——被当成字面量。
    #[test]
    fn reads_env_http_headers_as_references() {
        let t = TempHome::new();
        write_config(
            &t,
            "[mcp_servers.ctx]\nurl = \"https://mcp.context7.com/mcp\"\n\
             [mcp_servers.ctx.env_http_headers]\nAuthorization = \"CONTEXT7_API_KEY\"\n",
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Mcp, "ctx").unwrap().spec {
            Spec::Mcp {
                transport: McpTransport::Http { headers, .. },
                ..
            } => assert_eq!(headers["Authorization"].to_text(), "${CONTEXT7_API_KEY}"),
            other => panic!("{other:?}"),
        }
    }

    /// `env_vars` 是**同名转发**：清单侧的等价写法就是 `K: ${K}`。
    #[test]
    fn reads_env_vars_as_same_name_references() {
        let t = TempHome::new();
        write_config(
            &t,
            "[mcp_servers.ctx]\ncommand = \"npx\"\nenv_vars = [\"K\", \"W\"]\n",
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Mcp, "ctx").unwrap().spec {
            Spec::Mcp {
                transport: McpTransport::Stdio { env, .. },
                ..
            } => {
                assert_eq!(env["K"].to_text(), "${K}");
                assert_eq!(env["W"].to_text(), "${W}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// `bearer_token_env_var` 是 codex 那条 bearer 通道：读回来必须是清单里的
    /// `Authorization: Bearer ${VAR}`，否则「henv 自己按这个写法交付的条目」下一轮
    /// diff 会报 update（写下去的是 bearer 字段，读回来却什么都没有）。
    #[test]
    fn reads_bearer_token_env_var_as_a_bearer_reference() {
        let t = TempHome::new();
        write_config(
            &t,
            "[mcp_servers.ctx]\nurl = \"https://mcp.context7.com/mcp\"\n\
             bearer_token_env_var = \"CONTEXT7_API_KEY\"\n",
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Mcp, "ctx").unwrap().spec {
            Spec::Mcp {
                transport: McpTransport::Http { headers, .. },
                ..
            } => assert_eq!(
                headers["Authorization"].to_text(),
                "Bearer ${CONTEXT7_API_KEY}"
            ),
            other => panic!("{other:?}"),
        }
    }

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

    /// `toml_type_name` 的 7 条 arm 里，`a float` 与 `a datetime` 没有别的测试走到，
    /// 所以这两条专门钉它们：其余形状测试只覆盖了 string / integer / boolean / array / table。
    /// 少一条就是一个未被执行的 arm（Task 7/8 那轮就是为此补的四条）。
    #[test]
    fn a_float_mcp_command_is_an_error_naming_its_type() {
        let t = TempHome::new();
        write_config(&t, "[mcp_servers.m]\ncommand = 1.5\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &config_file(&t),
                "mcp server `m` field `command`",
                "a string",
                "a float"
            )
        );
    }

    #[test]
    fn a_datetime_mcp_command_is_an_error_naming_its_type() {
        let t = TempHome::new();
        write_config(&t, "[mcp_servers.m]\ncommand = 1979-05-27T07:32:00Z\n");
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &config_file(&t),
                "mcp server `m` field `command`",
                "a string",
                "a datetime"
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

    /// codex 对六种 kind 全部原生接纳（spec §2.1 矩阵）。`Spec::Command` 只是占位载荷——
    /// `supports` 只看 `kind` 与 `origin`，不看 `spec` 内容。
    #[test]
    fn supports_every_kind_natively() {
        let t = TempHome::new();
        let a = Codex::new(t.home.clone());
        for kind in Kind::all() {
            let cap = Capability::new(
                CapId::new(kind, "x"),
                Spec::Command {
                    path: std::path::PathBuf::from("/x"),
                },
            );
            assert_eq!(a.supports(&cap), Support::Native, "{kind:?}");
        }
    }
}
