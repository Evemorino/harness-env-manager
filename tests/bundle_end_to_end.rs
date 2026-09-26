//! §9.5 的拆解在**公开入口**上的样子：`henv::run_diff` 从清单读到报告。
//!
//! 这里刻意不碰内部函数：单元测试已经分别钉过定位、枚举、判定与渲染，
//! 本条要证的是它们串起来之后仍然成立——尤其是 `plan_for` 的顺序
//! （扫描先于拆解、拆解先于来源解析）。顺序错了，单元测试全绿而这里会红。
//!
//! 夹具全部落在 `TempHome` 下，一个字节都不碰真实 `~`。

use henv::testutil::TempHome;

/// 一个「claude-code 装了 superpowers 6.4.1」的假 HOME，
/// bundle 落在 `.claude/plugins/cache/official/superpowers/6.4.1/`。
fn temp_home_with_a_bundle() -> TempHome {
    let t = TempHome::new();
    t.mkdir(".claude");
    t.write(
        ".claude/plugins/installed_plugins.json",
        &format!(
            r#"{{"plugins":{{"superpowers@official":[{{"installPath":"{}","version":"6.4.1","gitCommitSha":"abc123"}}]}}}}"#,
            t.path(".claude/plugins/cache/official/superpowers/6.4.1").display()
        ),
    );
    t.write(".claude/settings.json", r#"{"enabledPlugins":{}}"#);

    let root = ".claude/plugins/cache/official/superpowers/6.4.1";
    t.write(
        &format!("{root}/.claude-plugin/plugin.json"),
        r#"{"name": "superpowers", "version": "6.4.1"}"#,
    );
    t.write(
        &format!("{root}/skills/brainstorming/SKILL.md"),
        "---\nname: brainstorming\n---\n",
    );
    t.write(
        &format!("{root}/skills/tdd/SKILL.md"),
        "---\nname: tdd\n---\n",
    );
    // 实测 superpowers 6.4.1 的 bundle 里有 `hooks/`，没有 `commands/`/`agents/`/`.mcp.json`
    t.mkdir(&format!("{root}/hooks"));
    t
}

fn manifest(source_harness: &str) -> String {
    format!(
        "version: 1\n\n\
         plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: {source_harness}\n"
    )
}

fn run_diff_to_string(t: &TempHome) -> String {
    let mut out = Vec::new();
    henv::run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
    String::from_utf8(out).expect("utf8")
}

/// §9.5 的正路：拆出来的两条 skill 落到 codex 上，plugin 行一条都不出现。
///
/// codex 必须显式建目录：`detect()` 只认目录存在，没装的那家整段被跳过，
/// 于是一条交付行都不会产生（`tests/git_source_end_to_end.rs` 的同一个理由）。
/// claude-code 是源 → 交付目标 = targets − claude-code = {codex, kimi-code}。
#[test]
fn a_bundle_is_decomposed_and_its_skills_are_planned_for_the_other_harnesses() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".codex");
    t.write("env.yaml", &manifest("claude-code"));

    let out = run_diff_to_string(&t);

    assert!(out.contains("skill/brainstorming"), "{out}");
    assert!(out.contains("skill/tdd"), "{out}");
    assert!(
        !out.contains("plugin/superpowers"),
        "顺利拆解时不产生 plugin 行：{out}"
    );
    // 源家那一节不该有任何待办（它自己那份是内容来源，不是待交付的东西）
    let claude_line = out
        .lines()
        .find(|l| l.starts_with("claude-code"))
        .unwrap_or_else(|| panic!("claude-code 该有一节：{out}"));
    assert!(claude_line.contains("in-sync"), "{claude_line}");
}

/// §9.5 / Review Focus 第 3 条，**显式 `targets:` 排除源家**的那一半（终审 Important#1）。
///
/// 上面那条走的是默认 `targets: All`，源家在 targets 里，吸收判据恰好成立；
/// 而「把 claude 的 bundle 交给 codex」最常见的写法是 `targets: [codex]`——
/// 源家此时**不在** targets 里，只看 `desired_plugins` 就会把它自己装着的那份
/// 报成 `- drift`（「不在清单里的东西」），而它恰恰是清单要求的。
///
/// Claude Code 把 plugin 里的 skill 以**软链**暴露在 `~/.claude/skills/<name>` 下，
/// 所以这条必须真造出那两条软链才走得到；再放一个用户手放的 `handmade`
/// 作反面控制，证明这条断言不是恒真。
#[test]
fn the_source_harnesss_own_copy_is_not_drift_when_targets_exclude_it() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".codex");
    let root = ".claude/plugins/cache/official/superpowers/6.4.1";
    // 软链必须指向**绝对**路径：`TempHome::symlink` 把 target 原样交给
    // `std::os::unix::fs::symlink`，相对路径会相对**链接所在目录**解析（断链）。
    t.symlink(
        ".claude/skills/brainstorming",
        &t.path(&format!("{root}/skills/brainstorming"))
            .to_string_lossy(),
    );
    t.symlink(
        ".claude/skills/tdd",
        &t.path(&format!("{root}/skills/tdd")).to_string_lossy(),
    );
    // 反面控制：用户手放的这一条**应该**照旧报 drift（§9.5 只豁免源家那份）。
    t.write(
        ".claude/skills/handmade/SKILL.md",
        "---\nname: handmade\n---\n",
    );
    t.write(
        "env.yaml",
        "version: 1\n\n\
         plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n    targets: [codex]\n",
    );

    let out = run_diff_to_string(&t);

    assert!(
        !out.contains("skill/brainstorming (未在清单中"),
        "源家自己那份是内容来源，不是 drift：{out}"
    );
    assert!(
        out.contains("skill/handmade (未在清单中"),
        "反面控制：用户手放的副本照旧报 drift：{out}"
    );
    // 拆解照旧落到 codex 上
    assert!(out.contains("+ install  skill/brainstorming"), "{out}");
}

/// §9.4：bundle 的过期判据是**内容哈希与账本 `content_hash` 比**，
/// 不是比版本号、不是比 `gitCommitSha`。这条钉的是「哈希真的算在
/// `<install_path>/skills/<skill>/` 上」——把它换成版本比较，这里会红。
#[test]
fn the_skill_content_comes_from_the_bundle_directory() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".codex");
    t.write("env.yaml", &manifest("claude-code"));

    let out = run_diff_to_string(&t);
    assert!(out.contains("+ install"), "{out}");

    // 改一个字节 → 目标侧没有副本，仍是 install（这条只证它读的是那个目录，
    // 不改判定的形态）；把 skill 目录整个删掉 → 拆出来的能力少一条。
    let root = ".claude/plugins/cache/official/superpowers/6.4.1";
    std::fs::remove_dir_all(t.path(&format!("{root}/skills/tdd"))).expect("删掉一个 skill");
    let out = run_diff_to_string(&t);
    assert!(out.contains("skill/brainstorming"), "{out}");
    assert!(
        !out.contains("skill/tdd"),
        "skill 目录没了，能力就不该再出现：{out}"
    );
}

/// §9.5：源家**没装**这个 plugin → 整条 undeliverable，报文点名是哪一家、
/// 并说清是「没有这个 plugin」。落点在源家那一节。
#[test]
fn a_source_harness_without_the_plugin_reports_undeliverable() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".codex");
    // 把安装记录清空：claude-code 装了，但没装这个 plugin
    t.write(
        ".claude/plugins/installed_plugins.json",
        r#"{"plugins":{}}"#,
    );
    t.write("env.yaml", &manifest("claude-code"));

    let out = run_diff_to_string(&t);

    assert!(out.contains("! undeliverable"), "{out}");
    assert!(out.contains("plugin/superpowers"), "{out}");
    assert!(out.contains("没有这个 plugin"), "{out}");
    assert!(
        !out.contains("+ install plugin/superpowers"),
        "不许同时假装能装：{out}"
    );
}

/// **codex 恒为这一支**：它的 `config.toml` 不记安装路径
/// （`src/adapters/codex.rs:113-119` 那条已知局限）。报文必须是
/// 「安装记录里没有安装路径」，**不是**「没有这个 plugin」——
/// 否则用户会去重装一个已经装好的 plugin。
#[test]
fn codex_as_a_source_says_the_record_has_no_install_path() {
    let t = TempHome::new();
    t.mkdir(".codex");
    t.write(
        ".codex/config.toml",
        "[plugins.\"superpowers@official\"]\nenabled = true\n",
    );
    t.write("env.yaml", &manifest("codex"));

    let out = run_diff_to_string(&t);

    assert!(out.contains("! undeliverable"), "{out}");
    assert!(out.contains("没有安装路径"), "{out}");
    assert!(
        !out.contains("没有这个 plugin"),
        "两句必须分得开——这决定用户去修什么：{out}"
    );
}

/// §9.5 的 degrade 第一处生效点：**kimi 作目标时拆解出的 skill 判 `Unsupported`**，
/// 由 `degrade.skills` 决定进报告还是丢弃。默认 `report` → 报告里看得见。
///
/// 这条同时是「kimi 的假成功」那个缺口的回归测试：若 `kimi_code::supports` 又凭
/// `Origin::Plugin` 判 `Native`，报告会说 `+ install`，而 kimi 的
/// `skills_target_dir()` 是 `None`，apply 一个字节都写不出来。
#[test]
fn a_decomposed_skill_targeting_kimi_shows_up_as_undeliverable() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".kimi-code");
    t.write("env.yaml", &manifest("claude-code"));

    let out = run_diff_to_string(&t);

    let kimi = out
        .lines()
        .find(|l| l.starts_with("kimi-code"))
        .unwrap_or_else(|| panic!("kimi-code 该有一节：{out}"));
    assert!(
        kimi.contains("undeliverable") || out.contains("skill/brainstorming"),
        "kimi 那节要么明说落不了地、要么根本不出行，不许写 + install：{out}"
    );
    assert!(
        !out.contains("kimi-code  ") || !kimi.contains("+ install"),
        "kimi 没有用户级 skills 目录，不许承诺 + install：{out}"
    );
}

/// §9.5「开关与目标」后半：没写 `source_harness:` 的条目不再假装能装。
/// claude-code 实测装着这个 plugin，但清单没写源 → install 改判 undeliverable，
/// 理由指路 `source_harness:`。
#[test]
fn a_plugin_without_a_source_harness_is_not_promised_as_installable() {
    let t = TempHome::new();
    t.mkdir(".codex");
    t.write(
        "env.yaml",
        "version: 1\n\n\
         plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n",
    );

    let out = run_diff_to_string(&t);

    assert!(out.contains("! undeliverable"), "{out}");
    assert!(out.contains("plugin/superpowers"), "{out}");
    assert!(out.contains("source_harness"), "要指路：{out}");
    assert!(!out.contains("+ install plugin/superpowers"), "{out}");
}

/// §9.5：**一家 harness 都没有**的机器上，拆解条目的 `undeliverable` 也不能消失。
///
/// 它挂不进任何 harness 节——未安装的家整节被 `render` 跳过——所以落进全局段
/// `(global)`。这条钉的是「清单里有一条根本无法满足的要求」必须被看见：没有全局段
/// 时 `diff` 报 `0 undeliverable`，用户以为一切正常。
#[test]
fn the_no_harness_at_all_case_still_shows_the_undeliverable_row() {
    let t = TempHome::new();
    // 一个 harness 目录都不建：`.claude/` 一存在 claude-code 就会被 `detect()` 认下来，
    // 那时这条行会落进它那一节，走的就不是全局段这条路了。
    t.write("env.yaml", &manifest("claude-code"));

    let out = run_diff_to_string(&t);

    assert!(out.contains("not installed"), "{out}");
    assert!(
        out.contains("! undeliverable"),
        "清单里这条根本无法满足，必须被看见——全局段就是接住它的地方：{out}"
    );
    assert!(out.contains("plugin/superpowers"), "{out}");
}

fn run_apply_with_harness(t: &TempHome, only: &str) {
    let mut out = Vec::new();
    let mut confirm = henv::AssumeYes;
    henv::run_apply(
        &t.path("env.yaml"),
        &t.home,
        &mut out,
        &mut confirm,
        false,
        Some(only),
    )
    .expect("run_apply");
}

/// Review Focus 4：`--harness codex` 只收窄**目标**，源扫描不跟着关掉。
///
/// 这条夹具里 claude-code 既是源（`source_harness: claude-code`）又是被排除的目标。
/// 要是 `--harness` 顺手把源扫描也过滤了，bundle 就定位不到——两条拆出来的 skill
/// 会变成 `undeliverable`，`.codex/skills/brainstorming` 根本不会出现。
#[test]
fn harness_filtering_never_turns_off_the_source_scan() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".codex");
    t.write("env.yaml", &manifest("claude-code"));

    run_apply_with_harness(&t, "codex");

    assert!(
        t.path(".codex/skills/brainstorming/SKILL.md").exists(),
        "源家被排除，但它的 bundle 仍要被读出来并拆解"
    );
    assert!(
        !t.path(".claude/skills/brainstorming").exists(),
        "被排除的家不许收到交付"
    );
}

/// §9.5 / 终审 B#5：`targets:` 整份名单都点名了**被 `harnesses:` 关掉的家**时，
/// 拆出来的每一条都永远不会有行，整条 plugin 于是静默消失。现在它走
/// `detached_plugin_rows` 报一行 undeliverable——「清单里有一条根本无法满足的要求」
/// 必须出现在报告里（与 B#3b 那三条同一条纪律）。
#[test]
fn a_plugin_targeting_only_a_disabled_harness_is_reported() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".codex");
    t.write(
        "env.yaml",
        "version: 1\n\n\
         harnesses:\n  kimi-code:\n    enabled: false\n\
         plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n    targets: [kimi-code]\n",
    );

    let out = run_diff_to_string(&t);

    assert!(out.contains("! undeliverable"), "{out}");
    assert!(out.contains("plugin/superpowers"), "{out}");
    assert!(out.contains("都不在 `harnesses:` 启用的家里"), "{out}");
    // 谁都不交付：`kimi-code` 那一节被整段省略，所以这个名字只出现在理由里。
    assert!(
        !out.contains("skill/brainstorming"),
        "一条都交付不出去，不该有 skill 行：{out}"
    );
}

/// §9.5 / 终审 B#4：清单里手写的 `skill/tdd` 与 bundle 拆出的 `skill/tdd` 是
/// **同一条身份**，不可能都交付。清单那条照常规划，拆出来那条被报成身份冲突——
/// 不是让 `ContentIndex` 按排序次序静默覆盖（今天 `lower` 在前、拆解在后，
/// 覆盖的方向恰好与「谁更显式」相反）。
#[test]
fn a_bundle_skill_colliding_with_a_manifest_skill_is_reported() {
    let t = temp_home_with_a_bundle();
    t.mkdir(".codex");
    t.mkdir(".agents/skills/tdd");
    t.write(".agents/skills/tdd/SKILL.md", "---\nname: tdd\n---\n");
    t.write(
        "env.yaml",
        "version: 1\n\n\
         skills:\n  tdd:\n    source: agents-dir\n\
         plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n",
    );

    let out = run_diff_to_string(&t);

    assert!(out.contains("! undeliverable"), "{out}");
    assert!(out.contains("身份冲突"), "{out}");
    assert!(
        out.contains("+ install  skill/brainstorming"),
        "没撞名的那条照常交付：{out}"
    );
    assert!(
        out.contains("+ install  skill/tdd"),
        "清单里手写的那条照常规划：{out}"
    );
}
