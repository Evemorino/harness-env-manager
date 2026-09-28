//! §9.6 的性质在 MCP 交付上的样子：**apply 之后 diff 就是 in-sync**、
//! **幂等**、**别家的配置一个字节没动**。
//!
//! codex 与 kimi 走公开入口（`run_apply`）。claude 不在这里：它的字节由外部 CLI 写，
//! 而 `run_apply` 走 `registry(home)`，没有办法把一条 shim 注进去。claude 那半由
//! `apply::mcp` 的单元测试（假 shim，证 argv 与顺序）与
//! `mcp_claude::tests::the_real_cli_...`（`#[ignore]`，证真 CLI 与读路径）合起来覆盖。

use henv::AssumeYes;
use henv::testutil::TempHome;

fn manifest() -> String {
    "version: 1\n\n\
     mcps:\n  context7:\n    command: npx\n    args: [\"-y\", \"@upstash/context7-mcp\"]\n    env:\n      K: ${K}\n"
        .to_string()
}

/// 不带凭据的那份：kimi 写不出任何「从环境取值」，所以它那半只能用这种条目验证
/// 交付本身（带凭据的那条由 `credentials_on_kimi_are_reported_not_written` 覆盖）。
fn manifest_without_credentials() -> String {
    "version: 1\n\n\
     mcps:\n  context7:\n    command: npx\n    args: [\"-y\", \"@upstash/context7-mcp\"]\n"
        .to_string()
}

fn temp_home_with_codex() -> TempHome {
    let t = TempHome::new();
    t.mkdir(".codex");
    t
}

fn run_apply_to_string(t: &TempHome) -> String {
    let mut out = Vec::new();
    let mut confirm = AssumeYes;
    henv::run_apply(
        &t.path("env.yaml"),
        &t.home,
        &mut out,
        &mut confirm,
        false,
        None,
    )
    .expect("run_apply");
    String::from_utf8(out).expect("utf8")
}

fn run_diff_to_string(t: &TempHome) -> String {
    let mut out = Vec::new();
    henv::run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
    String::from_utf8(out).expect("utf8")
}

/// 本文件每一份夹具**必须**让 claude 探测为假。
///
/// 这条不是整洁，是安全：`run_apply` 走 `registry(home)`，而 claude 的 MCP 写入是
/// **裸 `claude`**（§9.6）——子进程继承本进程的环境，其中包括**真实的 HOME**。
/// 夹具里但凡冒出 `.claude/` 目录或 `.claude.json` 文件，本文件的测试就会去改用户
/// 真实的 `~/.claude.json`。「测试绝不触碰真实的 `~`」是这个项目的硬约束，这条把
/// 它钉在夹具上：将来谁给 `temp_home_with_codex` 加一个 `.claude`，先红的是这里。
#[test]
fn the_fixtures_do_not_detect_claude_code() {
    let t2 = TempHome::new();
    t2.mkdir(".kimi-code");
    for t in [temp_home_with_codex(), t2] {
        let reg = henv::adapters::registry(&t.home);
        let claude = reg
            .iter()
            .find(|a| a.name() == henv::adapters::CLAUDE_CODE)
            .expect("claude-code 在 registry 里");
        assert!(
            !claude.detect(),
            "夹具里出现了 .claude/ 或 .claude.json —— MCP 交付会调起真实 CLI 并写真实 HOME"
        );
    }
}

#[test]
fn a_codex_mcp_goes_from_absent_to_delivered_to_in_sync() {
    let t = temp_home_with_codex();
    // 用户自己的一行配置与注释，必须活下来（§9.10 merge-safe）
    t.write(
        ".codex/config.toml",
        "# my codex config\nexperimental_bearer_token = \"x\"\n",
    );
    t.write("env.yaml", &manifest());

    // 1) diff 说该装，且一个字节都没写
    let before = run_diff_to_string(&t);
    assert!(before.contains("mcp/context7"), "{before}");
    assert!(
        !std::fs::read_to_string(t.path(".codex/config.toml"))
            .unwrap()
            .contains("context7"),
        "diff 不许写盘"
    );

    // 2) apply 真的写进去
    run_apply_to_string(&t);
    let cfg = std::fs::read_to_string(t.path(".codex/config.toml")).unwrap();
    assert!(cfg.contains("# my codex config"), "注释要留住：\n{cfg}");
    assert!(cfg.contains("experimental_bearer_token"), "{cfg}");
    assert!(cfg.contains("context7"), "{cfg}");
    // 凭据要走 codex 的**取值字段**（`env_vars` = 同名转发），不能把 `${K}` 文本写进
    // `env`——那是个字面量，codex 会把这段文本原样发给 MCP 服务端（2026-09-28 实测）。
    assert!(
        cfg.contains("env_vars"),
        "凭据要走 codex 的取值字段：\n{cfg}"
    );
    assert!(
        !cfg.contains("${K}"),
        "codex 不展开 ${{VAR}}，写进去等于把它当字面量发出去：\n{cfg}"
    );

    // 3) **性质**：apply 之后 codex 那一行必须是 in-sync。
    //    这条同时验了账本的哈希与从配置里读回来的 spec 是一致的——
    //    若账本记错了哈希，这里会变成 update。
    //
    //    断言落在**这家那一行**上，不落在整段输出上：渲染器把全同步的 harness
    //    收成一行汇总（`✓ in-sync  mcps:1`），逐条 label 不再打印——所以
    //    「输出里有 mcp/context7」在成功路径上反而为假。`mcps:1` 同时钉住
    //    读回来的确实是一条，把「汇总说有、实际一条都没读到」也挡在外面。
    let after = run_diff_to_string(&t);
    let codex_line = after
        .lines()
        .find(|l| l.starts_with("codex"))
        .unwrap_or_else(|| panic!("报告里没有 codex 那一行：\n{after}"));
    assert!(
        codex_line.contains("✓ in-sync") && codex_line.contains("mcps:1"),
        "apply 之后 codex 应当是 in-sync 且读到一条 mcp，实际：{codex_line}"
    );

    // 4) 幂等：再 apply 一次不交付任何东西
    let second = run_apply_to_string(&t);
    assert!(
        !second.contains("delivered"),
        "无事可做不该再交付：\n{second}"
    );

    // 5) 备份留着用户最初那份，且是单槽
    let bak = t.path(".codex/config.toml.henv.bak");
    let bak_text = std::fs::read_to_string(&bak).unwrap();
    assert!(bak_text.contains("# my codex config"), "{bak_text}");
    assert!(
        !bak_text.contains("context7"),
        "备份是改动前的那份：\n{bak_text}"
    );
}

#[test]
fn a_kimi_mcp_lands_in_its_own_file_and_other_keys_survive() {
    let t = TempHome::new();
    t.mkdir(".kimi-code");
    t.write(
        ".kimi-code/mcp.json",
        "{\n  \"mcpServers\": {\n    \"user-own\": { \"command\": \"their-binary\" }\n  }\n}\n",
    );
    t.write("env.yaml", &manifest_without_credentials());

    run_apply_to_string(&t);

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(t.path(".kimi-code/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        v["mcpServers"]["user-own"]["command"], "their-binary",
        "别人那条要留着"
    );
    assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
    assert!(
        v["mcpServers"]["context7"].get("env").is_none(),
        "没有凭据就不该冒出 env 键"
    );

    // apply 之后读回来的就是我们写下去的那条：清单声明的 context7 不再需要
    // install/update，报告里剩下的唯一动作是**没写进清单的** `user-own` 的 drift
    // ——它正是上面第一条断言要留住的那个「别人的条目」。
    //
    // 这里不能断言「整段输出含 `in-sync`」：user-own 的 drift 会把 kimi 那一行
    // 压成 drift，全同步的汇总行因此根本不出现。缺的是断言，不是功能——
    // context7 确实已经同步，只是渲染器没有为它单开一行。
    let after = run_diff_to_string(&t);
    assert!(after.contains("mcp/user-own"), "{after}");
    assert!(
        after.contains("0 install, 0 update"),
        "apply 之后清单里那条不该还要装或改：\n{after}"
    );
}

/// **kimi 上没有凭据的忠实写法**，所以带凭据的条目要如实报「交付不了」，
/// 绝不能退化成「把 `${K}` 当字面量写进去」——那是这个项目最忌讳的静默失败
/// （写下去之后没有任何一方会报错，效果却是服务端收到一段无意义的文本）。
///
/// 判据与三家实测依据见 `adapters::mcp_expressible`。
#[test]
fn credentials_on_kimi_are_reported_not_written() {
    let t = TempHome::new();
    t.mkdir(".kimi-code");
    t.write("env.yaml", &manifest());
    let before = std::fs::read(t.path(".kimi-code/mcp.json")).ok();

    let plan = run_diff_to_string(&t);
    assert!(
        plan.contains("undeliverable") && plan.contains("kimi-code"),
        "要报成交付不了：\n{plan}"
    );
    assert!(
        plan.contains("字面量"),
        "理由要说清是「字面量」这件事：\n{plan}"
    );
    assert!(
        plan.contains("0 install, 0 update"),
        "交付不了就不是 install：\n{plan}"
    );

    let applied = run_apply_to_string(&t);
    assert!(
        !applied.contains("delivered mcp/context7"),
        "不许交付：\n{applied}"
    );
    assert_eq!(
        std::fs::read(t.path(".kimi-code/mcp.json")).ok(),
        before,
        "一个字节都不该写"
    );
}

/// **同一份清单、两次 apply，第二次不该碰盘。** 第二次写下去的是同样的字节，
/// 但真正要防的是「每次都重新写一遍」——那会让 claude 侧多两次 CLI 调用、
/// 也会让 `.bak` 的时机变得不可预测。
#[test]
fn a_second_apply_writes_nothing() {
    let t = temp_home_with_codex();
    t.write("env.yaml", &manifest());
    run_apply_to_string(&t);
    let first = std::fs::read(t.path(".codex/config.toml")).unwrap();

    let second = run_apply_to_string(&t);
    assert!(!second.contains("delivered"), "{second}");
    assert_eq!(
        std::fs::read(t.path(".codex/config.toml")).unwrap(),
        first,
        "无事可做的 apply 不该重写配置"
    );
}

/// **`Authorization: Bearer ${VAR}` 是拿凭据喂两家的那条路**（2026-09-28）。
/// codex 有 `bearer_token_env_var`、kimi 有 `bearerTokenEnvVar`，两家都发
/// `Authorization: Bearer <token>`——清单里那一个写法正好对应它们，于是 kimi 上
/// 「带凭据的 MCP」第一次变得可交付（裸值 `${VAR}` 仍然交付不了，见上一条测试）。
#[test]
fn a_bearer_credential_lands_on_codex_and_kimi() {
    let t = TempHome::new();
    t.mkdir(".codex");
    t.mkdir(".kimi-code");
    t.write(
        "env.yaml",
        "version: 1\n\n\
         mcps:\n  context7:\n    url: https://mcp.context7.com/mcp\n    headers:\n      \
         Authorization: Bearer ${CONTEXT7_API_KEY}\n    targets: [codex, kimi-code]\n",
    );

    let planned = run_apply_to_string(&t);
    assert!(planned.contains("delivered mcp/context7"), "{planned}");

    // codex：照它自己的拼写写成 bearer 通道，绝不落 `${VAR}` 文本。
    let codex = std::fs::read_to_string(t.path(".codex/config.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&codex).unwrap();
    assert_eq!(
        doc["mcp_servers"]["context7"]["bearer_token_env_var"].as_str(),
        Some("CONTEXT7_API_KEY"),
        "{codex}"
    );
    assert!(!codex.contains("${"), "{codex}");

    // kimi：唯一的那条环境通道。
    let kimi: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(t.path(".kimi-code/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        kimi["mcpServers"]["context7"]["bearerTokenEnvVar"], "CONTEXT7_API_KEY",
        "{kimi}"
    );

    // 两家都读得回来：写下去的和读回来的必须是同一个形状。
    let after = run_diff_to_string(&t);
    assert!(
        after.contains("0 install, 0 update") && after.contains("0 undeliverable"),
        "apply 之后两家都该 in-sync：\n{after}"
    );
}
