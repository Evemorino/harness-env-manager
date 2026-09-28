//! codex 的 `[mcp_servers]` 子树 merge（spec §9.6 第 2 行）。
//!
//! 用 `toml_edit` 而不是 `toml`：`~/.codex/config.toml` 是**用户手写的**，
//! 里面有注释、格式和别的键。反序列化再序列化会把那些全丢掉——那是在改用户的
//! 文件，不是加一个键。§9.12 解禁 `toml_edit` 就是为了这一处。

use crate::apply::write::write_config;
use crate::capability::McpTransport;
use anyhow::Context as _;
use std::path::Path;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

/// 把一条 MCP 写进 codex 的 `config.toml`，整棵 `[mcp_servers.<name>]` 子树替换。
///
/// **整棵替换而不是逐键 merge**：清单改了一版之后，上一版多出来的 `args` / `env`
/// 会留在新配置里，用户得到一份混着两版设置的配置。
///
/// **凭据写成 codex 的取值字段，不写成 `${VAR}` 文本**（2026-09-28 实测）：codex 的
/// `env` / `http_headers` 是**字面量**，把引用写进去它会原样发出去；从环境取值它另有
/// `env_vars`（同名转发）与 `env_http_headers`（头名 → 变量名）。写成哪一个是
/// [`crate::adapters::mcp_expressible`] 判的，这里只负责拼对。
///
/// 失败一律**不动文件**（`write_config` 的 rename 保证）：读不出来、解不开、
/// 类型不对，都在这儿停下。
pub fn upsert_codex_mcp(target: &Path, name: &str, transport: &McpTransport) -> anyhow::Result<()> {
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
    slots.replace(name, transport)?;

    write_config(target, doc.to_string().as_bytes())
}

/// 清单里的 `${VAR}` → 那个**变量名**。codex 的取值字段收的是变量名，不是引用文本。
///
/// 不是引用就报错而不是照抄：照抄的后果是 codex 把 `${VAR}` 这段文本当值发出去，
/// 而那正是这次要根治的静默失败。计划侧（`mcp_expressible`）本来就不该放它过来，
/// 这里是第二道闸——写不忠实，宁可失败。
fn ref_var(value: &str) -> anyhow::Result<&str> {
    crate::secrets::parse_ref(value).ok_or_else(|| {
        anyhow::anyhow!("value is not a bare ${{VAR}} reference; refusing to write it")
    })
}

/// 一条 stdio `env` 项 → codex `env_vars` 里的那个变量名。
///
/// codex 的 `env_vars` 是**同名转发**（子进程按原名拿到环境变量）：清单里 `K: ${K}`
/// 正是它，`K: ${OTHER}` 在它这儿没有写法——那要求把环境变量换个名字塞给子进程，
/// 而 `env` 只能放字面量。计划侧已经判过一遍，这里是第二道闸。
fn forwarded_var<'a>(key: &str, value: &'a str) -> anyhow::Result<&'a str> {
    let var = ref_var(value)?;
    if var != key {
        anyhow::bail!("codex forwards an env var under its own name only (`{key}` <- `{var}`)");
    }
    Ok(var)
}

/// 从 codex 的 `config.toml` 里摘掉 `[mcp_servers.<name>]`（`upsert_codex_mcp` 的逆操作，§9.8）。
///
/// **键不在就不写**（同 `remove_kimi_mcp`）：`write_config` 会备份并 rename，
/// 不该为一次什么都没删的操作动用户的文件。
pub fn remove_codex_mcp(target: &Path, name: &str) -> anyhow::Result<()> {
    let existing = match std::fs::read_to_string(target) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("{}: cannot read", target.display())),
    };
    if existing.trim().is_empty() {
        return Ok(());
    }
    // **绝不把文件内容放进错误消息**：config.toml 里可能有 token（`experimental_bearer_token`）。
    let mut doc: DocumentMut = existing
        .parse()
        .with_context(|| format!("{}: not valid TOML, refusing to touch it", target.display()))?;

    // 借用收在块里：`slots` 活着的时候 `doc` 是被借出的，最后那次 `to_string` 需要它。
    let removed = {
        let Some(item) = doc.get_mut("mcp_servers") else {
            return Ok(());
        };
        let mut slots = Slots::of(item, target)?;
        slots.remove(name)
    };
    if !removed {
        return Ok(());
    }
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

    fn replace(&mut self, name: &str, transport: &McpTransport) -> anyhow::Result<()> {
        match self {
            // 块表可以装子表，env 因此能写成 `[mcp_servers.<n>.env]`，
            // 与 codex 自己写出来的形状一致。
            Slots::Block(t) => {
                let mut server = Table::new();
                match transport {
                    McpTransport::Stdio { command, args, env } => {
                        server.insert("command", toml_edit::value(command));
                        if !args.is_empty() {
                            let mut arr = toml_edit::Array::new();
                            for a in args {
                                arr.push(a.as_str());
                            }
                            server.insert("args", toml_edit::value(arr));
                        }
                        if !env.is_empty() {
                            let mut vars = toml_edit::Array::new();
                            for (k, v) in env {
                                vars.push(forwarded_var(k, v)?);
                            }
                            server.insert("env_vars", toml_edit::value(vars));
                        }
                    }
                    // 远端 MCP 的凭据有两条通道：
                    // - `Authorization: Bearer ${VAR}` → `bearer_token_env_var`（codex 那条
                    //   bearer 通道，只作用于 Authorization）；
                    // - 其余引用 → `env_http_headers`（头名 → 变量名）。
                    // 标量先插、子表后插：toml_edit 按插入顺序输出，而 TOML 里标量不能
                    // 出现在子表头之后。
                    McpTransport::Http { url, headers } => {
                        let mut bearer: Option<&str> = None;
                        let mut env_headers = Table::new();
                        for (header, value) in headers {
                            if let Some(var) = crate::secrets::parse_bearer_var(value) {
                                if !header.eq_ignore_ascii_case("authorization") {
                                    anyhow::bail!(
                                        "codex's bearer channel only fills `Authorization` \
                                         (`{header}` has no bearer form)"
                                    );
                                }
                                bearer = Some(var);
                                continue;
                            }
                            env_headers.insert(header, toml_edit::value(ref_var(value)?));
                        }
                        server.insert("url", toml_edit::value(url));
                        if let Some(var) = bearer {
                            server.insert("bearer_token_env_var", toml_edit::value(var));
                        }
                        if !env_headers.is_empty() {
                            server.insert("env_http_headers", Item::Table(env_headers));
                        }
                    }
                }
                t.insert(name, Item::Table(server));
            }
            // 内联表里只能放内联值——放 `Item::Table` 会产出非法 TOML。
            Slots::Inline(t) => {
                let mut server = InlineTable::new();
                match transport {
                    McpTransport::Stdio { command, args, env } => {
                        server.insert("command", command.into());
                        if !args.is_empty() {
                            let mut arr = toml_edit::Array::new();
                            for a in args {
                                arr.push(a.as_str());
                            }
                            server.insert("args", Value::Array(arr));
                        }
                        if !env.is_empty() {
                            let mut vars = toml_edit::Array::new();
                            for (k, v) in env {
                                vars.push(forwarded_var(k, v)?);
                            }
                            server.insert("env_vars", Value::Array(vars));
                        }
                    }
                    McpTransport::Http { url, headers } => {
                        let mut bearer: Option<&str> = None;
                        let mut env_headers = InlineTable::new();
                        for (header, value) in headers {
                            if let Some(var) = crate::secrets::parse_bearer_var(value) {
                                if !header.eq_ignore_ascii_case("authorization") {
                                    anyhow::bail!(
                                        "codex's bearer channel only fills `Authorization` \
                                         (`{header}` has no bearer form)"
                                    );
                                }
                                bearer = Some(var);
                                continue;
                            }
                            env_headers.insert(header, ref_var(value)?.into());
                        }
                        server.insert("url", url.into());
                        if let Some(var) = bearer {
                            server.insert("bearer_token_env_var", var.into());
                        }
                        if !env_headers.is_empty() {
                            server.insert("env_http_headers", Value::InlineTable(env_headers));
                        }
                    }
                }
                t.insert(name, Value::InlineTable(server));
            }
        }
        Ok(())
    }

    /// 摘掉一条 MCP 子树，返回是否真的摘到了（§9.8 的撤销原语）。
    ///
    /// **摘不到不是错误**：键本来就不在时返回 `false`，由调用方决定「不落盘」。
    /// 块表与内联表都要认——读路径两种都认（`Slots::of` 的理由），
    /// 一种认一种不认会让「diff 说该删、apply 说删不了」。
    fn remove(&mut self, name: &str) -> bool {
        match self {
            Slots::Block(t) => t.remove(name).is_some(),
            Slots::Inline(t) => t.remove(name).is_some(),
        }
    }
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

    /// 远端 MCP 的凭据走 `env_http_headers`（头名 → 变量名），**不是** `http_headers`：
    /// 后者是字面量，写进去 codex 会把 `${K}` 原样发出去（2026-09-28 实测，判据见
    /// `adapters::mcp_expressible`）。
    #[test]
    fn writes_an_http_server_with_url_and_env_http_headers() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        upsert_codex_mcp(&p, "ctx", &stdio_t("npx", &args2(), &BTreeMap::new())).unwrap();

        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "${K}".to_string());
        upsert_codex_mcp(
            &p,
            "ctx",
            &McpTransport::Http {
                url: "https://mcp.context7.com/mcp".to_string(),
                headers,
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("url = \"https://mcp.context7.com/mcp\""),
            "{text}"
        );
        assert!(
            text.contains("[mcp_servers.ctx.env_http_headers]"),
            "凭据要走 codex 的取值表：{text}"
        );
        assert!(
            text.contains("Authorization = \"K\""),
            "表里放的是变量名，不是引用文本：{text}"
        );
        assert!(
            !text.contains("[mcp_servers.ctx.http_headers]"),
            "字面量表里不许出现这条凭据：{text}"
        );
        assert!(
            !text.contains("command"),
            "换成 http 后旧 command 不许留下：{text}"
        );
    }

    #[test]
    fn writes_into_a_fresh_file() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        upsert_codex_mcp(&p, "context7", &stdio_t("npx", &args2(), &env1())).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        let s = &doc["mcp_servers"]["context7"];
        assert_eq!(s["command"].as_str(), Some("npx"));
        assert_eq!(s["args"][0].as_str(), Some("-y"));
        assert_eq!(s["env_vars"][0].as_str(), Some("K"), "同名转发要写成变量名");
        assert!(s.get("env").is_none(), "`env` 是字面量，凭据不该出现在那儿");
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
        upsert_codex_mcp(&p, "context7", &stdio_t("npx", &args2(), &env1())).unwrap();

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
        upsert_codex_mcp(&p, "context7", &stdio_t("npx", &args2(), &env1())).unwrap();
        upsert_codex_mcp(&p, "context7", &stdio_t("uvx", &[], &BTreeMap::new())).unwrap();

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
        upsert_codex_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap();

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
        upsert_codex_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap();

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
            upsert_codex_mcp(&p, name, &stdio_t("npx", &[], &BTreeMap::new())).unwrap();
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

        let err =
            upsert_codex_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap_err();
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

        let err =
            upsert_codex_mcp(&p, "context7", &stdio_t("npx", &[], &BTreeMap::new())).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("sk-live-abc"),
            "错误消息不许回显配置内容：{msg}"
        );
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// stdio 的 env：**同名转发**（`K: ${K}`）写成 `env_vars`；换个变量名（`W: ${A_B1}`）
    /// 在 codex 上没有写法，宁可报错也不写一个假的。
    #[test]
    fn stdio_env_becomes_env_vars_and_renames_are_refused() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        upsert_codex_mcp(&p, "context7", &stdio_t("npx", &[], &env1())).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            doc["mcp_servers"]["context7"]["env_vars"][0].as_str(),
            Some("K")
        );

        let before = std::fs::read(&p).unwrap();
        let mut renamed = BTreeMap::new();
        renamed.insert("W".to_string(), "${A_B1}".to_string());
        let err = upsert_codex_mcp(&p, "other", &stdio_t("npx", &[], &renamed)).unwrap_err();
        assert!(format!("{err:#}").contains("own name"), "{err:#}");
        assert_eq!(std::fs::read(&p).unwrap(), before, "报错就不能动文件");
    }

    /// Review Focus 5：键不在就逐字节不动（这里连注释一起钉住），也不落 `.bak`。
    #[test]
    fn removing_a_missing_key_leaves_the_bytes_alone() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(
            ".codex/config.toml",
            "# 用户自己写的\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"their-binary\"\n",
        );
        let before = std::fs::read(&p).unwrap();

        remove_codex_mcp(&p, "context7").unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), before, "空操作不许重写文件");
        assert!(!p.with_file_name("config.toml.henv.bak").exists());
    }

    /// 文件不存在 → 什么都不建。
    #[test]
    fn removing_from_a_missing_file_creates_nothing() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        remove_codex_mcp(&p, "context7").unwrap();
        assert!(!p.exists(), "撤销不是创建");
    }

    /// 文件在、但压根没有 `mcp_servers` 表 → 不写（不许凭空建一张空表出来）。
    #[test]
    fn a_file_without_mcp_servers_is_a_no_op() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "model = \"gpt-5\"\n");
        let before = std::fs::read(&p).unwrap();

        remove_codex_mcp(&p, "context7").unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// 真删块表：注释、别的键、别的 MCP 一个不少，只有那棵子树没了。
    #[test]
    fn removing_drops_only_the_named_block_table() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "# 用户自己写的\nmodel = \"gpt-5\"\n");
        upsert_codex_mcp(&p, "other", &stdio_t("their-binary", &[], &BTreeMap::new())).unwrap();
        upsert_codex_mcp(&p, "context7", &stdio_t("npx", &args2(), &env1())).unwrap();

        remove_codex_mcp(&p, "context7").unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("context7"), "{text}");
        assert!(text.contains("# 用户自己写的"), "注释必须活下来：{text}");
        let v: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(v["model"].as_str(), Some("gpt-5"));
        assert_eq!(
            v["mcp_servers"]["other"]["command"].as_str(),
            Some("their-binary")
        );
    }

    /// 内联拼法也要认——读路径两种都认（`Slots::of` 的理由），
    /// 一种认一种不认会让「diff 说该删、apply 说删不了」。
    #[test]
    fn removing_works_on_the_inline_form_too() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(
            ".codex/config.toml",
            "mcp_servers = { context7 = { command = \"npx\" }, other = { command = \"x\" } }\n",
        );

        remove_codex_mcp(&p, "context7").unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let v: toml::Value = toml::from_str(&text).unwrap();
        assert!(v["mcp_servers"].get("context7").is_none(), "{text}");
        assert_eq!(v["mcp_servers"]["other"]["command"].as_str(), Some("x"));
    }

    /// 内联拼法里键不在 → 字节不动。
    #[test]
    fn the_inline_form_without_the_key_is_a_no_op() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(
            ".codex/config.toml",
            "mcp_servers = { other = { command = \"x\" } }\n",
        );
        let before = std::fs::read(&p).unwrap();

        remove_codex_mcp(&p, "context7").unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// 语法坏掉的 config.toml → 报错、文件一字节不变。
    #[test]
    fn removing_from_a_broken_config_is_refused_and_never_echoed() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "[[[not toml\n");
        let before = std::fs::read(&p).unwrap();

        assert!(remove_codex_mcp(&p, "context7").is_err());

        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    /// `Authorization: Bearer ${VAR}` 走 codex 那条 bearer 通道（codex 自己发
    /// `Authorization: Bearer <token>`，与清单说的是同一件事）。同一份配置里别的头仍然
    /// 走 `env_http_headers`——两条通道可以并存。
    #[test]
    fn the_bearer_form_becomes_bearer_token_env_var() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer ${K}".to_string());
        headers.insert("X-Extra".to_string(), "${W}".to_string());
        upsert_codex_mcp(
            &p,
            "ctx",
            &McpTransport::Http {
                url: "https://mcp.context7.com/mcp".to_string(),
                headers,
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        let s = &doc["mcp_servers"]["ctx"];
        assert_eq!(s["bearer_token_env_var"].as_str(), Some("K"), "{text}");
        assert_eq!(
            s["env_http_headers"]["X-Extra"].as_str(),
            Some("W"),
            "{text}"
        );
        assert!(!text.contains("${K}"), "字面引用文本不许落进配置：\n{text}");
    }

    /// bearer 前缀只对应 `Authorization`；换个头名 codex 送不出去，宁可失败。
    #[test]
    fn a_bearer_value_on_another_header_is_refused() {
        let t = TempHome::new();
        let p = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "model = \"gpt-5\"\n");
        let before = std::fs::read(&p).unwrap();
        let mut headers = BTreeMap::new();
        headers.insert("X-Auth".to_string(), "Bearer ${K}".to_string());

        let err = upsert_codex_mcp(
            &p,
            "ctx",
            &McpTransport::Http {
                url: "https://mcp.context7.com/mcp".to_string(),
                headers,
            },
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("Authorization"), "{err:#}");
        assert_eq!(std::fs::read(&p).unwrap(), before, "报错就不能动文件");
    }
}
