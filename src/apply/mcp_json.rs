//! kimi-code 的 `mcpServers` 顶层键 merge（spec §9.6 第 3 行）。
//!
//! `~/.kimi-code/mcp.json` 结构简单、没有注释，所以直接 serde_json 读写。
//! 已知代价：serde_json 默认的 `Map` 是 `BTreeMap`，顶层键会被字典序重排。
//! 开 `preserve_order` 会全局改变 `serde_json::Map` 的类型（claude 侧的 MCP
//! 读取也用它，golden 会跟着动），收益只是键序——不值得。别的顶层键一个不少即可。

use crate::apply::write::write_config;
use crate::capability::McpTransport;
use anyhow::Context as _;
use std::path::Path;

/// 把一条 MCP 写进 kimi 的 `mcp.json`，`mcpServers.<name>` 整棵替换。
pub fn upsert_kimi_mcp(target: &Path, name: &str, transport: &McpTransport) -> anyhow::Result<()> {
    let existing = match std::fs::read_to_string(target) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("{}: cannot read", target.display())),
    };
    // 绝不把内容放进错误消息：mcp.json 里可能有明文密钥（值是 adapter 读回来的活配置）。
    let mut root: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&existing).with_context(|| {
            format!("{}: not valid JSON, refusing to touch it", target.display())
        })?
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: top level must be an object", target.display()))?;

    // 缺失与显式 `null` 都是「还没有」——读路径也这么认（§9.6 第 3 行）。
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if servers.is_null() {
        *servers = serde_json::json!({});
    }
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: `mcpServers` must be an object", target.display()))?;

    // 整棵替换：上一版多出来的 args / env 不许留在新配置里。
    //
    // **凭据字段一个都不写**（2026-09-28 实测）：kimi 的 `env` / `headers` 是字面量
    // （`mergeStdioEnv` 直接 `Object.assign`、`buildMcpRemoteHeaders` 直接
    // `{ ...config.headers }`），把 `${VAR}` 写进去它会把这段文本原样发出去。它唯一
    // 的环境取值通道 `bearerTokenEnvVar` 固定发 `Bearer <token>`，与清单里
    // `Authorization: ${VAR}` 这种裸值不是一回事——所以带凭据的条目在 kimi 上是
    // `undeliverable`（`mcp_expressible` 判的），走到这里还带着凭据就是计划错了。
    let mut server = serde_json::Map::new();
    match transport {
        McpTransport::Stdio { command, args, env } => {
            if !env.is_empty() {
                anyhow::bail!(
                    "kimi-code cannot express MCP env values; refusing to write them literally"
                );
            }
            server.insert("command".into(), command.as_str().into());
            if !args.is_empty() {
                server.insert("args".into(), serde_json::json!(args));
            }
        }
        // kimi 的远端 MCP 没有 `type` 字段（实测它的配置里只有 url/headers——
        // 与 claude 那份不同，那边靠 `type` 区分传输）。
        //
        // 凭据只有一条通道：`bearerTokenEnvVar`（kimi 固定发 `Bearer <token>`），
        // 所以只有 `Authorization: Bearer ${VAR}` 这一种写法能忠实落地；其余写法计划侧
        // 已经判成 undeliverable，走到这里还带着就是计划错了——宁可失败。
        McpTransport::Http { url, headers } => {
            for (header, value) in headers {
                let Some(var) = crate::secrets::parse_bearer_var(value) else {
                    anyhow::bail!(
                        "kimi-code cannot express this MCP header; refusing to write it literally"
                    );
                };
                if !header.eq_ignore_ascii_case("authorization") {
                    anyhow::bail!(
                        "kimi-code's bearer channel only fills `Authorization`; refusing to drop `{header}`"
                    );
                }
                server.insert("bearerTokenEnvVar".into(), var.into());
            }
            server.insert("url".into(), url.as_str().into());
        }
    }
    servers.insert(name.to_string(), serde_json::Value::Object(server));

    let mut body = serde_json::to_string_pretty(&root)
        .with_context(|| format!("{}: cannot serialize", target.display()))?;
    body.push('\n');
    write_config(target, body.as_bytes())
}

/// 从 kimi 的 `mcp.json` 里摘掉 `mcpServers.<name>`（`upsert_kimi_mcp` 的逆操作，§9.8）。
///
/// **键不在就不写**：`--prune` 重跑是正常路径，重写一份内容没变的文件只会平白
/// 制造一次 mtime 变动、一份 `.bak` 和一串「文件被动过」的疑问。
pub fn remove_kimi_mcp(target: &Path, name: &str) -> anyhow::Result<()> {
    let existing = match std::fs::read_to_string(target) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("{}: cannot read", target.display())),
    };
    if existing.trim().is_empty() {
        return Ok(());
    }
    // 绝不把内容放进错误消息：mcp.json 里可能有明文密钥（值是 adapter 读回来的活配置）。
    let mut root: serde_json::Value = serde_json::from_str(&existing)
        .with_context(|| format!("{}: not valid JSON, refusing to touch it", target.display()))?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{}: top level must be an object", target.display()))?;

    // 缺失与显式 `null` 都是「还没有」——不落盘。其余形状报错，理由同 `upsert_kimi_mcp`：
    // 拒绝写入的形状也必须拒绝「宣称删掉了」，否则账本会被摘掉而文件根本没被碰过。
    let servers = match obj.get_mut("mcpServers") {
        None | Some(serde_json::Value::Null) => return Ok(()),
        Some(serde_json::Value::Object(servers)) => servers,
        Some(_) => anyhow::bail!("{}: `mcpServers` must be an object", target.display()),
    };
    if servers.remove(name).is_none() {
        return Ok(());
    }

    let mut body = serde_json::to_string_pretty(&root)
        .with_context(|| format!("{}: cannot serialize", target.display()))?;
    body.push('\n');
    write_config(target, body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试里构造一条 stdio transport——调用点都只需要这一种形态。
    fn stdio_t(command: &str, args: &[String], env: &BTreeMap<String, String>) -> McpTransport {
        McpTransport::Stdio {
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
        }
    }
    use crate::testutil::TempHome;
    use std::collections::BTreeMap;

    fn env1() -> BTreeMap<String, String> {
        let mut e = BTreeMap::new();
        e.insert("K".to_string(), "${K}".to_string());
        e
    }

    fn args2() -> Vec<String> {
        vec!["-y".to_string(), "@upstash/context7-mcp".to_string()]
    }

    /// http 型写出来的是 `url`，**没有 `command`**——整棵替换的语义要覆盖到换形态这一层
    /// （上一版是 stdio，这一版是 http，旧字段不许留下）。
    #[test]
    fn writes_an_http_server_and_drops_the_stdio_fields() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        upsert_kimi_mcp(&p, "ctx", &stdio_t("npx", &args2(), &BTreeMap::new())).unwrap();

        upsert_kimi_mcp(
            &p,
            "ctx",
            &McpTransport::Http {
                url: "https://mcp.context7.com/mcp".to_string(),
                headers: BTreeMap::new(),
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let s = &v["mcpServers"]["ctx"];
        assert_eq!(s["url"], "https://mcp.context7.com/mcp");
        assert!(
            s["command"].is_null(),
            "换成 http 后旧 command 不许留下：{text}"
        );
        assert!(s["args"].is_null(), "换成 http 后旧 args 不许留下：{text}");
        assert!(
            s.get("headers").is_none(),
            "没有请求头就不该写这个键：{text}"
        );
    }

    /// **带凭据的条目在 kimi 上写不出来**（2026-09-28 实测：它的 `env` / `headers` 是
    /// 字面量，会把 `${VAR}` 原样发出去）。写器宁可失败，也不写一个假的。
    #[test]
    fn credential_values_are_refused_instead_of_written_literally() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"keep\": 1}\n");
        let before = std::fs::read(&p).unwrap();

        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "${K}".to_string());
        let err = upsert_kimi_mcp(
            &p,
            "ctx",
            &McpTransport::Http {
                url: "https://mcp.context7.com/mcp".to_string(),
                headers,
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("cannot express"), "{err:#}");

        let err = upsert_kimi_mcp(&p, "ctx", &stdio_t("npx", &[], &env1())).unwrap_err();
        assert!(format!("{err:#}").contains("cannot express"), "{err:#}");

        assert_eq!(std::fs::read(&p).unwrap(), before, "拒绝就不能动文件");
    }

    /// **唯一的例外是 bearer 形态**：kimi 自己也只有那一条环境通道（`bearerTokenEnvVar`，
    /// 固定发 `Authorization: Bearer <token>`），所以 `Authorization: Bearer ${VAR}`
    /// 能忠实落地——kimi 上因此不再是「只要带凭据就交付不了」。
    #[test]
    fn the_bearer_form_becomes_bearer_token_env_var() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer ${K}".to_string());
        upsert_kimi_mcp(
            &p,
            "ctx",
            &McpTransport::Http {
                url: "https://mcp.context7.com/mcp".to_string(),
                headers,
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let s = &v["mcpServers"]["ctx"];
        assert_eq!(s["url"], "https://mcp.context7.com/mcp");
        assert_eq!(s["bearerTokenEnvVar"], "K", "{text}");
        assert!(
            s.get("headers").is_none(),
            "凭据不许以字面量形式落盘：{text}"
        );
    }

    #[test]
    fn writes_into_a_fresh_file() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &args2(), &BTreeMap::new())).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
        assert_eq!(v["mcpServers"]["context7"]["args"][0], "-y");
        assert_eq!(
            v["mcpServers"]["context7"].get("env"),
            None,
            "kimi 写不出 env 取值，空 env 就不该有这个键"
        );
    }

    /// 点名的一条已知代价：顶层键会被字典序重排。这条测试不是「钉住这个行为」，
    /// 是让将来某天有人改了它时**知道自己在改什么**——两组键都要在。
    #[test]
    fn other_top_level_keys_survive_regardless_of_order() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\"zeta\": 1, \"alpha\": {\"a\": true}}\n",
        );
        upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["zeta"], 1);
        assert_eq!(v["alpha"]["a"], true);
        assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
    }

    /// 整棵子树替换，**旧版本多出来的键要消失**。逐键 merge 会让上一版配的
    /// `args` / `env` 留在新版里（用户改了清单，配置却混着两版）。
    #[test]
    fn replacing_a_server_drops_stale_keys() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &args2(), &BTreeMap::new())).unwrap();
        upsert_kimi_mcp(&p, "context7", &stdio_t("uvx", &[], &BTreeMap::new())).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["context7"]["command"], "uvx");
        assert!(v["mcpServers"]["context7"].get("args").is_none());
        assert!(v["mcpServers"]["context7"].get("env").is_none());
    }

    /// 别的服务器不受影响。
    #[test]
    fn other_servers_are_left_alone() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\"mcpServers\": {\"user-own\": {\"command\": \"their-binary\"}}}\n",
        );
        upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["user-own"]["command"], "their-binary");
        assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
    }

    /// 显式 `null` 是**正常状态**不是故障（读路径也这么认）：把它换成对象。
    #[test]
    fn an_explicit_null_mcp_servers_is_replaced_not_refused() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\"mcpServers\": null, \"keep\": 1}\n",
        );
        upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["keep"], 1);
        assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
    }

    /// 顶层不是对象 → 报错，不动文件。
    #[test]
    fn a_non_object_top_level_is_refused() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "[1, 2, 3]\n");
        let before = std::fs::read(&p).unwrap();
        assert!(upsert_kimi_mcp(&p, "x", &stdio_t("npx", &[], &BTreeMap::new())).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// `mcpServers` 不是对象（这里是个字符串）→ 报错，不动文件。
    /// 覆盖会连坐整个 kimi。
    #[test]
    fn removing_refuses_a_non_object_mcp_servers_without_touching_the_file() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"mcpServers\": \"oops\"}\n");
        let before = std::fs::read(&p).unwrap();

        let err =
            upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap_err();
        assert!(format!("{err:#}").contains("mcpServers"), "{err:#}");
        assert_eq!(std::fs::read(&p).unwrap(), before, "拒绝就不能动文件");
        assert!(!p.with_file_name("mcp.json.henv.bak").exists());
    }

    /// 语法坏掉的 mcp.json → 报错，且文件一字节不变（§9.7）。
    /// 错误消息里不许回显文件内容——那里面可能是明文密钥。
    #[test]
    fn removing_from_a_broken_config_is_refused_and_never_echoed() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\"doc\": \"sk-live-abc\"\n{\"broken\":\n",
        );
        let before = std::fs::read(&p).unwrap();

        let err =
            upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("sk-live-abc"),
            "错误消息不许回显配置内容：{msg}"
        );
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// Review Focus 5：键不在就**一个字节都不动**，也不落 `.bak`。
    /// 夹具里有意留了另一个 server 与一个顶层键——「不动」是逐字节的。
    #[test]
    fn removing_a_missing_key_leaves_the_file_untouched() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\n  \"keep\": 1,\n  \"mcpServers\": {\n    \"user-own\": {\n      \"command\": \"their-binary\"\n    }\n  }\n}\n",
        );
        let before = std::fs::read(&p).unwrap();

        remove_kimi_mcp(&p, "context7").unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), before, "空操作不许重写文件");
        assert!(
            !p.with_file_name("mcp.json.henv.bak").exists(),
            "空操作不许落备份"
        );
    }

    /// 文件不存在、与「这个键从来没写下过」是同一件事：什么都不做，也什么都不建。
    #[test]
    fn removing_from_a_missing_file_creates_nothing() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        remove_kimi_mcp(&p, "context7").unwrap();
        assert!(!p.exists(), "撤销不是创建");
        assert!(!p.parent().unwrap().exists(), "连父目录都不该建");
    }

    /// 真删：其余 server 与顶层键都留着，只有那一条消失。
    #[test]
    fn removing_drops_only_the_named_server() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"keep\": 1}\n");
        upsert_kimi_mcp(
            &p,
            "user-own",
            &stdio_t("their-binary", &[], &BTreeMap::new()),
        )
        .unwrap();
        upsert_kimi_mcp(&p, "context7", &stdio_t("npx", &args2(), &BTreeMap::new())).unwrap();

        remove_kimi_mcp(&p, "context7").unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"].get("context7").is_none());
        assert_eq!(v["mcpServers"]["user-own"]["command"], "their-binary");
        assert_eq!(v["keep"], 1);
    }

    /// 显式 `null` 是「还没有」不是故障（读路径也这么认）→ 不落盘。
    #[test]
    fn an_explicit_null_mcp_servers_is_a_no_op() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\"mcpServers\": null, \"keep\": 1}\n",
        );
        let before = std::fs::read(&p).unwrap();

        remove_kimi_mcp(&p, "context7").unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// `mcpServers` 不是对象 → 报错、不动文件。与 `upsert_kimi_mcp` 同一条纪律：
    /// **拒绝写入的形状也必须拒绝「宣称删掉了」**，否则账本会被摘掉而文件没被碰过。
    #[test]
    fn a_non_object_mcp_servers_is_refused_and_the_file_is_untouched() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"mcpServers\": \"oops\"}\n");
        let before = std::fs::read(&p).unwrap();

        let err = remove_kimi_mcp(&p, "context7").unwrap_err();
        assert!(format!("{err:#}").contains("mcpServers"), "{err:#}");
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// 语法坏掉的 mcp.json → 报错、文件一字节不变，且**不回显内容**（可能有明文密钥）。
    #[test]
    fn a_broken_config_is_refused_and_never_echoed() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\"doc\": \"sk-live-abc\"\n{\"broken\":\n",
        );
        let before = std::fs::read(&p).unwrap();

        let err = remove_kimi_mcp(&p, "context7").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("sk-live-abc"),
            "错误消息不许回显配置内容：{msg}"
        );
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }
}
