//! codex 的 `[mcp_servers]` 子树 merge（spec §9.6 第 2 行）。
//!
//! 用 `toml_edit` 而不是 `toml`：`~/.codex/config.toml` 是**用户手写的**，
//! 里面有注释、格式和别的键。反序列化再序列化会把那些全丢掉——那是在改用户的
//! 文件，不是加一个键。§9.12 解禁 `toml_edit` 就是为了这一处。

use crate::apply::write::write_config;
use anyhow::Context as _;
use std::collections::BTreeMap;
use std::path::Path;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

/// 把一条 MCP 写进 codex 的 `config.toml`，整棵 `[mcp_servers.<name>]` 子树替换。
///
/// **整棵替换而不是逐键 merge**：清单改了一版之后，上一版多出来的 `args` / `env`
/// 会留在新配置里，用户得到一份混着两版设置的配置。
///
/// 失败一律**不动文件**（`write_config` 的 rename 保证）：读不出来、解不开、
/// 类型不对，都在这儿停下。
pub fn upsert_codex_mcp(
    target: &Path,
    name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let existing = match std::fs::read_to_string(target) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("{}: cannot read", target.display()));
        }
    };
    // **绝不把文件内容放进错误消息**：config.toml 里可能有 token（`experimental_bearer_token`）。
    let mut doc: DocumentMut = existing
        .parse()
        .with_context(|| format!("{}: not valid TOML, refusing to touch it", target.display()))?;

    let item = doc
        .entry("mcp_servers")
        .or_insert(Item::Table(Table::new()));
    let mut slots = Slots::of(item, target)?;
    slots.replace(name, command, args, env);

    write_config(target, doc.to_string().as_bytes())
}

/// `mcp_servers` 的两种合法拼法。
///
/// **两种都要认**：`toml` crate 把 `mcp_servers = { ... }` 读成 `Value::Table`
/// （没有独立的 InlineTable 变体），所以读路径认它、diff 会说「该装」。
/// 写路径只认一种的话，用户就撞上「diff 让装、apply 说写不了」。
enum Slots<'a> {
    Block(&'a mut Table),
    Inline(&'a mut InlineTable),
}

impl<'a> Slots<'a> {
    fn of<'i>(item: &'i mut Item, target: &Path) -> anyhow::Result<Slots<'i>> {
        match item {
            Item::Table(t) => Ok(Slots::Block(t)),
            Item::Value(Value::InlineTable(t)) => Ok(Slots::Inline(t)),
            other => Err(anyhow::anyhow!(
                "{}: `mcp_servers` must be a table, found {}",
                target.display(),
                other.type_name()
            )),
        }
    }

    fn replace(
        &mut self,
        name: &str,
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) {
        match self {
            // 块表可以装子表，env 因此能写成 `[mcp_servers.<n>.env]`，
            // 与 codex 自己写出来的形状一致。
            Slots::Block(t) => {
                let mut server = Table::new();
                server.insert("command", toml_edit::value(command));
                if !args.is_empty() {
                    let mut arr = toml_edit::Array::new();
                    for a in args {
                        arr.push(a.as_str());
                    }
                    server.insert("args", toml_edit::value(arr));
                }
                if !env.is_empty() {
                    let mut e = Table::new();
                    for (k, v) in env {
                        e.insert(k, toml_edit::value(v.as_str()));
                    }
                    server.insert("env", Item::Table(e));
                }
                t.insert(name, Item::Table(server));
            }
            // 内联表里只能放内联值——放 `Item::Table` 会产出非法 TOML。
            Slots::Inline(t) => {
                let mut server = InlineTable::new();
                server.insert("command", command.into());
                if !args.is_empty() {
                    let mut arr = toml_edit::Array::new();
                    for a in args {
                        arr.push(a.as_str());
                    }
                    server.insert("args", Value::Array(arr));
                }
                if !env.is_empty() {
                    let mut e = InlineTable::new();
                    for (k, v) in env {
                        e.insert(k, v.as_str().into());
                    }
                    server.insert("env", Value::InlineTable(e));
                }
                t.insert(name, Value::InlineTable(server));
            }
        }
    }
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
        let p = t.path(".codex/config.toml");
        upsert_codex_mcp(&p, "context7", "npx", &args2(), &env1()).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        let s = &doc["mcp_servers"]["context7"];
        assert_eq!(s["command"].as_str(), Some("npx"));
        assert_eq!(s["args"][0].as_str(), Some("-y"));
        assert_eq!(s["env"]["K"].as_str(), Some("${K}"), "引用不许展开");
    }

    /// §9.10 的 merge-safe：用户文件里**别的**东西一个字节都不许变。
    /// 这条 fixture 里的三样（注释、顶层标量、带点的项目名）正是 `toml` 往返会丢的。
    #[test]
    fn existing_bytes_and_comments_survive() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(
            ".codex/config.toml",
            "# 我的 codex 配置\nexperimental_bearer_token = \"x\"\n\n\
             [projects.\"/Users/nava/thing\"]\ntrust_level = \"trusted\"\n",
        );
        upsert_codex_mcp(&p, "context7", "npx", &args2(), &env1()).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# 我的 codex 配置"), "注释丢了：\n{text}");
        assert!(text.contains("experimental_bearer_token = \"x\""), "{text}");
        assert!(text.contains("[projects.\"/Users/nava/thing\"]"), "{text}");
        assert!(text.contains("trust_level = \"trusted\""), "{text}");
        assert!(text.contains("mcp_servers"), "{text}");
    }

    /// 已经在里面的服务器：整棵子树替换，**旧版本多出来的键要消失**。
    /// 逐键 merge 会让上一版配的 `args` 留在新版里（用户改了清单，配置却混着两版）。
    #[test]
    fn replacing_a_server_drops_stale_keys() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        upsert_codex_mcp(&p, "context7", "npx", &args2(), &env1()).unwrap();
        upsert_codex_mcp(&p, "context7", "uvx", &[], &BTreeMap::new()).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            doc["mcp_servers"]["context7"]["command"].as_str(),
            Some("uvx")
        );
        assert!(
            doc["mcp_servers"]["context7"].get("args").is_none(),
            "{text}"
        );
        assert!(
            doc["mcp_servers"]["context7"].get("env").is_none(),
            "{text}"
        );
    }

    /// 别的服务器不受影响。
    #[test]
    fn other_servers_are_left_alone() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(
            ".codex/config.toml",
            "[mcp_servers.user-own]\ncommand = \"their-binary\"\nargs = [\"--flag\"]\n",
        );
        upsert_codex_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            doc["mcp_servers"]["user-own"]["command"].as_str(),
            Some("their-binary")
        );
        assert_eq!(
            doc["mcp_servers"]["context7"]["command"].as_str(),
            Some("npx")
        );
    }

    /// **读路径认的拼法，写路径也必须认。** `toml` crate 把 `mcp_servers = { ... }`
    /// 读成 `Value::Table`（没有独立的 InlineTable 变体），所以 diff 会说「该装」；
    /// 若写路径只处理 `Item::Table`，用户就撞上「diff 让装、apply 说写不了」。
    #[test]
    fn an_inline_mcp_servers_table_is_updated_in_place() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(
            ".codex/config.toml",
            "keep = 1\nmcp_servers = { user-own = { command = \"x\" } }\n",
        );
        upsert_codex_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc["keep"].as_integer(), Some(1));
        assert_eq!(
            doc["mcp_servers"]["user-own"]["command"].as_str(),
            Some("x"),
            "内联表里别人那条要留着"
        );
        assert_eq!(
            doc["mcp_servers"]["context7"]["command"].as_str(),
            Some("npx")
        );
    }

    /// 名字里有点、有空格、有引号：TOML 要引号，`toml_edit` 会自己加（实测过）。
    /// 清单的 `every_capability_key_is_a_single_path_component` 拦的是 `/` `\`，
    /// 点号与引号是**合法**的 MCP 名，不能在这里翻车。
    #[test]
    fn names_with_dots_spaces_and_quotes_round_trip() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        for name in ["with.dot", "with space", "a\"b", "c'd"] {
            upsert_codex_mcp(&p, name, "npx", &[], &BTreeMap::new()).unwrap();
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        for name in ["with.dot", "with space", "a\"b", "c'd"] {
            assert_eq!(
                doc["mcp_servers"][name]["command"].as_str(),
                Some("npx"),
                "{name} 没进去：\n{text}"
            );
        }
    }

    /// 类型不对 → 报错，且**文件一字节不变**。覆盖会连坐整个 codex。
    #[test]
    fn a_non_table_mcp_servers_is_refused_and_the_file_is_untouched() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "mcp_servers = \"oops\"\n");
        let before = std::fs::read(&p).unwrap();

        let err = upsert_codex_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap_err();
        assert!(format!("{err:#}").contains("mcp_servers"), "{err:#}");
        assert_eq!(std::fs::read(&p).unwrap(), before, "拒绝就不能动文件");
        assert!(!p.with_file_name("config.toml.henv.bak").exists());
    }

    /// 语法坏掉的 config.toml → 报错，且文件一字节不变（§9.7）。
    /// 错误消息里不许回显文件内容——那里面可能有 token。
    #[test]
    fn a_broken_config_is_refused_and_never_echoed() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(
            ".codex/config.toml",
            "secret-token = \"sk-live-abc\"\n[[[broken\n",
        );
        let before = std::fs::read(&p).unwrap();

        let err = upsert_codex_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap_err();
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
        let p = t.path(".codex/config.toml");
        let mut e = BTreeMap::new();
        e.insert("K".to_string(), "${K}".to_string());
        e.insert("W".to_string(), "${A_B1}".to_string());
        upsert_codex_mcp(&p, "context7", "npx", &[], &e).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            doc["mcp_servers"]["context7"]["env"]["K"].as_str(),
            Some("${K}")
        );
        assert_eq!(
            doc["mcp_servers"]["context7"]["env"]["W"].as_str(),
            Some("${A_B1}")
        );
    }
}
