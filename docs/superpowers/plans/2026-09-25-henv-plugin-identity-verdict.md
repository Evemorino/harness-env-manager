# Plugin 身份判据修订（B0）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除 plugin 版本假同步——`compare_versions` 不再「sha 相等即 Same」，判不了的 plugin 版本从被折进 `Action::InSync` 改为一个可见的 `Action::Unknown` 出口。

**Architecture:** 单 Rust 二进制，纯函数 diff（`f(desired, actual) → Plan`，只读）。改三处：`compare_versions` 的 plugin 分支（判据核心）、`diff::Action`/`Counts`/`render`（可见出口）、`lower`/adapter 的过时注释（对齐）。不新增 `Spec::Plugin` 字段、不读 bundle manifest、不引入任何写 API。

**Tech Stack:** Rust edition 2024（rust-version 1.85）；clap 4、serde_yaml_ng、serde_json、toml、anyhow；dev-deps insta 1.48、tempfile。

**Spec:** `docs/design.md` §2.1 第 3 条（三条判据）、§2.2 第 7 条（实测依据与覆盖面）、§9.4（取值来源修订）。

## Global Constraints

- **只读**：`tests/read_only_guard.rs` 的 `FORBIDDEN` 含 `"Command::new("`——新代码不得引入任何写 API。
- **失败隔离契约**：配置/清单文件**缺失 = None（正常）**，**存在但解不开 = 报错**。本计划不新增任何文件读取，只消费 `Spec` 已有字段。
- **测试永不碰真实 `~`**：一律用 `TempHome` / `FakeAdapter` 夹具。
- **不新增 `Spec::Plugin` 字段**；不读 `.claude-plugin/plugin.json`、`.codex-plugin/plugin.json` 或任何 bundle manifest。
- **只改本任务涉及的代码**：不改 `manifest.rs`，不重构无关函数，不格式化无关部分。
- **注释与 commit message 沿用本仓库中文惯例**（与现有 `src/*.rs`、`docs/design.md` 一致）。

## Review Focus

1. **前导 `v` 拼写差异**：期望 `v6.3.0` 与现状 `6.3.0` 应判 Same（去前导 `v` 后相等），不是 Different。→ Task 1 测试 `leading_v_is_normalized_away`。
2. **sha 形目录名 / 分支名不可比**：现状 version 是 `db467cc56673`（claude 8/19 实测）或 `main` 时不含小数点，判不了 → Unknown，绝不报假 drift。→ Task 1 测试 `non_version_form_locators_are_unknown`。
3. **sha 相等但 version 缺失**：两边 sha 相同、version 都 None 时，旧判据判 Same，新判据必须落 Unknown（gitCommitSha 可能陈旧，§2.2 第 7 条）。→ Task 1 测试 `equal_sha_without_version_is_unknown_not_same`。
4. **Unknown 必须有可见输出**：判不了的 plugin 不能静默折成 in-sync，要出 `? unknown` 行。→ Task 2 测试 `unknown_version_match_counts_as_unknown` + `bundled_skill_of_a_desired_plugin_is_not_drift`（diff）+ `unknown_action_renders_a_line_with_question_mark`（render）。
5. **页脚计数含 unknown，且 unknown 使计划「不干净」**：`Plan:` 行要列出 unknown 数量，`is_clean()` 必须把 unknown 计入。→ Task 2 测试 `unknown_counts_and_marks_plan_not_clean` + 页脚断言更新（render/lib）+ 新 render 测试。

---

### Task 1: 判据核心（`compare_versions` 的 plugin 分支 + `normalize_version`/`compare_plugins`）

**Files:**
- Modify: `src/capability.rs:164-281`（`VersionMatch::Unknown` doc、`compare_versions`、新增两个私有 helper）
- Modify: `src/capability.rs:371-422`（三个 plugin 测试：改名/改断言）
- Modify: `src/capability.rs:594-617`（`sha_plus_version_against_version_only_is_unknown` → 改名改断言）
- Modify: `src/diff.rs:341-366`（`same_sha_across_differing_version_strings_is_in_sync` → `_is_update`，断言 `in-sync` → `update`）

**Interfaces:**
- Consumes: `Spec::Plugin` 的现有字段 `sha: Option<String>`、`version: Option<String>`。
- Produces:
  - `fn normalize_version(value: &str) -> Option<String>`（私有）：去前导 `v`/`V`；仅当结果含 `.` 才 `Some`，否则 `None`。
  - `fn compare_plugins(desired: &Spec, actual: &Spec) -> VersionMatch`（私有）：三条判据的实现。
  - `pub fn compare_versions(desired: &Spec, actual: &Spec) -> VersionMatch`（签名不变，plugin 分支委托给 `compare_plugins`）。

- [ ] **Step 1: 写失败的测试**

在 `src/capability.rs` 的 `tests` 模块里，把 `same_sha_wins_over_differing_version_strings`（当前第 371-382 行）整段替换为：

```rust
    #[test]
    fn same_sha_across_differing_version_strings_is_different() {
        // 修订核心：sha 相同不再判 Same（claude 的 gitCommitSha 陈旧，§2.2 第 7 条），
        // version 6.4.1 vs v6.3.0 归一化后不同 → Different。
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        assert_eq!(
            compare_versions(
                &plugin(Some("6.4.1"), Some(sha)),
                &plugin(Some("v6.3.0"), Some(sha))
            ),
            VersionMatch::Different {
                from: "v6.3.0".into(),
                to: "6.4.1".into()
            }
        );
    }
```

把 `sha_plus_version_against_version_only_is_unknown`（当前第 600-617 行）整段替换为：

```rust
    /// 修订前这条是护栏：一边 sha+version 都有、另一边只有 version，旧实现判 Unknown。
    /// 修订后 sha 只在「两边都有且不等」时定 Different，其余 fall through 到 version 比较，
    /// 所以这一对（version 6.4.1 vs 6.3.0 归一化后不同）必须判 Different——sha 在一边缺失
    /// 不再阻断 version 比较。
    #[test]
    fn sha_on_one_side_does_not_block_version_comparison() {
        assert_eq!(
            compare_versions(
                &plugin(Some("6.4.1"), Some("abc123")),
                &plugin(Some("6.3.0"), None)
            ),
            VersionMatch::Different {
                from: "6.3.0".into(),
                to: "6.4.1".into()
            }
        );
        // 反方向同理
        assert_eq!(
            compare_versions(
                &plugin(Some("6.3.0"), None),
                &plugin(Some("6.4.1"), Some("abc123"))
            ),
            VersionMatch::Different {
                from: "6.4.1".into(),
                to: "6.3.0".into()
            }
        );
    }
```

在 `mixed_sha_and_version_only_is_unknown`（第 412-422 行）之后，追加三个新测试：

```rust
    #[test]
    fn leading_v_is_normalized_away() {
        // claude 写 `6.4.1`，kimi 写 `v6.4.1`，去前导 v 后应判 Same（spec §2.1 第 3 条规则 2）。
        assert_eq!(
            compare_versions(&plugin(Some("6.4.1"), None), &plugin(Some("v6.4.1"), None)),
            VersionMatch::Same
        );
    }

    #[test]
    fn non_version_form_locators_are_unknown() {
        // sha 形目录名（claude 8/19 实测）与分支名（`main`）不含小数点 → 不是 version 形，
        // 判不了 → Unknown，绝不报假 drift（spec §2.1 第 3 条规则 3、§2.2 第 7 条覆盖面）。
        assert_eq!(
            compare_versions(&plugin(Some("db467cc56673"), None), &plugin(Some("v6.3.0"), None)),
            VersionMatch::Unknown
        );
        assert_eq!(
            compare_versions(&plugin(Some("main"), None), &plugin(Some("main"), None)),
            VersionMatch::Unknown
        );
    }

    #[test]
    fn equal_sha_without_version_is_unknown_not_same() {
        // 「sha 相等 ⇒ Same」的废除核心：sha 相同但两边都没 version，无法确认内容一致
        // （gitCommitSha 可能陈旧，§2.2 第 7 条），只能 Unknown，不能撒谎 Same。
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        assert_eq!(
            compare_versions(&plugin(None, Some(sha)), &plugin(None, Some(sha))),
            VersionMatch::Unknown
        );
    }
```

在 `src/diff.rs` 里，把 `same_sha_across_differing_version_strings_is_in_sync`（第 341-366 行）整段替换为：

```rust
    #[test]
    fn same_sha_across_differing_version_strings_is_update() {
        // spec §2.1 第 3 条修订后的核心场景：sha 相同不再判 Same（gitCommitSha 陈旧，
        // §2.2 第 7 条），version 6.4.1 vs v6.3.0 归一化后不同 → Different → update。
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: Some(sha.into()),
                install_path: None,
                enabled: true,
            },
        );
        let actual = FakeAdapter::plugin("superpowers", Some("6.4.1"), Some(sha));
        let hp = run(
            &[d],
            FakeAdapter::native("claude-code").with_caps(vec![actual]),
        );
        assert_eq!(
            labels(&hp),
            vec![("plugin/superpowers".to_string(), "update".to_string())]
        );
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib same_sha_across_differing_version_strings_is_different sha_on_one_side_does_not_block_version_comparison leading_v_is_normalized_away non_version_form_locators_are_unknown equal_sha_without_version_is_unknown_not_same 2>&1 | tail -20`
Expected: FAIL——`same_sha_across_differing_version_strings_is_different` 断言 `VersionMatch::Same`（当前实现仍返回 Same）、`sha_on_one_side_does_not_block_version_comparison` 断言 `Unknown`、三个新测试报「函数未定义」或断言不符。`same_sha_across_differing_version_strings_is_update` 在 `cargo test --lib diff::` 下 FAIL（当前断言 in-sync）。

- [ ] **Step 3: 实现判据**

在 `src/capability.rs` 里：

(a) 把 `VersionMatch::Unknown` 的 doc 注释（第 172-174 行）替换为：

```rust
    /// 无法判定：跨 kind、信息不足、或 version 不是「version 形」（sha 形目录名 / 分支名）。
    /// diff 不再把 Unknown 折成 in-sync，而是报出可见的 `? unknown`（spec §2.1 第 3 条）。
    Unknown,
```

(b) 把 `compare_versions`（第 204-281 行）整段替换为：

```rust
/// 判据（spec §2.1 第 3 条，2026-09-25 修订）：plugin 用 entry 名作身份、版本比较见
/// [`compare_plugins`]；skill git 源比 `ref`；mcp 比 command/args/env；其余 Unknown。
pub fn compare_versions(desired: &Spec, actual: &Spec) -> VersionMatch {
    match (desired, actual) {
        (Spec::Plugin { .. }, Spec::Plugin { .. }) => compare_plugins(desired, actual),
        (
            Spec::Skill {
                source: SkillSource::Git { git_ref: d, .. },
                ..
            },
            Spec::Skill {
                source: SkillSource::Git { git_ref: a, .. },
                ..
            },
        ) => {
            if d == a {
                VersionMatch::Same
            } else {
                VersionMatch::Different {
                    from: a.clone(),
                    to: d.clone(),
                }
            }
        }
        (
            Spec::Mcp {
                command: dc,
                args: da,
                env: de,
            },
            Spec::Mcp {
                command: ac,
                args: aa,
                env: ae,
            },
        ) => {
            if dc == ac && da == aa && de == ae {
                VersionMatch::Same
            } else {
                VersionMatch::Different {
                    from: mcp_summary(ac, aa, ae),
                    to: mcp_summary(dc, da, de),
                }
            }
        }
        _ => VersionMatch::Unknown,
    }
}

/// plugin 的版本判据（spec §2.1 第 3 条，2026-09-25 修订）：
/// 1. 两边 SHA 都有且**不相等** ⇒ Different（内容不同，最硬的一刀）；
/// 2. 否则（都 None、或相等——相等不再直接判 Same，见 §2.2 第 7 条）比 version 形：
///    两边都能归一化出「version 形」才可比，归一化后相等 ⇒ Same、不等 ⇒ Different
///    （from = 现状原值、to = 期望原值）；
/// 3. 其余 ⇒ Unknown。
fn compare_plugins(desired: &Spec, actual: &Spec) -> VersionMatch {
    let (
        Spec::Plugin {
            sha: d_sha,
            version: d_ver,
            ..
        },
        Spec::Plugin {
            sha: a_sha,
            version: a_ver,
            ..
        },
    ) = (desired, actual)
    else {
        unreachable!("compare_plugins 只在两边都是 Plugin 时调用");
    };

    if let (Some(d), Some(a)) = (d_sha, a_sha) {
        if d != a {
            return VersionMatch::Different {
                from: a.clone(),
                to: d.clone(),
            };
        }
    }

    match (d_ver, a_ver) {
        (Some(d), Some(a)) => match (normalize_version(d), normalize_version(a)) {
            (Some(dn), Some(an)) => {
                if dn == an {
                    VersionMatch::Same
                } else {
                    VersionMatch::Different {
                        from: a.clone(),
                        to: d.clone(),
                    }
                }
            }
            _ => VersionMatch::Unknown,
        },
        _ => VersionMatch::Unknown,
    }
}

/// 归一化 version 串：去前导 `v`/`V`；仅当结果含小数点（`6.3.0` 形）才返回 Some，
/// sha 形目录名（`db467cc56673`）、分支名（`main`）等不含小数点 → None（不可比，落 Unknown）。
fn normalize_version(value: &str) -> Option<String> {
    let v = value.trim();
    let v = v
        .strip_prefix('v')
        .or_else(|| v.strip_prefix('V'))
        .unwrap_or(v);
    if v.contains('.') {
        Some(v.to_string())
    } else {
        None
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib capability:: 2>&1 | tail -25` 然后 `cargo test --lib diff:: 2>&1 | tail -25`
Expected: PASS——capability 全部绿（含三个新测试）；diff 的 `same_sha_across_differing_version_strings_is_update` 绿，`unknown_version_match_counts_as_in_sync` 与 `bundled_skill_of_a_desired_plugin_is_not_drift` 仍绿（diff 尚未改映射，Unknown 仍折 in-sync）。

- [ ] **Step 5: 提交**

```bash
git add src/capability.rs src/diff.rs
git commit -m "feat(capability): 修订 plugin 版本判据——sha 相等不再判 Same，version 形仲裁"
```

---

### Task 2: 可见出口（`Action::Unknown` 全链路 + render + golden + 页脚）

**Files:**
- Modify: `src/diff.rs:24-101`（`Action` 加变体、`rank`、`Counts` 加字段、`counts`、`is_clean`）
- Modify: `src/diff.rs:169-173`（`compute` 的 `Unknown → Action::Unknown`）
- Modify: `src/diff.rs:260-276`（`labels` 加 `Unknown` 臂）
- Modify: `src/diff.rs:318-339`（`unknown_version_match_counts_as_in_sync` → `_as_unknown`）
- Modify: `src/diff.rs:570-575`（`bundled_skill_of_a_desired_plugin_is_not_drift` 断言 in-sync → unknown）
- Modify: `src/render.rs:21-25`（页脚）、`src/render.rs:55-96`（match 加 Unknown 臂）
- Modify: `src/render.rs:386-402`（两个页脚测试）、追加新测试
- Modify: `src/snapshots/henv__render__tests__spec_example_matches_design_doc.snap`（页脚一行）
- Modify: `src/lib.rs:130`、`src/lib.rs:166`（两个页脚断言）

**Interfaces:**
- Consumes: `VersionMatch::Unknown`（Task 1 已产出）、`Counts` 结构、`Plan::counts()`/`is_clean()`。
- Produces:
  - `Action::Unknown`（unit variant）。
  - `Counts { unknown: usize }`（新字段）。
  - 页脚文案 `"Plan: {} install, {} update, {} remove, {} undeliverable, {} drift, {} unknown"`。
  - 渲染行 `? unknown  <label> (版本无法判定)`。

- [ ] **Step 1: 写失败的测试**

在 `src/diff.rs`：把 `unknown_version_match_counts_as_in_sync`（第 318-339 行）整段替换为：

```rust
    #[test]
    fn unknown_version_match_counts_as_unknown() {
        // codex 不记录 plugin 版本 → Unknown → 报可见的 ? unknown，而不是假装 in-sync
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: None,
                install_path: None,
                enabled: true,
            },
        );
        let actual = FakeAdapter::plugin("superpowers", None, None);
        let hp = run(&[d], FakeAdapter::native("codex").with_caps(vec![actual]));
        assert_eq!(
            labels(&hp),
            vec![("plugin/superpowers".to_string(), "unknown".to_string())]
        );
    }
```

把 `bundled_skill_of_a_desired_plugin_is_not_drift` 的断言（第 570-575 行）里的 `"in-sync"` 改为 `"unknown"`：

```rust
        let got = labels(&hp);
        assert_eq!(
            got,
            vec![("plugin/superpowers".to_string(), "unknown".to_string())],
            "bundle 分发出来的 skill 不是漂移: {got:?}"
        );
```

在 `is_clean_when_nothing_desired_and_nothing_installed`（第 732-749 行）之后追加：

```rust
    #[test]
    fn unknown_counts_and_marks_plan_not_clean() {
        // 判不了 ≠ 干净：counts().unknown 参与统计，is_clean() 必须把它计入。
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: None,
                install_path: None,
                enabled: true,
            },
        );
        let state = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![FakeAdapter::plugin("superpowers", None, None)],
        };
        let adapter = FakeAdapter::native("codex");
        let plan = compute(
            &[d],
            &[HarnessInput {
                state: &state,
                adapter: &adapter,
            }],
            &no_degrade(),
        );
        assert_eq!(plan.counts().unknown, 1);
        assert!(!plan.is_clean(), "判不了 ≠ 干净");
    }
```

在 `src/render.rs` 的 tests 模块（`empty_plan_has_no_leading_blank_line` 之后）追加：

```rust
    #[test]
    fn unknown_action_renders_a_line_with_question_mark() {
        let plan = Plan {
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![pe(Kind::Plugin, "superpowers", Action::Unknown)],
                vec![(Kind::Plugin, 1)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("claude-code   ? unknown  plugin/superpowers (版本无法判定)\n"),
            "{out}"
        );
        assert!(
            out.ends_with("Plan: 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 1 unknown\n"),
            "{out}"
        );
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib diff:: unknown_action_renders 2>&1 | tail -25`
Expected: FAIL 且多处**编译错误**——`Action::Unknown`、`Counts.unknown`、`labels` 缺 `Unknown` 臂、render 的 match 非穷尽（加了 `Action::Unknown` 后 `render_harness` 的 match 缺臂直接编译失败）。这一步的失败形态以编译错为主，正是「出口」接线未完成的信号。

- [ ] **Step 3: 实现出口**

在 `src/diff.rs`：

(a) 把 `Action` 枚举（第 24-37 行）替换为：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Install,
    /// from = 现状，to = 期望
    Update {
        from: String,
        to: String,
    },
    InSync,
    Undeliverable {
        reason: String,
    },
    Drift,
    /// 版本判不了（spec §2.1 第 3 条）：不改装、不报 update，但也**不**假装同步。
    Unknown,
}
```

(b) 把 `rank`（第 39-50 行）替换为：

```rust
impl Action {
    /// 输出分组顺序：install → update → undeliverable → drift → unknown → in-sync
    pub fn rank(&self) -> u8 {
        match self {
            Action::Install => 0,
            Action::Update { .. } => 1,
            Action::Undeliverable { .. } => 2,
            Action::Drift => 3,
            Action::Unknown => 4,
            Action::InSync => 5,
        }
    }
}
```

(c) 把 `Counts`（第 72-79 行）替换为：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub install: usize,
    pub update: usize,
    pub remove: usize,
    pub undeliverable: usize,
    pub drift: usize,
    pub unknown: usize,
}
```

(d) 把 `counts()` 的 match（第 86-92 行）替换为：

```rust
                match &e.action {
                    Action::Install => c.install += 1,
                    Action::Update { .. } => c.update += 1,
                    Action::Undeliverable { .. } => c.undeliverable += 1,
                    Action::Drift => c.drift += 1,
                    Action::Unknown => c.unknown += 1,
                    Action::InSync => {}
                }
```

(e) 把 `is_clean()`（第 98-101 行）替换为：

```rust
    pub fn is_clean(&self) -> bool {
        let c = self.counts();
        c.install == 0
            && c.update == 0
            && c.remove == 0
            && c.undeliverable == 0
            && c.drift == 0
            && c.unknown == 0
    }
```

(f) 把 `compute()` 里的映射（第 169-173 行）替换为：

```rust
                            _ => match compare_versions(&cap.spec, &a.spec) {
                                VersionMatch::Same => Action::InSync,
                                VersionMatch::Different { from, to } => Action::Update { from, to },
                                // 判不了不再折成 in-sync：报可见的 unknown（spec §2.1 第 3 条）
                                VersionMatch::Unknown => Action::Unknown,
                            },
```

(g) 把 `labels()` 里的 match（第 266-272 行）替换为：

```rust
                    match &e.action {
                        Action::Install => "install".to_string(),
                        Action::Update { .. } => "update".to_string(),
                        Action::InSync => "in-sync".to_string(),
                        Action::Undeliverable { .. } => "undeliverable".to_string(),
                        Action::Drift => "drift".to_string(),
                        Action::Unknown => "unknown".to_string(),
                    },
```

在 `src/render.rs`：

(h) 把页脚（第 20-25 行）替换为：

```rust
    let c = plan.counts();
    let _ = writeln!(
        out,
        "Plan: {} install, {} update, {} remove, {} undeliverable, {} drift, {} unknown",
        c.install, c.update, c.remove, c.undeliverable, c.drift, c.unknown
    );
```

(i) 在 `render_harness` 的 match 里，把 `Action::Drift` 臂之后、`Action::InSync` 空臂之前（第 86-95 行之间）插入：

```rust
            Action::Unknown => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} (版本无法判定)",
                    action_cell("? unknown"),
                    e.id.label()
                );
            }
```

(j) 把两个页脚测试（第 386-402 行）替换为：

```rust
    #[test]
    fn footer_counts_come_from_plan_not_from_lines() {
        let out = render(&spec_plan());
        assert!(
            out.ends_with(
                "\nPlan: 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift, 0 unknown\n"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn empty_plan_has_no_leading_blank_line() {
        let out = render(&Plan::default());
        assert_eq!(
            out,
            "Plan: 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"
        );
    }
```

在 `src/lib.rs`：

(k) 把第 130 行的断言替换为：

```rust
            out.ends_with("Plan: 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"),
```

(l) 把第 166 行的断言替换为：

```rust
            out.ends_with("Plan: 1 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"),
```

在 `src/snapshots/henv__render__tests__spec_example_matches_design_doc.snap`：

(m) 把最后一行（第 12 行）替换为：

```
Plan: 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift, 0 unknown
```

- [ ] **Step 4: 运行测试确认通过**

Run: `INSTA_UPDATE=always cargo test --lib render:: 2>&1 | tail -25` 然后 `cargo test --lib diff:: 2>&1 | tail -25` 然后 `cargo test --lib 2>&1 | tail -30`
Expected: PASS——render 的 golden 更新为新页脚、新测试绿；diff 的 `unknown_version_match_counts_as_unknown`、`bundled_skill_of_a_desired_plugin_is_not_drift`、`unknown_counts_and_marks_plan_not_clean` 全绿；全 `--lib` 绿。若 golden 仍与 `INSTA_UPDATE` 不一致，用 `cargo insta review` 或直接核对快照末行。

- [ ] **Step 5: 提交**

```bash
git add src/diff.rs src/render.rs src/snapshots/henv__render__tests__spec_example_matches_design_doc.snap src/lib.rs
git commit -m "feat(diff): 新增 Action::Unknown 可见出口，替代 Unknown 折成 in-sync"
```

---

### Task 3: 注释与文档对齐 + 全量门禁

**Files:**
- Modify: `src/adapters/codex.rs:114-118`（过时注释）
- Modify: `src/lower.rs:117-120`（过时注释）
- Modify: `src/adapters/mod.rs:54-57`（过时注释）

**Interfaces:**
- Consumes: Task 1/2 已产出的判据与出口。
- Produces: 无新接口，纯注释修正。

- [ ] **Step 1: 修正过时注释**

在 `src/adapters/codex.rs`，把第 114-118 行的 doc 注释替换为：

```rust
    /// **已知局限**（Plan C 的 doctor 要报出来）：codex 的 `config.toml` 不记录 plugin 的
    /// 版本或 SHA（缓存目录名可能是 semver、git sha 或字面量 `unknown`），所以 `version`
    /// 与 `sha` 恒为 `None` → `compare_versions` 判 `Unknown` → diff 报可见的 `? unknown`，
    /// 不误报 update。这也是本文件不 import `normalize_locator` 的原因：codex 一个 locator
    /// 字段都不读，没有需要归一化的东西。
```

在 `src/lower.rs`，把第 117-120 行的注释替换为：

```rust
                // 期望侧无 SHA：manifest 只有 ref（git 源）。ref 落在 version 位，
                // compare_versions 走 version 形比较（spec §2.1 第 3 条）；判不了则 Unknown。
                // 哨兵（`unknown`/空白）在此归成 None → 落到 Unknown，而不是报假 drift。
```

在 `src/adapters/mod.rs`，把第 54-57 行的注释替换为：

```rust
    /// - 从活配置里读出的每一个 version / sha / ref 串，装进 `Spec` 之前先过
    ///   [`crate::capability::normalize_locator`]：本机在 `gitCommitSha` 这类字段上
    ///   记的是字面量 `unknown`，不归一化的话两条 `Some("unknown")` 会被当成真 SHA
    ///   参与比较，两个真不同的构建被误判。
```

- [ ] **Step 2: 全量门禁**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings 2>&1 | tail -30` 然后 `cargo test 2>&1 | tail -40`
Expected: 全部通过——fmt 无改动、clippy 零警告、全量测试（含 `tests/read_only_guard.rs`、`tests/` 下的集成测试）绿。

- [ ] **Step 3: 提交**

```bash
git add src/adapters/codex.rs src/adapters/mod.rs src/lower.rs
git commit -m "docs: 对齐 plugin 判据修订后的注释"
```

---

## 自检记录（写计划时已核）

- **Spec 覆盖**：§2.1 第 3 条三条判据 → Task 1 `compare_plugins`/`normalize_version` + 测试；§2.2 第 7 条「sha 相等不再 Same」→ Task 1 `equal_sha_without_version_is_unknown_not_same` + `same_sha_across_differing_version_strings_is_different`；§9.4 取值来源修订 → Task 1 `lower.rs` 注释已在 Task 3 对齐（代码本身不动，因为 `lower` 仍只写 `version`、不写 sha）。可见出口 → Task 2 全链路。
- **无占位符**：所有代码块给出完整 before/after，无 TBD/「类似 Task N」。
- **类型一致**：`normalize_version`/`compare_plugins` 在 Task 1 定义、`Action::Unknown`/`Counts.unknown` 在 Task 2 定义，消费点逐一对应；`compare_versions` 签名不变。
- **Review Focus**：5 条各自落到 Task 1/2 的具名测试，无遗漏。
