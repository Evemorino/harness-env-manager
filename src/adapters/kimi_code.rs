use crate::adapters::claude_code::{
    expect_object, expect_string, mcp_capability, optional_array, optional_bool, optional_object,
    optional_string, read_json_if_present,
};
use crate::adapters::{HarnessAdapter, KIMI_CODE};
use crate::apply::mcp::{McpMechanism, McpTarget};
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
                // `enabled` 走第七个形状原语 `optional_bool`，**两步分开**：
                // 「缺键 = 未表态 = 启用」（与 claude 的门禁默认、codex 的
                // `toml_optional_bool` 默认同语义），「显式 `null` = 同样没表态」，
                // 「键在但不是布尔」才是故障。三者必须分开——写成一个
                // `.and_then(Value::as_bool).unwrap_or(true)` 就把用户关掉的 plugin
                // 报成已启用，写成一个裸 `expect_bool` 就把合法的 `null` 报成坏文件。
                enabled: optional_bool(
                    record.get("enabled"),
                    &file,
                    &format!("{context} field `enabled`"),
                )?
                .unwrap_or(true),
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
    /// `Adopted`。扫出来的每条都是**现状**（`Origin::Plugin` 表示「随着那个 plugin 装进来的」），
    /// 不是「henv 能往 kimi 写 skill」的证据——kimi 没有用户级 skills 目录，
    /// `supports()` 对这类能力一律 `Unsupported`（§9.5）。
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
                // 比较一律用解析后的真实路径（Task 7 契约 D5）；断链没有内容但有路径，
                // §9.3 的自愈要能看见它——两条都由 `SkillEntry::content_path` 兑现。
                // 先取再搬 `e.name`，顺序反了就是对一个被部分移动的值借方法。
                let path = e.content_path();
                out.push(Capability {
                    id: CapId::new(Kind::Skill, e.name),
                    origin: Origin::Plugin { name: p.id.clone() },
                    spec: Spec::Skill {
                        source: SkillSource::PluginBundle {
                            plugin: p.id.clone(),
                        },
                        path,
                    },
                    targets: Targets::All,
                });
            }
        }
        Ok(out)
    }

    /// 扫 `~/.agents/skills`：kimi 的 `USER_GENERIC_DIRS`（2026-09-28 从 kimi 2.0.2 的二进制
    /// 常量实证；它自带的内置 skill 文案也写着 "Kimi Code already supports `.agents` skills
    /// and AGENTS files by default"）。
    ///
    /// 这是**共享库**，内容的主人是别的工具（`npx skills`、`dws skill setup`…），kimi 只是
    /// 读者之一。所以扫出来的条目是 `Origin::Adopted`——它表达「kimi 现在读得到这一条」，
    /// 不表达「henv 往 kimi 写过什么」。`supports` 对这类条目放行，靠的就是这个来源。
    fn read_shared_skills(&self) -> anyhow::Result<Vec<Capability>> {
        let mut out = Vec::new();
        for e in try_read_skill_entries(&self.home.agents_skills(), &self.home)? {
            let path = e.content_path();
            out.push(Capability {
                id: CapId::new(Kind::Skill, e.name),
                origin: Origin::Adopted {
                    harness: KIMI_CODE.into(),
                },
                // 入口在共享库就是共享库来源，哪怕那一条本身是软链、真身在别家的目录里。
                spec: Spec::Skill {
                    source: SkillSource::AgentsDir,
                    path,
                },
                targets: Targets::All,
            });
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
        // 共享库排在 bundle 之后：同名时 `dedup_by` 保留先扫到的那条，而 kimi 自己那份
        // （随 plugin 装进来的）比共享库的那份更贴近「它实际用的是哪个」。
        caps.extend(self.read_shared_skills()?);
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

    /// kimi-code 没有用户级 skills 目录（§9.1）：它的散装 skill 只能随 plugin
    /// bundle 落地。`supports` 已经对散装 `Kind::Skill` 返回 `Unsupported`，
    /// 这里返回 `None` 是与之一致的事实陈述，不是第二处独立的拒绝判定。
    fn skills_target_dir(&self) -> Option<PathBuf> {
        None
    }

    fn mcp_target(&self) -> Option<McpTarget> {
        Some(McpTarget {
            path: self.home.kimi_mcp(),
            mechanism: McpMechanism::KimiJson,
        })
    }

    /// kimi 原生接纳 MCP 与 Plugin 两种 kind；skill 按**来源**分两种：
    ///
    /// - `SkillSource::AgentsDir` → `Native`：kimi 自己就读 `~/.agents/skills`（它的
    ///   `USER_GENERIC_DIRS`），这条能力此刻就在它眼前，对账结果是 in-sync 而不是
    ///   「交付不了」。
    /// - 其余来源（`local` / `git` / bundle 拆解）→ `Unsupported`：kimi 没有 henv 写得了的
    ///   用户级 skills 目录（`skills_target_dir()` 是 `None`），判 `Native` 会让
    ///   `deliver_skills` 走一遍空交付——报告写着 `+ install` 而盘上什么都没写。
    fn supports(&self, cap: &Capability) -> Support {
        match cap.id.kind {
            Kind::Mcp | Kind::Plugin => Support::Native,
            Kind::Skill => match &cap.spec {
                Spec::Skill {
                    source: SkillSource::AgentsDir,
                    ..
                } => Support::Native,
                _ => Support::Unsupported {
                    reason: "kimi-code 无用户级 skills 目录，拆解出的 skill 在它这儿没有落点"
                        .to_string(),
                },
            },
            Kind::Hook => Support::Unsupported {
                reason: "kimi-code 无用户级 hooks 配置".to_string(),
            },
            Kind::Command => Support::Unsupported {
                reason: "kimi-code 无用户级 commands 目录".to_string(),
            },
            Kind::Agent => Support::Unsupported {
                reason: "kimi-code 无用户级 agents 目录".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, McpTransport, Origin, SkillSource, Spec, Targets};
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
                transport: McpTransport::Stdio {
                    command: "/Applications/KimiCU.app/Contents/MacOS/kimi-cu".into(),
                    args: vec!["mcp".into(), "-s".into(), "user".into()],
                    env: Default::default(),
                },
            }
        );
        match &find(&caps, Kind::Mcp, "ctx").unwrap().spec {
            Spec::Mcp {
                transport: McpTransport::Stdio { env, .. },
                ..
            } => assert_eq!(env["CONTEXT7_API_KEY"].to_text(), "${CONTEXT7_API_KEY}"),
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

    /// `enabled: null` 是合法的「没表态」——不是布尔、也不是坏值。
    /// 今天它走 `expect_bool` 硬错，`read_plugins` 整个上抛：
    /// 写坏一条记录，这家**一个 plugin 都读不出来**——而这条记录恰恰是
    /// `source_harness: kimi-code` 要去定位的那一条。
    #[test]
    fn a_null_enabled_reads_as_unset_not_as_a_broken_record() {
        let t = TempHome::new();
        write_installed(
            &t,
            &installed_doc(r#"{ "id": "p", "root": "/nope", "enabled": null }"#),
        );
        let caps = KimiCode::new(t.home.clone())
            .read_state()
            .expect("null 是「没表态」，不是坏值");

        let plugin = caps
            .iter()
            .find(|c| c.id == CapId::new(Kind::Plugin, "p"))
            .expect("这条 plugin 必须读得出来");
        match &plugin.spec {
            Spec::Plugin { enabled, .. } => {
                assert!(*enabled, "没表态 = 启用，与 claude/codex 同默认")
            }
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

    /// 共享库里的条目**本身是一条软链**时，来源仍记「共享库」。
    ///
    /// 入口在 `~/.agents/skills`，真身在别家目录里（实测：`mcp-builder` 指回
    /// `~/.claude/skills/mcp-builder`、`superpowers` 指向 `~/.codex/superpowers/skills`）。
    /// 按真身判会把这些记成 `Local`，于是 `supports` 判 kimi 交付不了——而它明明是通过那个
    /// 入口读到的，报告就对一条能用的能力说谎（真机上正是这两条被误报成 undeliverable）。
    #[test]
    fn a_shared_entry_that_is_a_symlink_is_still_agents_dir() {
        let t = TempHome::new();
        t.write(".claude/skills/foo/SKILL.md", "# foo")
            .symlink(".agents/skills/foo", "../../.claude/skills/foo");

        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let foo = find(&caps, Kind::Skill, "foo").expect("共享库里的条目要扫得到");
        assert_eq!(
            foo.spec,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
            "入口在共享库就是共享库来源，哪怕真身在 claude 的目录里"
        );
    }

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

    /// 断链是数据不是故障：条目照常出现，来源不可判定（交给 doctor），但 `path` 要给得出
    /// 链接本身——三个 adapter 对同一个字段给同一种答案，`content_path` 的规则才有一处出处。
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
                path: Some(root.join("skills/gone")),
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

    // ============ supports()：kimi 的接纳度矩阵（spec §2.1） ============

    /// `bearerTokenEnvVar` 是 kimi **唯一**的环境取值通道（它固定发
    /// `Authorization: Bearer <token>`）：读回来必须是清单里那个一等写法，否则
    /// 「henv 自己按 bearer 交付的条目」下一轮 diff 会被报成 update。
    #[test]
    fn reads_bearer_token_env_var_as_a_bearer_reference() {
        let t = TempHome::new();
        t.write(
            ".kimi-code/mcp.json",
            r#"{ "mcpServers": { "ctx": {
                "url": "https://mcp.context7.com/mcp",
                "bearerTokenEnvVar": "CONTEXT7_API_KEY"
            } } }"#,
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let got = find(&caps, Kind::Mcp, "ctx").expect("读得到");
        match &got.spec {
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

    /// kimi 原生接纳 MCP 与 Plugin 两种 kind；Skill/Hook/Command/Agent 无用户级目录或
    /// 配置，散装（非 Plugin origin）一律 Unsupported。`Spec::Mcp` 只是占位载荷。
    #[test]
    fn supports_mcp_and_plugin_natively() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());
        for kind in [Kind::Mcp, Kind::Plugin] {
            let cap = Capability::new(
                CapId::new(kind, "x"),
                Spec::Mcp {
                    transport: McpTransport::Stdio {
                        command: "c".into(),
                        args: vec![],
                        env: Default::default(),
                    },
                },
            );
            assert_eq!(a.supports(&cap), Support::Native, "{kind:?}");
        }
    }

    /// 散装（非 Plugin origin）的 Skill/Hook/Command/Agent 四个 kind 全部 Unsupported，
    /// 且 reason 非空、点名 harness 名（报告里的 `!` 行要能自解释）。
    #[test]
    fn refuses_loose_skill_hook_command_agent_with_reason() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());
        for kind in [Kind::Skill, Kind::Hook, Kind::Command, Kind::Agent] {
            let cap = Capability::new(
                CapId::new(kind, "x"),
                Spec::Hook {
                    event: "E".into(),
                    command: "c".into(),
                },
            );
            match a.supports(&cap) {
                Support::Unsupported { reason } => {
                    assert!(
                        !reason.is_empty(),
                        "{kind:?} needs a reason for the `!` line"
                    );
                    assert!(
                        reason.contains("kimi-code"),
                        "{kind:?} reason names the harness: {reason}"
                    );
                }
                other => panic!("{kind:?} expected Unsupported, got {other:?}"),
            }
        }
    }

    /// §9.5 的 degrade 第一处生效点：kimi 作目标时，拆解出的 skill 判 `Unsupported`，
    /// 由 `degrade.skills` 决定进报告还是静默丢弃。
    ///
    /// **判据落在「这家有没有用户级 skills 目录」，不是「它是不是来自 bundle」。**
    /// kimi 没有；于是散装的与 bundle 里拆出来的同判——这正是 B3b 要修的那件事：
    /// 旧行为凭 `Origin::Plugin` 判 `Native`，而 `skills_target_dir()` 是 `None`，
    /// `deliver_skills` 会提前返回空报告，报告写着 `+ install skill/x`、盘上什么都没写。
    #[test]
    fn every_skill_is_refused_because_kimi_has_no_user_level_skills_dir() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());

        let loose = Capability::new(
            CapId::new(Kind::Skill, "my-draft"),
            Spec::Skill {
                source: SkillSource::Local {
                    path: PathBuf::from("/x"),
                },
                path: None,
            },
        );
        assert!(matches!(a.supports(&loose), Support::Unsupported { .. }));

        // 同一个 kind、来自 bundle 的 skill：**同样 Unsupported**。
        let bundled = Capability {
            id: CapId::new(Kind::Skill, "brainstorming"),
            origin: Origin::Plugin {
                name: "superpowers".into(),
            },
            spec: Spec::Skill {
                source: SkillSource::PluginBundle {
                    plugin: "superpowers".into(),
                },
                path: None,
            },
            targets: Targets::All,
        };
        match a.supports(&bundled) {
            Support::Unsupported { reason } => assert!(
                reason.contains("kimi-code") && reason.contains("skills"),
                "理由要能自解释，且指的方向是「没有落点」：{reason}"
            ),
            other => panic!("bundle 里的 skill 也必须 Unsupported，得到 {other:?}"),
        }
        assert!(
            a.skills_target_dir().is_none(),
            "判据的依据就在这里：这家没有用户级 skills 目录"
        );
    }

    /// 同一条道理推到 Hook：`Origin::Plugin` 不授予任何特权。
    /// `skills_target_dir()` 那条注释（`src/adapters/mod.rs`）说 kimi 的散装 skill
    /// 「只能随 plugin bundle 分发」——那是**关于 kimi 怎么装东西**的事实，
    /// 不是**关于 henv 能不能往里写**的事实。henv 不装原生 plugin（§9.1）。
    #[test]
    fn a_bundled_hook_is_refused_too() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());
        let bundled_hook = Capability {
            id: CapId::new(Kind::Hook, "superpowers:session-start"),
            origin: Origin::Plugin {
                name: "superpowers".into(),
            },
            spec: Spec::Hook {
                event: "SessionStart".into(),
                command: "run-hook".into(),
            },
            targets: Targets::All,
        };
        assert!(matches!(
            a.supports(&bundled_hook),
            Support::Unsupported { .. }
        ));
    }
}
