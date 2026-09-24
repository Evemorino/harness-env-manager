use crate::adapters::{CLAUDE_CODE, HarnessAdapter};
use crate::capability::{
    CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets, normalize_locator,
};
use crate::home::Home;
use crate::skills_dir::{SkillStore, plugin_from_path, try_read_skill_entries};
use anyhow::Context;
use serde_json::Value;
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
        let Some(root) = read_json_if_present(&self.home.claude_json())? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if let Some(servers) = root.get("mcpServers").and_then(Value::as_object) {
            for (name, cfg) in servers {
                out.push(mcp_capability(name, cfg, CLAUDE_CODE));
            }
        }
        Ok(out)
    }

    /// plugin 的**安装记录**在 `~/.claude/plugins/installed_plugins.json`（键是
    /// `name@marketplace`，值是安装数组），**门禁**在 settings.json 的 `enabledPlugins`。
    /// 一个键可能对应多条安装（实测就是数组），v1 只取第一条；空数组 = 没装成，跳过。
    fn read_plugins(&self) -> anyhow::Result<Vec<Capability>> {
        let Some(root) = read_json_if_present(&self.home.claude_installed_plugins())? else {
            return Ok(Vec::new());
        };
        // 门禁读不出来就是读不出来：退化成「全部 enabled」会把用户关掉的 plugin 报成
        // 已启用，正是失败隔离契约禁止的那种「看起来合理」的假同步。
        let enabled_map = self.read_enabled_plugins()?;

        let mut out = Vec::new();
        let plugins = root.get("plugins").and_then(Value::as_object);
        for (key, installs) in plugins.into_iter().flatten() {
            let (entry, marketplace) = split_plugin_key(key);
            let Some(install) = installs.as_array().and_then(|a| a.first()) else {
                continue;
            };
            let enabled = enabled_map.get(key).copied().unwrap_or(true);
            out.push(Capability {
                id: CapId::new(Kind::Plugin, entry.clone()),
                origin: Origin::Adopted {
                    harness: CLAUDE_CODE.into(),
                },
                spec: Spec::Plugin {
                    marketplace,
                    entry,
                    version: normalize_locator(install.get("version").and_then(Value::as_str)),
                    sha: normalize_locator(install.get("gitCommitSha").and_then(Value::as_str)),
                    install_path: install
                        .get("installPath")
                        .and_then(Value::as_str)
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
        let Some(root) = read_json_if_present(&self.home.claude_settings())? else {
            return Ok(BTreeMap::new());
        };
        let mut out = BTreeMap::new();
        if let Some(obj) = root.get("enabledPlugins").and_then(Value::as_object) {
            for (k, v) in obj {
                // 非布尔的门禁值是坏数据，不是「没表态」：猜成 true 就是假同步。
                let Some(b) = v.as_bool() else {
                    anyhow::bail!(
                        "failed to parse enabledPlugins entry {k}: expected a boolean, found {}",
                        match v {
                            Value::Null => "null",
                            Value::String(_) => "a string",
                            Value::Number(_) => "a number",
                            _ => "a container",
                        }
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

/// 读 JSON；文件不存在返回 Ok(None)，语法错误返回带路径的 Err。
///
/// 「没有这个文件」和「文件在但读不出/解析不了」必须是两种返回——前者是正常状态，
/// 后者按计划里的失败隔离契约整次 diff 失败。Task 10/11 共用。
pub(crate) fn read_json_if_present(path: &Path) -> anyhow::Result<Option<Value>> {
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

/// 把一个 MCP 配置对象归一化。env 值保持 `${VAR}` 引用形态，绝不展开。
///
/// 只搬 `command` / `args` / `env` 三样：`type` 之类先不进 `Spec`（Task 3 的 `Spec::Mcp`
/// 里没有承载它的字段）。Task 10/11 共用。
pub(crate) fn mcp_capability(name: &str, cfg: &Value, harness: &str) -> Capability {
    let mut env = BTreeMap::new();
    if let Some(obj) = cfg.get("env").and_then(Value::as_object) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }
    Capability {
        id: CapId::new(Kind::Mcp, name.to_string()),
        origin: Origin::Adopted {
            harness: harness.into(),
        },
        spec: Spec::Mcp {
            command: cfg
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            args: cfg
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
            env,
        },
        targets: Targets::All,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec, Targets};
    use crate::testutil::TempHome;
    use std::path::PathBuf;

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
    }

    #[test]
    fn project_level_mcp_servers_are_out_of_scope() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": {}, "projects": { "/p": { "mcpServers": { "proj-only": { "command": "c" } } } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(
            find(&caps, Kind::Mcp, "proj-only").is_none(),
            "v1 只读用户级 MCP"
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
    /// 所以这里上抛，并留这条测试看守（把 `?` 换回 `.unwrap_or_default()` 只红这一条）。
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
}
