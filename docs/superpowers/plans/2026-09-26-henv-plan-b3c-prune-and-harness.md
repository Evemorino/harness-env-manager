# Plan B3c：`--prune` 与 `--harness <name>` 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `henv apply --prune` 真的**撤销 henv 自己做过的事**（删掉自己交付的 skill 拷贝、从三家配置里摘掉自己写过的 MCP），并让 `henv apply --harness <name>` 把一次运行限定在一家。两件事都严守不变式二——**只碰账本里记着的条目**，非 henv 的 drift 一个字节不动。

**Architecture:** `--prune` **不碰** `diff::compute`（它今天、以后都不产删除动作）。新增一条纯函数 `diff::prune_plan(&Plan, &Ledger) -> Plan`，把「账本里记着**且**本轮判为 `Drift`」的条目改写成新的 `Action::Remove`；`run_apply` 只在 `--prune` 时用它替换计划，渲染与执行都吃这份产物——于是「删什么」在纯函数里可测，`compute` 与 `diff` 的输出逐字节不变。执行侧新增 `src/apply/prune.rs`：`Kind::Skill` 删账本 `path` 指向的目录（复用 `write::remove_any`），`Kind::Mcp` 按 adapter 的机制分派（claude 走现成的 `claude_remove`，codex/kimi 新增删键原语），**每条成功才从账本摘除**，失败进 `partial` 不中断其余（§9.7）。`--harness` 只过滤**目标 pairs**，**不过滤源扫描**——`source_harness:` 那家即使被排除也照样要读（§9.5）。

**Tech Stack:** Rust 2024 / clap 4.6 / serde 1 / serde_json / toml_edit 0.25 / anyhow。**本计划不新增任何依赖。** dev-deps：insta、tempfile。`henv` 的 `testutil` 是非默认 feature（自指 dev-dependency 打开）。

**Spec:** `docs/design.md` §9.8（`henv apply [-m env.yaml] [--yes] [--prune] [--harness <name>]`；**prune = 撤销 henv 自己做过的事**，只有 provenance 里记着的条目在删除范围内，非 henv 条目一律只报不删）、§9.3（不变式二与账本键 `(harness, kind, name)`）、§9.7（事务与失败隔离）、§9.9（写 API 白名单：`src/apply/` 与 `src/store/`）、§9.10（测试策略）、§5（drift 的处理方向）、§4（`KNOWN_HARNESSES`）。

## Global Constraints

- 对话中文；代码标识符、注释、commit message 一律英文。
- **仓库没有远程**：不 push、不创建远程、不改写历史。每个 Task 的 commit 步骤只提交本 Task 涉及的文件；是否真的提交由我在每个 Task 结束时决定。
- **测试绝不触碰真实 `~`。** 一切夹具落在 `TempHome`（`TempDir`）下。
- **`tests/read_only_guard.rs` 的两张表都不许放宽。** 本计划**不新增写路径**：撤销与交付一样落在 `src/apply/`（`WRITE_WHITELIST` 已放行的目录），`claude mcp remove` 的 shell out 落在 `src/apply/mcp_claude.rs`（`PROCESS_ALLOWLIST` 已点名它）。**两张表一个字符都不改。**
- **失败隔离（§9.7）。** 单条撤销失败**绝不**上抛打断整次运行：进报告，其余照常。返回 `Err` 只留给「冲突」与「压根没法开始」。
- **不变式二（§9.3）是承重的。** 删除范围**只有账本里 `(harness, kind, name)` 记着的条目**。`--prune` **不是**「让 harness 目录等于清单」：非账本条目一律只报不删，报告里照旧是 drift。
- **`diff` 的输出逐字节不变**：`compute` 签名不动、`diff` 永不产 `Action::Remove`，`src/snapshots/` 一个字节不动。`Counts::remove` 只在 `apply --prune` 的渲染里非零。
- **不改无关代码。** 不顺手重构、不格式化无关文件、不改与任务无关的注释。
- **rust-version 1.85**：**没有 let-chains**（`let ... &&` 要 1.88）。需要「两个条件同时成立」时写嵌套 `if`。
- 本仓库**今天就有既有 lint 债**（4 个与 B3c 无关的文件 `cargo fmt --check` 不过：`src/main.rs`、`src/store/git.rs`、`src/store/skills.rs`、`tests/git_source_end_to_end.rs`；`src/store/skills.rs` 有两条 `clippy::unnecessary_get_then_check`）。**本计划不清理那笔债**，也不把它当门槛：只在**新增/改动的行**上保持 `cargo fmt` 与 clippy 干净。

## Review Focus

按「最可能先咬人」排序。每一条都在它该在的那个 Task 里有测试钉着。

1. **非 henv 的 drift 绝不能被删。** 用户手放的 `~/.claude/skills/handmade`（真目录、有内容）在 `--prune` 下必须**逐字节不变**，报告里仍是 drift。→ Task 3、Task 8。
2. **没给 `--prune` 时零删除。** `apply`（无 `--prune`）跑完，账本里的条目与盘上的目录都在原处——「apply 默认不动 drift」是不变式，不是默认值。→ Task 7、Task 8。
3. **失败隔离：一条删不掉，其余照删，且只有删成功的那条被摘账。** → Task 6。
4. **`--harness` 不许把源扫描也一起关掉。** `--harness codex` 而 `source_harness: claude-code` 时，claude-code 仍要被读、bundle 仍要被拆出来。→ Task 7。
5. **空操作不重写文件。** 要删的 MCP 键本来就不在配置里时，配置文件**一个字节不变**、也不落 `.bak`。→ Task 4。
6. **`diff` 与 golden 不动。** `cargo test` 里 `spec_example_matches_design_doc` 与 `remove 恒为 0` 两条既有断言必须原样通过。→ Task 1、Task 3。

## File Structure

**新增**
- `src/apply/prune.rs` —— 撤销的执行层：skills 删目录、mcps 删键，逐条失败隔离，成功才摘账。
- `tests/prune_end_to_end.rs` —— 走公开入口的端到端：交付 → 清单移除 → drift（不删）→ `--prune` 删掉且非账本条目未被碰。

**修改**
- `src/diff.rs` —— `Action::Remove` + `rank()` + `counts()`；新增纯函数 `prune_plan`。
- `src/render.rs` —— `- remove` 渲染分支。
- `src/store/provenance.rs` —— `Ledger::remove`（`record` 的逆操作）。
- `src/apply/write.rs` —— `remove_any` 提升为 `pub`（撤销的删除原语）。
- `src/apply/mcp_json.rs` —— `remove_kimi_mcp`。
- `src/apply/mcp_toml.rs` —— `Slots::remove` + `remove_codex_mcp`。
- `src/apply/mod.rs` —— `mod prune;`；`HarnessReport.removed`；`ApplyReport::removed_total`；`execute` 串撤销。
- `src/lib.rs` —— `plan_for(.., only)`、`run_apply(.., prune, only)`。
- `src/main.rs` —— `--prune` / `--harness <name>` 两个旗标 + 值域校验。

---

### Task 1: `Action::Remove` 与它的 rank

**Files:**
- Modify: `src/diff.rs`（`Action` 枚举、`rank()`、`counts()`）
- Modify: `src/render.rs`（`render_entries` 的 match 加一条臂）
- Test: `src/diff.rs`、`src/render.rs` 的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 无（本 Task 只加一个枚举变体与计数）。
- Produces: `diff::Action::Remove`（单元变体，无载荷）；`Action::rank()` 的新顺序 **conflict(0) → remove(1) → install(2) → update(3) → undeliverable(4) → drift(5) → unknown(6) → in-sync(7)**；`Counts::remove` 开始真正计数。Task 3 产它，Task 5/6 执行它，Task 1 只把形状定下来——**本 Task 结束时 `diff` 仍然一条 `Remove` 都不产**。

- [ ] **Step 1: 写失败的测试（rank 与计数）**

`src/diff.rs` 的 `mod tests` 里，紧挨现有那条 `Action::Conflict { .. }.rank()` 断言的地方加：

```rust
    /// §9.8：`Action::Remove` 是 `--prune` 的执行形状。`diff` 永不产它，
    /// 但 rank 必须先定好——渲染与排序都读它，插进来也不能打乱既有相对顺序。
    #[test]
    fn remove_ranks_after_conflict_and_before_install() {
        let update = Action::Update {
            from: "a".into(),
            to: "b".into(),
        };
        let undeliverable = Action::Undeliverable { reason: "r".into() };
        assert_eq!(Action::Conflict { reason: "x".into() }.rank(), 0);
        assert_eq!(Action::Remove.rank(), 1);
        assert!(Action::Remove.rank() < Action::Install.rank());
        assert!(Action::Install.rank() < update.rank());
        assert!(update.rank() < undeliverable.rank());
        assert!(undeliverable.rank() < Action::Drift.rank());
        assert!(Action::Drift.rank() < Action::Unknown.rank());
        assert!(Action::Unknown.rank() < Action::InSync.rank());
    }

    /// `remove` 今天恒为 0（`diff` 不产删除），但计数逻辑必须已经在，
    /// 且 `is_clean()` 认它——否则 `--prune` 的计划会被报成「干净」。
    #[test]
    fn a_remove_entry_counts_and_makes_the_plan_unclean() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![PlanEntry::new(
                    CapId::new(Kind::Skill, "gone"),
                    Action::Remove,
                )],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
        };
        assert_eq!(plan.counts().remove, 1);
        assert!(!plan.is_clean());
    }
```

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib remove_ranks_after_conflict_and_before_install`
Expected: FAIL — `no variant named Remove found for enum Action`（编译错）。

- [ ] **Step 3: 加变体、改 rank、改计数**

`src/diff.rs`，`enum Action` 里（`Conflict` 之后、`Install` 之前）加：

```rust
    /// §9.8：`apply --prune` 独有——撤销一条 henv 自己交付过、清单里已经不要了的东西。
    ///
    /// **只有 `prune_plan` 产它**：`diff::compute` 永不产删除动作，
    /// `diff` 对「actual 有而 desired 无」一律只报 `Drift`。
    Remove,
```

`Action::rank()` 换成（注释与数值一起改，相对顺序是契约）：

```rust
    /// 输出分组顺序：conflict → remove → install → update → undeliverable → drift → unknown → in-sync
    ///
    /// `remove` 紧挨 `conflict`：两者都是「必须先看一眼才轮到别的」的动作，
    /// 破坏性的那个排在最前。
    pub fn rank(&self) -> u8 {
        match self {
            Action::Conflict { .. } => 0,
            Action::Remove => 1,
            Action::Install => 2,
            Action::Update { .. } => 3,
            Action::Undeliverable { .. } => 4,
            Action::Drift => 5,
            Action::Unknown => 6,
            Action::InSync => 7,
        }
    }
```

`Plan::counts()` 的 match 加一条：

```rust
                Action::Remove => c.remove += 1,
```

**还有第三处 `match &e.action` 要补**：`mod tests` 里的 `labels()` 助手
（把动作映射成字符串，给排序断言用）。它不在生产路径上，但 `enum` 加了变体之后
不补就编不过——加 `Action::Remove => "remove".to_string(),`。

- [ ] **Step 4: 跑测试，确认它们通过**

Run: `cargo test --lib diff::tests`
Expected: PASS。**特别注意既有那条 `remove 恒为 0，prune 属 Plan B`**：它断言的是 `compute` 的产物，本 Task 没有让 `compute` 产 `Remove`，所以它必须**原样通过**——若它红了，说明你把 `Remove` 接到了 `compute` 里，那是 Task 3 之后才允许的事，而且走的是 `prune_plan` 这条旁路。

- [ ] **Step 5: 写失败的测试（渲染）**

`src/render.rs` 的 `mod tests` 末尾加：

```rust
    /// §9.8：`- remove` 行。前缀必须与其余动作列对齐（与 golden 里
    /// `- drift    ...` 同一套 `action_cell`）。
    #[test]
    fn a_remove_entry_renders_with_its_own_prefix() {
        let plan = Plan {
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![pe(Kind::Skill, "gone", Action::Remove)],
                Vec::new(),
            )],
            unattached: Vec::new(),
        };
        let out = render(&plan);
        assert!(out.contains("- remove   skill/gone"), "{out}");
        assert!(out.contains("1 remove"), "汇总要数得到删除：{out}");
    }
```

- [ ] **Step 6: 跑测试，确认它失败**

Run: `cargo test --lib a_remove_entry_renders_with_its_own_prefix`
Expected: FAIL — 渲染里 `Action::Remove` 落进已有的 `_ =>` 分支或编译不过（`match` 不穷尽），断言拿不到 `- remove`。

- [ ] **Step 7: 加渲染分支**

`src/render.rs` 的 `render_entries` 里，`Action::Conflict { .. }` 那条臂之后加：

```rust
            Action::Remove => {
                let line = format!(
                    "{lead}{}{} (从 henv 账本移除)",
                    action_cell("- remove"),
                    e.id.label()
                );
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
```

- [ ] **Step 8: 跑测试与全量**

Run: `cargo test`
Expected: 全绿。`spec_example_matches_design_doc` 的 golden **逐字节未变**（那条 fixture 里没有 `Remove`）。

- [ ] **Step 9: Commit**

```bash
git add src/diff.rs src/render.rs
git commit -m "feat(diff): add Action::Remove and its render line"
```

---

### Task 2: `Ledger::remove`——`record` 的逆操作

**Files:**
- Modify: `src/store/provenance.rs`
- Test: `src/store/provenance.rs` 的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 现有 `Ledger::{record, find}` 与它们的键 `(harness, kind, name)`。
- Produces: `Ledger::remove(&mut self, harness: &str, kind: Kind, name: &str) -> bool`。Task 6 在每条撤销成功后调用它。

- [ ] **Step 1: 写失败的测试**

`src/store/provenance.rs` 的 `mod tests` 里，紧挨 `record_overwrites_the_same_harness_and_name` 之后加：

```rust
    /// §9.8：`--prune` 成功后摘账。键与 `find`/`record` 完全一致——
    /// 同名不同 kind、同名不同家都不许连坐。
    #[test]
    fn remove_drops_exactly_one_slot() {
        let mut l = Ledger::default();
        l.record(entry_of("claude-code", Kind::Skill, "foo", "h"));
        l.record(entry_of("claude-code", Kind::Mcp, "foo", "m"));
        l.record(entry_of("codex", Kind::Skill, "foo", "h2"));

        assert!(l.remove("claude-code", Kind::Skill, "foo"));
        assert!(l.find("claude-code", Kind::Skill, "foo").is_none());
        assert!(
            l.find("claude-code", Kind::Mcp, "foo").is_some(),
            "同名不同 kind 不能连坐"
        );
        assert!(
            l.find("codex", Kind::Skill, "foo").is_some(),
            "同名不同家不能连坐"
        );
        assert_eq!(l.entries.len(), 2);

        // 再删一次是 false，不是 panic——重跑 `--prune` 是正常路径。
        assert!(!l.remove("claude-code", Kind::Skill, "foo"));
        assert_eq!(l.entries.len(), 2);
    }
```

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib remove_drops_exactly_one_slot`
Expected: FAIL — `no method named remove found for struct Ledger`（编译错）。

- [ ] **Step 3: 实现**

`src/store/provenance.rs`，紧挨 `record` 之后加：

```rust
    /// 摘掉一条记录，返回是否真的摘到了（§9.8：`--prune` 成功后调用）。
    ///
    /// 键与 [`Ledger::find`] / [`Ledger::record`] 完全一致：`(harness, kind, name)`。
    /// **摘不到不是错误**：重跑一次 `--prune` 是正常路径，返回 `false` 让调用方能区分
    /// 「摘掉了」与「本来就没了」，而不是把后者报成故障。
    pub fn remove(&mut self, harness: &str, kind: Kind, name: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|e| !(e.harness == harness && e.kind == kind && e.name == name));
        self.entries.len() != before
    }
```

- [ ] **Step 4: 跑测试，确认它通过**

Run: `cargo test --lib store::provenance`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/store/provenance.rs
git commit -m "feat(ledger): add Ledger::remove for prune"
```

---

### Task 3: `diff::prune_plan`——只有账本记着的 drift 才是删除候选

**Files:**
- Modify: `src/diff.rs`
- Test: `src/diff.rs` 的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: Task 1 的 `Action::Remove`；`Ledger::{find, entries}`；现有的 `Plan`/`HarnessPlan`/`PlanEntry`。
- Produces: `pub fn prune_plan(plan: &Plan, ledger: &Ledger) -> Plan`。Task 7 在 `--prune` 时用它替换 `planned.plan`。

- [ ] **Step 1: 写失败的测试**

`src/diff.rs` 的 `mod tests` 里加（`Ledger`/`LedgerEntry` 已经在该模块的 `use` 里，`PathBuf` 也是）：

```rust
    fn skill_entry(name: &str, action: Action) -> PlanEntry {
        PlanEntry::new(CapId::new(Kind::Skill, name), action)
    }

    fn ledger_entry(harness: &str, kind: Kind, name: &str) -> LedgerEntry {
        LedgerEntry {
            harness: harness.into(),
            kind,
            name: name.into(),
            path: PathBuf::from("/somewhere").join(name),
            source_id: "agents-dir".into(),
            content_hash: "h".into(),
            delivered_at_unix: 1_700_000_000,
        }
    }

    /// §9.8 的核心判决：**账本里记着的 drift 才是删除候选**。
    /// 非账本 drift（用户手放的）原样保留——`--prune` 不是「让目录等于清单」。
    #[test]
    fn prune_turns_only_ledger_recorded_drift_into_removals() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![
                    skill_entry("owned", Action::Drift),
                    skill_entry("handmade", Action::Drift),
                    skill_entry("wanted", Action::Install),
                ],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
        };
        let mut ledger = Ledger::default();
        ledger.record(ledger_entry("claude-code", Kind::Skill, "owned"));

        let pruned = prune_plan(&plan, &ledger);

        let action = |name: &str| {
            pruned.harnesses[0]
                .entries
                .iter()
                .find(|e| e.id.name == name)
                .unwrap()
                .action
                .clone()
        };
        assert_eq!(action("owned"), Action::Remove);
        assert_eq!(action("handmade"), Action::Drift, "非账本条目只报不删");
        assert_eq!(action("wanted"), Action::Install, "其余动作不受影响");

        // 原计划一个字节没动——`prune_plan` 是纯函数。
        assert_eq!(
            plan.harnesses[0].entries[0].action,
            Action::Drift,
            "不许改入参"
        );
    }

    /// 账本键含 harness：`codex` 名下的一条记录不能把 `claude-code` 的 drift
    /// 变成删除，反过来也一样。
    #[test]
    fn a_ledger_entry_from_another_harness_does_not_authorize_a_removal() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![skill_entry("foo", Action::Drift)],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
        };
        let mut ledger = Ledger::default();
        ledger.record(ledger_entry("codex", Kind::Skill, "foo"));

        assert_eq!(prune_plan(&plan, &ledger).harnesses[0].entries[0].action, Action::Drift);
    }

    /// 空账本 = 一条都不删。这是「账本丢了只会退化成什么都不敢动」那条性质的
    /// 具体形态（§9.3）。
    #[test]
    fn an_empty_ledger_prunes_nothing() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![skill_entry("foo", Action::Drift)],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
        };
        assert_eq!(
            prune_plan(&plan, &Ledger::default()).harnesses[0].entries[0].action,
            Action::Drift
        );
    }

    /// 改判会改变 rank：删除条目必须重排到它该在的位置，
    /// 否则 `render` 的「同组连续」假设会破。
    #[test]
    fn prune_re_sorts_entries_after_re_judging() {
        let plan = Plan {
            harnesses: vec![HarnessPlan {
                harness: "claude-code".into(),
                detected: true,
                entries: vec![
                    skill_entry("a", Action::Drift),
                    skill_entry("b", Action::Install),
                ],
                actual_by_kind: BTreeMap::new(),
            }],
            unattached: Vec::new(),
        };
        let mut ledger = Ledger::default();
        ledger.record(ledger_entry("claude-code", Kind::Skill, "a"));

        let entries = &prune_plan(&plan, &ledger).harnesses[0].entries;
        assert_eq!(entries[0].id.name, "a", "remove(1) 排在 install(2) 之前");
        assert_eq!(entries[0].action, Action::Remove);
    }
```

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib diff::tests`
Expected: FAIL — `cannot find function prune_plan in this scope`（编译错）。

- [ ] **Step 3: 实现**

`src/diff.rs`，紧挨 `detached_plugin_rows` 之后（`NO_NATIVE_INSTALL` 之前）加：

```rust
/// §9.8：`--prune` 的规划层。纯函数、幂等、只读——与 `compute` 同一条纪律。
///
/// 把「账本里记着**且**本轮判为 [`Action::Drift`]」的条目改写成 [`Action::Remove`]，
/// 其余 drift 原样保留。**这是不变式二在规划层的落点**：账本说这个 slot 是 henv 的，
/// 才有资格删；用户手放的条目在账本里没有记录，于是永远是 `Drift`——
/// `--prune` 因此**不是**「让 harness 目录等于清单」。
///
/// 键必须带 `harness`：账本本来就是逐 harness 的，`codex` 名下的一条记录
/// 不能授权删掉 `claude-code` 的同名条目。
///
/// `Plan::unattached` 不参与：它只装 `undeliverable`（§9.5），没有 drift 可改判。
pub fn prune_plan(plan: &Plan, ledger: &Ledger) -> Plan {
    let mut out = plan.clone();
    for h in &mut out.harnesses {
        for e in &mut h.entries {
            if !matches!(e.action, Action::Drift) {
                continue;
            }
            if ledger.find(&h.harness, e.id.kind, &e.id.name).is_none() {
                continue;
            }
            e.action = Action::Remove;
        }
        // 改判改变了 rank，必须重排：`render` 依赖「同组连续」。
        h.entries.sort_by(|x, y| {
            x.action
                .rank()
                .cmp(&y.action.rank())
                .then_with(|| x.id.label().cmp(&y.id.label()))
        });
    }
    out
}
```

- [ ] **Step 4: 跑测试，确认它们通过**

Run: `cargo test --lib diff::tests`
Expected: PASS。

- [ ] **Step 5: 跑全量，确认 `diff` 仍然不产删除**

Run: `cargo test`
Expected: 全绿。**关键回归**：既有那条 `remove 恒为 0，prune 属 Plan B` 与 `spec_example_matches_design_doc` 必须原样通过——本 Task 只加了一个**没人调用**的纯函数。

- [ ] **Step 6: Commit**

```bash
git add src/diff.rs
git commit -m "feat(diff): add prune_plan, ledger-recorded drift becomes Remove"
```

---

### Task 4: MCP 撤销原语——`remove_kimi_mcp` 与 `remove_codex_mcp`

**Files:**
- Modify: `src/apply/mcp_json.rs`
- Modify: `src/apply/mcp_toml.rs`
- Test: 两个文件各自的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `write::write_config`（临时文件 → fsync → rename，单槽 `<file>.henv.bak`）、`Slots::of`、现有的两个 `upsert_*`（测试用它们铺场）。
- Produces: `pub fn remove_kimi_mcp(target: &Path, name: &str) -> anyhow::Result<()>`、`pub fn remove_codex_mcp(target: &Path, name: &str) -> anyhow::Result<()>`、`Slots::remove(&mut self, name: &str) -> bool`。Task 6 按 `McpMechanism` 分派到这两个；claude 那一支复用现成的 `mcp_claude::claude_remove`（已有，本 Task 不动它）。

**三条契约（Review Focus 5 钉第 3 条）：**
1. **键不在 → `Ok(())`，且不落盘**：不重写文件、不落 `.bak`、连父目录都不建。文件不存在与「文件里没有这张表」都是这种情形。
2. **键在 → 摘掉那一条，其余内容保留**。codex 是字节级保留（`toml_edit` 只动那一棵子树，注释与别的键原样）；kimi 走 serde_json，别的顶层键一个不少但键序会被字典序重排——这是 §9.6 已记录的代价，不是本 Task 引入的。
3. **读不出/解不开 → `Err`，文件一字节不动**，且错误消息**绝不回显文件内容**（`config.toml` 里可能有 token）。

- [ ] **Step 1: 写失败的测试（kimi 侧）**

`src/apply/mcp_json.rs` 的 `mod tests` 末尾加：

```rust
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
        upsert_kimi_mcp(&p, "user-own", "their-binary", &[], &BTreeMap::new()).unwrap();
        upsert_kimi_mcp(&p, "context7", "npx", &args2(), &env1()).unwrap();

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
    fn removing_refuses_a_non_object_mcp_servers_without_touching_the_file() {
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
    fn removing_from_a_broken_config_is_refused_and_never_echoed() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"doc\": \"sk-live-abc\"\n{\"broken\":\n");
        let before = std::fs::read(&p).unwrap();

        let err = remove_kimi_mcp(&p, "context7").unwrap_err();
        let msg = format!("{err:#}");
        assert!(!msg.contains("sk-live-abc"), "错误消息不许回显配置内容：{msg}");
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }
```

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib apply::mcp_json`
Expected: FAIL — `cannot find function remove_kimi_mcp in this scope`（编译错）。

- [ ] **Step 3: 实现 kimi 侧**

`src/apply/mcp_json.rs`，紧挨 `upsert_kimi_mcp` 之后加：

```rust
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

    // 缺失与显式 `null` 都是「还没有」——不落盘。其余形状报错，理由见
    // `removing_refuses_a_non_object_mcp_servers_without_touching_the_file`。
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
```

- [ ] **Step 4: 跑测试，确认它们通过**

Run: `cargo test --lib apply::mcp_json`
Expected: PASS（原有 10 条 + 新增 6 条）。

- [ ] **Step 5: 写失败的测试（codex 侧）**

`src/apply/mcp_toml.rs` 的 `mod tests` 末尾加：

```rust
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
        upsert_codex_mcp(&p, "other", "their-binary", &[], &BTreeMap::new()).unwrap();
        upsert_codex_mcp(&p, "context7", "npx", &args2(), &env1()).unwrap();

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
```

- [ ] **Step 6: 跑测试，确认它们失败**

Run: `cargo test --lib apply::mcp_toml`
Expected: FAIL — `cannot find function remove_codex_mcp in this scope`（编译错）。

- [ ] **Step 7: 实现 codex 侧**

`src/apply/mcp_toml.rs`，紧挨 `upsert_codex_mcp` 之后加：

```rust
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
    // 绝不把文件内容放进错误消息：config.toml 里可能有 token。
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
```

并在 `impl<'a> Slots<'a>` 里、`replace` 之后加：

```rust
    /// 摘掉一条 MCP 子树，返回是否真的摘到了（§9.8 的撤销原语）。
    ///
    /// **摘不到不是错误**：键本来就不在时返回 `false`，由调用方决定「不落盘」。
    fn remove(&mut self, name: &str) -> bool {
        match self {
            Slots::Block(t) => t.remove(name).is_some(),
            Slots::Inline(t) => t.remove(name).is_some(),
        }
    }
```

- [ ] **Step 8: 跑测试，确认它们通过**

Run: `cargo test --lib apply::mcp_toml`
Expected: PASS。

（另有两个**改名**：`mcp_toml` 的测试模块里已经有 `a_broken_config_is_refused_and_never_echoed`，
`mcp_json` 里已经有 `a_non_object_mcp_servers_is_refused_and_the_file_is_untouched`
与 `an_explicit_null_mcp_servers_is_replaced_not_refused`——本 Task 新增的测试因此
统一带 `removing_` 前缀，避免重名。）

**两个执行期才发现、但必须写进计划的事实：**

1. `toml::Value` 的 `FromStr` 在 toml 1.x 上解析的是**一个值**，不是一份文档——
   `text.parse::<toml::Value>()` 会在第一个 `=` 之后报 “unexpected content”。
   夹具必须用 `toml::from_str(&text)`（既有测试全都这么写，照抄即可）。
2. 删掉**最后一个**块表子表时，`toml_edit` 会留下一个空的 `[mcp_servers]` 头。
   那是合法 TOML，codex 读成空表。**不额外清理**：清理要判断「表空了」并动
   别的键的写入路径，收益只是好看，风险是把用户自己写的空表也删掉。
   `removing_drops_only_the_named_block_table` 的 `toml::from_str` 断言已经
   把「这份文件仍然是合法 TOML」钉住了。

- [ ] **Step 9: 跑该模块全量 + 既有回归**

Run: `cargo test --lib apply::`
Expected: 全绿。**关键回归**：`an_install_writes_the_mcp_and_records_it` 与既有 golden 一字不动——本 Task 只加了两个**没人调用**的函数与一个 `Slots` 方法。

- [ ] **Step 10: Commit**

```bash
git add src/apply/mcp_json.rs src/apply/mcp_toml.rs
git commit -m "feat(apply): add MCP removal primitives for kimi and codex"
```

---

### Task 5: `remove_any` 提升为 `pub`；`HarnessReport.removed`

**Files:**
- Modify: `src/apply/write.rs`（`remove_any` 可见性）
- Modify: `src/apply/mod.rs`（`HarnessReport`、`absorb`、`ApplyReport::removed_total`）
- Modify: `src/apply/mcp.rs`（构造处补字段）
- Test: `src/apply/mod.rs` 的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 现有 `write::remove_any`（不跟随软链、不存在即 `Ok`）、`HarnessReport::absorb` 的形状。
- Produces: `pub fn write::remove_any(&Path) -> anyhow::Result<()>`；`HarnessReport { harness, delivered, removed, failure }`；`ApplyReport::removed_total() -> usize`。Task 6 三项都用到，Task 7 用 `removed_total()` 决定要不要落账本。

**为什么现在就加 `removed`**：`--prune` 的结果要能和交付结果**并进同一份报告**（一家两种写路径只能有一个报告，这是 §9.6 已经定下的形状）。要是给 prune 单开一个报告类型，`ApplyReport::partial()`、`run_apply` 的输出循环、账本保存条件就都得各写两份。

- [ ] **Step 1: 提升 `remove_any` 的可见性**

`src/apply/write.rs`，把私有的 `remove_any` 改成 `pub`，并在文档注释里写清它的契约：

```rust
/// 删掉一个条目，**不跟随软链**：断链要能被删掉，目录要整棵删。
/// 本来就不存在不是错误。
///
/// `--prune` 的 skill 撤销原语（§9.8）：`deliver_skill` 的逆操作。
/// **不比对内容哈希**——§9.3 已经接受「用户手改过的副本会被删除」这一取舍；
/// 有资格调用它的是账本，不是这里。
pub fn remove_any(path: &Path) -> anyhow::Result<()> {
```

（下面函数体一字不改。）

- [ ] **Step 2: 写失败的测试（合并与计数）**

`src/apply/mod.rs` 的 `mod tests` 里加：

```rust
    /// `--prune` 的删除结果与交付结果同构：合并后重排，且两份一条都不丢。
    #[test]
    fn absorb_merges_removals_too() {
        let mut a = HarnessReport {
            harness: "claude-code".into(),
            delivered: vec!["skill/z".into()],
            removed: vec!["mcp/context7".into()],
            failure: None,
        };
        a.absorb(HarnessReport {
            harness: "claude-code".into(),
            delivered: Vec::new(),
            removed: vec!["skill/a".into(), "mcp/b".into()],
            failure: None,
        });

        assert_eq!(a.removed, vec!["mcp/b", "mcp/context7", "skill/a"]);
        assert_eq!(a.delivered, vec!["skill/z"], "交付那一半不受影响");
    }

    /// 账本保存条件读它。少了这条，删成功却不落盘的那次运行会把已删条目
    /// 在下一次 diff 里再报一遍 drift。
    #[test]
    fn removed_total_sums_across_harnesses() {
        let r = ApplyReport {
            harnesses: vec![
                HarnessReport {
                    harness: "claude-code".into(),
                    delivered: Vec::new(),
                    removed: vec!["skill/x".into()],
                    failure: None,
                },
                HarnessReport {
                    harness: "codex".into(),
                    delivered: Vec::new(),
                    removed: vec!["skill/y".into(), "mcp/z".into()],
                    failure: None,
                },
            ],
        };
        assert_eq!(r.removed_total(), 3);
    }
```

- [ ] **Step 3: 跑测试，确认它们失败**

Run: `cargo test --lib apply::tests::removed_total_sums_across_harnesses`
Expected: FAIL — `struct HarnessReport has no field named removed`（编译错）。

- [ ] **Step 4: 加字段、改合并、加计数**

`src/apply/mod.rs`，`HarnessReport` 加字段：

```rust
    /// 本次真正交付的条目 label（如 `skill/foo`），按 label 排好序。
    pub delivered: Vec<String>,
    /// 本次真正**撤销**掉的条目 label（如 `skill/foo`）。只有 `--prune` 会填它；
    /// 撤销不交付任何东西，所以这两个列表在同一次运行里互斥。
    pub removed: Vec<String>,
```

`absorb` 加一句（放在 `self.delivered.sort();` 之后）：

```rust
        self.removed.extend(other.removed);
        self.removed.sort();
```

`ApplyReport` 加：

```rust
    pub fn removed_total(&self) -> usize {
        self.harnesses.iter().map(|h| h.removed.len()).sum()
    }
```

- [ ] **Step 5: 补齐所有 `HarnessReport { .. }` 字面量**

`cargo build` 会逐条点名。已知 7 处，每处补 `removed: Vec::new(),`：
`src/apply/mod.rs` 的 `deliver_skills` 起始报告、`execute` 的 `Err` 臂、以及 `mod tests` 里 4 处夹具；`src/apply/mcp.rs` 的 `deliver_mcps` 起始报告 1 处。

**不要**给 `HarnessReport` 加 `#[derive(Default)]` 来偷懒：`harness` 会默认成空串，
把「忘了填家名」从编译错变成一条静默写进报告的空标签。

- [ ] **Step 6: 跑测试，确认它们通过**

Run: `cargo test --lib apply::`
Expected: PASS。既有 `absorb_merges_two_reports_of_the_same_harness` 等断言原样通过（只是夹具多了个 `removed: Vec::new(),`）。

- [ ] **Step 7: Commit**

```bash
git add src/apply/write.rs src/apply/mod.rs src/apply/mcp.rs
git commit -m "feat(apply): report removals alongside deliveries"
```

---

### Task 6: `src/apply/prune.rs`——撤销的执行层

**Files:**
- New: `src/apply/prune.rs`
- Modify: `src/apply/mod.rs`（`pub mod prune;` + `execute` 串撤销）
- Test: `src/apply/prune.rs` 的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `diff::{Action::Remove, HarnessPlan}`、`Ledger::{find, remove}`、`write::remove_any`、`mcp_claude::claude_remove`、`mcp_toml::remove_codex_mcp`、`mcp_json::remove_kimi_mcp`、`adapters::HarnessAdapter::{name, mcp_target}`、`McpMechanism`。
- Produces: `pub fn prune_harness(adapter: &dyn HarnessAdapter, slice: &HarnessPlan, ledger: &mut Ledger) -> HarnessReport`。Task 7 不直接调它——它挂在 `execute` 里，于是 `run_apply` 只要换掉计划就自动生效。

**两条承重条款：**
- **目标路径只能来自账本**。`PlanEntry` 里没有路径，这是刻意的：计划层不该知道写在哪。所以「删什么」先由 `prune_plan` 用账本授权，再由这里用账本给路径。
- **只有成功才摘账**（§9.7）。失败时账本原样保留——「这里还有 henv 的东西」在失败时是事实，重跑还得靠它。

- [ ] **Step 1: 写失败的测试**

新建 `src/apply/prune.rs`，**只写**这一节（实现留到 Step 3）：

```rust
//! `--prune` 的执行层（spec §9.8/§9.7）：撤销 henv 自己交付过的东西。

use crate::adapters::HarnessAdapter;
use crate::apply::mcp::McpMechanism;
use crate::apply::write::remove_any;
use crate::apply::{mcp_claude, mcp_json, mcp_toml, HarnessReport};
use crate::capability::Kind;
use crate::diff::{Action, HarnessPlan};
use crate::store::provenance::Ledger;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{self, CODEX};
    use crate::capability::CapId;
    use crate::diff::PlanEntry;
    use crate::store::provenance::{LedgerEntry, Ledger as L};
    use crate::testutil::TempHome;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn remove_entry(kind: Kind, name: &str) -> PlanEntry {
        PlanEntry::new(CapId::new(kind, name), Action::Remove)
    }

    fn slice(harness: &str, entries: Vec<PlanEntry>) -> HarnessPlan {
        HarnessPlan {
            harness: harness.into(),
            detected: true,
            entries,
            actual_by_kind: BTreeMap::new(),
        }
    }

    fn record(ledger: &mut L, kind: Kind, name: &str, path: PathBuf) {
        ledger.record(LedgerEntry {
            harness: CODEX.into(),
            kind,
            name: name.into(),
            path,
            source_id: "agents-dir".into(),
            content_hash: "h".into(),
            delivered_at_unix: 1_700_000_000,
        });
    }

}
```

测试体逐条贴进 `mod tests`：

```rust
    /// 正路：账本记着的 skill 拷贝被删掉，且**只有成功的那条**被摘账。
    #[test]
    fn a_recorded_skill_copy_is_removed_and_unrecorded() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/foo/SKILL.md", "# foo");

        let mut ledger = L::default();
        record(&mut ledger, Kind::Skill, "foo", t.path(".codex/skills/foo"));

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Skill, "foo")]),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["skill/foo"]);
        assert!(report.failure.is_none());
        assert!(report.delivered.is_empty(), "撤销不交付任何东西");
        assert!(!t.path(".codex/skills/foo").exists(), "目录要整棵删掉");
        assert!(ledger.find(CODEX, Kind::Skill, "foo").is_none(), "成功才摘账");
    }

    /// 盘上已经没有了、账本还在 → 仍然是成功（目标状态已达成，§9.8），照常摘账。
    /// 重跑一次 `--prune` 走的就是这条路。
    #[test]
    fn removing_an_already_absent_copy_is_a_success() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();

        let mut ledger = L::default();
        record(&mut ledger, Kind::Skill, "foo", t.path(".codex/skills/foo"));

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Skill, "foo")]),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["skill/foo"]);
        assert!(report.failure.is_none());
        assert!(ledger.find(CODEX, Kind::Skill, "foo").is_none());
    }

    /// MCP 正路：`[mcp_servers.context7]` 被摘掉，别的键还在。
    #[test]
    fn a_recorded_codex_mcp_key_is_removed() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(
            ".codex/config.toml",
            "model = \"gpt-5\"\n\n[mcp_servers.context7]\ncommand = \"npx\"\n",
        );

        let mut ledger = L::default();
        record(&mut ledger, Kind::Mcp, "context7", t.path(".codex/config.toml"));

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Mcp, "context7")]),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["mcp/context7"]);
        let text = std::fs::read_to_string(t.path(".codex/config.toml")).unwrap();
        assert!(!text.contains("context7"), "{text}");
        assert!(text.contains("model"), "{text}");
        assert!(ledger.find(CODEX, Kind::Mcp, "context7").is_none());
    }

    /// Review Focus 3（失败隔离）：一条删不掉，其余照删，且**只有成功的那条被摘账**。
    /// 这里用一份坏掉的 `config.toml` 让 MCP 那条失败——skill 那条不受影响。
    #[test]
    fn one_failure_does_not_stop_the_rest_and_only_successes_are_unrecorded() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/foo/SKILL.md", "# foo");
        t.write(".codex/config.toml", "[[[not toml\n");

        let mut ledger = L::default();
        record(&mut ledger, Kind::Skill, "foo", t.path(".codex/skills/foo"));
        record(&mut ledger, Kind::Mcp, "context7", t.path(".codex/config.toml"));

        let report = prune_harness(
            adapter,
            &slice(
                CODEX,
                vec![
                    remove_entry(Kind::Mcp, "context7"),
                    remove_entry(Kind::Skill, "foo"),
                ],
            ),
            &mut ledger,
        );

        assert_eq!(report.removed, vec!["skill/foo"], "成功的那条照删");
        let why = report.failure.expect("失败要被报出来");
        assert!(why.contains("mcp/context7"), "{why}");
        assert!(!t.path(".codex/skills/foo").exists());
        assert!(ledger.find(CODEX, Kind::Skill, "foo").is_none());
        assert!(
            ledger.find(CODEX, Kind::Mcp, "context7").is_some(),
            "失败的那条不许摘账——否则重跑就没有依据了"
        );
    }

    /// Review Focus 1 的执行层一半：**非 `Remove` 的条目一个都不许动**。
    /// 账本里就算记着它，计划判的是 drift，执行层就不许越过计划自己改主意。
    #[test]
    fn non_remove_entries_are_never_touched() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/handmade/SKILL.md", "# handmade");
        let before = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();

        let mut ledger = L::default();
        record(&mut ledger, Kind::Skill, "handmade", t.path(".codex/skills/handmade"));

        let report = prune_harness(
            adapter,
            &slice(
                CODEX,
                vec![PlanEntry::new(
                    CapId::new(Kind::Skill, "handmade"),
                    Action::Drift,
                )],
            ),
            &mut ledger,
        );

        assert!(report.removed.is_empty());
        assert!(report.failure.is_none());
        assert_eq!(
            std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
            before
        );
        assert!(t.path(".codex/skills/handmade").exists());
    }

    /// 计划说要删、账本里却没有 → **不猜路径、不删**，报出来。
    /// `prune_plan` 不会产这种计划，所以真出现了就是计划与账本不同源。
    #[test]
    fn a_remove_without_a_ledger_record_is_reported_and_deletes_nothing() {
        let t = TempHome::new();
        let reg = adapters::registry(&t.home);
        let adapter = adapters::lookup(&reg, CODEX).unwrap();
        t.write(".codex/skills/foo/SKILL.md", "# foo");

        let mut ledger = L::default();

        let report = prune_harness(
            adapter,
            &slice(CODEX, vec![remove_entry(Kind::Skill, "foo")]),
            &mut ledger,
        );

        assert!(report.removed.is_empty());
        assert!(
            report.failure.unwrap().contains("账本里没有这条"),
            "要说清为什么不删"
        );
        assert!(t.path(".codex/skills/foo").exists(), "没有账本授权就不许删");
    }
```

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib apply::prune`
Expected: FAIL — `cannot find function prune_harness in this scope`（编译错）。
同时把 Step 1 里那个 `fn codex(...)` 占位**删掉**——它只是用来让这次编译错先出现在
「缺 `prune_harness`」上，不是要留着的东西。

- [ ] **Step 3: 实现**

`src/apply/prune.rs`，在 `use` 之后、`mod tests` 之前加：

```rust
/// 撤销一家人家清单里已经不要、但账本记着是 henv 放的东西（§9.8）。
///
/// 返回的 [`HarnessReport`] 里 `delivered` 恒空——撤销不交付任何东西；
/// `removed` 是成功摘掉的 label（`skill/foo` / `mcp/context7`）。
///
/// **不变式二在这里的落点**：进这个函数的条目已经是 [`Action::Remove`]，
/// 而那个动作只可能由 [`crate::diff::prune_plan`] 产生——它要求账本里有一条
/// `(harness, kind, name)` 记录。执行层因此不再判一次「这是不是我们的」，
/// 它做的只是：**按账本给的路径删，删成功才摘账**。
///
/// 失败隔离（§9.7）：逐条执行，单条失败进报告、不中断其余。本函数**永不上抛**——
/// 它没有「压根没法开始」这种状态（每一条都是独立的、可单独失败的）。
pub fn prune_harness(
    adapter: &dyn HarnessAdapter,
    slice: &HarnessPlan,
    ledger: &mut Ledger,
) -> HarnessReport {
    let harness = adapter.name().to_string();
    let mut report = HarnessReport {
        harness: harness.clone(),
        delivered: Vec::new(),
        removed: Vec::new(),
        failure: None,
    };
    let mut failures: Vec<String> = Vec::new();

    for e in &slice.entries {
        if !matches!(e.action, Action::Remove) {
            continue;
        }
        let kind = e.id.kind;
        let name = e.id.name.clone();
        let label = e.id.label();

        // 目标路径**只能来自账本**：`PlanEntry` 里没有路径，这是刻意的——
        // 计划层不该知道写在哪，那是账本与 adapter 的事。
        let Some(recorded) = ledger.find(&harness, kind, &name) else {
            failures.push(format!("{label}：账本里没有这条，无法撤销"));
            continue;
        };
        let path = recorded.path.clone();

        let outcome = match kind {
            Kind::Skill => remove_any(&path),
            Kind::Mcp => match adapter.mcp_target() {
                Some(target) => match &target.mechanism {
                    McpMechanism::ClaudeCli { binary } => mcp_claude::claude_remove(binary, &name),
                    McpMechanism::CodexToml => mcp_toml::remove_codex_mcp(&target.path, &name),
                    McpMechanism::KimiJson => mcp_json::remove_kimi_mcp(&target.path, &name),
                },
                None => Err(anyhow::anyhow!(
                    "这家没有 MCP 落点，账本记录却指向 {}",
                    path.display()
                )),
            },
            // B3 只交付 skill 与 mcp，账本里因此只可能有这两种。剩下的 kind 若出现，
            // 说明账本被人手改过——报出来比猜一条删除路径安全。
            other => Err(anyhow::anyhow!("`{}` 还没有撤销路径", other.as_str())),
        };

        match outcome {
            Ok(()) => {
                report.removed.push(label);
                // §9.7：**只有成功才摘账**。失败时账本原样保留——
                // 「这里还有 henv 的东西」在失败时是事实，重跑还得靠它。
                ledger.remove(&harness, kind, &name);
            }
            Err(err) => failures.push(format!("{label}: {err:#}")),
        }
    }

    report.removed.sort();
    if !failures.is_empty() {
        report.failure = Some(failures.join("; "));
    }
    report
}
```

- [ ] **Step 4: 跑测试，确认它们通过**

Run: `cargo test --lib apply::prune`
Expected: PASS（6 条）。

- [ ] **Step 5: 把撤销挂进 `execute`**

`src/apply/mod.rs`，模块声明处加：

```rust
pub mod mcp;
pub mod mcp_claude;
pub mod mcp_json;
pub mod mcp_toml;
pub mod prune;
pub mod write;
```

`execute` 的循环体里，把现有的 `match adapter.apply(hp, desired, content, ledger) { ... }`
那一段（`Ok(r) => report.harnesses.push(r)` 与 `Err(e) => ...` 两条臂）换成：

```rust
        let mut harness_report = match adapter.apply(hp, desired, content, ledger) {
            Ok(r) => r,
            // adapter 级失败（连目标目录都建不出来）——也走 partial，不上抛。
            Err(e) => HarnessReport {
                harness: hp.harness.clone(),
                delivered: Vec::new(),
                removed: Vec::new(),
                failure: Some(format!("{e:#}")),
            },
        };
        // §9.8：撤销与交付隔离在**条目**这一级，所以两半各跑各的、合成一份报告。
        // 计划里没有 `Remove` 条目时这是一个空报告——`--prune` 之外零影响，
        // 因为 `diff::compute` 与 `diff` 都不产 `Remove`。
        harness_report.absorb(prune::prune_harness(adapter, hp, ledger));
        report.harnesses.push(harness_report);
```

- [ ] **Step 6: 跑全量**

Run: `cargo test`
Expected: 全绿。既有 5 个 `run_apply` 调用点、`execute` 的 5 个单元测试全部原样通过
——本 Task 没有改任何签名。

- [ ] **Step 7: Commit**

```bash
git add src/apply/prune.rs src/apply/mod.rs
git commit -m "feat(apply): execute pruned removals with per-entry failure isolation"
```

---

### Task 7: `--prune` 与 `--harness <name>` 串线

**Files:**
- Modify: `src/lib.rs`（`plan_for(.., only)`、`run_apply(.., prune, only)`、确认文案、输出、账本保存条件）
- Modify: `src/main.rs`（两个旗标 + 值域校验 + `long_about`）
- Modify: `tests/mcp_end_to_end.rs`、`tests/git_source_end_to_end.rs`（调用点补两个实参）
- Modify: `tests/bundle_end_to_end.rs`（新增一条「`--harness` 不动源扫描」的公开入口测试）
- Test: `src/lib.rs` 的 `mod tests`、`src/main.rs` 的 `mod tests`、`tests/bundle_end_to_end.rs`

**Interfaces:**
- Consumes: Task 3 的 `diff::prune_plan`、Task 6 挂在 `execute` 里的撤销、`manifest::KNOWN_HARNESSES`。
- Produces: `run_apply(manifest_path, home, out, confirm, prune: bool, only: Option<&str>)`；`plan_for(.., only: Option<&str>)`；CLI `henv apply [--prune] [--harness <NAME>]`。**没有新增任何写路径**，`tests/read_only_guard.rs` 两张表一个字符不改。

**三条顺序上的讲究：**
1. `--prune` 的改判必须发生在**渲染之前**：用户要看到的就是「这次会删什么」。
2. **`--harness` 只过滤 `pairs`**（目标侧）。源扫描走 `scan_source_harnesses(&sources, &registry)` 且**不接收 `only`**——`source_harness:` 那家哪怕被排除也照样要被读。
3. 账本保存条件从「交付过东西」扩成「交付过**或**删成功过」。删成功却不落盘的话，下一次 diff 会把已删的条目再报一遍 drift，而账本里那份记录还在。

- [ ] **Step 1: 写失败的测试（lib.rs）**

`src/lib.rs` 的 `mod tests` 里，把现有的 `apply` 夹具改成下面这样（**保持既有调用点不变**），并加一个新夹具：

```rust
    /// 在 TempHome 上跑一次 run_apply，返回渲染文本。
    fn apply(t: &TempHome, manifest: &str, confirm: &mut dyn Confirm) -> anyhow::Result<String> {
        t.write("env.yaml", manifest);
        let mut out = Vec::new();
        run_apply(&t.path("env.yaml"), &t.home, &mut out, confirm, false, None)?;
        Ok(String::from_utf8(out).expect("utf8"))
    }

    /// 同上，但能指定 `--prune` 与 `--harness`。
    fn apply_with(t: &TempHome, manifest: &str, prune: bool, only: Option<&str>) -> String {
        t.write("env.yaml", manifest);
        let mut out = Vec::new();
        run_apply(
            &t.path("env.yaml"),
            &t.home,
            &mut out,
            &mut AssumeYes,
            prune,
            only,
        )
        .expect("run_apply");
        String::from_utf8(out).expect("utf8")
    }
```

再加四条测试：

```rust
    /// Review Focus 4：`--harness` 只收窄**目标**。没被点名的家一个字节都不写。
    /// （「源扫描不跟着关掉」那条在 `tests/bundle_end_to_end.rs` 里以公开入口的形态钉。）
    #[test]
    fn harness_narrows_the_targets_only() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");

        let out = apply_with(&t, one_skill_manifest(), false, Some("codex"));

        assert!(t.path(".codex/skills/foo").exists(), "{out}");
        assert!(
            !t.path(".claude/skills/foo").exists(),
            "没被点名的家一个字节都不该写：\n{out}"
        );
    }

    /// Review Focus 2：没给 `--prune` 时，清单里已经不要的条目**一个都不删**，
    /// 账本也不动——这是不变式，不是默认值。
    #[test]
    fn apply_without_prune_never_deletes_a_drifted_copy() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        apply_with(&t, one_skill_manifest(), false, Some("codex"));
        assert!(t.path(".codex/skills/foo").exists(), "先交付到盘上");

        // 清单里去掉 foo，再跑一次**不带** `--prune` 的 apply。
        let out = apply_with(&t, "version: 1\n", false, Some("codex"));

        assert!(
            t.path(".codex/skills/foo").exists(),
            "没给 --prune 就不许删：\n{out}"
        );
        assert!(!out.contains("removed"), "{out}");
        let ledger = crate::store::provenance::Ledger::load(&t.home).unwrap();
        assert!(
            ledger.find(CODEX, Kind::Skill, "foo").is_some(),
            "账本也不许动"
        );
    }

    /// 正路：`--prune` 删掉账本记着的那一条，非账本的 drift 逐字节不动。
    /// 端到端那条在 `tests/prune_end_to_end.rs` 里再走一遍公开入口。
    #[test]
    fn prune_deletes_only_what_the_ledger_records() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        t.write(".codex/skills/handmade/SKILL.md", "# handmade");
        apply_with(&t, one_skill_manifest(), false, Some("codex"));
        let handmade = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();

        let out = apply_with(&t, "version: 1\n", true, Some("codex"));

        assert!(out.contains("removed skill/foo"), "{out}");
        assert!(!t.path(".codex/skills/foo").exists(), "账本记着的要删掉");
        assert!(
            t.path(".codex/skills/handmade").exists(),
            "非账本的 drift 不许动"
        );
        assert_eq!(
            std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
            handmade,
            "逐字节不变"
        );
        let ledger = crate::store::provenance::Ledger::load(&t.home).unwrap();
        assert!(
            ledger.find(CODEX, Kind::Skill, "foo").is_none(),
            "删成功要把账本落盘"
        );
    }

    /// 确认文案必须说清这次会删东西——删除不可逆（§9.7 没有回滚命令）。
    /// 这里顺手把「拒绝之后什么都不发生」也钉住。
    #[test]
    fn the_prompt_says_how_many_items_will_be_deleted() {
        struct Recorder(Vec<String>);
        impl Confirm for Recorder {
            fn ask(&mut self, prompt: &str) -> bool {
                self.0.push(prompt.to_string());
                false
            }
        }

        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".codex");
        apply_with(&t, one_skill_manifest(), false, Some("codex"));

        t.write("env.yaml", "version: 1\n");
        let mut out = Vec::new();
        let mut rec = Recorder(Vec::new());
        run_apply(
            &t.path("env.yaml"),
            &t.home,
            &mut out,
            &mut rec,
            true,
            Some("codex"),
        )
        .unwrap();

        assert_eq!(rec.0, vec!["将向 codex 交付，并删除 1 项。继续？"]);
        assert!(
            t.path(".codex/skills/foo").exists(),
            "拒绝之后什么都不该发生"
        );
    }
```

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib lib::tests`（或 `cargo test --lib`）
Expected: FAIL — `this function takes 4 arguments but 6 were supplied`（编译错）。

- [ ] **Step 3: 改 `plan_for` 与 `run_diff`**

`src/lib.rs`，`plan_for` 加第四个参数：

```rust
fn plan_for(
    manifest_path: &Path,
    home: &Home,
    mode: store::skills::SourceMode,
    only: Option<&str>,
) -> anyhow::Result<Planned> {
```

registry 循环里、`is_enabled` 那段判定的**紧后面**加：

```rust
        // §9.8 的 `--harness <name>`：**只过滤目标 pairs**。源扫描在下面，
        // 走的是 `scan_source_harnesses(&sources, &registry)`，不接收 `only`——
        // `source_harness:` 那家哪怕被排除也照样要被读，否则它的 bundle
        // 会凭空变成「定位不到」，用户得到一份假的 undeliverable。
        if let Some(only) = only {
            if a.name() != only {
                continue;
            }
        }
```

（`rust-version 1.85` 没有 let-chains，所以是嵌套 `if`，不是 `if let ... && ...`。）

`run_diff` 的调用点补 `None`：

```rust
    let planned = plan_for(manifest_path, home, store::skills::SourceMode::Cached, None)?;
```

- [ ] **Step 4: 改 `run_apply`**

签名、改判顺序、确认文案、输出、账本条件：

```rust
pub fn run_apply(
    manifest_path: &Path,
    home: &Home,
    out: &mut dyn Write,
    confirm: &mut dyn Confirm,
    prune: bool,
    only: Option<&str>,
) -> anyhow::Result<()> {
    let mut planned = plan_for(manifest_path, home, store::skills::SourceMode::Fetch, only)?;

    // §9.8：`--prune` 只在这里改计划——`diff::compute` 与 `diff` 都不产删除动作，
    // 于是「删什么」是一件可测的纯函数，而 `diff` 的输出逐字节不变。
    // 改判必须在渲染之前：用户要看到的就是「这次会删什么」。
    if prune {
        planned.plan = diff::prune_plan(&planned.plan, &planned.ledger);
    }
    write!(out, "{}", render::render(&planned.plan))?;
```

（下面冲突检查那一段原样不动。）确认那一段改成：

```rust
    // 确认文案要说清这次**不只是交付**：删除不可逆（§9.7 没有回滚命令）。
    // 删除条数为 0 时文案与从前一字不差——既有测试与人的肌肉记忆都不受影响。
    let removals = planned.plan.counts().remove;
    let mut what = format!("将向 {} 交付", pending.join(", "));
    if removals > 0 {
        let _ = write!(what, "，并删除 {removals} 项");
    }
    if !confirm.ask(&format!("{what}。继续？")) {
        let _ = writeln!(out, "cancelled");
        return Ok(());
    }
```

`execute` 之后的输出与落盘改成：

```rust
    for h in &report.harnesses {
        for id in &h.delivered {
            let _ = writeln!(out, "{:<13} delivered {}", h.harness, id);
        }
        for id in &h.removed {
            let _ = writeln!(out, "{:<13} removed {}", h.harness, id);
        }
    }
    // 账本只在真的动过之后写：一次空跑不该改动 henv 的状态文件。
    // 「删成功」也算动过——不落盘的话，下一次 diff 会把已删的条目再报一遍 drift，
    // 而账本里那份记录还在（§9.7 的修复路径依赖它）。
    if report.delivered_total() > 0 || report.removed_total() > 0 {
        planned.ledger.save(home)?;
    }
```

- [ ] **Step 5: 改 `--help` 的披露文字**

`--help` 的输出**不进 insta golden**（没有任何测试断言 `long_about`），所以这里
可以放心改；但**别去改 `render()` 的产物**——那个被 golden 逐字节锁着。

`src/main.rs` 的 `long_about` 整段换成：

```rust
    long_about = "比对 env.yaml 与本机现状，报告切换 harness 时会失效的能力；`apply` 还能把清单要的能力交付到各家。\n\n\
写盘边界：`diff` 全程只输出报告，不写任何配置文件，有 drift 也算成功退出（门禁语义留给 Plan C 的 doctor）；\n\
`apply` 会写——它向各家的 skills 目录真拷贝内容，并把「放了什么」记进 ~/.local/state/henv/provenance.json。\n\
`apply` 落盘前会先渲染计划并要一次 y/N 确认（`--yes` 跳过，供脚本与 CI 用）；\n\
`apply --prune` 只撤销账本里记着的条目（henv 自己交付过的 skill 拷贝与 MCP 键），用户手放的 drift 一律只报不删；\n\
`apply --harness <name>` 把一次运行限定在一家（值域：claude-code / codex / kimi-code）。\n\
`skills list` 同样只读：它只把仓库取到 henv 自己的缓存目录下看完即删。\n\n\
degrade: 政策决定「目标 harness 吃不下、但内容已经拆出来了」的能力是进报告还是静默丢弃（§9.5）。"
```

最后那句必须一起改：它今天还写着 `degrade:` 是「Plan A 的声明性占位、两侧都不生效」，
而 B3b 之后它真的参与判决了。**留着它就是在 `--help` 里说反话**，而这一段
本来就要因为 `--prune` 而动。

`src/lib.rs` 的 `run_apply` rustdoc 里补一句边界（同样只动这一处）：

```rust
/// `prune` 只撤销**账本里记着**的条目——henv 自己交付过的 skill 拷贝与 MCP 键；
/// 用户手放的 drift 一律只报不删（§9.8）。`only` 把一次运行限定在一家，
/// 且**只收窄目标**：源扫描不受它影响。
```

- [ ] **Step 6: 改 `main.rs` 的两个旗标**

```rust
    /// 交付：把清单里的能力真拷贝到各家，并记录在账本里（会写文件）
    Apply {
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
        /// 不问确认直接执行（用于脚本与 CI）
        #[arg(long)]
        yes: bool,
        /// 撤销 henv 自己交付过、清单里已经不要了的东西（只删账本里记着的）
        #[arg(long)]
        prune: bool,
        /// 只对这一家生效（claude-code / codex / kimi-code）
        #[arg(long, value_name = "NAME", value_parser = parse_harness)]
        harness: Option<String>,
    },
```

文件底部加：

```rust
/// `--harness <name>` 的值域校验（§9.8）。
///
/// 在 CLI 层就拒绝未知名字：`plan_for` 的 `only` 是一张**过滤器**，
/// 一个永远不匹配的名字会让整次运行静默变成「没有已安装的 harness」——
/// 用户敲错一个字母得到的是「无事可做」，不是报错。
fn parse_harness(s: &str) -> Result<String, String> {
    if henv::manifest::KNOWN_HARNESSES.contains(&s) {
        Ok(s.to_string())
    } else {
        Err(format!(
            "unknown harness `{s}` (known: {})",
            henv::manifest::KNOWN_HARNESSES.join(", ")
        ))
    }
}
```

`main()` 的分支改成：

```rust
        Command::Apply {
            manifest,
            yes,
            prune,
            harness,
        } => {
            let mut confirm: Box<dyn Confirm> = if yes {
                Box::new(AssumeYes)
            } else {
                Box::new(StdinConfirm)
            };
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::run_apply(
                &manifest,
                &home,
                &mut lock,
                confirm.as_mut(),
                prune,
                harness.as_deref(),
            )?;
        }
```

`mod tests` 里加：

```rust
    /// §9.8 的 `--harness <name>`：值域在 CLI 层就判，敲错一个字母得到的是报错，
    /// 不是「无事可做」。
    #[test]
    fn apply_rejects_an_unknown_harness_name() {
        let err = Cli::try_parse_from(["henv", "apply", "--harness", "cursor"]).unwrap_err();
        assert!(err.to_string().contains("unknown harness"), "{err}");
        assert!(Cli::try_parse_from(["henv", "apply", "--harness", "codex"]).is_ok());
    }
```

- [ ] **Step 7: 补两个集成测试的调用点**

`tests/mcp_end_to_end.rs` 与 `tests/git_source_end_to_end.rs` 各一处，补两个实参：

```rust
    henv::run_apply(&t.path("env.yaml"), &t.home, &mut out, &mut confirm, false, None)
        .expect("run_apply");
```

- [ ] **Step 8: 加「`--harness` 不动源扫描」的公开入口测试**

`tests/bundle_end_to_end.rs` 加：

```rust
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
```

- [ ] **Step 9: 跑全量**

Run: `cargo test`
Expected: 全绿。**关键回归**：`diff` 的 golden（`spec_example_matches_design_doc`）一字不动
——`--prune` 的改判只在 `run_apply` 里发生，`render` 只是多了一条今天永远走不到的臂。

- [ ] **Step 10: 手工看一眼帮助文本**

Run: `cargo run -- apply --help`
Expected: 两个旗标都在，`--harness` 的值名是 `NAME`，帮助里说清 `--prune` 只删账本记着的。

- [ ] **Step 11: Commit**

```bash
git add src/lib.rs src/main.rs tests/mcp_end_to_end.rs tests/git_source_end_to_end.rs tests/bundle_end_to_end.rs
git commit -m "feat(cli): wire --prune and --harness into apply"
```

---

### Task 8: `tests/prune_end_to_end.rs`——走公开入口的端到端

**Files:**
- New: `tests/prune_end_to_end.rs`
- Test: 本文件自身

**Interfaces:**
- Consumes: `henv::run_diff`、`henv::run_apply(.., prune, only)`、`henv::AssumeYes`、`henv::store::provenance::Ledger`、`henv::testutil::TempHome`。
- Produces: 什么都不给——它是本计划的**验收测试**：Review Focus 1 与 2 的最终形态。

**夹具只装 codex**（`claude-code: { enabled: false }`）：`run_apply` 走 `registry(home)`，
claude 的 MCP 写入是**裸 `claude`**（§9.6），夹具里但凡让 claude 被交付，测试就会去动
真实的 `~/.claude.json`。「测试绝不碰真实 `~`」是硬约束，所以这里连 `enabled: true` 都不给。

- [ ] **Step 1: 写测试**

新建 `tests/prune_end_to_end.rs`：

```rust
//! §9.8 的 `--prune` 在**公开入口**上的样子。
//!
//! 三条串起来的性质：**没给 `--prune` 就零删除**、**`--prune` 只删账本记着的**、
//! **删成功才落账本**。夹具全部落在 `TempHome` 下——只装 codex，于是没有任何
//! 一条路径会去调真 CLI、也没有一个字节落在真实 `~`。

use henv::AssumeYes;
use henv::testutil::TempHome;

/// 只装 codex。`enabled: false` 那两家连 `pairs` 都不进。
const ONLY_CODEX: &str = "harnesses:\n  claude-code: { enabled: false }\n  codex: { enabled: true }\n  kimi-code: { enabled: false }\n";

fn manifest_with_foo() -> String {
    format!("version: 1\n{ONLY_CODEX}skills:\n  foo:\n    source: agents-dir\n")
}

fn manifest_without_foo() -> String {
    format!("version: 1\n{ONLY_CODEX}")
}

fn run_diff(t: &TempHome) -> String {
    let mut out = Vec::new();
    henv::run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
    String::from_utf8(out).expect("utf8")
}

fn run_apply(t: &TempHome, prune: bool) -> String {
    let mut out = Vec::new();
    let mut confirm = AssumeYes;
    henv::run_apply(&t.path("env.yaml"), &t.home, &mut out, &mut confirm, prune, None)
        .expect("run_apply");
    String::from_utf8(out).expect("utf8")
}

/// 全程走一遍：交付 → 清单里去掉 → diff 报 drift 且不删 → 不带 `--prune` 还是不删
/// → `--prune` 删掉自己那份、手放的那份逐字节不动 → 账本落盘、再 diff 只剩手放的那条。
#[test]
fn prune_removes_only_the_ledger_recorded_copy() {
    let t = TempHome::new();
    t.write(".agents/skills/foo/SKILL.md", "# foo");
    t.mkdir(".codex");
    t.write(".codex/skills/handmade/SKILL.md", "# handmade");
    let handmade = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();

    // 1) 交付。
    t.write("env.yaml", &manifest_with_foo());
    let out = run_apply(&t, false);
    assert!(t.path(".codex/skills/foo/SKILL.md").exists(), "{out}");
    let ledger = henv::store::provenance::Ledger::load(&t.home).unwrap();
    assert!(ledger.find("codex", henv::capability::Kind::Skill, "foo").is_some());

    // 2) 清单里去掉 foo：两份都是 drift，diff 一个字节都不删。
    t.write("env.yaml", &manifest_without_foo());
    let report = run_diff(&t);
    assert!(report.contains("- drift"), "{report}");
    assert!(report.contains("skill/foo"), "{report}");
    assert!(report.contains("skill/handmade"), "{report}");
    assert!(t.path(".codex/skills/foo").exists(), "diff 只读：\n{report}");

    // 3) 不带 `--prune` 的 apply：还是不删（Review Focus 2）。
    let out = run_apply(&t, false);
    assert!(
        t.path(".codex/skills/foo").exists(),
        "没给 --prune 就不许删：\n{out}"
    );
    assert!(!out.contains("removed"), "{out}");

    // 4) `--prune`：删掉自己那份，手放的那份逐字节不动（Review Focus 1）。
    let out = run_apply(&t, true);
    assert!(out.contains("removed skill/foo"), "{out}");
    assert!(!t.path(".codex/skills/foo").exists(), "{out}");
    assert!(t.path(".codex/skills/handmade").exists(), "{out}");
    assert_eq!(
        std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
        handmade,
        "非 henv 的 drift 逐字节不变"
    );

    let ledger = henv::store::provenance::Ledger::load(&t.home).unwrap();
    assert!(
        ledger.find("codex", henv::capability::Kind::Skill, "foo").is_none(),
        "删成功要把账本落盘"
    );

    // 5) 再 diff：只剩手放的那条还是 drift，且报告如实说它还在。
    let report = run_diff(&t);
    assert!(!report.contains("skill/foo"), "{report}");
    assert!(report.contains("skill/handmade"), "{report}");
}

/// 重跑一次 `--prune` 是正常路径：什么都不删，账本仍然是可读的合法 JSON
/// ——「空账本」与「账本坏了」是两回事。
#[test]
fn prune_is_idempotent() {
    let t = TempHome::new();
    t.write(".agents/skills/foo/SKILL.md", "# foo");
    t.mkdir(".codex");
    t.write("env.yaml", &manifest_with_foo());
    run_apply(&t, false);
    t.write("env.yaml", &manifest_without_foo());

    let first = run_apply(&t, true);
    assert!(first.contains("removed skill/foo"), "{first}");

    let second = run_apply(&t, true);
    assert!(!second.contains("removed"), "第二次不该再删什么：\n{second}");
    assert!(!t.path(".codex/skills/foo").exists());

    let ledger = henv::store::provenance::Ledger::load(&t.home).unwrap();
    assert!(ledger.entries.is_empty(), "{:?}", ledger.entries);
}

/// 账本是空的（这台机器从没 apply 过）：`--prune` 一个都不删，连账本文件都不建。
#[test]
fn prune_with_an_empty_ledger_deletes_nothing() {
    let t = TempHome::new();
    t.mkdir(".codex");
    t.write(".codex/skills/handmade/SKILL.md", "# handmade");
    let handmade = std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap();
    t.write("env.yaml", &manifest_without_foo());

    let out = run_apply(&t, true);

    assert!(!out.contains("removed"), "{out}");
    assert!(t.path(".codex/skills/handmade").exists(), "空账本 = 什么都不敢动");
    assert_eq!(
        std::fs::read(t.path(".codex/skills/handmade/SKILL.md")).unwrap(),
        handmade
    );
    assert!(
        !t.home.henv_provenance().exists(),
        "既没交付也没删除 → 不落账本"
    );
}
```

- [ ] **Step 2: 跑测试，确认它们通过**

Run: `cargo test --test prune_end_to_end`
Expected: PASS（3 条）。若 `prune_removes_only_the_ledger_recorded_copy` 红了，
先看第 3 步的输出里有没有 `removed`——那说明「没给 `--prune` 就零删除」这条破了。

- [ ] **Step 3: 跑全量 + 守卫**

Run: `cargo test`
Expected: 全绿，含 `tests/read_only_guard.rs` 的两张表**一字未改**地通过。

- [ ] **Step 4: 跑 fmt 与 clippy（只看新增行）**

Run: `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`
Expected: 只有既有那 4 个文件的 fmt 差异与 `src/store/skills.rs` 的两条
`unnecessary_get_then_check` **照旧**存在；**新增/改动的行零告警**。
新文件里 `removed: Vec::new(),` 这类必须过；不要在既有债务文件上做任何格式化。

- [ ] **Step 5: Commit**

```bash
git add tests/prune_end_to_end.rs
git commit -m "test(apply): end-to-end prune on the public entry points"
```

---

## Self-Review

### Spec 覆盖

| Spec | 落点 | 钉它的测试 |
| --- | --- | --- |
| §9.8 `henv apply [--prune] [--harness <name>]` | Task 7 Step 6 | `apply_rejects_an_unknown_harness_name`、`the_prompt_says_how_many_items_will_be_deleted` |
| §9.8 prune = 撤销 henv 自己做过的事 | Task 3、Task 6 | `prune_turns_only_ledger_recorded_drift_into_removals`、`a_recorded_skill_copy_is_removed_and_unrecorded`、`prune_removes_only_the_ledger_recorded_copy` |
| §9.8 非 henv 条目只报不删 | Task 3、Task 6、Task 8 | `an_empty_ledger_prunes_nothing`、`non_remove_entries_are_never_touched`、`prune_with_an_empty_ledger_deletes_nothing` |
| §9.3 账本键 `(harness, kind, name)` | Task 2 | `remove_drops_exactly_one_slot`、`a_ledger_entry_from_another_harness_does_not_authorize_a_removal` |
| §9.3 不变式二（只碰账本记着的） | Task 3、Task 6 | 同上 + `a_remove_without_a_ledger_record_is_reported_and_deletes_nothing` |
| §9.7 失败隔离：单条失败不中断 | Task 6 | `one_failure_does_not_stop_the_rest_and_only_successes_are_unrecorded` |
| §9.7 事务：tmp+fsync+rename、单槽 `.bak` | Task 4 | `write_config` 既有测试 + 「空操作不落 `.bak`」三条 |
| §9.9 写 API 白名单 | 全程 | `tests/read_only_guard.rs` **一字未改**地通过（撤销落在 `src/apply/`，shell out 落在 `src/apply/mcp_claude.rs`） |
| §9.10 测试策略：公开入口 e2e | Task 8 | `tests/prune_end_to_end.rs`（3 条）、`tests/bundle_end_to_end.rs`（+1 条） |
| §9.5 源扫描与目标解耦 | Task 7 | `harness_filtering_never_turns_off_the_source_scan` |

### Placeholder 扫描

本文件里没有 `TBD` / `TODO` / 「类似 Task N」这类占位。每个 Step 要么是可直接粘贴的
代码块，要么是 `Run:` + `Expected:` 的命令，没有「照上面那样写」的转述。

### 类型一致性

- `Action::Remove`（单元变体，无载荷）——Task 1 定形，Task 3 产它，Task 6 认它。
- `Ledger::remove(&mut self, harness: &str, kind: Kind, name: &str) -> bool`——Task 2 定义，
  Task 6 调用；返回 `bool` 是因为「重跑 `--prune`」必须能区分「摘到了」与「本来就没了」。
- `HarnessReport { harness, delivered, removed, failure }`——Task 5 加 `removed`，
  Task 6 填它，Task 7 读 `removed_total()`。
- `run_apply(manifest_path, home, out, confirm, prune: bool, only: Option<&str>)`——
  Task 7 之后共 **6 个调用点**，全部在 Task 7 里改完：`src/main.rs` 1 处、
  `src/lib.rs` 的 3 处测试（`apply` / `apply_with` / 确认文案那条）、
  `tests/mcp_end_to_end.rs` 与 `tests/git_source_end_to_end.rs` 各 1 处。
- `plan_for(.., mode, only)`——多出的 `only` 只影响 `pairs` 的构造；
  `scan_source_harnesses(&sources, &registry)` 的**签名一个字都没动**，它就是「源扫描
  不受 `--harness` 影响」这条性质的实现保证。

### Review Focus 对照

1. **非 henv 的 drift 绝不能被删**——Task 3 的 `prune_turns_only_ledger_recorded_drift_into_removals`
   与 `an_empty_ledger_prunes_nothing`；Task 6 的 `non_remove_entries_are_never_touched`；
   Task 8 的 `prune_removes_only_the_ledger_recorded_copy`（逐字节比对）与
   `prune_with_an_empty_ledger_deletes_nothing`。
2. **没给 `--prune` 时零删除**——Task 7 的 `apply_without_prune_never_deletes_a_drifted_copy`；
   Task 8 的第 3 步。实现上的保证是：`prune_plan` 只在 `run_apply` 的 `if prune` 里被调用，
   而 `diff::compute` 永不产 `Action::Remove`。
3. **失败隔离且只有成功摘账**——Task 6 的
   `one_failure_does_not_stop_the_rest_and_only_successes_are_unrecorded`（坏 TOML 让 MCP 失败，
   skill 那半照删；断言失败那条**仍在账本里**）。
4. **`--harness` 不许把源扫描一起关掉**——Task 7 的
   `harness_filtering_never_turns_off_the_source_scan`（`source_harness: claude-code`
   而 `--harness codex`，拆出来的 skill 仍要落到 codex 上）。
5. **空操作不重写文件**——Task 4 的三条「不落盘、不落 `.bak`、不建父目录」。
6. **`diff` 与 golden 不动**——Task 1 只加一个没人调的枚举臂与计数；Task 3 加一个没人调的
   纯函数；`src/snapshots/` 全程零改动，`spec_example_matches_design_doc` 与 `remove 恒为 0`
   原样通过。

### 已知取舍与遗留（**都不是本计划的 bug，但要在 review 时说清**）

- **`- drift` 行现在写着 `(未在清单中，--prune 可删)`，而 `--prune` 只删账本记着的。**
  这句提示是 Plan A 留下的、被 insta golden 逐字节锁住的字面串，
  而 Review Focus 6 要求 golden 不动。于是**手放的 drift 会被提示「可删」而实际删不掉**。
  本计划按裁决**不改它**。建议的后续（不在本计划内）：单独一次改动把提示改成
  「账本记着的可删」，并同步 `src/snapshots/`——那是一次**只动文案**的小改动，
  与 B3c 的实现耦合度为零。
- **claude 的撤销不可端到端测。** `run_apply` 走 `registry(home)`，没法把 shim 注进去，
  所以 `--prune` 删 claude 的 MCP 只能靠 `mcp_claude::tests`（假 shim 证 argv）+ 那条
  `#[ignore]` 的真 CLI 冒烟测试。这与 B3a 交付侧**同一条**已知边界，不是新缺口。
- **删除不校验内容哈希。** §9.3 已接受「用户手改过的副本会被 `--prune` 删掉」这一取舍；
  有资格决定删不删的是账本，不是哈希。这条要写进最终 review 的「已知代价」。
- **`--prune` 与 `--harness` 的组合**只由 Task 7 的 `prune_deletes_only_what_the_ledger_records`
  （`prune=true, only=Some("codex")`）覆盖一次；「`--harness` 点名一家没装的家」不在本计划内
  ——那条走的是既有的 `detected` 跳过路径，与 prune 无交互。
- **`examples/env.yaml:35` 有一句同源的 stale 注释**（「degrade 策略在 Plan A 只是声明性占位」），
  与 Task 7 Step 5 改掉的那句同病。它不在 B3c 的功能路径上，本计划**不动它**，
  免得多出一个与本次 review 无关的 hunk；建议在 review 时一并决定是否顺手改成一句事实。
