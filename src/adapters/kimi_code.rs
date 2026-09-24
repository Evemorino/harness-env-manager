use crate::adapters::claude_code::{
    expect_bool, expect_object, expect_string, mcp_capability, optional_array, optional_object,
    optional_string, read_json_if_present,
};
use crate::adapters::{HarnessAdapter, KIMI_CODE};
use crate::capability::{
    CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets, normalize_locator,
};
use crate::home::Home;
use crate::skills_dir::try_read_skill_entries;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub struct KimiCode {
    home: Home,
}

impl KimiCode {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    /// 用户级 MCP 在 `~/.kimi-code/mcp.json` 的 `mcpServers`。
    /// 与 claude 的实测形状差别是：这里**没有** `type` 字段、键序也不同，归一化后同形，
    /// 所以整条读取复用 claude 那一套形状原语与 `mcp_capability`——**走 JSON 的两家共用**；
    /// codex 的 TOML 那一套类型域不通，不适用。
    fn read_mcps(&self) -> anyhow::Result<Vec<Capability>> {
        let file = self.home.kimi_mcp();
        let Some(root) = read_json_if_present(&file)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        // 「没这个键」或「值是显式 null」→ 空（正常，没配 MCP）；「有键、值既非 null 也非对象」
        // → 故障。用 `optional_object` 而不是 `root.get(..).and_then(as_object)`：后者会把
        // `{"mcpServers": []}` 报成「这家没配任何 MCP」，正是「形状故障也是故障」那块禁止的塌缩。
        if let Some(servers) = optional_object(root.get("mcpServers"), &file, "key `mcpServers`")? {
            for (name, cfg) in servers {
                out.push(mcp_capability(name, cfg, KIMI_CODE, &file)?);
            }
        }
        Ok(out)
    }

    /// `~/.kimi-code/plugins/installed.json` 的 `plugins` 是**数组**（claude 的是对象），
    /// 身份直接躺在 `id` 字段里，所以这里不需要 `split_plugin_key`。
    /// 形状差异压平在 `InstalledPlugin` 这一层，plugin 能力与 bundle skill 共用同一份记录。
    fn read_plugins(&self) -> anyhow::Result<Vec<InstalledPlugin>> {
        let file = self.home.kimi_installed_plugins();
        let Some(root) = read_json_if_present(&file)? else {
            return Ok(Vec::new());
        };
        // 缺键或显式 `null` = 「一个都没装」（正常）；键在有非数组的值上 = 故障。
        // 不要写 `root.get("plugins").and_then(Value::as_array)`：那等于把
        // `{"plugins": "oops"}` 报成「什么都没装」，也就是失败隔离契约禁止的假同步。
        let Some(list) = optional_array(root.get("plugins"), &file, "key `plugins`")? else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for (i, p) in list.iter().enumerate() {
            // 数组元素没有「缺失」这层含义：一条记录必须是对象。
            let context = format!("plugins[{i}]");
            let record = expect_object(p, &file, &context)?;
            // `id` 是这条记录的身份，缺了或非字符串都认不出来——按「坏值」处理，
            // 绝不 `continue` 丢条目（丢条目 = 漏报，只读工具最坏的方向）。
            let Some(id_value) = record.get("id") else {
                anyhow::bail!("invalid shape in {}: {context} has no `id`", file.display());
            };
            let id = expect_string(id_value, &file, &format!("{context} field `id`"))?;
            let (marketplace, version, sha) = self.read_github_block(record, &file, &context)?;
            out.push(InstalledPlugin {
                id: id.to_string(),
                // `root` 是可选位：没写不是坏配置，只是没有 bundle 可扫（`install_path: None`）。
                root: optional_string(record, "root", &file, &context)?.map(PathBuf::from),
                // `enabled` 走第七个形状原语 `expect_bool`，**两步分开**：
                // 「缺键 = 未表态 = 启用」（与 claude 的门禁默认、codex 的 `toml_optional_bool`
                // 默认同语义），「键在但不是布尔」是故障。写成
                // `.and_then(Value::as_bool).unwrap_or(true)` 就把用户关掉的 plugin 报成已启用。
                enabled: match record.get("enabled") {
                    Some(v) => expect_bool(v, &file, &format!("{context} field `enabled`"))?,
                    None => true,
                },
                marketplace,
                version,
                sha,
            });
        }
        Ok(out)
    }

    /// 解析一条记录里的 `github` 块 → `(marketplace, version, sha)`。
    ///
    /// **这是 Task 11 显式裁决过的那条路**（计划给的另一个选项是保持示例里那条嵌套
    /// `and_then` 链并注释说明）。裁决：**逐层验证到形状为止，但必填位一个都不加**——
    /// 容器用 `optional_object`（缺键与显式 `null` = 「没有 marketplace 信息」，正常状态），
    /// 块内 `owner` / `repo` / `installedSha` 用 `optional_string`、`ref` 用
    /// `optional_object` + `optional_string`。于是：
    /// - 「类型写坏」在**每一层**都是故障，不再与「没写」不可区分（那条 `and_then` 链的代价）；
    /// - 「没写这一项」仍然是「没写」，不会因为 kimi 少写一个 `ref` 就把一份健康配置
    ///   判成读不出来（同一个方向的分界见 `claude_code::gate_enabled` 文档里
    ///   「残留门禁键不能当故障上抛」那一段）。
    ///
    /// 已知并接受的残留：`owner` 与 `repo` 是一对，只写了一半时 `marketplace` 仍是 `None`
    /// 且不报错——「写了 owner 忘了 repo」与「没有 repo 来源标识」仍不可区分。
    /// 由 `a_github_block_with_only_an_owner_reports_no_marketplace` 钉着，改动必须红点什么。
    ///
    /// `ref.kind`（实测 `"tag"`）在 `Spec::Plugin` 里没有承载它的字段，v1 不读，
    /// 与 claude 侧不读 MCP 的 `type` 同一取舍。
    fn read_github_block(
        &self,
        record: &Map<String, Value>,
        file: &Path,
        context: &str,
    ) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
        let github_context = format!("{context} field `github`");
        let Some(github) = optional_object(record.get("github"), file, &github_context)? else {
            return Ok((None, None, None));
        };
        let owner = optional_string(github, "owner", file, &github_context)?;
        let repo = optional_string(github, "repo", file, &github_context)?;
        let marketplace = owner.zip(repo).map(|(o, r)| format!("{o}/{r}"));

        let ref_context = format!("{github_context} field `ref`");
        let version = normalize_locator(
            match optional_object(github.get("ref"), file, &ref_context)? {
                Some(reference) => optional_string(reference, "value", file, &ref_context)?,
                None => None,
            },
        );
        // installedSha 与 claude 的 gitCommitSha 是同一个 SHA，必须进 `sha` 位，
        // SHA 优先比较才成立（spec §2.1 第 3 条）。
        let sha = normalize_locator(optional_string(
            github,
            "installedSha",
            file,
            &github_context,
        )?);
        Ok((marketplace, version, sha))
    }

    /// kimi 没有用户级 skills 目录：skill 只随 plugin bundle 分发在
    /// `<root>/skills/`（spec §2.1 第 4 条）。所以这里的来源一律是
    /// `SkillSource::PluginBundle` + `Origin::Plugin`，而不是 claude/codex 那两家的
    /// `Adopted`——Task 12 的 `supports()` 例外正认这一位：`Origin::Plugin` 的 bundle
    /// skill 判 Native，其余（散装）skill 判 Unsupported。
    ///
    /// 返回 `Result` 而不是计划示例里的 `Vec`：生产入口 `try_read_skill_entries` 可失败，
    /// 把「读不出这个目录」吞成空表就是破坏失败隔离契约。
    /// 只有「目录不存在」才是空的，那由 `try_read_skill_entries` 自己判（`NotFound` → 空）。
    fn read_bundled_skills(&self, plugins: &[InstalledPlugin]) -> anyhow::Result<Vec<Capability>> {
        let mut out = Vec::new();
        for p in plugins {
            if !p.enabled {
                continue;
            }
            let Some(root) = &p.root else { continue };
            for e in try_read_skill_entries(&root.join("skills"), &self.home)? {
                out.push(Capability {
                    id: CapId::new(Kind::Skill, e.name),
                    origin: Origin::Plugin { name: p.id.clone() },
                    spec: Spec::Skill {
                        source: SkillSource::PluginBundle {
                            plugin: p.id.clone(),
                        },
                        // 比较一律用 `resolved`（Task 7 契约 D5：`path` 保持扫描现场的形态）
                        path: e.resolved,
                    },
                    targets: Targets::All,
                });
            }
        }
        Ok(out)
    }
}

/// kimi `installed.json` 条目压平后的形状：一条记录同时喂 plugin 能力和 bundle skill 扫描。
struct InstalledPlugin {
    id: String,
    root: Option<PathBuf>,
    enabled: bool,
    marketplace: Option<String>,
    version: Option<String>,
    sha: Option<String>,
}

impl HarnessAdapter for KimiCode {
    fn name(&self) -> &str {
        KIMI_CODE
    }

    fn detect(&self) -> bool {
        self.home.kimi_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        // plugins 先读：bundle skill 与 plugin 能力共用这一份记录。副作用是两个文件同时
        // 写坏时先报 installed.json——顺序确定，Task 14 的 golden 才不抖。
        let plugins = self.read_plugins()?;
        let mut caps = self.read_mcps()?;
        caps.extend(self.read_bundled_skills(&plugins)?);
        for p in &plugins {
            caps.push(Capability {
                id: CapId::new(Kind::Plugin, p.id.clone()),
                origin: Origin::Adopted {
                    harness: KIMI_CODE.into(),
                },
                spec: Spec::Plugin {
                    marketplace: p.marketplace.clone(),
                    entry: p.id.clone(),
                    version: p.version.clone(),
                    sha: p.sha.clone(),
                    install_path: p.root.clone(),
                    enabled: p.enabled,
                },
                targets: Targets::All,
            });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec, Targets};
    use crate::testutil::TempHome;
    use std::path::{Path, PathBuf};

    fn find<'a>(caps: &'a [Capability], kind: Kind, name: &str) -> Option<&'a Capability> {
        caps.iter().find(|c| c.id.kind == kind && c.id.name == name)
    }

    /// 形状故障的报错按**整串**钉死（不 `contains`），与 claude / codex 两家的 `shape_msg`
    /// 一字不差：文件路径、位置（只到键名为止）、期望类型、实际类型就是全部内容，
    /// 多出来的任何一个字都可能是漏出的值。Task 14 的 golden 也不该三家三种样子。
    fn shape_msg(file: &Path, what: &str, expected: &str, found: &str) -> String {
        format!(
            "invalid shape in {}: {what} must be {expected}, found {}",
            file.display(),
            found
        )
    }

    fn read_state_err(t: &TempHome) -> String {
        KimiCode::new(t.home.clone())
            .read_state()
            .expect_err("形状故障必须上抛，不能塌成空表")
            .to_string()
    }

    fn installed_file(t: &TempHome) -> PathBuf {
        t.path(".kimi-code/plugins/installed.json")
    }

    fn write_installed(t: &TempHome, body: &str) {
        t.write(".kimi-code/plugins/installed.json", body);
    }

    /// 一条 `installed.json` 文档，`records` 是塞进 `plugins` 数组的元素串。
    fn installed_doc(records: &str) -> String {
        format!(r#"{{ "version": 1, "plugins": [ {records} ] }}"#)
    }

    /// 只带身份和「一个不存在的目录」的记录：不产生 skill，报错位置只由被测那一段决定。
    fn record(fields: &str) -> String {
        installed_doc(&format!(
            r#"{{ "id": "p", "root": "/definitely/not/here", {fields} }}"#
        ))
    }

    // ============ mcp.json：MCP ============

    #[test]
    fn reads_mcp_from_mcp_json() {
        let t = TempHome::new();
        t.write(
            ".kimi-code/mcp.json",
            r#"{ "mcpServers": {
                  "kimi-cu": { "args": ["mcp","-s","user"], "command": "/Applications/KimiCU.app/Contents/MacOS/kimi-cu" },
                  "ctx": { "command": "npx", "args": ["-y","@upstash/context7-mcp"],
                           "env": { "CONTEXT7_API_KEY": "${CONTEXT7_API_KEY}" } } } }"#,
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        // 与 claude 的实测形状差别：kimi 没有 `type` 字段、键序也不同，归一化后应当同形。
        assert_eq!(
            find(&caps, Kind::Mcp, "kimi-cu").unwrap().spec,
            Spec::Mcp {
                command: "/Applications/KimiCU.app/Contents/MacOS/kimi-cu".into(),
                args: vec!["mcp".into(), "-s".into(), "user".into()],
                env: Default::default(),
            }
        );
        match &find(&caps, Kind::Mcp, "ctx").unwrap().spec {
            Spec::Mcp { env, .. } => assert_eq!(env["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}"),
            other => panic!("{other:?}"),
        }
    }

    /// `mcp_capability` 的第三、四参数由**调用方**填，所以这两半都在 kimi 这一侧的
    /// 可变异点上：传成 `CLAUDE_CODE` 或传成 claude 的文件路径，现有测试一条都不会红。
    #[test]
    fn mcps_are_attributed_to_kimi_code_and_name_the_kimi_file_in_faults() {
        let t = TempHome::new();
        t.write(
            ".kimi-code/mcp.json",
            r#"{ "mcpServers": { "ok": { "command": "c" } } }"#,
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        assert_eq!(
            find(&caps, Kind::Mcp, "ok").unwrap().origin,
            Origin::Adopted {
                harness: "kimi-code".into()
            }
        );
        assert_eq!(
            find(&caps, Kind::Mcp, "ok").unwrap().targets,
            Targets::All,
            "actual 侧读出来的能力不该自带 targets 限制"
        );

        t.write(".kimi-code/mcp.json", r#"{ "mcpServers": { "ctx": 5 } }"#);
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".kimi-code/mcp.json"),
                "mcp server `ctx`",
                "an object",
                "a number"
            )
        );
    }

    #[test]
    fn mcp_servers_key_that_is_not_an_object_is_an_error_naming_the_key() {
        let t = TempHome::new();
        t.write(".kimi-code/mcp.json", r#"{ "mcpServers": [] }"#);
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &t.path(".kimi-code/mcp.json"),
                "key `mcpServers`",
                "an object",
                "an array"
            )
        );
    }

    #[test]
    fn a_null_mcp_servers_container_reads_as_no_servers_configured() {
        let t = TempHome::new();
        t.write(".kimi-code/mcp.json", r#"{ "mcpServers": null }"#);
        let caps = KimiCode::new(t.home.clone())
            .read_state()
            .expect("容器层 `null` = 没配，不是故障");
        assert!(caps.is_empty());
    }

    // ============ installed.json：plugin 记录 ============

    #[test]
    fn reads_plugin_with_installed_sha_and_ref() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/superpowers");
        t.mkdir(".kimi-code/plugins/managed/superpowers");
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "superpowers", "root": "{}", "source": "github", "enabled": true,
                        "originalSource": "https://github.com/obra/superpowers",
                        "github": {{ "owner": "obra", "repo": "superpowers",
                                     "ref": {{ "kind": "tag", "value": "v6.3.0" }},
                                     "installedSha": "b36e0829c6d0140e93cfef2ca599b1b07d4a7797" }} }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let p = find(&caps, Kind::Plugin, "superpowers").unwrap();
        assert_eq!(
            p.spec,
            Spec::Plugin {
                marketplace: Some("obra/superpowers".into()),
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: Some("b36e0829c6d0140e93cfef2ca599b1b07d4a7797".into()),
                install_path: Some(root),
                enabled: true,
            }
        );
        assert_eq!(
            p.origin,
            Origin::Adopted {
                harness: "kimi-code".into()
            }
        );
        assert_eq!(p.targets, Targets::All);
    }

    #[test]
    fn zip_url_plugin_without_github_block_has_no_sha() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/kimi-cu");
        t.mkdir(".kimi-code/plugins/managed/kimi-cu");
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "kimi-cu", "root": "{}", "source": "zip-url", "enabled": true,
                        "originalSource": "https://cdn.kimi.com/kimi-computer-use/latest/kimi-cu-plugin.zip" }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "kimi-cu").unwrap().spec {
            Spec::Plugin {
                sha,
                version,
                marketplace,
                ..
            } => {
                assert_eq!(*sha, None);
                assert_eq!(*version, None);
                assert_eq!(*marketplace, None, "zip-url 没有 repo 来源标识");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn disabled_plugin_is_reported_as_disabled() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""source": "github", "enabled": false"#));
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(!enabled),
            other => panic!("{other:?}"),
        }
    }

    /// 「未列 = 未表态 = 启用」与 claude / codex 的默认一致。这条与上一条是**两个**方向：
    /// 把默认写成 `false` 只有本条红，把 `enabled` 整行硬编码成 `true` 只有上一条红。
    #[test]
    fn an_absent_enabled_field_defaults_to_enabled() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""source": "github""#));
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(*enabled, "缺键是没表态，不是「已禁用」"),
            other => panic!("{other:?}"),
        }
    }

    /// `enabled` 用第七个形状原语 `expect_bool`，**不是**
    /// `.and_then(Value::as_bool).unwrap_or(true)`：后者把「用户写坏了布尔」读成「已启用」，
    /// 于是 diff 把一件用户关掉的东西念成已同步——Task 9 判为 Critical 的那类塌缩。
    #[test]
    fn a_non_boolean_enabled_is_a_shape_fault_that_never_echoes_the_value() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""enabled": "yes""#));
        let err = read_state_err(&t);
        assert_eq!(
            err,
            shape_msg(
                &installed_file(&t),
                "plugins[0] field `enabled`",
                "a boolean",
                "a string"
            )
        );
        assert!(!err.contains("yes"), "报错不得带出原值：{err}");
    }

    /// `id` 是这条记录的身份：缺了就是认不出来，按坏值处理，绝不 `continue` 丢条目。
    /// 用第二条记录触发，顺带钉住报错里的**下标**来自 enumerate 而不是写死的 0。
    #[test]
    fn a_record_without_an_id_is_an_error_naming_its_index() {
        let t = TempHome::new();
        write_installed(
            &t,
            &installed_doc(
                r#"{ "id": "first", "root": "/nope" }, { "root": "/nope", "enabled": true }"#,
            ),
        );
        assert_eq!(
            read_state_err(&t),
            format!(
                "invalid shape in {}: plugins[1] has no `id`",
                installed_file(&t).display()
            )
        );
    }

    #[test]
    fn a_non_string_id_is_a_shape_fault_naming_the_field() {
        let t = TempHome::new();
        write_installed(&t, &installed_doc(r#"{ "id": 42, "root": "/nope" }"#));
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &installed_file(&t),
                "plugins[0] field `id`",
                "a string",
                "a number"
            )
        );
    }

    /// kimi 的 `plugins` 是**数组**（claude 的是对象）。`{"plugins": "oops"}` 必须上抛：
    /// 报成「这家一个 plugin 都没装」就是「形状故障也是故障」那块点名的假同步。
    #[test]
    fn plugins_key_that_is_not_an_array_is_an_error_naming_the_key() {
        let t = TempHome::new();
        write_installed(&t, r#"{ "version": 1, "plugins": { "superpowers": [] } }"#);
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &installed_file(&t),
                "key `plugins`",
                "an array",
                "an object"
            )
        );
    }

    #[test]
    fn a_null_plugins_key_reads_as_nothing_installed() {
        let t = TempHome::new();
        write_installed(&t, r#"{ "version": 1, "plugins": null }"#);
        let caps = KimiCode::new(t.home.clone())
            .read_state()
            .expect("容器层 `null` = 一个都没装，不是故障");
        assert!(caps.is_empty());
    }

    /// 数组元素没有「缺失」这层含义：一条记录必须是对象。
    #[test]
    fn a_plugin_record_that_is_not_an_object_is_a_shape_fault_naming_the_index() {
        let t = TempHome::new();
        write_installed(&t, &installed_doc(r#""not-a-record""#));
        assert_eq!(
            read_state_err(&t),
            shape_msg(&installed_file(&t), "plugins[0]", "an object", "a string")
        );
    }

    #[test]
    fn a_non_string_root_is_a_shape_fault_naming_the_field() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""root": 5, "enabled": true"#));
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &installed_file(&t),
                "plugins[0] field `root`",
                "a string",
                "a number"
            )
        );
    }

    /// `root` 是**可选**位：brief 的实测样本里两条记录都带它，但**少写**它不构成坏配置
    /// （`install_path` 本来就是 `Option`）——缺键 → `None` 且没有 bundle 可扫，记录照常上报。
    /// 把它改成 `expect_string` 就是「把健康配置判成读不出来」那个方向的错。
    #[test]
    fn a_record_without_a_root_still_reports_the_plugin() {
        let t = TempHome::new();
        write_installed(
            &t,
            &installed_doc(r#"{ "id": "orphan", "source": "zip-url" }"#),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let p = find(&caps, Kind::Plugin, "orphan").expect("缺 root 不是丢条目的理由");
        match &p.spec {
            Spec::Plugin {
                install_path,
                enabled,
                ..
            } => {
                assert_eq!(*install_path, None);
                assert!(*enabled, "缺 root 不该连带把 enabled 也猜成 false");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            caps.iter().all(|c| c.id.kind != Kind::Skill),
            "没有 root 就没有 bundle 可扫"
        );
    }

    /// `version` / `sha` 都过 `normalize_locator`：codex 的缓存目录名实测会写 `unknown`，
    /// kimi 同源的哨兵值不能当成一个真定位符去比。
    #[test]
    fn sentinel_version_and_sha_become_none() {
        let t = TempHome::new();
        write_installed(
            &t,
            &record(
                r#""github": { "owner": "obra", "repo": "superpowers",
                              "ref": { "kind": "branch", "value": "unknown" },
                              "installedSha": "" }"#,
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin {
                version,
                sha,
                marketplace,
                ..
            } => {
                assert_eq!(*sha, None, "空串不是定位符");
                assert_eq!(*version, None, "`unknown` 不是版本号");
                assert_eq!(
                    marketplace.as_deref(),
                    Some("obra/superpowers"),
                    "哨兵裁决只管定位符，不该顺手清空来源标识"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    // ============ `github` 块的裁决（Task 11 显式选择的那条路） ============

    /// 裁决：**验证到形状层为止**——`github` 容器用 `optional_object`，块内每个字段用
    /// `optional_string` / `optional_object`，于是「类型写坏」 everywhere 都是故障，
    /// 而「没写这一项」仍然是「没写」。计划给的 (a) 那条嵌套 `and_then` 链会把
    /// `"github": "obra/superpowers"` 静默读成 None，也就是「配置写坏了」与
    /// 「没有 marketplace 信息」不可区分；本条就是那条链的死刑测试。
    #[test]
    fn a_github_block_that_is_not_an_object_is_a_shape_fault() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""github": "obra/superpowers""#));
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &installed_file(&t),
                "plugins[0] field `github`",
                "an object",
                "a string"
            )
        );
    }

    #[test]
    fn a_github_owner_that_is_not_a_string_is_a_shape_fault() {
        let t = TempHome::new();
        write_installed(
            &t,
            &record(r#""github": { "owner": 5, "repo": "superpowers" }"#),
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &installed_file(&t),
                "plugins[0] field `github` field `owner`",
                "a string",
                "a number"
            )
        );
    }

    /// `ref` 是「对象或没有」，不是「随便什么」：实测形状是
    /// `{"kind": "tag", "value": "v6.3.0"}`，写成裸字符串就该报坏，而不是安静地没版本。
    #[test]
    fn a_github_ref_that_is_not_an_object_is_a_shape_fault() {
        let t = TempHome::new();
        write_installed(
            &t,
            &record(r#""github": { "owner": "obra", "repo": "superpowers", "ref": "v6.3.0" }"#),
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &installed_file(&t),
                "plugins[0] field `github` field `ref`",
                "an object",
                "a string"
            )
        );
    }

    #[test]
    fn a_github_installed_sha_that_is_not_a_string_is_a_shape_fault() {
        let t = TempHome::new();
        write_installed(
            &t,
            &record(r#""github": { "owner": "obra", "repo": "r", "installedSha": true }"#),
        );
        assert_eq!(
            read_state_err(&t),
            shape_msg(
                &installed_file(&t),
                "plugins[0] field `github` field `installedSha`",
                "a string",
                "a boolean"
            )
        );
    }

    /// `github: null` 与缺键同义（容器层 `null` 裁决），不是故障。
    #[test]
    fn a_null_github_block_reads_as_no_marketplace_info() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""github": null"#));
        let caps = KimiCode::new(t.home.clone())
            .read_state()
            .expect("`github: null` 是「没写」，不是坏配置");
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin {
                marketplace,
                version,
                sha,
                ..
            } => {
                assert_eq!(marketplace.as_deref(), None);
                assert_eq!(version.as_deref(), None);
                assert_eq!(sha.as_deref(), None);
            }
            other => panic!("{other:?}"),
        }
    }

    /// **本裁决已知并接受的残留**：owner / repo 各自都是可选位，所以只写了一半的
    /// `github` 块仍然产不出来源标识，而且不报错——「写了 owner 忘了 repo」与「就是没有
    /// marketplace 信息」在这条路下依然不可区分。把它钉下来（而不是修掉）是为了让下一次
    /// 改动必须红点什么：把任一字段改成必填时本条会红，那正是需要重新裁决的时刻。
    #[test]
    fn a_github_block_with_only_an_owner_reports_no_marketplace() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""github": { "owner": "obra" }"#));
        let caps = KimiCode::new(t.home.clone())
            .read_state()
            .expect("半个 github 块不是读不出，只是没有 repo");
        match &find(&caps, Kind::Plugin, "p").unwrap().spec {
            Spec::Plugin { marketplace, .. } => assert_eq!(marketplace.as_deref(), None),
            other => panic!("{other:?}"),
        }
    }

    // ============ bundle 内的 skills（spec §2.1 第 4 条） ============

    #[test]
    fn reads_bundled_skills_of_installed_plugins() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/superpowers");
        t.mkdir(".kimi-code/plugins/managed/superpowers/skills/brainstorming")
            .mkdir(".kimi-code/plugins/managed/superpowers/skills/test-driven-development");
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "superpowers", "root": "{}", "source": "github", "enabled": true }}
                   ] }}"#,
                root.display()
            ),
        );

        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "brainstorming").expect("bundled skill surfaced");
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::PluginBundle {
                    plugin: "superpowers".into()
                },
                path: Some(t.path(".kimi-code/plugins/managed/superpowers/skills/brainstorming")),
            }
        );
        // kimi 的 skill 只随 bundle 分发（`~/.kimi-code` 没有用户级 skills 目录），
        // 所以 origin 一律是 Plugin 而不是 claude/codex 那两家的 Adopted——Task 12 的
        // `supports()` 例外分支就靠这一位区分 bundle 分发物与散装 skill。
        assert_eq!(
            s.origin,
            Origin::Plugin {
                name: "superpowers".into()
            }
        );
        assert_eq!(s.targets, Targets::All);
        assert!(find(&caps, Kind::Skill, "test-driven-development").is_some());
    }

    #[test]
    fn disabled_plugin_bundled_skills_are_not_reported() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/off");
        t.mkdir(".kimi-code/plugins/managed/off/skills/ghost");
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "off", "root": "{}", "source": "github", "enabled": false }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        assert!(
            find(&caps, Kind::Skill, "ghost").is_none(),
            "disabled bundle contributes nothing"
        );
        assert!(
            find(&caps, Kind::Plugin, "off").is_some(),
            "禁用只影响 bundle 内容，plugin 记录本身还得报出来"
        );
    }

    #[test]
    fn plugin_root_that_does_not_exist_yields_no_skills_but_keeps_the_plugin() {
        let t = TempHome::new();
        write_installed(&t, &record(r#""source": "github", "enabled": true"#));
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Plugin, "p").is_some());
        assert!(caps.iter().all(|c| c.id.kind != Kind::Skill));
    }

    /// 失败隔离契约在 bundle 扫描这一侧：`<root>/skills` **读不出**（这里是普通文件 →
    /// ENOTDIR）必须是 `Err`，不是空表。用 lossy 的 `read_skill_entries` 会直接破坏这条。
    #[test]
    fn an_unreadable_bundle_skills_dir_is_an_error_not_an_empty_scan() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/ghost");
        t.mkdir(".kimi-code/plugins/managed/ghost")
            .write(".kimi-code/plugins/managed/ghost/skills", "not a directory");
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "ghost", "root": "{}", "source": "github", "enabled": true }}
                   ] }}"#,
                root.display()
            ),
        );
        let err = KimiCode::new(t.home.clone())
            .read_state()
            .expect_err("读不出的 bundle skills 目录必须上抛，不能吞成空表");
        let msg = err.to_string();
        assert!(msg.starts_with("failed to read skills dir "), "{msg}");
    }

    /// 断链是数据不是故障：条目照常出现，`path` 落 None，交给 doctor。
    #[test]
    fn a_broken_link_inside_a_bundle_still_surfaces() {
        let t = TempHome::new();
        t.mkdir(".kimi-code/plugins/managed/superpowers/skills")
            .symlink(
                ".kimi-code/plugins/managed/superpowers/skills/gone",
                "../../nowhere",
            );
        let root = t.path(".kimi-code/plugins/managed/superpowers");
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "superpowers", "root": "{}", "source": "github" }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "gone").expect("断链必须作为条目出现");
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::PluginBundle {
                    plugin: "superpowers".into()
                },
                path: None,
            }
        );
    }

    // ============ 文件级：缺失 / 语法 / 顶层形状 ============

    #[test]
    fn missing_files_are_not_errors() {
        let t = TempHome::new();
        t.mkdir(".kimi-code");
        let caps = KimiCode::new(t.home.clone())
            .read_state()
            .expect("no config != error");
        assert!(caps.is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_naming_the_file() {
        let t = TempHome::new();
        t.write(".kimi-code/mcp.json", "{ broken");
        let err = KimiCode::new(t.home.clone())
            .read_state()
            .unwrap_err()
            .to_string();
        assert!(err.contains("mcp.json"), "{err}");
    }

    /// 「JSON 不回显」在两个 kimi 文件上都得成立：整条 cause 链（`{:#}` / `{:?}`）都查，
    /// 只看首行会漏掉 `with_context` 管不住的下面几层。与 claude 的同名测试同一配方，
    /// 密钥写在**出错那一行**上。
    #[test]
    fn a_json_parse_failure_never_echoes_the_offending_line() {
        for rel in [".kimi-code/mcp.json", ".kimi-code/plugins/installed.json"] {
            let t = TempHome::new();
            t.write(
                rel,
                "{\n  \"plugins\": [\n    { \"id\": { \"TOKEN\": sk-live-9f3a2b7c } }\n  ]\n}\n",
            );
            let err = KimiCode::new(t.home.clone())
                .read_state()
                .expect_err("语法坏的配置文件必须是故障");
            for rendered in [format!("{err:#}"), format!("{err:?}")] {
                assert!(
                    !rendered.contains("sk-live-9f3a2b7c"),
                    "整条报错链都不得带出原值：{rendered}"
                );
                assert!(
                    !rendered.contains("TOKEN"),
                    "报错不得回显出错那一行的原文：{rendered}"
                );
                assert!(
                    rendered.contains(&t.path(rel).display().to_string()),
                    "报错必须点明是哪个文件：{rendered}"
                );
                assert!(
                    rendered.contains("expected value"),
                    "报错必须留下解析器给的原因文本：{rendered}"
                );
            }
        }
    }

    /// 顶层不是对象时 `root.get(..)` 只会给 `None`，坏文件于是长得和「一个键都没写」一样。
    /// 守卫在共用的 `read_json_if_present` 里，kimi 的两个文件都该走到（少一家就是少一次看守）。
    #[test]
    fn a_config_file_whose_top_level_is_not_an_object_is_an_error() {
        for rel in [".kimi-code/mcp.json", ".kimi-code/plugins/installed.json"] {
            let t = TempHome::new();
            t.write(rel, "[ 1, 2, 3 ]");
            assert_eq!(
                read_state_err(&t),
                shape_msg(&t.path(rel), "top level", "an object", "an array"),
                "{rel}"
            );
        }
    }

    /// 整个文档就是一个 `null`：容器层的「无」，与「没有这个文件」同义，不是故障。
    #[test]
    fn a_document_that_is_just_null_reads_as_configured_nothing() {
        for rel in [".kimi-code/mcp.json", ".kimi-code/plugins/installed.json"] {
            let t = TempHome::new();
            t.write(rel, "null");
            let caps = KimiCode::new(t.home.clone())
                .read_state()
                .unwrap_or_else(|e| panic!("{rel} 是 null 文档，不该报错：{e}"));
            assert!(caps.is_empty(), "{rel}");
        }
    }

    // ============ 汇总：排序 / 去重 ============

    #[test]
    fn output_is_sorted_and_deduplicated() {
        let t = TempHome::new();
        t.mkdir(".kimi-code/plugins/managed/superpowers/skills/zzz")
            .mkdir(".kimi-code/plugins/managed/superpowers/skills/aaa");
        let root = t.path(".kimi-code/plugins/managed/superpowers");
        t.write(
            ".kimi-code/mcp.json",
            r#"{ "mcpServers": { "z": { "command": "c" }, "a": { "command": "c" } } }"#,
        );
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "superpowers", "root": "{}", "source": "github" }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "{labels:?}");
        let mut dedup = labels.clone();
        dedup.dedup();
        assert_eq!(labels.len(), dedup.len(), "duplicate ids: {labels:?}");
    }

    /// 身份是 `kind + name` 两半，不只是名字：同一个名字作为 MCP、plugin `id` 和 bundle
    /// 内的 skill 各出现一次时，三条**都**要留下（`CapId` 的 kind 不同 → 三个不同身份）。
    ///
    /// **这条的 fixture 是变异实测改出来的**，不是顺手写的：第一版只让 MCP 与 skill 同名、
    /// plugin 叫别的名字，结果 `dedup_by(|a, b| a.id.name == b.id.name)` 这个变异体**存活**
    /// （36 条全绿）。原因是 `dedup_by` 只合并**相邻**项，而 `"mcp/" < "plugin/" < "skill/"`
    /// 对任意 label 恒成立——列表里只要有一条 plugin 能力，`mcp/x` 与 `skill/x` 之间就永远
    /// 隔着它，按名字的错判据根本不触发。要让三种 kind 的同名能力两两相邻，唯一办法就是
    /// 让它们**共用同一个名字**，于是这里 MCP / plugin / skill 三条全叫 `dup`。
    #[test]
    fn three_kinds_sharing_one_name_are_all_reported() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/dup");
        t.mkdir(".kimi-code/plugins/managed/dup/skills/dup");
        t.write(
            ".kimi-code/mcp.json",
            r#"{ "mcpServers": { "dup": { "command": "c" } } }"#,
        );
        write_installed(
            &t,
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "dup", "root": "{}", "source": "github" }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        assert_eq!(
            labels,
            [
                "mcp/dup".to_string(),
                "plugin/dup".to_string(),
                "skill/dup".to_string()
            ],
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
        assert_eq!(
            find(&caps, Kind::Plugin, "dup").map(|c| c.id.kind),
            Some(Kind::Plugin)
        );
    }

    /// 数组格式允许两条记录写同一个 `id`（claude 的对象格式做不到），所以
    /// 「去重真的发生了」在 kimi 这一侧必须由本条钉：删掉 `dedup_by` 那一行只有这里会红。
    /// 留下的是数组里在前的那条（`sort_by_key` 稳定），按 `enabled` 这个**读取级**字段判，
    /// 不按条数判。要不要区分同 id 的两条安装是 Task 13/14 的裁决，不在 Task 11。
    #[test]
    fn two_records_with_the_same_id_collapse_to_one_capability() {
        let t = TempHome::new();
        write_installed(
            &t,
            &installed_doc(
                r#"{ "id": "dup", "root": "/nope", "enabled": true },
                    { "id": "dup", "root": "/nope", "enabled": false }"#,
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let dups: Vec<&Capability> = caps
            .iter()
            .filter(|c| c.id.kind == Kind::Plugin && c.id.name == "dup")
            .collect();
        assert_eq!(dups.len(), 1, "同 id 的多条会被 dedup 合并成一条");
        match &dups[0].spec {
            Spec::Plugin {
                install_path,
                enabled,
                ..
            } => {
                assert!(
                    *enabled,
                    "留下的必须是数组里在前的那条（enabled = true），不是第二条的 false"
                );
                assert_eq!(install_path.as_deref(), Some(Path::new("/nope")));
            }
            other => panic!("{other:?}"),
        }
    }
}
