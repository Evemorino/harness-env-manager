//! kimi-code 的 `mcpServers` 顶层键 merge（spec §9.6 第 3 行）。
//!
//! `~/.kimi-code/mcp.json` 结构简单、没有注释，所以直接 serde_json 读写。
//! 已知代价：serde_json 默认的 `Map` 是 `BTreeMap`，顶层键会被字典序重排。
//! 开 `preserve_order` 会全局改变 `serde_json::Map` 的类型（claude 侧的 MCP
//! 读取也用它，golden 会跟着动），收益只是键序——不值得。别的顶层键一个不少即可。

use crate::apply::write::write_config;
use anyhow::Context as _;
use std::collections::BTreeMap;
use std::path::Path;

/// 把一条 MCP 写进 kimi 的 `mcp.json`，`mcpServers.<name>` 整棵替换。
pub fn upsert_kimi_mcp(
    target: &Path,
    name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
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
    let mut server = serde_json::Map::new();
    server.insert("command".into(), command.into());
    if !args.is_empty() {
        server.insert("args".into(), serde_json::json!(args));
    }
    if !env.is_empty() {
        server.insert("env".into(), serde_json::json!(env));
    }
    servers.insert(name.to_string(), serde_json::Value::Object(server));

    let mut body = serde_json::to_string_pretty(&root)
        .with_context(|| format!("{}: cannot serialize", target.display()))?;
    body.push('\n');
    write_config(target, body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn writes_into_a_fresh_file() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        upsert_kimi_mcp(&p, "context7", "npx", &args2(), &env1()).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
        assert_eq!(v["mcpServers"]["context7"]["args"][0], "-y");
        assert_eq!(
            v["mcpServers"]["context7"]["env"]["K"], "${K}",
            "引用不许展开"
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
        upsert_kimi_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap();

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
        upsert_kimi_mcp(&p, "context7", "npx", &args2(), &env1()).unwrap();
        upsert_kimi_mcp(&p, "context7", "uvx", &[], &BTreeMap::new()).unwrap();

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
        upsert_kimi_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap();

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
        upsert_kimi_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap();

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
        assert!(upsert_kimi_mcp(&p, "x", "npx", &[], &BTreeMap::new()).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// `mcpServers` 不是对象（这里是个字符串）→ 报错，不动文件。
    /// 覆盖会连坐整个 kimi。
    #[test]
    fn a_non_object_mcp_servers_is_refused_and_the_file_is_untouched() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"mcpServers\": \"oops\"}\n");
        let before = std::fs::read(&p).unwrap();

        let err = upsert_kimi_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap_err();
        assert!(format!("{err:#}").contains("mcpServers"), "{err:#}");
        assert_eq!(std::fs::read(&p).unwrap(), before, "拒绝就不能动文件");
        assert!(!p.with_file_name("mcp.json.henv.bak").exists());
    }

    /// 语法坏掉的 mcp.json → 报错，且文件一字节不变（§9.7）。
    /// 错误消息里不许回显文件内容——那里面可能是明文密钥。
    #[test]
    fn a_broken_config_is_refused_and_never_echoed() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(
            ".kimi-code/mcp.json",
            "{\"doc\": \"sk-live-abc\"\n{\"broken\":\n",
        );
        let before = std::fs::read(&p).unwrap();

        let err = upsert_kimi_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("sk-live-abc"),
            "错误消息不许回显配置内容：{msg}"
        );
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// env 的值原样落盘，一个字节都不改——包括看起来像 shell 的东西。
    #[test]
    fn env_values_are_written_verbatim() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        let mut e = BTreeMap::new();
        e.insert("K".to_string(), "${K}".to_string());
        e.insert("W".to_string(), "${A_B1}".to_string());
        upsert_kimi_mcp(&p, "context7", "npx", &[], &e).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["context7"]["env"]["K"], "${K}");
        assert_eq!(v["mcpServers"]["context7"]["env"]["W"], "${A_B1}");
    }
}
