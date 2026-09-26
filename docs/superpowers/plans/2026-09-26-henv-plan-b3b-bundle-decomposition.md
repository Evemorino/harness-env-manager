# Plan B3b：plugin bundle 拆解 + `degrade:` 真正生效

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让清单里写了 `source_harness:` 的 plugin 条目真的被**拆解**——从源 harness 已安装的 bundle 里枚举出 `skills/<n>/`，每条变成一条独立的 `Kind::Skill` 能力，拷进其余目标 harness 的 skills 目录并记账；同时让 `degrade:` 第一次真的参与判决，并让**没写** `source_harness:` 的 plugin 条目不再假装能装。

**Architecture:** 今天三家 adapter 的 `supports()` 对 `Kind::Plugin` 一律 `Native`，于是 `diff` 承诺 `+ install plugin/x`，而 `deliver_skills` 对非 `Kind::Skill` 的条目**静默跳过**——apply 一个字节都不写，两层都不报。B3b 关掉的就是这个假成功。做法是**把「plugin」这条不能交付的承诺换成能交付的东西**：源解析之前多一步「拆解」——按 `source_harness:` 在源家的 `read_state()` 里找到 `Kind::Plugin` 记录取 `install_path`，枚举其 `skills/*/` 产出独立能力；这些能力走**已有的** `deliver_skills` 那条路（不改 apply 一行），`content_verdict` 用已有的 `<install_path>/skills/<skill>/` 哈希与账本比对（§9.4 的 bundle 过期判据由此自动落地）。定位不到、差集为空、拆不出东西三种情况各产生一条 `undeliverable` 的 `Kind::Plugin` 行；顺利拆解时一条 plugin 行都不产生。

**Tech Stack:** Rust 2024 / clap 4.6 / serde 1 / serde_json / serde_yaml_ng / anyhow / sha2。**本计划不新增任何依赖。** dev-deps：insta、tempfile。`henv` 的 `testutil` 是非默认 feature（自指 dev-dependency 打开）。

**Spec:** `docs/design.md` §9.5（三个小节：开关与目标 / 来源与定位 / 交付什么）、§4 的 `source_harness:` 字段与 Schema 决策第 5 条、§9.4 的 bundle 过期判据（对 `<install_path>/skills/<skill>/` 算哈希与账本 `content_hash` 比，**不是**比版本号、**不是**比 `gitCommitSha`）、§9.3（逐 harness 拷贝交付与账本约定）、§9.7（失败隔离：单条失败不中断）、§9.9（写 API 白名单与 shell out 允许位）、§9 的 B3b 边界说明。

## Global Constraints

- 对话中文；代码标识符、注释、commit message 一律英文。
- **仓库没有远程**：不 push、不创建远程、不改写历史。每个 Task 的 commit 步骤只提交本 Task 涉及的文件；是否真的提交由我在每个 Task 结束时决定。
- **测试绝不触碰真实 `~`。** 一切夹具落在 `TempHome`（`TempDir`）下。
- **`tests/read_only_guard.rs` 的两张表都不许放宽。** 本计划**不新增任何写路径**：拆解只**读** bundle 目录（`try_read_skill_entries` / `hash_dir` 都是只读），交付复用已有的 `src/apply/write.rs`。所以 `WRITE_WHITELIST` 与 `PROCESS_ALLOWLIST` 都保持原样，一个字符都不改。`production_code_never_calls_the_lossy_skill_scanner` 意味着**新代码里枚举 skill 目录一律用 `try_read_skill_entries`**，不许用便利版 `read_skill_entries`。
- **失败隔离（§9.7）。** 单条 plugin 条目的失败**绝不**上抛打断整次运行，也绝不让其余条目不受影响地静默消失——失败的条目进报告，其余照常。`decompose_bundles` 因此返回 `Decomposition{ failures }` 而不是 `Err`。**唯一的例外**是「源 harness 压根扫不了」（`read_state` 上抛）：那是这台机器的故障，与 `plan_for` 里既有那几处同样处理。
- **不改无关代码。** 不顺手重构、不格式化无关文件、不改与任务无关的注释。
- **rust-version 1.85**：**没有 let-chains**（`let ... &&` 要 1.88）。需要「两个条件同时成立」时写嵌套 `if`。
- 本仓库**今天就有既有 lint 债**（4 个 B3a 未触碰的文件里有 `cargo fmt --check` 不过的地方，`src/store/skills.rs:372,435` 有两条 `clippy::unnecessary_get_then_check`）。**本计划不清理那笔债**，也不把它当门槛：只在**新增/改动的行**上保持 `cargo fmt` 与 clippy 干净。

## Review Focus

按「最可能先咬人」排序。每一条都在它该在的那个 Task 里有测试钉着。

1. **`source_harness:` 指向的那家在 `harnesses:` 里被关掉了。** 那家仍然要被**读**（它只是内容来源，不是交付目标），但它**绝不能**因此凭空多出一节 diff、也不能被写。→ Task 6、Task 9。
2. **拆解出的 skill 在 kimi 上被判 `Native` 之后静默不落地。** `deliver_skills` 在 `target_dir == None` 时提前返回空报告（`src/apply/mod.rs:67`）。若 kimi 的 `supports()` 继续凭 `Origin::Plugin` 判 `Native`，一条拆解出的 skill 会拿到 `+ install` 却什么都没写——**这正是 B3b 要消灭的那类假成功换了个入口**。判据必须落在「这家有没有用户级 skills 目录」，不是「它是不是来自 bundle」。→ Task 8。
3. **源家自己装着的那一份被报成 drift。** 源 harness 的 `read_state()` 一定扫得到那个 plugin（我们刚从这里取的 `install_path`）。若它落进 drift 那一段，用户会看到「你有一个不在清单里的 plugin」——而它恰恰是清单要求的。→ Task 9。
4. **一个 bundle 里有 `skills/`、但里面是空的（或压根没有 `skills/`）。** 顺利拆解会让 plugin 行消失，于是「什么都没装成」在报告里变成一片干净。→ Task 6、Task 9。
5. **降级目标本身没有落点**：`source_harness: claude-code`、targets 只写 claude。差集为空，拆出来的 skill 无处可去。→ Task 3、Task 6、Task 9。
6. **bundle manifest 存在但解不开**（半截 JSON、写坏的 `plugin.json`）。→ Task 5。
7. **`source_harness:` 拼错了**（`claude_code` / `Claude-Code`）。→ Task 2。
8. **`degrade:` 写了 `skip`。** 一个有 `hooks/` 的 bundle 在 `skip` 下不许在报告里留任何痕迹。→ Task 6。
9. **一家 harness 都没有的机器上有一条 bundle 条目。** 三家都不 `detected` → `compute` 对未安装的家产出空 `entries`、`render` 整节跳过，于是那条 `undeliverable` 无处可放。**这是本计划显式承认的已知缺口**（不是静默的：`Plan` 今天没有全局段，加全局段属于 B3c/doctor 的范畴）。Task 9 用一个 `compute` 层的测试把**行为**钉死（行落进第一家、且未安装时不可见），缺口写进 Task 10 的测试注释。

> **2026-09-26 更正：这条缺口已经闭合。** 收官时按用户要求补了 `Plan.unattached` 与
> `render` 的 `(global)` 段——没有已安装的家时，那条行落进 `(global)` 并且**可见**。
> Task 10 的 `the_no_harness_at_all_case_is_a_known_gap` 已翻转为
> `the_no_harness_at_all_case_still_shows_the_undeliverable_row`（断言行必须可见），
> `docs/design.md` §9.5 补了三向落点。经过见
> `.superpowers/sdd/2026-09-26-henv-plan-b3b-bundle-decomposition/progress.md` 的 Post-plan fix 段。

---

## File Structure

| 文件 | 动作 | 职责 |
| --- | --- | --- |
| `src/adapters/claude_code.rs` | 修改 | 新增第七个 JSON 形状原语 `optional_bool` |
| `src/adapters/kimi_code.rs` | 修改 | `enabled: null` 走 `optional_bool`；`supports()` 去掉 `Origin::Plugin → Native` 的捷径 |
| `src/manifest.rs` | 修改 | `PluginEntry.source_harness: Option<String>` + 校验期必须是 `KNOWN_HARNESSES` 之一 |
| `src/capability.rs` | 修改 | `Targets::minus` / `Targets::is_empty` |
| `src/lower.rs` | 修改 | `SourceHarnessTable`（旁路表，`DegradeTable` 的先例）；`plugin_identity` 放开到 `pub(crate)` |
| `src/decompose.rs` | 新建 | 拆解这一步：`SourceState`、`scan_source_harnesses`、`read_bundle_manifest`、`bundle_source_id`、`decompose_bundles`、`Decomposition` |
| `src/store/skills.rs` | 修改 | `ContentIndex.notes`；`LocatedBundle`/`BundleTable`；`resolve_sources` 第 4 参 + `PluginBundle` 分支 |
| `src/diff.rs` | 修改 | `compute` 第 6 参 `&SourceHarnessTable`；`detached_plugin_rows`；拆解条目的 plugin 行语义；没写 `source_harness:` 的 install/update 改判 |
| `src/lib.rs` | 修改 | `plan_for` 重排（扫描 → 拆解 → 解析）；修正撒谎的 `degrade:` 注释 |
| `tests/bundle_end_to_end.rs` | 新建 | 走公开入口 `run_diff`：拆解交付、定位失败、kimi 作目标 |

**没有 Task 碰 `src/apply/`。** 这是本设计最要紧的一点：拆解出的 skill 与清单里的 skill 在交付层是**同一种东西**，`deliver_skills` 一行都不用改。

---

### Task 1: `optional_bool` + kimi 的 `enabled: null` 对齐

**Files:**
- Modify: `src/adapters/claude_code.rs`（`optional_string` 附近，第 457 行上下一带）+ 同文件 `mod tests`
- Modify: `src/adapters/kimi_code.rs:84-87` + 同文件 `mod tests`

**Interfaces:**
- Consumes: `expect_bool(value: &Value, file: &Path, what: &str) -> anyhow::Result<bool>`（`src/adapters/claude_code.rs:449`，已存在）。
- Produces: `pub(crate) fn optional_bool(value: Option<&Value>, file: &Path, what: &str) -> anyhow::Result<Option<bool>>`，放在 `src/adapters/claude_code.rs`，与 `optional_string`/`optional_object`/`optional_array` 并列。**Task 6 不用它**（拆解不读布尔），但 kimi 的 `enabled` 从此读得出来，`source_harness: kimi-code` 才可能定位到任何东西。

**为什么这件事在 B3b 的范围里**：kimi 的 `installed.json` 里 `"enabled": null` 是合法状态（用户没表态），今天走 `expect_bool` 直接硬错，**整条 `read_plugins` 全废**——写坏一条记录，这家一个 plugin 都读不出来。而 §9.5 允许 `source_harness: kimi-code`，它必须能读到。

- [ ] **Step 1: 写失败的测试（原语层）**

在 `src/adapters/claude_code.rs` 的 `mod tests` 里，紧挨已有的 `optional_string` / `optional_object` 测试之后加：

```rust
    /// 第七个形状原语：`null` 与缺键同义（都是「没表态」），
    /// 而**不是**布尔 = 坏值。写成 `.and_then(Value::as_bool).unwrap_or(true)`
    /// 就把用户显式关掉的 plugin 报成已启用；只写 `expect_bool` 则把一个
    /// 合法的 `null` 报成整份文件读不出来。两条都不行。
    #[test]
    fn optional_bool_treats_null_like_a_missing_key() {
        let f = Path::new("x.json");
        let what = "field `enabled`";
        assert_eq!(optional_bool(None, f, what).unwrap(), None);
        assert_eq!(
            optional_bool(Some(&serde_json::Value::Null), f, what).unwrap(),
            None
        );
        assert_eq!(
            optional_bool(Some(&serde_json::json!(true)), f, what).unwrap(),
            Some(true)
        );
        assert_eq!(
            optional_bool(Some(&serde_json::json!(false)), f, what).unwrap(),
            Some(false)
        );
    }

    /// 放宽的只有 `null`：类型错了仍然是坏值。这是「缺键」与「写坏了」的分界，
    /// 也是失败隔离契约要求的那条线。
    #[test]
    fn optional_bool_still_refuses_a_wrong_type() {
        let f = Path::new("x.json");
        let e = optional_bool(Some(&serde_json::json!("yes")), f, "field `enabled`")
            .expect_err("字符串不是布尔");
        assert!(format!("{e:#}").contains("enabled"), "{e:#}");
    }
```

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib optional_bool`
Expected: 编译失败，`cannot find function optional_bool in this scope`。这正是要的失败：原语还不存在。

- [ ] **Step 3: 实现原语**

在 `src/adapters/claude_code.rs` 的 `optional_string` 之前插入：

```rust
/// 可选布尔：**缺键与显式 `null` 都是「没表态」**，其余交给 [`expect_bool`]。
///
/// 与 `optional_string` / `optional_object` / `optional_array` 同一条分界：
/// 容器级的 `null` 在 JSON 里是「这个键没有值」，不是「这个键的值是坏东西」。
/// 少了这一支，kimi 的 `installed.json` 里一条 `"enabled": null` 就会让
/// **整份文件**读不出来——用户的写法明明是合法的。
pub(crate) fn optional_bool(
    value: Option<&Value>,
    file: &Path,
    what: &str,
) -> anyhow::Result<Option<bool>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(expect_bool(v, file, what)?)),
    }
}
```

- [ ] **Step 4: 跑测试，确认它通过**

Run: `cargo test --lib optional_bool`
Expected: PASS 2/2。

- [ ] **Step 5: 写失败的测试（kimi 行为层）**

在 `src/adapters/kimi_code.rs` 的 `mod tests` 里，紧挨 `read_plugins` 那一族测试之后加：

```rust
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
            Spec::Plugin { enabled, .. } => assert!(*enabled, "没表态 = 启用，与 claude/codex 同默认"),
            other => panic!("{other:?}"),
        }
    }
```

- [ ] **Step 6: 跑测试，确认它失败**

Run: `cargo test --lib a_null_enabled_reads_as_unset`
Expected: FAIL，`read_state` 返回 `Err`，消息里带 `field \`enabled\``。这正是要的失败：`expect_bool` 把 `null` 判成了坏值。

- [ ] **Step 7: 改 kimi 的调用点**

`src/adapters/kimi_code.rs`，把 `enabled:` 那一整段（今天的 `match record.get("enabled") { Some(v) => expect_bool(...)?, None => true }`）换成：

```rust
                // `enabled` 走第七个形状原语 `optional_bool`，**两步分开**：
                // 「缺键 = 未表态 = 启用」（与 claude 的门禁默认、codex 的
                // `toml_optional_bool` 默认同语义），「显式 `null` = 同样没表态」，
                // 「键在但不是布尔」才是故障。三者必须分开——写成一个
                // `.and_then(Value::as_bool).unwrap_or(true)` 就把用户关掉的 plugin
                // 报成已启用，写成一个裸 `expect_bool` 就把合法的 `null` 报成坏文件。
                enabled: optional_bool(record.get("enabled"), &file, &format!("{context} field `enabled`"))?
                    .unwrap_or(true),
```

并在文件顶部的 `use super::claude_code::{...}` 那一行（或等价的 `use crate::adapters::claude_code::…`）里加上 `optional_bool`。**注意**：kimi 今天已从 `claude_code` 借了若干 JSON 原语；沿用同一条 use 行，不新开一行。

- [ ] **Step 8: 跑测试，确认它通过**

Run: `cargo test --lib a_null_enabled_reads_as_unset`
Expected: PASS。

- [ ] **Step 9: 跑全量测试**

Run: `cargo test`
Expected: 全绿。特别确认 `supports_mcp_and_plugin_natively`、`read_plugins` 那一族、`tests/read_only_guard.rs` 全过——本条没有引入任何写 API。

- [ ] **Step 10: Commit**

```bash
git add src/adapters/claude_code.rs src/adapters/kimi_code.rs
git commit -m "fix(kimi): read a null \`enabled\` as unset, not as a broken record"
```

---

### Task 2: `PluginEntry.source_harness` + 校验期必须是已知 harness

**Files:**
- Modify: `src/manifest.rs:88-99`（`PluginEntry`）、`src/manifest.rs:578-606`（plugins 校验循环）+ 同文件 `mod tests`

**Interfaces:**
- Consumes: `KNOWN_HARNESSES: [&str; 3]`（`src/manifest.rs:19`，已存在）。
- Produces: `PluginEntry.source_harness: Option<String>`。**Task 4、Task 6 都读它**。`None` = 没写 = 不拆解。

**为什么是 `Option<String>` 而不是拿 `degrade:` 当开关**（§4 Schema 决策 5）：`degrade` 是 `#[serde(default)]` 的，代码区分不出「没写」与「写了空的」；`source_harness` 是 `Option`，它的 `Some`/`None` 天然就是二分。

- [ ] **Step 1: 写失败的测试**

在 `src/manifest.rs` 的 `mod tests` 里，紧挨 `rejects_unknown_harness_name`（第 1077 行）之后加：

```rust
    /// §9.5 的开关：`source_harness:` 的**存在本身就是拆解请求**。
    /// 没写 = `None` = 不拆解，它只做版本对账。
    #[test]
    fn a_plugin_without_source_harness_parses_to_none() {
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n",
        )
        .expect("没写 source_harness 是正常清单");
        assert_eq!(m.plugins["superpowers"].source_harness, None);
    }

    #[test]
    fn source_harness_round_trips() {
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: codex\n",
        )
        .expect("codex 是已知 harness");
        assert_eq!(
            m.plugins["superpowers"].source_harness.as_deref(),
            Some("codex")
        );
    }

    /// 拼错的家必须在**校验期**就被拒，不能拖到运行时当成「没装」——
    /// `claud-code` 与「claude-code 没装这个 plugin」是两件完全不同的事，
    /// 后者要用户去修安装，前者要用户去修清单。
    #[test]
    fn rejects_an_unknown_source_harness() {
        let e = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  p:\n    source: claude-marketplace\n    marketplace: official\n    entry: p\n    source_harness: claude_code\n",
        )
        .expect_err("下划线不是已知 harness 的拼法");
        let msg = format!("{e:#}");
        assert!(msg.contains("source_harness"), "{msg}");
        assert!(msg.contains("claude-code"), "消息要点名合法取值：{msg}");
    }
```

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib source_harness`
Expected: 编译失败，`no field \`source_harness\` on type \`PluginEntry\``。这正是要的失败。

- [ ] **Step 3: 加字段**

`src/manifest.rs`，`PluginEntry` 里 `degrade` 之后加：

```rust
    /// 从哪一家的**已安装 bundle** 取内容。
    ///
    /// **写了就是请求拆解**（§9.5）：交付目标 = `targets` − 这家。不拿
    /// 「`degrade:` 写没写」当开关——`degrade` 是 `#[serde(default)]` 的，
    /// 代码区分不出「没写」与「写了空的」，而这里是 `Option`，`Some`/`None`
    /// 天然就是二分。
    ///
    /// 值必须是 [`KNOWN_HARNESSES`] 之一，校验期就拒；**能不能定位到是另一件事**，
    /// 那是运行时的判决（定位不到整条 undeliverable，见 §9.5）。
    ///
    /// 不许从「哪些 harness 装了它」反推：本机同一个 plugin 可以有 3 份、2 个版本，
    /// 反推必然挑错版本。
    #[serde(default)]
    pub source_harness: Option<String>,
```

- [ ] **Step 4: 加校验**

`src/manifest.rs` 的 plugins 校验循环里，紧跟 `reject_blank(&at, "repo", p.repo.as_deref(), &mut e);` 之后插入：

```rust
            // §9.5：`source_harness:` 的值域就是已知 harness 名。
            // 这里**只**判值域，不判「那家装没装这个 plugin」——那是运行时的判决，
            // 而且它的失败理由完全不同（「claude-code 没装」要用户去装，
            // 「claude_code 拼错了」要用户去改清单）。
            if let Some(s) = &p.source_harness {
                if !KNOWN_HARNESSES.contains(&s.as_str()) {
                    e.push(format!(
                        "{at}: `source_harness` must be one of {} (got `{s}`)",
                        KNOWN_HARNESSES.join(", ")
                    ));
                }
            }
```

- [ ] **Step 5: 跑测试，确认它通过**

Run: `cargo test --lib source_harness`
Expected: PASS 3/3。

- [ ] **Step 6: 跑全量测试**

Run: `cargo test`
Expected: 全绿。`PluginEntry` 的构造只有一处字面量（结构体定义本身），其余都走 YAML 反序列化，所以没有别处要跟着改。若哪条既有测试因为消息形状变了而红，改它的期望串，**不改校验逻辑**。

- [ ] **Step 7: Commit**

```bash
git add src/manifest.rs
git commit -m "feat(manifest): add plugins.<name>.source_harness"
```

---

### Task 3: `Targets::minus` 与 `Targets::is_empty`

**Files:**
- Modify: `src/capability.rs:126-138`（`impl Targets`）+ 同文件 `mod tests`

**Interfaces:**
- Consumes: `Targets`（`All` / `Only(Vec<String>)`）。
- Produces:
  - `pub fn minus(&self, harness: &str, known: &[&str]) -> Targets`
  - `pub fn is_empty(&self) -> bool`
  **Task 6（拆解出的 skill 的 targets）与 Task 9（差集为空的判决）都消费它们。**

**为什么 `known` 要当参数传**：`Targets::All` 减去一家之后要变成 `Only(其余几家)`，而 `All` 本身不知道有哪几家。把名单当参数传进来，`capability` 就不必依赖 `manifest::KNOWN_HARNESSES`（那会把一个纯数据类型模块和清单层绑死，而 `capability.rs` 今天一个清单概念都没有）。调用方传 `compute` 手里那几家（= registry 的名字，与 `KNOWN_HARNESSES` 相等，`src/adapters/mod.rs:173` 已把两者钉成相等）。

- [ ] **Step 1: 写失败的测试**

在 `src/capability.rs` 的 `mod tests` 里加：

```rust
    /// §9.5 的交付目标 = `targets` − `source_harness`。
    /// `All` 是默认值，它必须能**具体化**成「除这一家之外的三家」——
    /// 否则「默认三家全要」的清单写上一个源 harness 之后就哪家都不交付了。
    #[test]
    fn minus_a_harness_from_all_names_the_rest() {
        let known = ["claude-code", "codex", "kimi-code"];
        assert_eq!(
            Targets::All.minus("claude-code", &known),
            Targets::Only(vec!["codex".into(), "kimi-code".into()])
        );
    }

    #[test]
    fn minus_a_harness_from_an_explicit_list_keeps_the_order() {
        let known = ["claude-code", "codex", "kimi-code"];
        let t = Targets::Only(vec!["kimi-code".into(), "claude-code".into()]);
        assert_eq!(
            t.minus("claude-code", &known),
            Targets::Only(vec!["kimi-code".into()])
        );
    }

    /// 减掉一家不在名单里的 harness 是恒等变换——`targets` 里本来就没它。
    #[test]
    fn minus_something_that_is_not_there_changes_nothing() {
        let known = ["claude-code", "codex", "kimi-code"];
        let t = Targets::Only(vec!["codex".into()]);
        assert_eq!(t.minus("claude-code", &known), t);
    }

    /// `All` 减去唯一已知的一家 = 空集。这正是 §9.5 「目标只有源 harness 自己」
    /// 那个要报 undeliverable 的情形，所以「空」必须判得出来。
    #[test]
    fn only_and_all_cannot_be_empty_until_they_are() {
        assert!(!Targets::All.is_empty(), "All 永远不空");
        assert!(!Targets::Only(vec!["codex".into()]).is_empty());
        assert!(Targets::Only(vec![]).is_empty());
        assert!(Targets::All.minus("claude-code", &["claude-code"]).is_empty());
    }
```

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib targets::`
Expected: 编译失败，`no method named \`minus\``。

- [ ] **Step 3: 实现**

`src/capability.rs`，`impl Targets` 里 `includes` 之后加：

```rust
    /// §9.5：交付目标 = `targets` − `source_harness`。
    ///
    /// `known` 是「有哪几家 harness」——`All` 不知道这件事，只有调用方知道
    /// （`compute` 手里那几家的名字，与 `KNOWN_HARNESSES` 相等）。当参数传而不是
    /// 从 `manifest` 读，是为了让本模块继续不认识「清单」这个概念。
    pub fn minus(&self, harness: &str, known: &[&str]) -> Targets {
        match self {
            Targets::All => Targets::Only(
                known
                    .iter()
                    .filter(|h| **h != harness)
                    .map(|h| (*h).to_string())
                    .collect(),
            ),
            Targets::Only(list) => {
                Targets::Only(list.iter().filter(|h| h.as_str() != harness).cloned().collect())
            }
        }
    }

    /// 空目标集：这个能力谁都不交付。
    pub fn is_empty(&self) -> bool {
        matches!(self, Targets::Only(list) if list.is_empty())
    }
```

- [ ] **Step 4: 跑测试，确认它通过**

Run: `cargo test --lib targets::`
Expected: PASS 4/4。

- [ ] **Step 5: 跑全量测试**

Run: `cargo test`
Expected: 全绿。`tests/read_only_guard.rs::the_allowlist_is_not_the_whole_tree` 对 `src/capability.rs` 断言零违规——本条一个字都不碰写 API，它照样过。

- [ ] **Step 6: Commit**

```bash
git add src/capability.rs
git commit -m "feat(capability): add Targets::minus and Targets::is_empty"
```

---

### Task 4: `lower::SourceHarnessTable`（旁路表）

**Files:**
- Modify: `src/lower.rs:57`（`plugin_identity` 的可见性）、`src/lower.rs:133-165`（`DegradeTable` 一带）+ 同文件 `mod tests`

**Interfaces:**
- Consumes: `PluginEntry.source_harness`（Task 2）、`plugin_identity(key, p) -> String`（已存在，私有）。
- Produces:
  - `pub(crate) fn plugin_identity(key: &str, p: &PluginEntry) -> String`
  - `pub struct SourceHarnessTable { by_plugin: BTreeMap<String, String> }`
    - `pub fn new(m: &Manifest) -> Self`
    - `pub fn get(&self, plugin: &str) -> Option<&str>`
    - `pub fn declared(&self) -> BTreeSet<&str>`
  **Task 6 消费全部四个；Task 9 消费 `get`。**

**为什么走旁路表、不进 `Spec::Plugin`**（§9.5「来源与定位」）：`Spec` 的职责是「这东西装的是什么」——`diff` 的版本判决只认它，`§2.2 第 3 条`实测证明它不需要新字段。`source_harness` 是「henv 该去哪取内容」，与装的是什么无关。`DegradeTable` 已经立了这个先例：清单里的声明若与「装的是什么」无关，就放进它自己的表。

- [ ] **Step 1: 写失败的测试**

在 `src/lower.rs` 的 `mod tests` 里加：

```rust
    /// §9.5：源 harness 走 `DegradeTable` 那个先例的**旁路表**，不进 `Spec`。
    /// `Spec::Plugin` 的职责是「装的是什么」，`diff` 的版本判决只认它；
    /// 「从哪取内容」与装的是什么无关，混进去会让纯函数多知道一件不相干的事。
    #[test]
    fn source_harness_table_is_keyed_by_plugin_identity() {
        // key 是 `superpowers`，`entry` 是 `sp` → 身份取 `entry`（§2.1 第 3 条）
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: sp\n    source_harness: codex\n",
        )
        .expect("清单合法");
        let t = SourceHarnessTable::new(&m);
        assert_eq!(t.get("sp"), Some("codex"), "身份是 entry，不是 key");
        assert_eq!(t.get("superpowers"), None);
        assert_eq!(t.declared().into_iter().collect::<Vec<_>>(), vec!["codex"]);
    }

    /// 没写 `source_harness:` 的条目不进表——它的 `None` 才是「不拆解」的表达。
    #[test]
    fn a_plugin_without_a_source_harness_is_absent_from_the_table() {
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  p:\n    source: github\n    repo: x/y\n",
        )
        .expect("清单合法");
        let t = SourceHarnessTable::new(&m);
        assert_eq!(t.get("p"), None);
        assert!(t.declared().is_empty());
    }

    /// 两家都声明同一个源时 `declared` 只出现一次——它是「要扫哪几家」的输入，
    /// 重复会让 `scan_source_harnesses` 白扫一遍。
    #[test]
    fn declared_deduplicates() {
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  a:\n    source: github\n    repo: x/a\n    source_harness: codex\n  b:\n    source: github\n    repo: x/b\n    source_harness: codex\n",
        )
        .expect("清单合法");
        let t = SourceHarnessTable::new(&m);
        assert_eq!(t.declared().len(), 1);
    }
```

（若 `src/lower.rs` 的测试里已有别的构造 `Manifest` 的帮手，沿用它；上面用的是 `Manifest::parse`，与本文件既有测试一致。）

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib source_harness_table`
Expected: 编译失败，`cannot find type \`SourceHarnessTable\``。

- [ ] **Step 3: 实现**

`src/lower.rs`，把 `fn plugin_identity` 的 `fn` 改成 `pub(crate) fn`，并在 `DegradeTable` 的 `impl` 块之后插入：

```rust
/// 旁路表：plugin 身份 → 声明的源 harness（§9.5）。
///
/// 与 [`DegradeTable`] 同一个先例，同一个理由：清单里的这层声明**不进 `Spec`**。
/// `Spec::Plugin` 回答「装的是什么」，`diff` 的版本判决只读它；「从哪取内容」
/// 与装的是什么无关。两件事混进一个类型，纯函数就得知道一件它不需要知道的事。
///
/// **`None` 与「不在表里」是同一件事**：没写 `source_harness:` = 不拆解。
#[derive(Debug, Default)]
pub struct SourceHarnessTable {
    by_plugin: BTreeMap<String, String>,
}

impl SourceHarnessTable {
    pub fn new(m: &Manifest) -> Self {
        let mut by_plugin = BTreeMap::new();
        for (key, p) in &m.plugins {
            if let Some(s) = &p.source_harness {
                by_plugin.insert(plugin_identity(key, p), s.clone());
            }
        }
        Self { by_plugin }
    }

    pub fn get(&self, plugin: &str) -> Option<&str> {
        self.by_plugin.get(plugin).map(String::as_str)
    }

    /// 本次运行**要扫哪几家**——去重且有序（`BTreeSet`）。
    /// 被声明为源的家即便在 `harnesses:` 里被关掉也在这个集合里：它只是内容来源，
    /// 不是交付目标（§9.5）。
    pub fn declared(&self) -> BTreeSet<&str> {
        self.by_plugin.values().map(String::as_str).collect()
    }
}
```

若 `src/lower.rs` 顶部还没有 `use std::collections::BTreeSet;`，补上。

- [ ] **Step 4: 跑测试，确认它通过**

Run: `cargo test --lib source_harness_table`
Expected: PASS 3/3。

- [ ] **Step 5: 跑全量测试**

Run: `cargo test`
Expected: 全绿。`DegradeTable` 一行没动。

- [ ] **Step 6: Commit**

```bash
git add src/lower.rs
git commit -m "feat(lower): add SourceHarnessTable side table"
```

---

### Task 5: bundle manifest → 版本、`source_id`

**Files:**
- Create: `src/decompose.rs`（本 Task 只放 manifest 探针与 `source_id` 组装；Task 6 补齐拆解主体）
- Modify: `src/lib.rs:1-40` 一带的模块声明（加 `pub mod decompose;`）
- Test: `src/decompose.rs` 的 `mod tests`

**Interfaces:**
- Consumes: `crate::adapters::claude_code::read_json_if_present(path: &Path) -> anyhow::Result<Option<Value>>`（`src/adapters/claude_code.rs:230`，已是 `pub(crate)`；容器级 `null` 当缺文件）、`crate::capability::normalize_locator(Option<&str>) -> Option<String>`。
- Produces:
  - `const BUNDLE_MANIFESTS: [&str; 4]`
  - `pub(crate) fn read_bundle_manifest(install_path: &Path) -> anyhow::Result<Option<String>>`（返回**版本**）
  - `pub(crate) fn bundle_source_id(source_harness: &str, version: Option<&str>, sha: Option<&str>) -> String`
  **Task 6 消费这三个。**

**`source_id` 的形状**（§9.5）：`bundle@<source_harness>@<version>@<sha>`，**缺段整段省略、不补占位**。它只给人看，没有解析器——所以「`bundle@claude-code@abc123` 里 abc123 是版本还是 sha」这种歧义在本设计里是被接受的（要消歧的话得加占位符，那是往一个纯展示串里塞结构，不划算）。

**版本只进 `source_id`，不进任何判决**：`compare_versions` 一条都不碰它。bundle 的「过时不」由 `<install_path>/skills/<skill>/` 的哈希与账本 `content_hash` 比出来（§9.4），**不是**比版本号（版本号回答不了「这份拷贝的内容对不对」），更**不是**比 `gitCommitSha`（§2.2 第 7 条实测它是陈的）。

- [ ] **Step 1: 建文件与失败的测试**

创建 `src/decompose.rs`：

```rust
//! §9.5 的拆解一步：把「某家已安装的 plugin bundle」翻成一组**独立能力**。
//!
//! 这一步在 `compute` **之前**、`resolve_sources` **之前**跑，因为它是唯一
//! 知道「bundle 里有哪些 skill」的地方——而那正是内容层要去算哈希的路径。
//!
//! 为什么不是 `lower` 的一部分：`lower` 是纯函数，只看得见清单，够不着盘；
//! 而 skill 目录名只有读了 bundle 才知道。为什么不是 `resolve_sources` 的一部分：
//! 解析的职责是「给定一条能力，它的内容在哪」，而这一步**产出**能力。

use crate::capability::{Kind, Spec, normalize_locator};
use crate::home::Home;
use std::path::Path;

/// bundle 自带 manifest 的四种形状（§9.5）。按顺序取**第一个存在且解析得开**的。
///
/// 四种都对，因为一个 bundle 是谁造的只有它自己知道：claude 的 marketplace 缓存、
/// codex、kimi 各写各的，而 kimi 还多一个根目录下的形状。
const BUNDLE_MANIFESTS: [&str; 4] = [
    ".claude-plugin/plugin.json",
    ".codex-plugin/plugin.json",
    ".kimi-plugin/plugin.json",
    "kimi.plugin.json",
];

/// 读 bundle 自带 manifest 里声明的**版本**。
///
/// - 四个位置一个都不存在 → `Ok(None)`：**不是错误**，不是每个 bundle 都自带 manifest。
/// - 存在但读不开 / 解不开 → `Err`：那份文件坏了，我们说不清它声明的是什么，
///   而「猜一个 None」会让一条坏 bundle 看起来和一条朴素 bundle 一样。
/// - 存在且解得开但没有 `version` → `Ok(None)`：**不再往下找**。同一个 bundle
///   里出现两份 manifest 本身就是怪事，第一份命中即止比「挑一份有版本号的」好解释。
///
/// 字段按宽松读（`get("version").and_then(Value::as_str)`）：这里只取一个装饰性的
/// 版本串，为它建一套形状原语是杀鸡用牛刀。归一化仍然过 [`normalize_locator`]，
/// 于是本机写着的字面量 `unknown` 不会变成一个假的版本段。
#[allow(dead_code)] // Task 6 用
pub(crate) fn read_bundle_manifest(install_path: &Path) -> anyhow::Result<Option<String>> {
    for rel in BUNDLE_MANIFESTS {
        let p = install_path.join(rel);
        if !p.exists() {
            continue;
        }
        let Some(v) = crate::adapters::claude_code::read_json_if_present(&p)? else {
            continue; // 文件里写着 `null`：与缺段同义，继续找下一个形状
        };
        return Ok(normalize_locator(v.get("version").and_then(|x| x.as_str())));
    }
    Ok(None)
}

/// §9.5 的 `source_id`：`bundle@<source_harness>@<version>@<sha>`，缺段整段省略。
///
/// 纯展示串，**没有任何解析器**，也不参与任何判决（`compare_versions` 不看它）。
/// 它唯一的作用是让账本里的一行回答「这份拷贝是从哪来的」。
pub(crate) fn bundle_source_id(
    source_harness: &str,
    version: Option<&str>,
    sha: Option<&str>,
) -> String {
    let mut s = format!("bundle@{source_harness}");
    for seg in [version, sha] {
        if let Some(seg) = seg {
            s.push('@');
            s.push_str(seg);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    #[test]
    fn source_id_drops_missing_segments() {
        assert_eq!(
            bundle_source_id("claude-code", Some("6.4.1"), Some("abc123")),
            "bundle@claude-code@6.4.1@abc123"
        );
        assert_eq!(
            bundle_source_id("claude-code", Some("6.4.1"), None),
            "bundle@claude-code@6.4.1"
        );
        assert_eq!(bundle_source_id("claude-code", None, None), "bundle@claude-code");
        assert_eq!(
            bundle_source_id("codex", None, Some("abc123")),
            "bundle@codex@abc123"
        );
    }

    #[test]
    fn a_bundle_without_a_manifest_has_no_version() {
        let t = TempHome::new();
        t.mkdir("bundle/skills/foo");
        assert_eq!(
            read_bundle_manifest(&t.path("bundle")).expect("缺 manifest 不是错误"),
            None
        );
    }

    #[test]
    fn the_claude_manifest_shape_is_read() {
        let t = TempHome::new();
        t.write(
            "bundle/.claude-plugin/plugin.json",
            r#"{"name": "superpowers", "version": "6.4.1"}"#,
        );
        assert_eq!(
            read_bundle_manifest(&t.path("bundle")).unwrap().as_deref(),
            Some("6.4.1")
        );
    }

    /// kimi 的第二种形状：manifest 就躺在 bundle 根目录下。
    #[test]
    fn the_kimi_root_manifest_shape_is_read() {
        let t = TempHome::new();
        t.write("bundle/kimi.plugin.json", r#"{"version": "6.3.0"}"#);
        assert_eq!(
            read_bundle_manifest(&t.path("bundle")).unwrap().as_deref(),
            Some("6.3.0")
        );
    }

    /// 存在但解不开 = 报错，**不**退化成 `None`。半截 JSON 说明这份 bundle 是坏的，
    /// 而「坏」与「朴素」在报告里必须是两件事（Task 6 把它落成该条目的一条 undeliverable）。
    #[test]
    fn a_broken_manifest_is_an_error_not_a_missing_version() {
        let t = TempHome::new();
        t.write("bundle/.claude-plugin/plugin.json", r#"{"version": "6.4"#);
        let e = read_bundle_manifest(&t.path("bundle")).expect_err("半截 JSON 是坏的");
        assert!(format!("{e:#}").contains("plugin.json"), "{e:#}");
    }

    /// 本机在版本位写着的字面量 `unknown` 不变成假版本段（`normalize_locator` 的职责）。
    #[test]
    fn the_literal_unknown_is_not_a_version() {
        let t = TempHome::new();
        t.write(
            "bundle/.claude-plugin/plugin.json",
            r#"{"version": "unknown"}"#,
        );
        assert_eq!(read_bundle_manifest(&t.path("bundle")).unwrap(), None);
    }
}
```

在 `src/lib.rs` 的模块声明区加一行。那里的风格是**按字母序的 `pub mod`**（`src/lib.rs:21-31`：`adapters` → `apply` → `capability` → `diff` → … → `store`），所以插在 `capability` 与 `diff` 之间：

```rust
pub mod decompose;
```

`pub mod`（不是 `pub(crate) mod`）是照既有风格：`decompose.rs` 里没有任何东西要暴露给库外，但这份文件里除 `testutil` 外一律 `pub mod`，跟着走。

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib decompose::tests`
Expected: 编译失败，`cannot find module \`decompose\``（还没加模块声明），或加了声明后 `read_bundle_manifest` 未实现。**先把模块声明加上再跑**，看到的是「函数体是空的 / 变量未使用」这类失败即可。

- [ ] **Step 3: 实现**

上面 Step 1 已给出完整实现（`BUNDLE_MANIFESTS` / `read_bundle_manifest` / `bundle_source_id`）。若 Step 1 是照抄，本步为空——但仍要跑 Step 4。

- [ ] **Step 4: 跑测试，确认它通过**

Run: `cargo test --lib decompose::tests`
Expected: PASS 6/6。若 `#[allow(dead_code)]` 报「未使用」，确认 `bundle_source_id` 与 `read_bundle_manifest` 都还没被调用（Task 6 会调用），此时保留该标注；Task 6 落地后删掉它。

- [ ] **Step 5: 跑全量测试**

Run: `cargo test`
Expected: 全绿。`src/decompose.rs` 只读文件（`read_json_if_present` 是只读的），`tests/read_only_guard.rs` 不受影响。

- [ ] **Step 6: Commit**

```bash
git add src/decompose.rs src/lib.rs
git commit -m "feat(decompose): read the bundle manifest for a version, compose source_id"
```

---

### Task 6: `decompose_bundles` —— 定位、枚举、装不下的部分

**Files:**
- Modify: `src/store/skills.rs`（新增 `LocatedBundle` / `BundleTable`，紧挨 `ResolvedContent`）
- Modify: `src/decompose.rs`（补齐主体）+ 同文件 `mod tests`

**Interfaces:**
- Consumes: `lower::{plugin_identity, SourceHarnessTable}`（Task 4）、`Targets::minus`（Task 3）、`PluginEntry.source_harness`（Task 2）、`read_bundle_manifest` / `bundle_source_id`（Task 5）、`crate::skills_dir::try_read_skill_entries(dir: &Path, home: &Home) -> anyhow::Result<Vec<SkillEntry>>`、`crate::adapters::{registry, HarnessAdapter}`。
- Produces:
  - `src/store/skills.rs`：`pub struct LocatedBundle { pub install_path: PathBuf, pub source_id: String }`、`pub type BundleTable = BTreeMap<String, LocatedBundle>`
  - `src/decompose.rs`：
    - `pub enum SourceState { NotInstalled, Scanned(Vec<Capability>) }`
    - `pub fn scan_source_harnesses(sources: &SourceHarnessTable, registry: &[Box<dyn HarnessAdapter>]) -> anyhow::Result<BTreeMap<String, SourceState>>`
    - `#[derive(Debug, Default)] pub struct Decomposition { pub capabilities: Vec<Capability>, pub notes: BTreeMap<String, String>, pub failures: BTreeMap<String, String>, pub locations: BundleTable }`
    - `pub fn decompose_bundles(manifest: &Manifest, source_states: &BTreeMap<String, SourceState>, known: &[&str], home: &Home) -> anyhow::Result<Decomposition>`
  **Task 7 消费 `BundleTable`；Task 9 消费全部四个。**
  > **2026-09-26 更正（收尾轮之二）**：`scan_source_harnesses` 交付时多了一个参数
  > `already: &BTreeMap<String, SourceState>`（同时 `SourceState` 派生 `Clone`），
  > 用来让「既是启用目标又是清单声明的源」的那家只读一次 `read_state()`；
  > `decompose_bundles` 里另加了两条判据（同名 `CapId` 碰撞、交付目标全被
  > `harnesses:` 关掉），都只改 `capabilities`/`failures` 两个表的内容。
  > 本文件下面 Task 10 代码块里那份签名同样以 `src/decompose.rs` 的现码为准。

**三张表的键统一是 plugin 身份**（`plugin_identity(key, p)` 的产物）：`failures` 与 `notes` 由 `compute` 按 `CapId::new(Kind::Plugin, identity)` 与 `Origin::Plugin{name}` 取回，`locations` 由 `resolve_sources` 按 `SkillSource::PluginBundle{plugin}` 取回。

**`Decomposition` 永远不返回 `Err` 来表达「某一条坏了」**：条目级失败进 `failures`（§9.7 失败隔离）。唯一的 `Err` 来自 `try_read_skill_entries` 的 I/O 失败——那是「这个目录读不了」，读不了就不能假装它里面没有 skill。

- [ ] **Step 1: 写失败的测试**

在 `src/decompose.rs` 的 `mod tests` 里加（并加所需 use）：

```rust
    use crate::capability::{CapId, Capability, Origin, SkillSource, Targets};
    use crate::lower::SourceHarnessTable;
    use crate::manifest::Manifest;
    use std::collections::{BTreeMap, BTreeSet};

    const KNOWN: [&str; 3] = ["claude-code", "codex", "kimi-code"];

    /// 一个「claude-code 装了 superpowers」的源状态：安装路径指向 `t` 下那个 bundle。
    fn claude_with(install_path: std::path::PathBuf, sha: Option<&str>) -> BTreeMap<String, SourceState> {
        let mut m = BTreeMap::new();
        m.insert(
            "claude-code".to_string(),
            SourceState::Scanned(vec![Capability {
                id: CapId::new(Kind::Plugin, "superpowers"),
                origin: Origin::Adopted {
                    harness: "claude-code".into(),
                },
                spec: Spec::Plugin {
                    marketplace: Some("official".into()),
                    entry: "superpowers".into(),
                    version: Some("6.4.1".into()),
                    sha: sha.map(str::to_string),
                    install_path: Some(install_path),
                    enabled: true,
                },
                targets: Targets::All,
            }]),
        );
        m
    }

    fn manifest_with_source_harness() -> Manifest {
        Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n",
        )
        .expect("清单合法")
    }

    /// 一个装了 superpowers 的假 HOME：`bundle/` 是安装目录，里面两个 skill。
    fn home_with_bundle() -> (TempHome, std::path::PathBuf) {
        let t = TempHome::new();
        let root = t.path("bundle");
        t.write("bundle/skills/brainstorming/SKILL.md", "---\nname: b\n---\n");
        t.write("bundle/skills/tdd/SKILL.md", "---\nname: t\n---\n");
        t.write(
            "bundle/.claude-plugin/plugin.json",
            r#"{"version": "6.4.1"}"#,
        );
        (t, root)
    }

    /// §9.5「交付什么」：`<install_path>/skills/<skill>/` 下每个目录各是一条
    /// 独立的 `Kind::Skill`，`Origin::Plugin`，`SkillSource::PluginBundle`。
    /// 它们**不是** `lower` 的产出——skill 目录名只有读了 bundle 才知道。
    #[test]
    fn each_skill_directory_becomes_a_capability() {
        let (t, root) = home_with_bundle();
        let m = manifest_with_source_harness();
        let d = decompose_bundles(&m, &claude_with(root.clone(), None), &KNOWN, &t.home).unwrap();

        let mut names: Vec<&str> = d
            .capabilities
            .iter()
            .map(|c| c.id.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["brainstorming", "tdd"]);
        assert!(d.failures.is_empty(), "{:?}", d.failures);

        let cap = &d.capabilities[0];
        assert_eq!(cap.id.kind, Kind::Skill);
        assert_eq!(
            cap.origin,
            Origin::Plugin {
                name: "superpowers".into()
            }
        );
        match &cap.spec {
            Spec::Skill { source, path } => {
                assert_eq!(
                    source,
                    &SkillSource::PluginBundle {
                        plugin: "superpowers".into()
                    }
                );
                assert_eq!(*path, None, "清单侧声明的内容路径由内容层解析，不在这里填");
            }
            other => panic!("{other:?}"),
        }
    }

    /// §9.5「开关与目标」：交付目标 = `targets` − 源 harness。
    /// 清单没写 `targets`（默认 `All`），于是拆出来的 skill 目标是另外两家。
    #[test]
    fn the_source_harness_is_subtracted_from_targets() {
        let (t, root) = home_with_bundle();
        let m = manifest_with_source_harness();
        let d = decompose_bundles(&m, &claude_with(root, None), &KNOWN, &t.home).unwrap();
        assert_eq!(
            d.capabilities[0].targets,
            Targets::Only(vec!["codex".into(), "kimi-code".into()])
        );
    }

    /// §9.5：定位得到时报文取「安装记录里的路径」，并组出 `source_id`。
    #[test]
    fn the_bundle_is_located_and_gets_a_source_id() {
        let (t, root) = home_with_bundle();
        let m = manifest_with_source_harness();
        let d = decompose_bundles(
            &m,
            &claude_with(root.clone(), Some("abc123")),
            &KNOWN,
            &t.home,
        )
        .unwrap();
        let located = d.locations.get("superpowers").expect("定位到了");
        assert_eq!(located.install_path, root);
        assert_eq!(located.source_id, "bundle@claude-code@6.4.1@abc123");
    }

    /// 源 harness 本机没装：整条 undeliverable，报文点名是哪一家没装。
    #[test]
    fn an_uninstalled_source_harness_fails_the_whole_entry() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let mut states = BTreeMap::new();
        states.insert("claude-code".to_string(), SourceState::NotInstalled);

        let d = decompose_bundles(&m, &states, &KNOWN, &t.home).unwrap();
        assert!(d.capabilities.is_empty());
        let msg = d.failures.get("superpowers").expect("要报出来");
        assert!(msg.contains("claude-code"), "{msg}");
        assert!(msg.contains("没装"), "{msg}");
    }

    /// 源家装了别的 plugin、没有这一条：与「那家没装」是**两件事**——
    /// 前者要用户去装这个 plugin，后者要用户去装那家 harness。
    #[test]
    fn a_harness_without_this_plugin_says_so_distinctly() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let states = claude_with(t.path("does-not-matter"), None);
        // 把那条记录的名字改掉，模拟「装了 claude-code，但装的是别的 plugin」
        let mut states = states;
        if let Some(SourceState::Scanned(caps)) = states.get_mut("claude-code") {
            caps[0].id = CapId::new(Kind::Plugin, "something-else");
        }

        let d = decompose_bundles(&m, &states, &KNOWN, &t.home).unwrap();
        let msg = d.failures.get("superpowers").expect("要报出来");
        assert!(msg.contains("没有这个 plugin"), "{msg}");
    }

    /// **codex 恒为这一支**：它的安装记录里根本没有路径（`Spec::Plugin::install_path`
    /// 永远是 `None`，`src/adapters/codex.rs:113-119` 是那条已知局限）。
    /// 报文必须与「没装这个 plugin」分得开——否则用户会去重装一个已经装好的 plugin。
    #[test]
    fn a_record_without_an_install_path_says_so_distinctly() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let mut states = claude_with(t.path("irrelevant"), None);
        if let Some(SourceState::Scanned(caps)) = states.get_mut("claude-code") {
            if let Spec::Plugin { install_path, .. } = &mut caps[0].spec {
                *install_path = None;
            }
        }

        let d = decompose_bundles(&m, &states, &KNOWN, &t.home).unwrap();
        let msg = d.failures.get("superpowers").expect("要报出来");
        assert!(msg.contains("没有安装路径"), "{msg}");
        assert!(!msg.contains("没有这个 plugin"), "两句必须分得开：{msg}");
    }

    /// §9.5「装不下的部分」：`hooks/` 只探存在性、不解析内容，按 `degrade.<kind>`
    /// 逐 kind 决定进报告还是丢弃。默认是 `report`。
    #[test]
    fn leftover_parts_land_in_the_note_when_degrade_says_report() {
        let (t, root) = home_with_bundle();
        t.mkdir("bundle/hooks");
        let m = manifest_with_source_harness();
        let d = decompose_bundles(&m, &claude_with(root, None), &KNOWN, &t.home).unwrap();
        let note = d.notes.get("superpowers").expect("有装不下的部分就要留话");
        assert!(note.contains("hook"), "{note}");
        assert!(!note.contains("command"), "commands/ 不存在，不许凭空说：{note}");
    }

    /// `degrade.hooks: skip` → 那一项一个字都不留。
    #[test]
    fn degrade_skip_drops_a_leftover_part_silently() {
        let (t, root) = home_with_bundle();
        t.mkdir("bundle/hooks");
        t.mkdir("bundle/commands");
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: official\n    entry: superpowers\n    source_harness: claude-code\n    degrade:\n      hooks: skip\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &claude_with(root, None), &KNOWN, &t.home).unwrap();
        let note = d.notes.get("superpowers").expect("commands/ 还在，注释仍要有");
        assert!(!note.contains("hook"), "skip 的必须消失：{note}");
        assert!(note.contains("command"), "{note}");
    }

    /// 没有 `source_harness:` 的条目**不进结果表**——它不是拆解请求。
    #[test]
    fn a_plugin_without_a_source_harness_is_not_decomposed() {
        let (t, _root) = home_with_bundle();
        let m = Manifest::parse(
            "version: 1\n\n\
             plugins:\n  superpowers:\n    source: github\n    repo: x/y\n",
        )
        .expect("清单合法");
        let d = decompose_bundles(&m, &BTreeMap::new(), &KNOWN, &t.home).unwrap();
        assert!(d.capabilities.is_empty());
        assert!(d.failures.is_empty());
        assert!(d.notes.is_empty());
        assert!(d.locations.is_empty());
    }

    /// 源家被 `harnesses:` 关掉也照样扫：`source_harness:` 只请求**读**它，
    /// 不请求往它写（§9.5）。这一条钉的是扫描那一步的选择，
    /// 「不因此多出一节 diff」由 Task 9 钉。
    #[test]
    fn a_disabled_harness_is_still_scanned_when_it_is_a_source() {
        let (t, root) = home_with_bundle();
        // claude-code 被关掉、codex 被关掉，只剩 kimi 是「开着」的目标
        t.write(
            "env.yaml",
            "version: 1\n\nharnesses:\n  claude-code:\n    enabled: false\n  codex:\n    enabled: false\n",
        );
        let manifest = Manifest::parse(
            "version: 1\n\n\
             harnesses:\n  claude-code:\n    enabled: false\n",
        )
        .expect("清单合法");
        let table = SourceHarnessTable::new(&manifest);
        let registry = crate::adapters::registry(&t.home);

        // 造一个真实的 claude 安装记录，让 read_state 真的能扫出那条 plugin
        t.mkdir(".claude");
        t.write(
            ".claude/plugins/installed_plugins.json",
            &format!(
                r#"{{"plugins":{{"superpowers@official":[{{"installPath":"{}","version":"6.4.1"}}]}}}}"#,
                root.display()
            ),
        );
        t.write(".claude/settings.json", r#"{"enabledPlugins":{}}"#);

        let states = scan_source_harnesses(&table, &registry).unwrap();
        match states.get("claude-code") {
            Some(SourceState::Scanned(caps)) => assert!(
                caps.iter().any(|c| c.id == CapId::new(Kind::Plugin, "superpowers")),
                "被关掉的家仍然要被读到"
            ),
            other => panic!("claude-code 应该被扫到：{other:?}"),
        }
    }

    /// 源家本机没装（`detect()` 假）→ `NotInstalled`，**不是**空表——
    /// 「没装」与「装了但什么都没扫到」必须是两种状态（否则「没装」会被报成
    /// 「没有这个 plugin」，指错方向）。
    #[test]
    fn a_missing_source_harness_scans_as_not_installed() {
        let t = TempHome::new();
        let m = manifest_with_source_harness();
        let table = SourceHarnessTable::new(&m);
        let registry = crate::adapters::registry(&t.home);
        let states = scan_source_harnesses(&table, &registry).unwrap();
        assert!(matches!(
            states.get("claude-code"),
            Some(SourceState::NotInstalled)
        ));
    }

    /// **只扫被声明的那些家。** 没有任何 `source_harness:` 时一次 `read_state` 都不发。
    #[test]
    fn only_declared_harnesses_are_scanned() {
        let t = TempHome::new();
        let m = Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n",
        )
        .expect("清单合法");
        let table = SourceHarnessTable::new(&m);
        assert_eq!(table.declared(), BTreeSet::new());
        let registry = crate::adapters::registry(&t.home);
        assert!(scan_source_harnesses(&table, &registry).unwrap().is_empty());
    }
```

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib decompose::tests`
Expected: 编译失败，`cannot find function \`decompose_bundles\``。

- [ ] **Step 3: 加两个类型到 `src/store/skills.rs`**

在 `ResolvedContent` 定义之后、`TargetContent` 之前插入：

```rust
/// 一个被定位到的 bundle：它在盘上的根，以及该写进账本的来源标识（§9.5）。
///
/// 这是拆解那一步的产物、内容层的入参——所以类型住在这里而不是 `decompose`：
/// 内容层声明它**需要什么**，拆解那一步负责**给出来**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedBundle {
    /// 源 harness 的安装记录里写的安装根目录。
    pub install_path: PathBuf,
    /// `bundle@<来源>@<版本>@<sha>`，缺段省略（纯展示串，见 `decompose::bundle_source_id`）。
    pub source_id: String,
}

/// plugin 身份 → 定位到的 bundle。拆解没定位到的条目**不在表里**——
/// 那时内容层该报的是「这个 bundle 没定位到」，而那条已经由 `Kind::Plugin` 的
/// `undeliverable` 行说了（§9.5），这里不必再说一遍。
pub type BundleTable = BTreeMap<String, LocatedBundle>;
```

- [ ] **Step 4: 实现 `scan_source_harnesses` 与 `decompose_bundles`**

在 `src/decompose.rs` 里补齐（把 Task 5 那两个函数上面的 `#[allow(dead_code)]` 去掉）：

```rust
/// 一个源 harness 这次扫描的结果。
///
/// **两态而不是 `Option<Vec<_>>`**：「没装这家」与「装了但按它自己说的什么都没扫到」
/// 必须是两种状态——前者要报「`claude-code` 本机没装」，后者要报
/// 「`claude-code` 的安装记录里没有这个 plugin」。把两者压成空的 `Vec`，
/// 前者就会被报成后者，指着一个已经装好的 harness 让用户去重装。
#[derive(Debug)]
pub enum SourceState {
    NotInstalled,
    Scanned(Vec<Capability>),
}

/// 按需扫描被声明为源的 harness（§9.5）。
///
/// **不参与 `targets`**：`source_harness:` 只请求**读**它，不请求往它写。
/// 把它收进 `plan_for` 的 `pairs` 会让它凭空多出一节 diff，而它在 `harnesses:`
/// 里很可能是关掉的——那正是「我只想拿它的内容」的写法。
///
/// 用的是同一个 `read_state()`：拆解不许有第二条读路径，否则
/// 「diff 里读到的」与「拆解取到的」能不一致，而消灭这两种不一致正是这个项目的存在理由。
pub fn scan_source_harnesses(
    sources: &SourceHarnessTable,
    registry: &[Box<dyn HarnessAdapter>],
) -> anyhow::Result<BTreeMap<String, SourceState>> {
    let mut out = BTreeMap::new();
    for name in sources.declared() {
        let Some(a) = crate::adapters::lookup(registry, name) else {
            // 校验期已保证值域在 KNOWN_HARNESSES 内，而 registry 与它逐个相等
            // （`src/adapters/mod.rs:173` 把两者钉成相等）。走到这里是代码 bug，
            // 但按「没装」处理比 panic 好：报告里会显式说出来，不是静默。
            out.insert(name.to_string(), SourceState::NotInstalled);
            continue;
        };
        if !a.detect() {
            out.insert(name.to_string(), SourceState::NotInstalled);
            continue;
        }
        // 上抛：这家扫不了是这台机器的问题，与 `plan_for` 里既有的几处同样处理。
        let caps = a
            .read_state()
            .with_context(|| format!("cannot scan source harness {name} state"))?;
        out.insert(name.to_string(), SourceState::Scanned(caps));
    }
    Ok(out)
}

/// 拆解这一步的全部产物。四张表，键统一是 **plugin 身份**。
///
/// 为什么不返回 `Result` 来表达「某一条坏了」：条目级失败是**正常结果**，
/// 不是异常（§9.7 失败隔离）。唯一的 `Err` 来自目录读不了——
/// 那是「我们说不清那里有什么」，不能假装成「那里什么都没有」。
#[derive(Debug, Default)]
pub struct Decomposition {
    /// 追加进 `desired` 的独立能力。**`lower` 的产物不含它们**——
    /// `lower` 是纯函数、够不着盘，而 skill 目录名只有读了 bundle 才知道。
    pub capabilities: Vec<Capability>,
    /// plugin 身份 → 报告注记（§9.5「装不下的部分」）。
    pub notes: BTreeMap<String, String>,
    /// plugin 身份 → 失败理由，成品的整句，可直接进 `undeliverable`。
    pub failures: BTreeMap<String, String>,
    /// plugin 身份 → 定位到的 bundle。
    pub locations: BundleTable,
}

/// §9.5 的拆解主体：清单里写了的每一条 plugin，去源家的已安装 bundle 里找东西。
///
/// `known` 是「有哪几家 harness」（`compute` 手里那几家的名字），
/// 用来把 `Targets::All` 具体化成「除源之外的几家」。
pub fn decompose_bundles(
    manifest: &Manifest,
    source_states: &BTreeMap<String, SourceState>,
    known: &[&str],
    home: &Home,
) -> anyhow::Result<Decomposition> {
    let mut out = Decomposition::default();

    for (key, entry) in &manifest.plugins {
        let Some(source) = entry.source_harness.as_deref() else {
            continue;
        };
        let identity = plugin_identity(key, entry);

        // ① 定位：源家的安装记录里那条 `Kind::Plugin` 的 `install_path`。
        //    消息三态必须分得开，因为三种失败要用户做三件不同的事。
        let located = match source_states.get(source) {
            Some(SourceState::Scanned(caps)) => {
                let actual = caps
                    .iter()
                    .find(|c| c.id == CapId::new(Kind::Plugin, identity.clone()));
                match actual {
                    None => Err(format!(
                        "plugin `{identity}`：`{source}` 的安装记录里没有这个 plugin（§9.5）"
                    )),
                    Some(c) => match &c.spec {
                        Spec::Plugin {
                            install_path: Some(p),
                            ..
                        } => Ok(p.clone()),
                        // codex 恒为这一支：它的 config.toml 不记路径。
                        _ => Err(format!(
                            "plugin `{identity}`：`{source}` 的安装记录里没有安装路径（§9.5）"
                        )),
                    },
                }
            }
            _ => Err(format!(
                "plugin `{identity}`：源 harness `{source}` 本机没装，取不到 bundle（§9.5）"
            )),
        };
        let install_path = match located {
            Ok(p) => p,
            Err(reason) => {
                out.failures.insert(identity, reason);
                continue;
            }
        };

        // ② 版本只进 `source_id`（§9.5）。manifest 解不开 = 这条坏了，
        //    不退化成「没有版本」——那会让一条坏 bundle 与一条朴素 bundle 长得一样。
        let version = match read_bundle_manifest(&install_path) {
            Ok(v) => v,
            Err(e) => {
                out.failures
                    .insert(identity.clone(), format!("plugin `{identity}`：bundle manifest 解不开：{e:#}"));
                continue;
            }
        };
        let sha = match source_states.get(source) {
            Some(SourceState::Scanned(caps)) => caps
                .iter()
                .find(|c| c.id == CapId::new(Kind::Plugin, identity.clone()))
                .and_then(|c| match &c.spec {
                    Spec::Plugin { sha, .. } => sha.clone(),
                    _ => None,
                }),
            _ => None,
        };
        out.locations.insert(
            identity.clone(),
            LocatedBundle {
                install_path: install_path.clone(),
                source_id: bundle_source_id(source, version.as_deref(), sha.as_deref()),
            },
        );

        // ③ 拆：`<install_path>/skills/<skill>/` 每个目录各是一条独立能力。
        //    targets 减去源家——「写了就拆，源不交付」（§9.5）。
        let targets = entry.targets.minus(source, known);
        for e in try_read_skill_entries(&install_path.join("skills"), home)? {
            out.capabilities.push(Capability {
                id: CapId::new(Kind::Skill, e.name),
                origin: Origin::Plugin {
                    name: identity.clone(),
                },
                spec: Spec::Skill {
                    source: SkillSource::PluginBundle {
                        plugin: identity.clone(),
                    },
                    // 清单侧声明的内容路径由内容层解析（表里那个 `install_path`），
                    // 与 `lower` 给 `AgentsDir`/`Git` 的 `None` 同一个道理。
                    path: None,
                },
                targets: targets.clone(),
            });
        }

        // ④ 「装不下的部分」：**只探存在性、不解析内容**（§9.5）。
        //    它们进的是这条目的报告说明，**不为它们新建能力身份**——
        //    建了就会出现一堆永远 undeliverable 的行，把报告读成噪音。
        //    `degrade.<kind> = skip` 的那一项一个字都不留。
        let mut leftovers: Vec<&str> = Vec::new();
        let probe: [(Kind, &str); 3] = [
            (Kind::Hook, "hooks"),
            (Kind::Command, "commands"),
            (Kind::Agent, "agents"),
        ];
        for (kind, dir) in probe {
            if install_path.join(dir).is_dir()
                && entry.degrade.action_for(kind) == DegradeAction::Report
            {
                leftovers.push(kind.as_str());
            }
        }
        if (install_path.join(".mcp.json").is_file() || install_path.join("mcp.json").is_file())
            && entry.degrade.action_for(Kind::Mcp) == DegradeAction::Report
        {
            leftovers.push(Kind::Mcp.as_str());
        }
        if !leftovers.is_empty() {
            out.notes.insert(
                identity,
                format!(
                    "bundle `{source}` 里还有 {}，未拆解（degrade.<kind> = report；要静音把它改成 skip）（§9.5）",
                    leftovers.join("、")
                ),
            );
        }
    }

    // 稳定的顺序：与 `lower` 一样按 label 排。两条能力同名时谁在前不重要，
    // 但同一次输入两次运行必须一样（§9.10 的幂等）。
    out.capabilities.sort_by_key(|c| c.id.label());
    Ok(out)
}
```

补齐 `src/decompose.rs` 顶部的 use（`DegradeAction` 住在 `manifest`，不在 `capability`——`src/manifest.rs:107`；`entry.degrade` 是 `DegradePolicy`，它自己的字段就是 `DegradeAction`）：

```rust
use crate::adapters::HarnessAdapter;
use crate::capability::{Capability, Kind, Origin, SkillSource, Spec};
use crate::lower::{SourceHarnessTable, plugin_identity};
use crate::manifest::{DegradeAction, Manifest};
use crate::skills_dir::try_read_skill_entries;
use crate::store::skills::{BundleTable, LocatedBundle};
use anyhow::Context as _;
use std::collections::BTreeMap;
```

- [ ] **Step 5: 跑测试，确认它通过**

Run: `cargo test --lib decompose::tests`
Expected: PASS 15/15（Task 5 的 6 条 + 本 Task 的 9 条）。

- [ ] **Step 6: 跑全量测试**

Run: `cargo test`
Expected: 全绿。`src/store/skills.rs` 只多了两个类型定义，一行逻辑没动，它的 14 个调用点不受影响。

- [ ] **Step 7: Commit**

```bash
git add src/decompose.rs src/store/skills.rs
git commit -m "feat(decompose): locate bundles, enumerate bundled skills, probe leftovers"
```

---

### Task 7: `resolve_sources` 接 `&BundleTable`

**Files:**
- Modify: `src/store/skills.rs:56-66`（`ContentIndex` 加 `notes`）、`:77-112`（`resolve_sources` 签名与 `PluginBundle` 分支）+ 同文件 `mod tests`

**Interfaces:**
- Consumes: `BundleTable` / `LocatedBundle`（Task 6）。
- Produces:
  - `ContentIndex.notes: BTreeMap<String, String>`
  - `pub fn resolve_sources(desired: &[Capability], home: &Home, mode: SourceMode, bundles: &BundleTable) -> anyhow::Result<ContentIndex>`

**为什么内容是「表里的 `install_path` + `skills/<名字>`」而不是读 `Spec::Skill.path`**：`Spec::Skill.path` 在清单侧是「声明」（`lower` 给 `None`），在 adapter 侧是「实测」（`read_bundled_skills` 给 `Some`）。拆解出的能力属于**清单侧**，所以 `path: None`；它的内容位置由 `SkillSource::PluginBundle { plugin }` 指向的那张表回答——表是「这个 bundle 在哪」的唯一权威，因为它是定位那一步的产物。

**§9.4 的 bundle 过期判据就此自动落地**：`resolve_one` 对 `<install_path>/skills/<skill>/` 算 `hash_dir`，`content_verdict` 拿它与目标侧哈希、账本 `content_hash` 比。**不碰版本号**（版本号回答不了「这份拷贝的内容对不对」），**不碰 `gitCommitSha`**（§2.2 第 7 条实测它是陈的）。

- [ ] **Step 1: 写失败的测试**

在 `src/store/skills.rs` 的 `mod tests` 里加：

```rust
    /// §9.5「交付什么」：拆解出的 skill 的内容来自**定位到的 bundle**，
    /// 不是清单声明的路径（清单只声明了 `source_harness:`，没声明任何一个 skill）。
    #[test]
    fn a_plugin_bundle_skill_resolves_from_the_located_bundle() {
        let t = TempHome::new();
        t.write("bundle/skills/foo/SKILL.md", "hello\n");

        let cap = Capability {
            id: CapId::new(Kind::Skill, "foo"),
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
        let mut bundles = BundleTable::new();
        bundles.insert(
            "superpowers".to_string(),
            LocatedBundle {
                install_path: t.path("bundle"),
                source_id: "bundle@claude-code@6.4.1".into(),
            },
        );

        let idx = resolve_sources(
            std::slice::from_ref(&cap),
            &t.home,
            SourceMode::Cached,
            &bundles,
        )
        .unwrap();

        let r = idx.sources.get(&cap.id).expect("要解析出内容");
        assert_eq!(r.path, t.path("bundle/skills/foo"));
        assert_eq!(r.source_id, "bundle@claude-code@6.4.1");
        assert!(idx.unresolved.is_empty());
    }

    /// 表里没有这条 → 解析不出来，**理由要说是 bundle 没定位到**。
    /// 这条路径今天达不到（定位失败时拆解压根不产出能力），但它是内容层的
    /// 不变量：不许把「表里没有」静默成「源目录不存在」。
    #[test]
    fn a_plugin_bundle_skill_without_a_location_is_unresolved() {
        let t = TempHome::new();
        let cap = Capability {
            id: CapId::new(Kind::Skill, "foo"),
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
        let idx = resolve_sources(
            std::slice::from_ref(&cap),
            &t.home,
            SourceMode::Cached,
            &BundleTable::new(),
        )
        .unwrap();

        assert!(idx.sources.is_empty());
        match idx.unresolved.get(&cap.id) {
            Some(Unresolved::Failed(r)) => assert!(r.contains("superpowers"), "{r}"),
            other => panic!("{other:?}"),
        }
    }

    /// 四个空表的 `Default`：Plan A 的行为**逐字节**保持（这是既有 306 条测试的前提，
    /// 也是本计划能小步落地的基础）。加 `notes` 是第四个空表，不是第四种行为。
    #[test]
    fn an_empty_content_index_is_still_default() {
        let idx = ContentIndex::default();
        assert!(idx.sources.is_empty());
        assert!(idx.targets.is_empty());
        assert!(idx.unresolved.is_empty());
        assert!(idx.notes.is_empty());
    }
```

（`mod tests` 若还没有 `use crate::store::skills::{BundleTable, LocatedBundle};` 与 `SkillSource`、`Targets`，就近补。）

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib resolve_sources`
Expected: 编译失败，`expected 3 arguments, found 4` / `no field \`notes\``。

- [ ] **Step 3: 加 `notes` 字段**

`src/store/skills.rs` 的 `ContentIndex` 里，`unresolved` 之后加：

```rust
    /// plugin 身份 → 报告注记（§9.5「装不下的部分」：bundle 里的 hooks/commands/
    /// agents/MCP 只探了存在性，没拆）。
    ///
    /// 它**不参与任何判决**，只跟着那条目拆出来的 skill 行出现在报告里——
    /// 「它们进的是该条目的报告说明，不是 diff 行」。与 `sources`/`unresolved`/
    /// `targets` 一样，空表时行为与 Plan A 逐字节相同。
    pub notes: BTreeMap<String, String>,
```

并把 `ContentIndex` 的文档注释里「三个空表的 `Default`」改成「四个空表的 `Default`」。

- [ ] **Step 4: 改签名与分支**

`src/store/skills.rs`：

```rust
pub fn resolve_sources(
    desired: &[Capability],
    home: &Home,
    mode: SourceMode,
    bundles: &BundleTable,
) -> anyhow::Result<ContentIndex> {
```

`PluginBundle` 那一支从 `{}` 换成：

```rust
            // §9.5：拆解出的 skill 的内容来自**定位到的 bundle**。
            // 路径从表里重算，不读 `Spec::Skill.path`——表是「这个 bundle 在哪」的
            // 唯一权威（它是定位那一步的产物），而清单侧的 `path` 按约定是 `None`。
            SkillSource::PluginBundle { plugin } => match bundles.get(plugin) {
                Some(b) => resolve_one(
                    &mut out,
                    cap,
                    b.install_path.join("skills").join(&cap.id.name),
                    b.source_id.clone(),
                )?,
                None => {
                    out.unresolved.insert(
                        cap.id.clone(),
                        Unresolved::Failed(format!(
                            "plugin `{plugin}` 的 bundle 没有定位到，取不到它的内容（§9.5）"
                        )),
                    );
                }
            },
```

`SkillSource::Unknown` 那一支保持不变。

- [ ] **Step 5: 修所有调用点**

Run: `cargo build 2>&1 | grep "resolve_sources"` 然后逐个补第 4 参。

已知 14 处：生产 1 处（`src/lib.rs:102`，由 Task 9 改成传 `&decomposed.locations`），其余 13 处都在 `#[cfg(test)]` 块里，统一补 `&BundleTable::new()`。

- [ ] **Step 6: 跑测试，确认它通过**

Run: `cargo test --lib store::skills`
Expected: PASS，含新加的三条。

- [ ] **Step 7: 跑全量测试**

Run: `cargo test`
Expected: 全绿。**特别注意** `src/diff.rs` 与 `src/apply/mod.rs` 里那几个 `ContentIndex { ... }` 字面量（`src/apply/mod.rs:479`、`src/diff.rs:1289,1333,1345`）：它们必须补 `notes: BTreeMap::new()`（用 `..Default::default()` 也行，但**不许**把 `Default` 当默认参数——`ContentIndex` 是显式构造的，漏一个字段编译器会当场拦住，这正是要的）。

- [ ] **Step 8: Commit**

```bash
git add src/store/skills.rs src/lib.rs src/apply/mod.rs src/diff.rs
git commit -m "feat(store): resolve plugin-bundle skills from the located bundle"
```

---

### Task 8: kimi 的 `supports()` 去掉 `Origin::Plugin → Native` 捷径

**Files:**
- Modify: `src/adapters/kimi_code.rs:240-260`（`supports`）、`:148-153`（`read_bundled_skills` 的文档注释）+ 同文件 `mod tests`

**Interfaces:**
- Consumes: `Support::{Native, Unsupported}`、`Capability.id.kind`。
- Produces: `KimiCode::supports` 的新行为——**同一个 kind 无论来源一律同判**。Task 9、Task 10 依赖它。

**为什么必须改**：`supports` 只对**清单侧**能力调用（`src/diff.rs:365`，全文只有这一处）。B3b 是第一次让 `Origin::Plugin` 出现在**清单侧**——拆解出的 skill 就带着它。若不改，kimi 会把它判 `Native`，而 kimi 的 `skills_target_dir()` 是 `None`，`deliver_skills` 在 `src/apply/mod.rs:67` 提前返回空报告：报告写 `+ install skill/foo`，盘上什么都没写，两层还都不报。**这正是 B3b 要消灭的那类假成功换了个入口。**

判据必须落在「**这家有没有用户级 skills 目录**」，不是「它是不是来自 bundle」。kimi 没有——所以散装的、bundle 里拆出来的，一视同仁。

- [ ] **Step 1: 改写钉着旧行为的测试**

`src/adapters/kimi_code.rs` 的 `mod tests` 里，把 `loose_skill_is_refused_but_bundled_skill_is_native`（第 1176 行）**整条替换**为：

```rust
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
```

并把 `bundled_hook_is_native_too`（第 1222 行）整条替换为：

```rust
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
```

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib kimi_code::tests::every_skill_is_refused`
Expected: FAIL，`bundle 里的 skill 也必须 Unsupported, got Native`。这正是要的失败。

- [ ] **Step 3: 改 `supports`**

`src/adapters/kimi_code.rs`，把 `supports` 开头的

```rust
        // bundle 携带的能力随 kimi 原生 plugin 安装一起落地
        if matches!(cap.origin, Origin::Plugin { .. }) {
            return Support::Native;
        }
```

整段删掉，并把函数文档注释改成：

```rust
    /// kimi 原生接纳 MCP 与 Plugin 两种 kind；Skill/Hook/Command/Agent 在这家
    /// **没有用户级落点**，一律 `Unsupported`——**不论它来自哪里**。
    ///
    /// 判据是「这家有没有那个目录」，不是「它是不是来自 bundle」
    /// （`Origin::Plugin` 曾经在这里换到 `Native`，那是错的：kimi 的
    /// `skills_target_dir()` 是 `None`，判 `Native` 会让 `deliver_skills`
    /// 提前返回空报告，报告写着 `+ install` 而盘上什么都没写）。
    /// 拆解出的 skill 正是带着 `Origin::Plugin` 来的，所以这条判据必须落对。
```

`Kind::Skill` 那一支的 reason 换成（旧的「只能随 plugin bundle 分发」在今天已经不准确）：

```rust
            Kind::Skill => Support::Unsupported {
                reason: "kimi-code 无用户级 skills 目录，拆解出的 skill 在它这儿没有落点".to_string(),
            },
```

- [ ] **Step 4: 改 `read_bundled_skills` 的文档注释**

`src/adapters/kimi_code.rs:148-153` 那段注释里，删掉「Task 12 的 `supports()` 例外正认这一位：`Origin::Plugin` 的 bundle skill 判 Native」这一句，换成：

```rust
    /// 扫出来的每条都是**现状**（`Origin::Plugin` 表示「随着那个 plugin 装进来的」），
    /// 不是「henv 能往 kimi 写 skill」的证据——kimi 没有用户级 skills 目录，
    /// `supports()` 对这类能力一律 `Unsupported`（§9.5）。
```

- [ ] **Step 5: 跑测试，确认它通过**

Run: `cargo test --lib kimi_code`
Expected: PASS，含改写后的两条。`supports_mcp_and_plugin_natively` 不动且仍过——`Mcp`/`Plugin` 的判据没变。

- [ ] **Step 6: 跑全量测试**

Run: `cargo test`
Expected: 全绿。**若 `src/diff.rs` 里有测试依赖「kimi 判 bundle skill 为 Native」**（形如 `bundled_skill_of_a_desired_plugin_is_not_drift` 之外的某条），它的期望要跟着改——但先看清它钉的是哪个 harness：`FakeAdapter` 自己的 `supports` 与 kimi 无关，那部分不该动。

- [ ] **Step 7: Commit**

```bash
git add src/adapters/kimi_code.rs
git commit -m "fix(kimi): judge capability by target, not by bundle origin"
```

---

### Task 9: `diff::compute` 的 plugin 语义 + `lib.rs` 串线

**Files:**
- Modify: `src/diff.rs:323-333`（`compute` 签名）、`:352-460`（主体）、新增 `detached_plugin_rows` 与 `decomposed_skill_count`、`:1289,1333,1345` 附近的 `ContentIndex` 字面量 + 同文件 `mod tests`
- Modify: `src/lib.rs:85-160`（`plan_for` 重排）、`:167-168`（撒谎的注释）
- Modify: `src/lower.rs:133-140`（`DegradeTable` 撒谎的注释）、`src/store/skills.rs:105-107`（`PluginBundle` 撒谎的注释）
- Modify: `src/apply/mod.rs:498`（`compute` 调用点补第 6 参）

**Interfaces:**
- Consumes: Task 6 的 `Decomposition` / `scan_source_harnesses` / `SourceState`；Task 4 的 `SourceHarnessTable::get`；Task 3 的 `Targets::{minus, is_empty}`；Task 7 的 `ContentIndex.notes`。
- Produces: `pub fn compute(desired, inputs, degrade, content, ledger, sources: &SourceHarnessTable) -> Plan`。**没有别的 Task 消费它。**

**这一步落地的四条语义**（§9.5）：

1. 写了 `source_harness:` 的条目，**它的 `Kind::Plugin` 行在每一家都不出现**——顺利拆解时由拆出来的 skill 行承担，不顺利时由一条被「挂」到某一节去的 `undeliverable` 行承担。
2. **源家自己装着的那一份不是 drift**：那是我们取内容的地方。
3. 没写 `source_harness:` 的条目，`install`/`update` 改判 `undeliverable`，**`in-sync`/`unknown`/`conflict` 原样保留**——它仍然做版本对账，只是不再假装能装。
4. 拆解出的 skill 行带上该条目的**「装不下的部分」注记**。

- [ ] **Step 1: 写失败的测试**

在 `src/diff.rs` 的 `mod tests` 里加（`mod tests` 里已有 `no_degrade()` / `plan_a(desired, inputs)` 等帮手，沿用它们；再加一个空的旁路表帮手）：

```rust
    fn no_source_harness() -> SourceHarnessTable {
        SourceHarnessTable::default()
    }

    /// 一条写了 `source_harness:` 的 plugin 能力（`lower` 的产物形状 + 旁路表里有它）。
    fn plugin_with_source(name: &str) -> (Capability, SourceHarnessTable) {
        let m = crate::manifest::Manifest::parse(&format!(
            "version: 1\n\nplugins:\n  {name}:\n    source: github\n    repo: x/y\n    source_harness: claude-code\n"
        ))
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let table = SourceHarnessTable::new(&m);
        let cap = desired
            .into_iter()
            .find(|c| c.id.kind == Kind::Plugin)
            .expect("lower 会产出这条 plugin");
        (cap, table)
    }

    fn fake(harness: &str) -> FakeAdapter {
        FakeAdapter::native(harness) // `native` 已经 detected: true
    }

    /// 一批 adapter → 一批现状。
    ///
    /// **不返回 `HarnessInput`**：那个类型借 `state` 与 `adapter` 两样
    /// （`pub struct HarnessInput<'a> { state: &'a HarnessState, adapter: &'a dyn HarnessAdapter }`），
    /// 借不出函数体内的局部量。所以两样都由调用方持有，各返回一份。
    /// 既有的 `run_with_ledger` 在函数里内联了这两行；本 Task 的测试要一次给
    /// `compute` 两三家，才提出来。
    fn harness_states(adapters: Vec<FakeAdapter>) -> (Vec<HarnessState>, Vec<FakeAdapter>) {
        let states = adapters
            .iter()
            .map(|a| HarnessState {
                name: a.name().to_string(),
                detected: a.detect(),
                actual: a.read_state().unwrap(),
            })
            .collect();
        (states, adapters)
    }

    fn harness_inputs<'a>(
        states: &'a [HarnessState],
        adapters: &'a [FakeAdapter],
    ) -> Vec<HarnessInput<'a>> {
        states
            .iter()
            .zip(adapters)
            .map(|(state, adapter)| HarnessInput { state, adapter })
            .collect()
    }
```

每个用到多家的测试都照这个形状开头：

```rust
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
```

```rust
    /// §9.5：写了 `source_harness:`、顺利拆解 → **不产生 `Kind::Plugin` 行**。
    /// 今天它会落一条 `+ install plugin/x`——而 apply 一个字节都写不出来。
    #[test]
    fn a_decomposed_plugin_produces_no_plugin_row() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        // 拆出来的那条 skill：目标是除源之外的家
        let skill = Capability {
            id: CapId::new(Kind::Skill, "brainstorming"),
            origin: Origin::Plugin { name: "p".into() },
            spec: Spec::Skill {
                source: SkillSource::PluginBundle { plugin: "p".into() },
                path: None,
            },
            targets: Targets::Only(vec!["codex".into(), "claude-code".into()]),
        };

        let plan = compute(
            &[plugin.clone(), skill],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
        );

        for h in &plan.harnesses {
            assert!(
                !h.entries.iter().any(|e| e.id.kind == Kind::Plugin),
                "{} 不该有 plugin 行：{:?}",
                h.harness,
                h.entries
            );
        }
    }

    /// §9.5 第 3 条：差集为空（`targets` 只有源家）→ 一条 `undeliverable`，
    /// 理由写明「目标只有源 harness 自己」。落点在源家那一节。
    #[test]
    fn an_empty_delivery_target_set_lands_as_one_row_on_the_source() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n    source_harness: claude-code\n    targets: claude-code\n",
        )
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let sources = SourceHarnessTable::new(&m);
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
        );

        let claude = plan
            .harnesses
            .iter()
            .find(|h| h.harness == "claude-code")
            .unwrap();
        let row = claude
            .entries
            .iter()
            .find(|e| e.id.kind == Kind::Plugin)
            .expect("要有一条 plugin 行");
        match &row.action {
            Action::Undeliverable { reason } => {
                assert!(reason.contains("差集为空"), "{reason}");
                assert!(reason.contains("claude-code"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            plan.harnesses
                .iter()
                .flat_map(|h| &h.entries)
                .filter(|e| e.id.kind == Kind::Plugin)
                .count(),
            1,
            "整份计划里只许有一条 plugin 行"
        );
    }

    /// §9.5 第 3 条后半：拆不出任何 skill 时也要有一条 `undeliverable`。
    /// 少了这一条，一个只有 `hooks/` 的 bundle 在报告里会是**一片干净**。
    #[test]
    fn a_bundle_that_yields_no_skill_still_reports_one_row() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        // 清单里只有那条 plugin，`desired` 里一条 `Origin::Plugin` 的 skill 都没有
        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
        );
        let row = plan
            .harnesses
            .iter()
            .flat_map(|h| &h.entries)
            .find(|e| e.id.kind == Kind::Plugin)
            .expect("拆不出东西要报出来");
        match &row.action {
            Action::Undeliverable { reason } => assert!(reason.contains("拆不出"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    /// §9.5「来源与定位」：定位不到 → 整条 undeliverable，理由原样来自内容层，
    /// 不再被包一层通用话（那是 `ResolvedContent` 那条路的行为，这里是 plugin 的）。
    #[test]
    fn an_unlocated_bundle_carries_the_content_layer_reason() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        let mut content = ContentIndex::default();
        content.unresolved.insert(
            plugin.id.clone(),
            Unresolved::Failed("plugin `p`：`claude-code` 的安装记录里没有安装路径（§9.5）".into()),
        );

        let plan = compute(&[plugin], &inputs, &no_degrade(), &content, &Ledger::default(), &sources);
        let row = plan
            .harnesses
            .iter()
            .flat_map(|h| &h.entries)
            .find(|e| e.id.kind == Kind::Plugin)
            .expect("要报出来");
        match &row.action {
            Action::Undeliverable { reason } => assert!(reason.contains("没有安装路径"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    /// §9.5 的 drift 陷阱：源家**一定**扫得到那个 plugin（我们刚从这里取的
    /// `install_path`）。它绝不能被报成 `- drift`——那是「不在清单里的东西」，
    /// 而它恰恰是清单要求的。
    #[test]
    fn the_installed_copy_on_the_source_harness_is_not_drift() {
        let (plugin, sources) = plugin_with_source("p");
        // 源家的现状里有这条 plugin（Origin::Adopted，与 adapter 扫出来的同形）
        let (states, adapters) = harness_states(vec![
            fake("claude-code").with_caps(vec![FakeAdapter::plugin("p", None, None)]),
            fake("codex"),
        ]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &[plugin],
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &sources,
        );
        let claude = plan.harnesses.iter().find(|h| h.harness == "claude-code").unwrap();
        assert!(
            claude.entries.iter().all(|e| e.action != Action::Drift),
            "源家那份是内容来源，不是 drift：{:?}",
            claude.entries
        );
    }

    /// §9.5「开关与目标」后半：**没写** `source_harness:` 的条目，
    /// `install` 改判 `undeliverable`（不再假装能装），理由指路。
    #[test]
    fn an_install_for_a_plugin_without_a_source_harness_becomes_undeliverable() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n",
        )
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let (states, adapters) = harness_states(vec![fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
        );
        match &plan.harnesses[0].entries[0].action {
            Action::Undeliverable { reason } => {
                assert!(reason.contains("source_harness"), "指路要指对：{reason}");
                assert!(reason.contains("§9.5"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 同一条道理推到 `update`：理由里要带上「清单要什么、本机是什么」。
    #[test]
    fn an_update_for_a_plugin_without_a_source_harness_becomes_undeliverable() {
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n    ref: v2\n",
        )
        .expect("清单合法");
        let desired = crate::lower::lower(&m);
        let (states, adapters) = harness_states(vec![
            fake("codex").with_caps(vec![FakeAdapter::plugin("p", Some("v1"), None)]),
        ]);
        let inputs = harness_inputs(&states, &adapters);

        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
        );
        match &plan.harnesses[0].entries[0].action {
            Action::Undeliverable { reason } => {
                assert!(reason.contains("v2") && reason.contains("v1"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// **`in-sync` 与 `unknown` 原样保留**（§9.5）：没写 `source_harness:` 的条目
    /// 仍然做版本对账，只是不再假装能装。改判范围只到 install/update 为止。
    #[test]
    fn a_plugin_without_a_source_harness_keeps_in_sync_and_unknown() {
        // 版本同 → in-sync
        let m = crate::manifest::Manifest::parse(
            "version: 1\n\nplugins:\n  p:\n    source: github\n    repo: x/y\n    ref: v1\n",
        )
        .unwrap();
        let desired = crate::lower::lower(&m);
        let (states, adapters) = harness_states(vec![
            fake("codex").with_caps(vec![FakeAdapter::plugin("p", Some("v1"), None)]),
        ]);
        let inputs = harness_inputs(&states, &adapters);
        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
        );
        assert_eq!(plan.harnesses[0].entries[0].action, Action::InSync);

        // 版本无法判定 → unknown（codex 的常态：它不记版本）
        let (states, adapters) = harness_states(vec![
            fake("codex").with_caps(vec![FakeAdapter::plugin("p", None, None)]),
        ]);
        let inputs = harness_inputs(&states, &adapters);
        let plan = compute(
            &desired,
            &inputs,
            &no_degrade(),
            &ContentIndex::default(),
            &Ledger::default(),
            &no_source_harness(),
        );
        assert_eq!(plan.harnesses[0].entries[0].action, Action::Unknown);
    }

    /// §9.5「交付什么」：bundle 里装不下的部分（hooks/commands/...）进的是
    /// **该条目的报告说明**，跟着拆解出的 skill 行走，不是 diff 行、
    /// 也不为它们新建能力身份。
    #[test]
    fn the_leftover_note_rides_every_decomposed_skill_row() {
        let (plugin, sources) = plugin_with_source("p");
        let (states, adapters) = harness_states(vec![fake("claude-code"), fake("codex")]);
        let inputs = harness_inputs(&states, &adapters);
        let skill = Capability {
            id: CapId::new(Kind::Skill, "brainstorming"),
            origin: Origin::Plugin { name: "p".into() },
            spec: Spec::Skill {
                source: SkillSource::PluginBundle { plugin: "p".into() },
                path: None,
            },
            targets: Targets::Only(vec!["codex".into()]),
        };
        let mut content = ContentIndex::default();
        content.notes.insert(
            "p".into(),
            "bundle `claude-code` 里还有 hook，未拆解（degrade.<kind> = report）".into(),
        );

        let plan = compute(
            &[plugin, skill],
            &inputs,
            &no_degrade(),
            &content,
            &Ledger::default(),
            &sources,
        );
        let row = plan
            .harnesses
            .iter()
            .flat_map(|h| &h.entries)
            .find(|e| e.id.kind == Kind::Skill)
            .expect("拆出来的 skill 有一行");
        let note = row.note.as_deref().expect("注记要跟着走");
        assert!(note.contains("hook"), "{note}");
    }
```

（本 Task 的测试一律用上面那两个帮手起头。`HarnessInput` 借 `state` 与 `adapter`
两样，所以 `states`/`adapters` 两个局部量必须在 `compute` 调用之前声明、活在同一个块里。）

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib diff::tests`
Expected: 编译失败，`expected 5 arguments, found 6`。先把签名改了（Step 3）再跑，看到的是各条行为断言失败——那才是要的失败。

- [ ] **Step 3: 改 `compute` 签名**

`src/diff.rs`：

```rust
pub fn compute(
    desired: &[Capability],
    inputs: &[HarnessInput],
    degrade: &DegradeTable,
    content: &ContentIndex,
    ledger: &Ledger,
    sources: &SourceHarnessTable,
) -> Plan {
```

并把文档注释里那句「`content` 与 `ledger` 让「字节一不一样、是谁放的」参与判决」补一行：

```rust
/// `sources` 是 §9.5 的旁路表：这一条 plugin 的内容从哪一家取。
/// 它只回答「拆不拆、从哪拆」，不进 `Spec`、不参与版本判决。
```

- [ ] **Step 4: 加两个帮手**

在 `compute` 之前插入：

```rust
/// 这条 plugin 条目拆出了几条 skill（§9.5 的「拆不出东西」判据）。
///
/// 按 `desired` 数而不是另立一张计数表：拆解产的 skill 就在 `desired` 里，
/// 数它是最直接的读法，也省掉一处会在两次运行之间漂移的状态。
fn decomposed_skill_count(desired: &[Capability], plugin: &str) -> usize {
    desired
        .iter()
        .filter(|c| {
            c.id.kind == Kind::Skill
                && matches!(&c.origin, Origin::Plugin { name } if name == plugin)
        })
        .count()
}

/// 拆解条目的 `Kind::Plugin` 行（§9.5「开关与目标」）。
///
/// 顺利拆解时**一条都不产生**；三种情况各产生一条 `undeliverable`：
/// 定位不到、交付目标差集为空、拆不出任何 skill。这第三种是「一片干净」的缺口——
/// 一个只有 `hooks/` 的 bundle 若什么都不报，用户看到的报告与「一切正常」分不开。
///
/// **落点**：源 harness 那一节——三种情况的问题都在它身上（它是内容来源）。
/// 它没有一节时（在 `harnesses:` 里被关掉了）落第一家有节的；一家都没有时这一行
/// 无处可放。本函数的选择是「宁可落错一节、不可丢掉」（理由串自身点名了源家，
/// 落错也不会指错方向）。
///
/// **2026-09-26 更正：** 最后那种情形已经补上了落点——收尾时加了 `Plan.unattached`
/// 与 `render` 的 `(global)` 段（见上面 Review Focus 第 9 条的更正块）。
fn detached_plugin_rows(
    desired: &[Capability],
    inputs: &[HarnessInput],
    content: &ContentIndex,
    sources: &SourceHarnessTable,
) -> BTreeMap<String, Vec<PlanEntry>> {
    let known: Vec<&str> = inputs.iter().map(|i| i.state.name.as_str()).collect();
    let mut out: BTreeMap<String, Vec<PlanEntry>> = BTreeMap::new();

    for cap in desired {
        if cap.id.kind != Kind::Plugin {
            continue;
        }
        let Some(source) = sources.get(&cap.id.name) else {
            continue; // 没写 `source_harness:`：走 `Spec` 那条常规路，不在这里分行
        };
        let reason = match content.unresolved.get(&cap.id) {
            // 内容层已经写好了整句（定位不到、manifest 解不开），原样带上。
            Some(Unresolved::Failed(r) | Unresolved::Deferred(r)) => r.clone(),
            None if cap.targets.minus(source, &known).is_empty() => format!(
                "plugin `{}`：交付目标 = targets − `{source}`，差集为空（目标只有源 harness 自己）（§9.5）",
                cap.id.name
            ),
            None if decomposed_skill_count(desired, &cap.id.name) == 0 => format!(
                "plugin `{}`：bundle `{source}` 里拆不出任何 skill（§9.5）",
                cap.id.name
            ),
            None => continue,
        };
        let slot = inputs
            .iter()
            .find(|i| i.state.name == source && i.state.detected)
            .or_else(|| inputs.iter().find(|i| i.state.detected))
            .or_else(|| inputs.first());
        let Some(slot) = slot else {
            continue; // 一家 harness 都没有：无处可放（见 Review Focus 第 9 条）
        };
        out.entry(slot.state.name.clone()).or_default().push(PlanEntry::new(
            cap.id.clone(),
            Action::Undeliverable { reason },
        ));
    }
    out
}

/// §9.5：henv **不代装原生 plugin**（§9.1）。没写 `source_harness:` 的条目
/// 落在这个理由上——它仍然做版本对账，只是不再假装能装。
const NO_NATIVE_INSTALL: &str =
    "henv 不代装原生 plugin；写 `source_harness:` 可从某家的已安装 bundle 拆解交付（§9.5）";
```

- [ ] **Step 5: 改 `compute` 主体**

`src/diff.rs`，三处改动。

**(a) `Plan` 开头取走挂着的行。** 在 `for input in inputs {` 之前加：

```rust
    // §9.5：拆解条目的报错行按落点分好，等各节出场时取走。
    let mut detached = detached_plugin_rows(desired, inputs, content, sources);
```

并在 `let mut entries: Vec<PlanEntry> = Vec::new();` 之后（**在 `if !state.detected` 那个早退之后**，因为未安装的家本来就不出任何行）加：

```rust
        // 挂到这一节的拆解报错行（源家定位不到、差集为空、拆不出 skill）。
        entries.extend(detached.remove(&harness).unwrap_or_default());
```

**(b) 拆解条目的 plugin 行在每节都不出现。** 在现有的

```rust
        if !cap.targets.includes(&harness) {
            continue;
        }
```

**之前**插入：

```rust
            // §9.5：写了 `source_harness:` 的条目**不产生 `Kind::Plugin` 行**——
            // 顺利拆解时由拆出来的 skill 行承担，不顺利时由上面那批挂着的行承担。
            //
            // 源家那一段必须**先于** targets 判据：源家通常不在 targets 里
            // （交付目标 = targets − 源），但它的 `read_state()` 一定扫得到那个
            // plugin（我们刚从这里取的 `install_path`），不先吸收掉就会掉进
            // 下面的 drift 段，被报成「不在清单里的东西」——而它恰恰是清单要求的。
            if cap.id.kind == Kind::Plugin {
                if let Some(src) = sources.get(&cap.id.name) {
                    if harness == src {
                        matched.insert(cap.id.clone());
                        continue;
                    }
                    // 其余家：只要它是交付目标就吸收掉（这条的 plugin 行由拆出来的
                    // skill 承担）。**不在 targets 里的家照旧走 drift 判据**——
                    // 「清单说这条只给 codex」而 claude 也有时，今天报 drift，
                    // 这个行为不属于 B3b 要改的范围。
                    if cap.targets.includes(&harness) {
                        matched.insert(cap.id.clone());
                        continue;
                    }
                }
            }
```

（`harness == src`：`src` 是 `&str`，`harness` 是 `String`，写成 `harness == src` 会因 `String: PartialEq<&str>` 而成立——若不成立就写 `harness.as_str() == src`。注意 **1.85 没有 let-chains**，所以 `if ... { if let Some(src) = ... { ... } }` 必须写成上面这种嵌套形式。）

**(c) 没写 `source_harness:` 的 plugin：install/update 改判。** 在 `let action = match (unresolved, &action) { ... };` 之后、算 `note` 之前插入：

```rust
                    // §9.5：没写 `source_harness:` 的条目不再假装能装（§9.1）。
                    // 只改 install/update 两条——`in-sync`/`unknown`/`conflict` 是
                    // 「对账结果」，照旧保留：这条目仍然回答「本机是不是清单要的那个版本」。
                    let action = if cap.id.kind == Kind::Plugin
                        && sources.get(&cap.id.name).is_none()
                    {
                        match &action {
                            Action::Install => Action::Undeliverable {
                                reason: NO_NATIVE_INSTALL.to_string(),
                            },
                            Action::Update { from, to } => Action::Undeliverable {
                                reason: format!(
                                    "{NO_NATIVE_INSTALL}（清单要 {to}，本机是 {from}）"
                                ),
                            },
                            other => other.clone(),
                        }
                    } else {
                        action
                    };
```

**(d) 注记跟着拆解出的 skill 行走。** 把 `let note = match unresolved { ... };` 之后的

```rust
                    entries.push(PlanEntry::new(cap.id.clone(), action).with_note(note));
```

换成：

```rust
                    // §9.5：bundle 里装不下的部分（hooks/commands/…）进的是
                    // **这条目的报告说明**，不是 diff 行。拆解出的 skill 正是这条目的产物，
                    // 所以注记跟着它们走；两份注记同时在时都要留着（源过期 / 还有没拆的东西
                    // 是两件事）。
                    let note = match &cap.origin {
                        Origin::Plugin { name } => match (note, content.notes.get(name)) {
                            (Some(a), Some(b)) => Some(format!("{b}；{a}")),
                            (Some(a), None) => Some(a),
                            (None, Some(b)) => Some(b.clone()),
                            (None, None) => None,
                        },
                        _ => note,
                    };
                    entries.push(PlanEntry::new(cap.id.clone(), action).with_note(note));
```

- [ ] **Step 6: 改 `src/lib.rs` 的 `plan_for`**

把第 95-153 行那一段换成（顺序是这一步的要点：**扫描先于拆解，拆解先于解析**）：

```rust
    let mut desired = lower::lower(&manifest);
    let degrade = lower::DegradeTable::new(&manifest);
    let sources = lower::SourceHarnessTable::new(&manifest);

    let registry = adapters::registry(home);
    // state 与它的 adapter 必须成对交给 compute，故先收进同一个 Vec 再借用。
    let mut pairs: Vec<(diff::HarnessState, &dyn HarnessAdapter)> = Vec::new();
    for a in &registry {
        if !is_enabled(&manifest, a.name()) {
            continue;
        }
        let detected = a.detect();
        // 未安装 → 空现状 + detected=false，由 render 显示跳过原因；
        // 不去 read_state，免得对不存在的目录做无谓 IO。
        let actual = if detected {
            a.read_state()
                .with_context(|| format!("cannot scan {} state", a.name()))?
        } else {
            Vec::new()
        };
        pairs.push((
            diff::HarnessState {
                name: a.name().to_string(),
                detected,
                actual,
            },
            &**a,
        ));
    }

    // 源 harness 按需扫（§9.5）：**不进 `pairs`**——`source_harness:` 只请求读它，
    // 不请求往它写，也不请求它多出一节 diff。它在 `harnesses:` 里很可能是关掉的，
    // 那正是「我只想拿它的内容」的写法。
    let known: Vec<&str> = pairs.iter().map(|(s, _)| s.name.as_str()).collect();
    let source_states = decompose::scan_source_harnesses(&sources, &registry)
        .context("cannot scan source harness state")?;

    // §9.5 的拆解：把 bundle 里能拆的能力变成独立能力，追加进清单侧。
    // 必须早于 `resolve_sources`——拆出来的 skill 也要解析内容（它们的源是 bundle）。
    let decomposed = decompose::decompose_bundles(&manifest, &source_states, &known, home)?;
    desired.extend(decomposed.capabilities.iter().cloned());
    desired.sort_by_key(|c| c.id.label());

    // 目标侧内容扫描：逐家算，按 harness 名收进一张表。
    let mut targets: std::collections::BTreeMap<String, _> = Default::default();
    for (state, _) in &pairs {
        targets.insert(
            state.name.clone(),
            store::skills::target_content(&state.actual)?,
        );
    }

    let inputs: Vec<diff::HarnessInput> = pairs
        .iter()
        .map(|(state, adapter)| diff::HarnessInput {
            state,
            adapter: *adapter,
        })
        .collect();

    // §9.2 第 1 步：来源解析。唯一在 compute 之前的 I/O。
    // `mode` 在这里分岔成全链路唯一的两种行为：`Cached` 不联网（`diff`），
    // `Fetch` 会去取（`apply`）。模式必须一路传到这一层——分类「延期还是失败」
    // 是它的责任（§9.4），而这一层是唯一知道「源到底取到没有」的地方。
    let mut content = store::skills::resolve_sources(&desired, home, mode, &decomposed.locations)?;
    content.targets = targets;
    content.notes = decomposed.notes.clone();
    // 定位失败的 plugin 条目：理由由内容层带进来（`compute` 按 `CapId` 取回，
    // 与普通 skill 源失败走的是同一个 `unresolved` 表）。
    for (plugin, reason) in &decomposed.failures {
        content.unresolved.insert(
            capability::CapId::new(capability::Kind::Plugin, plugin.clone()),
            store::skills::Unresolved::Failed(reason.clone()),
        );
    }

    let ledger = store::provenance::Ledger::load(home)?;
    let plan = diff::compute(&desired, &inputs, &degrade, &content, &ledger, &sources);
    Ok(Planned {
        plan,
        desired,
        content,
        ledger,
    })
```

- [ ] **Step 7: 修正三处已经变成谎话的注释**

`src/lib.rs:167-168`，`run_diff` 的文档注释里那句「`degrade:` 在 Plan A 只是声明性占位」换成：

```rust
/// 注：`degrade:` 从 B3b 起真的参与判决——它决定「目标 harness 吃不下、
/// 但内容已经拆出来了」的那些能力是进报告还是静默丢弃（§9.5）。
```

`src/lower.rs` 的 `DegradeTable` 文档注释里「Plan A 里两侧都是惰性的」那一族句子换成：

```rust
/// 它从 B3b 起真的被读：`compute` 用它决定 `Unsupported` 的能力进报告还是丢弃
/// （§9.5），`decompose` 用它决定 bundle 里装不下的部分（hooks/commands/…）
/// 要不要在报告里留一句话。
```

`src/store/skills.rs:105-107` 的 `PluginBundle` 分支注释（Task 7 已经改过那一支的代码，这里确认注释也不再写「不是今天的事」；若 Task 7 已改干净，本步只做确认）。

- [ ] **Step 8: 修剩下的调用点**

Run: `cargo build 2>&1 | grep -A2 "compute"`
`src/apply/mod.rs:498` 补 `&SourceHarnessTable::default()`（它的测试夹具里没有拆解，空表就是「没有旁路声明」）。`src/diff.rs` 里那 7 个测试调用点统一补 `&no_source_harness()`。`src/diff.rs:1289,1333,1345` 三个 `ContentIndex` 字面量补 `notes: BTreeMap::new()`。

- [ ] **Step 9: 跑测试，确认全绿**

Run: `cargo test`
Expected: 全绿，含本 Task 新加的 9 条。

- [ ] **Step 10: Commit**

```bash
git add src/diff.rs src/lib.rs src/lower.rs src/store/skills.rs src/apply/mod.rs
git commit -m "feat(diff): decompose plugin bundles into deliverable skills"
```

---

### Task 10: 端到端 —— 走公开入口的拆解

**Files:**
- Create: `tests/bundle_end_to_end.rs`
- （不改 `src/`。本 Task 只加测试；若它抓到行为缺陷，回到拥有那段代码的 Task 去改。）

**Interfaces:**
- Consumes: `henv::run_diff(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<()>`、`henv::testutil::TempHome`。

**为什么走公开入口**：单元测试已经分别钉过定位、枚举、判定与渲染；本条要证的是**它们串起来之后仍然成立**——尤其是 `plan_for` 的**顺序**（扫描先于拆解、拆解先于解析）。顺序错了，单元测试全绿而这里会红。

- [ ] **Step 1: 写测试**

```rust
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
    t.write(&format!("{root}/skills/tdd/SKILL.md"), "---\nname: tdd\n---\n");
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
    t.write(".claude/plugins/installed_plugins.json", r#"{"plugins":{}}"#);
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

/// **一家 harness 都没有**的机器上，拆解条目的 `undeliverable` 必须**看得见**。
///
/// （2026-09-26 改写：这条测试原本叫 `..._is_a_known_gap`，钉的是当时的缺口——
/// 行落进第一节、那一节是「未安装」→ 整节被跳过、报告里看不见。收尾时补了
/// `Plan.unattached` + `render` 的 `(global)` 段，缺口闭合，断言随之翻转：
/// 原来断言「看不见」，现在断言「必须可见」。）
#[test]
fn the_no_harness_at_all_case_still_shows_the_undeliverable_row() {
    let t = TempHome::new();
    t.write(".claude/plugins/installed_plugins.json", r#"{"plugins":{}}"#);
    t.write("env.yaml", &manifest("claude-code"));

    let out = run_diff_to_string(&t);

    assert!(
        out.contains("(global)"),
        "没有已安装的家时，`undeliverable` 落在 `(global)` 段：{out}"
    );
    assert!(out.contains("! undeliverable"), "{out}");
}
```

（`std::fs::remove_dir_all` 在 `tests/` 下不受 `WRITE_WHITELIST` 约束——那张表只扫 `src/`。但本文件仍需保证只动 `TempHome` 里的路径。）

- [ ] **Step 2: 跑测试，确认它失败（若实现有缺口）或直接通过**

Run: `cargo test --test bundle_end_to_end`
Expected: 若前九个 Task 都做对了，本条应当**直接全绿**——这是「单元测试已分别钉过、本条只证串起来」的正常形态。**若有红**，不要在本条里改断言：回到拥有那段代码的 Task 去修，因为它抓到的是那个 Task 的缺陷。特别地：
- `a_decomposed_skill_targeting_kimi_...` 红 → Task 8 的 `supports` 没改干净。
- `a_source_harness_without_the_plugin_...` 红 → Task 9 的 `detached_plugin_rows` 落点或 `plan_for` 的调用顺序。
- `the_skill_content_comes_from_the_bundle_directory` 红 → Task 7 的 `PluginBundle` 分支取错了路径。

- [ ] **Step 3: 跑全量测试**

Run: `cargo test`
Expected: 全绿。**特别注意 `tests/smoke_real_home.rs`**：它若读真实 `~`，本计划没碰过它，但它会暴露「有人在生产代码里读了真 HOME」——那不属于本计划的范围，若它本来就红，记下来，不动它。

- [ ] **Step 4: 跑 clippy（只在自己改动的文件上看新增告警）**

Run: `cargo clippy --all-targets --all-features 2>&1 | tail -40`
Expected: 既有的两条 `unnecessary_get_then_check`（`src/store/skills.rs:372,435`）仍在——那是既有债，本计划不清理。**本计划新增的行不许有告警**；有就改掉。

- [ ] **Step 5: Commit**

```bash
git add tests/bundle_end_to_end.rs
git commit -m "test: bundle decomposition end to end via run_diff"
```

---

## Self-Review

**1. Spec coverage（逐节对 `docs/design.md`）**

| Spec | 落在哪 |
| --- | --- |
| §4 `source_harness:` 字段 | Task 2 |
| §4 Schema 决策 5（存在即请求、`degrade` 不能当开关、没写的一律不拆） | Task 2（`Option`）+ Task 9(c)(d) |
| §9.5「开关与目标」（写了就没 plugin 行 / 拆不出就有一条 undeliverable / 目标 = targets − 源 / 差集为空 / 没写的 install·update 改判、in-sync·unknown 保留） | Task 3、Task 9 Step 5(a)(b)(c) |
| §9.5「来源与定位」（清单声明不反推 / 不进 `Spec` 走旁路表 / 三家都能当源+校验期值域 / 从 `read_state` 的 `Kind::Plugin` 取 `install_path` / 三态报文分得开 / 四种 manifest 形状 / 版本只进 `source_id`） | Task 2、Task 4、Task 5、Task 6 |
| §9.5「交付什么」（`skills/*/` → 独立 `Kind::Skill` + `Origin::Plugin` / 直接拷进目标 skills 目录不经 `~/.agents/skills` / 非 `lower` 产出 / degrade 第一处 / degrade 第二处「只探存在性、按 kind 进报告」） | Task 6、Task 7、Task 8、Task 9(d) |
| §9.4 bundle 过期判据（对 `<install_path>/skills/<skill>/` 算哈希比 `content_hash`） | Task 7 Step 4（复用既有 `resolve_one` + `content_verdict`，**不改 diff 的判定顺序**） |
| §9.3 逐 harness 拷贝交付与账本 | **无新代码**：复用 `deliver_skills` + `ledger.record`（`source_id` 由 Task 5 供） |
| §9 roadmap B3b（含 `optional_bool`） | Task 1 |
| §9.7 失败隔离 | Task 6（`Decomposition.failures` 不上抛）、Task 9（每节独立） |
| §9.9 只读守卫 | Global Constraints；全计划不新增写 API |

**2. Placeholder scan:** 无 TBD/TODO；每个 code step 都给了可粘贴的代码；每个 run step 都给了命令与 `Expected:`。

**3. Type consistency:** `LocatedBundle{install_path, source_id}`、`BundleTable = BTreeMap<String, LocatedBundle>`、`SourceState::{NotInstalled, Scanned}`、`Decomposition{capabilities, notes, failures, locations}`、`SourceHarnessTable::{new, get, declared}`、`Targets::{minus, is_empty}`、`optional_bool(Option<&Value>, &Path, &str)`、`bundle_source_id(&str, Option<&str>, Option<&str>)`、`read_bundle_manifest(&Path) -> Result<Option<String>>`、`detached_plugin_rows(...) -> BTreeMap<String, Vec<PlanEntry>>`、`decomposed_skill_count(&[Capability], &str) -> usize`、`NO_NATIVE_INSTALL: &str`——在定义它们的 Task 与消费它们的 Task 里同名同型。

**4. Review Focus:** 九条各有一个 owning Task 的测试（Task 6 两条、Task 3 一条、Task 8 两条、Task 9 三条、Task 10 一条）。
