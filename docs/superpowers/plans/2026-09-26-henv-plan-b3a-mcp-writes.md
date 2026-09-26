# Plan B3a：MCP 三家写入实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `henv apply` 把清单里的 MCP 真的写进 claude-code / codex / kimi-code 三家的用户级配置，并记账，使得「apply 之后再 diff 就是 in-sync」。

**Architecture:** 读路径已经存在（B1/B2 的 `read_state` 会从三家配置里扫出 MCP 能力）。本计划补的是写路径：`src/apply/` 下新增一组 MCP 写入器，**落点由 adapter 的 `mcp_target()` 给出**，与读路径指向同一个文件——「写进去了、读不出来」于是不是被测试抓出来的，是结构上不可能的。claude 走 shell out（`claude mcp add-json -s user`，因为 `~/.claude.json` 被 harness 频繁整体重写，直接 merge 会与它并发写竞争），codex 走 `toml_edit` 子树级 merge，kimi 走 serde_json 顶层键 merge。写盘原语（同目录临时文件 → fsync → rename + 单槽 `.bak`）在 `src/apply/write.rs` 里与 skill 交付并列。

**Tech Stack:** Rust 2024 / clap 4.6 / serde 1 / serde_json / serde_yaml_ng / toml 1.1 / anyhow / sha2；**本计划新增 `toml_edit = "0.25.15"`**（§9.12 已解禁，只用于 §9.6 的子树级 merge）。dev-deps：insta、tempfile。

**Spec:** `docs/design.md` §9.6（三家的写路径、机制选择、账本判定表）、§9.7（写盘协议与失败隔离）、§9.9（写 API 白名单与 shell out 允许位）、§9.10（测试策略）、§9.12（依赖裁决）。另见 §3 决策 3（密钥永不进任何产物）与 §9.3 不变式二。

## Global Constraints

- 对话中文；代码标识符、注释、commit message 一律英文。
- **`tests/read_only_guard.rs` 的 `PROCESS_ALLOWLIST` 只按文件豁免，绝不按目录**（§9.9）。本计划唯一新增的 shell out 文件是 `src/apply/mcp_claude.rs`，它必须被加进那张表，并带一行理由。`the_allowlist_is_not_the_whole_tree` 对 `src/capability.rs` 断言零违规——这条不许放宽。
- 写 API（`fs::write(` 等）只允许出现在 `src/apply/` 与 `src/store/` 之下（`WRITE_WHITELIST`）。
- **测试绝不触碰真实 `~`。** 一切夹具落在 `TempHome`（`TempDir`）下。所有会起外部进程的测试，HOME 必须由 fixture 钉死。
- env 值**永不展开、永不重新引号、永不剥花括号**（`lower.rs:91` 与 `manifest::env_refs` 已保证清单侧的值一律是裸 `${VAR}`，`secrets::parse_ref` 是这条的机械保证）。写进任何配置文件或 argv 的都是 `${VAR}` 那个串本身。
- `mcp_summary` **只渲染 env 键名，绝不渲染 env 值**（§3 决策 3，`src/capability.rs:193`）。一切面向用户的 MCP 摘要都复用它，不许另写一个。
- 不主动 commit、push；不创建远程。每个 Task 的 commit 步骤只提交本 Task 涉及的文件。
- 只改本计划点名的文件，不顺手重构无关部分。

## Review Focus

按「最可能先咬人」排序。每一条都在它该在的那个 Task 里有测试钉着。

1. **claude CLI 不在 PATH 上 / 版本太老 / 没登录。** 三家 adapter 里 claude 是唯一靠外部程序的。用户机器上没装 CLI 时，`apply` 必须报成**这一家的 partial 失败**（§9.7 失败隔离），而不是上抛打断另外两家，更不能静默当成成功。→ Task 8、Task 9。
2. **`~/.codex/config.toml` 语法坏了。** 用户手改坏了配置，`apply` 必须**拒绝写并让文件一字节不变**——半写入一个语法坏掉的 config.toml 会连坐整个 codex。→ Task 6。
3. **`mcp_servers` / `mcpServers` 存在但类型不对**（是字符串、是数组）。读路径报故障，写路径也必须报故障、不许覆盖。→ Task 6、Task 7。
4. **env 值里带 shell 元字符。** 值只可能是 `${VAR}`，但 claude 那条路把 JSON 当 **argv** 传给外部程序——任何形式的 shell 拼串（`sh -c`）都会让 `${VAR}` 被展开。必须走 `Command::args`，一个字节都不许经过 shell。→ Task 8。
5. **同名条目既是 skill 又是 MCP。** 账本今天按 `(harness, name)` 找，两条会互相覆盖；而 `~/.claude/skills/foo` 与 `~/.claude.json` 里的 `foo` 是两件不同的东西。→ Task 1。
6. **codex 的 `mcp_servers = { ... }` 内联表写法。** `toml` crate 把它读成 `Value::Table`（没有独立的 InlineTable 变体），所以读路径认它；若写路径只认 `Item::Table`，用户就会遇到「diff 说该装、apply 说写不了」。→ Task 6。
7. **同一次 apply 里对同一个 claude server 写两次**（两家的 targets 重叠、或 update 的 remove+add 中途失败）。→ Task 8、Task 9。

---

## File Structure

| 文件 | 动作 | 职责 |
| --- | --- | --- |
| `src/store/provenance.rs` | 修改 | `find`/`record` 的键加入 `kind`；同名 skill/mcp 回归测试 |
| `src/store/hash.rs` | 修改 | 新增 `hash_mcp_spec`（MCP 三段载荷的规范化哈希） |
| `src/diff.rs` | 修改 | `mcp_verdict`（§9.6 账本判定表）；`compute` 里接上它 |
| `src/capability.rs` | 修改 | `mcp_summary` 由私有改 `pub(crate)`（diff 要复用） |
| `src/adapters/mod.rs` | 修改 | trait 新增 `mcp_target()`；默认 `apply` 串起两种交付 |
| `src/adapters/claude_code.rs` | 修改 | `mcp_target()` → `~/.claude.json` + `ClaudeCli` |
| `src/adapters/codex.rs` | 修改 | `mcp_target()` → `~/.codex/config.toml` + `CodexToml` |
| `src/adapters/kimi_code.rs` | 修改 | `mcp_target()` → `~/.kimi-code/mcp.json` + `KimiJson` |
| `src/testutil.rs` | 修改 | `FakeAdapter::with_mcp_target`；新增 `FakeCli` shim 夹具 |
| `src/apply/mcp.rs` | 新建 | `McpTarget`/`McpMechanism` 类型；`deliver_mcps` 与写入器分派 |
| `src/apply/write.rs` | 修改 | 新增 `write_config`（tmp → fsync → rename + 单槽 `.bak`） |
| `src/apply/mcp_claude.rs` | 新建 | **唯一新增的 shell out 点**：`claude mcp remove` / `add-json` |
| `src/apply/mcp_toml.rs` | 新建 | codex 的 `[mcp_servers]` 子树 merge（`toml_edit`） |
| `src/apply/mcp_json.rs` | 新建 | kimi 的 `mcpServers` 顶层键 merge（serde_json） |
| `src/apply/mod.rs` | 修改 | `HarnessReport::absorb`；`execute` 接收 `desired` |
| `src/lib.rs` | 修改 | `Planned` 增加 `desired`；`plan_for` 填它；`run_apply` 传下去 |
| `tests/read_only_guard.rs` | 修改 | `PROCESS_ALLOWLIST` 加入 `src/apply/mcp_claude.rs` |
| `Cargo.toml` | 修改 | 新增 `toml_edit = "0.25.15"` |
| `tests/mcp_end_to_end.rs` | 新建 | §9.10 的性质在 MCP 交付上的样子（codex / kimi 走 `run_apply`） |
| `docs/design.md` | 修改 | §9.6 的 `hash_spec` 改名为 `hash_mcp_spec` 并写理由；§9.7 补 `.bak` 与临时文件的命名 |

---

### Task 1: 账本键加入 `kind`

**Files:**
- Modify: `src/store/provenance.rs:109-124`（`find`/`record`）、同文件 `mod tests`
- Modify: `src/diff.rs:199`、`src/apply/mod.rs:243,308,503,504`、`src/adapters/mod.rs:421`、`src/lib.rs:535,536,538`

**Interfaces:**
- Consumes: `crate::capability::Kind`（`LedgerEntry.kind` 字段已存在，B1 留的）。
- Produces: `Ledger::find(&self, harness: &str, kind: Kind, name: &str) -> Option<&LedgerEntry>`、`Ledger::record(&mut self, entry: LedgerEntry)`（签名不变，语义变为按三元组 upsert）。**Task 9 的 `deliver_mcps` 依赖这个签名。**

- [ ] **Step 1: 写失败的回归测试**

在 `src/store/provenance.rs` 的 `mod tests` 里，先把 `entry` 帮手改成带 kind：

```rust
    fn entry(harness: &str, name: &str, hash: &str) -> LedgerEntry {
        entry_of(harness, Kind::Skill, name, hash)
    }

    fn entry_of(harness: &str, kind: Kind, name: &str, hash: &str) -> LedgerEntry {
        LedgerEntry {
            harness: harness.into(),
            kind,
            name: name.into(),
            path: PathBuf::from("/somewhere").join(name),
            source_id: "agents-dir".into(),
            content_hash: hash.into(),
            delivered_at_unix: 1_700_000_000,
        }
    }
```

再加这条测试（放在 `record_overwrites_the_same_harness_and_name` 之后）：

```rust
    /// B3a：同名的一个 skill 与一个 mcp 是**两件不同的东西**——
    /// `~/.claude/skills/foo` 与 `~/.claude.json` 里的 `foo` 没有任何关系。
    /// B1 的键只有 `(harness, name)`，mcp 一进来两边就会互相顶掉：
    /// 记了 mcp 之后 diff 会以为那个 skill 没交付过，转头报成 conflict。
    #[test]
    fn a_skill_and_an_mcp_with_the_same_name_occupy_different_slots() {
        let mut l = Ledger::default();
        l.record(entry_of(CLAUDE_CODE, Kind::Mcp, "foo", "mcp-hash"));
        l.record(entry_of(CLAUDE_CODE, Kind::Skill, "foo", "skill-hash"));

        assert_eq!(l.entries.len(), 2, "同名不同 kind 是两条记录，不是一条");
        assert_eq!(
            l.find(CLAUDE_CODE, Kind::Skill, "foo").unwrap().content_hash,
            "skill-hash"
        );
        assert_eq!(
            l.find(CLAUDE_CODE, Kind::Mcp, "foo").unwrap().content_hash,
            "mcp-hash"
        );

        // record 的覆盖也按三元组：同名 mcp 再记一次是替换那一条，不动 skill 那条
        l.record(entry_of(CLAUDE_CODE, Kind::Mcp, "foo", "mcp-v2"));
        assert_eq!(l.entries.len(), 2, "upsert 不该新增行");
        assert_eq!(
            l.find(CLAUDE_CODE, Kind::Mcp, "foo").unwrap().content_hash,
            "mcp-v2"
        );
        assert_eq!(
            l.find(CLAUDE_CODE, Kind::Skill, "foo").unwrap().content_hash,
            "skill-hash",
            "改 mcp 不能碰到同名 skill"
        );
    }
```

文件顶部需要 `CLAUDE_CODE`（若还没有，用 `crate::adapters::CLAUDE_CODE`；若该常量在别处，直接写 `"claude-code"` 字面量即可，本测试不关心常量名）。

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib store::provenance::tests::a_skill_and_an_mcp_with_the_same_name_occupy_different_slots`
Expected: 编译失败，报 `find` 参数个数不对 / `expected 3 arguments, found 2`。这正是要的失败：签名还没改。

- [ ] **Step 3: 改签名**

`src/store/provenance.rs`：

```rust
    /// 按 `(harness, kind, name)` 找。**kind 不可省**：同名的一个 skill 与一个 mcp
    /// 落在不同的目标位置上（前者是 `~/.claude/skills/<n>`，后者是 `~/.claude.json`
    /// 里的一个键），账本把它们当成一条记录的话，后交付的那个会把前一个顶掉。
    pub fn find(&self, harness: &str, kind: Kind, name: &str) -> Option<&LedgerEntry> {
        self.entries
            .iter()
            .find(|e| e.harness == harness && e.kind == kind && e.name == name)
    }

    /// 按 `(harness, kind, name)` upsert。
    pub fn record(&mut self, entry: LedgerEntry) {
        match self.entries.iter_mut().find(|e| {
            e.harness == entry.harness && e.kind == entry.kind && e.name == entry.name
        }) {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }
```

（`save` 已经按 `(&a.harness, &a.kind, &a.name)` 排序，不用动。）

- [ ] **Step 4: 修所有调用点**

逐个改成传 kind。生产侧一处：

`src/diff.rs:199`（`content_verdict` 内）：
```rust
            match ledger.find(harness, Kind::Mcp, &cap.id.name) {
```
——**注意**：这一步只是让它编译通过；Task 3 会把这一行所在的 `content_verdict` 换成 `ownership_verdict`。此处先传 `cap.id.kind`，Task 3 再改。**先写 `cap.id.kind`**：

```rust
            match ledger.find(harness, cap.id.kind, &cap.id.name) {
```

测试侧（全部是 `#[cfg(test)]`）：
- `src/adapters/mod.rs:421` → `ledger.find("claude-code", Kind::Skill, "foo")`
- `src/apply/mod.rs:243` → `ledger.find("claude-code", Kind::Skill, "foo")`
- `src/apply/mod.rs:308` → `ledger.find("claude-code", Kind::Skill, "bad")`
- `src/apply/mod.rs:503` → `ledger.find(CLAUDE_CODE, Kind::Skill, "foo")`
- `src/apply/mod.rs:504` → `ledger.find("codex", Kind::Skill, "foo")`
- `src/lib.rs:535` → `ledger.find(CLAUDE_CODE, Kind::Skill, "foo")`
- `src/lib.rs:536` → `ledger.find(CODEX, Kind::Skill, "foo")`
- `src/lib.rs:538` → `ledger.find(CLAUDE_CODE, Kind::Skill, "foo")`

（这些文件若没 import `Kind`，补一行 `use crate::capability::Kind;`——测试模块里就近 import，不许提到文件顶部污染生产 use。）

- [ ] **Step 5: 跑全量测试**

Run: `cargo test`
Expected: 全绿，含新加的那条。

- [ ] **Step 6: Commit**

```bash
git add src/store/provenance.rs src/diff.rs src/apply/mod.rs src/adapters/mod.rs src/lib.rs
git commit -m "fix(ledger): key entries by (harness, kind, name)"
```

---

### Task 2: `hash_mcp_spec`

**Files:**
- Modify: `src/store/hash.rs`
- Test: `src/store/hash.rs`（同文件的 `mod tests`）

**Interfaces:**
- Produces: `pub fn hash_mcp_spec(command: &str, args: &[String], env: &BTreeMap<String, String>) -> String`。**Task 3 的 `mcp_verdict` 与 Task 9 的 `deliver_mcps` 都用它。**

**关于与 §9.6 的字面出入（Ruling，写进 ledger）：** §9.6 写的是 `store::hash::hash_spec`（对 spec 的规范化序列化取哈希）。本计划实现为 `hash_mcp_spec(command, args, env)`：`Spec` 没有 `Serialize`（derive 的是 `Debug, Clone, PartialEq, Eq`），为了一个哈希去 derive serde 会把序列化形状变成对外契约；而 `&Spec` 版本要为另外五个变体写**永远走不到**的 arm，那种 arm 是错误藏身处。三个原生字段还让「拿一个 skill spec 来哈希」在类型上不可表达。代价：将来若需要其它 kind 的 spec 哈希，得再写一个同形状的函数。Task 10 把 §9.6 的措辞改成 `hash_mcp_spec`。

- [ ] **Step 1: 写失败的测试**

在 `src/store/hash.rs` 的 `mod tests` 里：

```rust
    /// 与 `mcp_summary` 的关键区别：**env 值参与哈希**。`compare_versions` 的 Mcp arm
    /// 比的是 `dc == ac && da == aa && de == ae`——env 值不同就是 Different，
    /// 所以哈希也必须把值算进去，否则账本会说「这是我交付的」而 diff 说不一致。
    #[test]
    fn hash_mcp_spec_covers_all_three_fields() {
        let none = BTreeMap::new();
        let a = hash_mcp_spec("npx", &["-y".into(), "pkg".into()], &none);
        assert_eq!(a, hash_mcp_spec("npx", &["-y".into(), "pkg".into()], &none), "同输入同哈希");
        assert_ne!(a, hash_mcp_spec("npxx", &["-y".into(), "pkg".into()], &none), "command 要算");
        assert_ne!(a, hash_mcp_spec("npx", &["-y".into()], &none), "args 要算");
        assert_ne!(a, hash_mcp_spec("npx", &["y".into(), "pkg".into()], &none), "args 逐项比");

        let mut e1 = BTreeMap::new();
        e1.insert("K".to_string(), "${K}".to_string());
        let mut e2 = BTreeMap::new();
        e2.insert("K".to_string(), "${OTHER}".to_string());
        assert_ne!(
            hash_mcp_spec("npx", &[], &e1),
            hash_mcp_spec("npx", &[], &e2),
            "env 的**值**要算——只算键名会让两种不同的配置哈希相同"
        );
        assert_ne!(hash_mcp_spec("npx", &[], &none), hash_mcp_spec("npx", &[], &e1));
    }

    /// 长度前缀不可省：`("ab","c")` 与 `("a","bc")` 不许喂出同一串字节。
    /// 这条测试对着 `feed` 的那条注释，注释里的承诺必须真的成立。
    #[test]
    fn hash_mcp_spec_is_not_confused_by_field_boundaries() {
        let none = BTreeMap::new();
        assert_ne!(
            hash_mcp_spec("ab", &["c".into()], &none),
            hash_mcp_spec("a", &["bc".into()], &none)
        );
    }
```

若文件顶部没有 `use std::collections::BTreeMap;` 与 `use super::*;`，补上。

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib store::hash::tests::hash_mcp_spec_covers_all_three_fields`
Expected: 编译失败，`cannot find function hash_mcp_spec`。

- [ ] **Step 3: 实现**

在 `src/store/hash.rs`，紧接 `hash_str` 之后：

```rust
/// MCP 的三段载荷的规范化哈希，用作账本的 `content_hash`。
///
/// **为什么收三个字段而不是一个 `&Spec`**：`Spec` 没有 `Serialize`；为哈希去 derive
/// serde 会把序列化形状变成对外契约。而 `&Spec` 版本还得为另外五个变体写永远走不到的
/// arm——错误最喜欢住在那儿。三个原生字段顺带让「拿一个 skill spec 来算 mcp 哈希」
/// 在类型上不可表达。
///
/// **env 的值参与哈希**，与 `capability::mcp_summary`（只渲染键名，§3 决策 3）不同：
/// 摘要是给人看的，哈希是给 `compare_versions` 的 Same/Different 判定配套的账面。
pub fn hash_mcp_spec(command: &str, args: &[String], env: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update([0u8, 0]);
    hasher.update(command.as_bytes());
    hasher.update([0]);
    hasher.update((args.len() as u64).to_le_bytes());
    for a in args {
        hasher.update((a.len() as u64).to_le_bytes());
        hasher.update(a.as_bytes());
    }
    hasher.update((env.len() as u64).to_le_bytes());
    for (k, v) in env {
        hasher.update((k.len() as u64).to_le_bytes());
        hasher.update(k.as_bytes());
        hasher.update((v.len() as u64).to_le_bytes());
        hasher.update(v.as_bytes());
    }
    hex(hasher.finalize().as_slice())
}
```

（`BTreeMap` 的遍历顺序由键决定，跨机器稳定；长度前缀让字段边界不可伪造。）

- [ ] **Step 4: 跑测试，确认它通过**

Run: `cargo test --lib store::hash`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/store/hash.rs
git commit -m "feat(store): add hash_mcp_spec for MCP ledger content hashes"
```

---

### Task 3: `mcp_verdict`——MCP 的账本判定

**Files:**
- Modify: `src/capability.rs:193`（`mcp_summary` 改 `pub(crate)`）
- Modify: `src/diff.rs`
- Test: `src/diff.rs`（同文件 `mod tests`）

**Interfaces:**
- Consumes: `hash_mcp_spec`（Task 2）、`Ledger::find(harness, kind, name)`（Task 1）、`mcp_summary`。
- Produces: `fn ownership_verdict(harness: &str, cap: &Capability, actual: Option<&Capability>, content: &ContentIndex, ledger: &Ledger) -> Option<Verdict>`（私有，diff 内部用）。**Task 4 之后 `compute` 的调用点形状不变。**

**判定表（§9.6，逐行实现）：**

| 现状 vs 清单 | 账本 | 结论 |
| --- | --- | --- |
| 相同 | （不看） | `in-sync` |
| 不同 | 无 | `conflict`（不是 henv 交付的，绝不覆盖） |
| 不同 | 有，且账本哈希 == 现状哈希 | `update { from: 现状, to: 期望 }` |
| 不同 | 有，但账本哈希 != 现状哈希 | `update` + 注「副本被手改过」 |

**关键：哈希算的是「现状」而不是「期望」。** 拿期望的哈希去比账本，会让清单改一次之后所有 MCP 都假报「被手改过」——这是镜像 `content_verdict` 时必须照抄的一点。

- [ ] **Step 1: 把 `mcp_summary` 改成 `pub(crate)`**

`src/capability.rs:193`：

```rust
pub(crate) fn mcp_summary(command: &str, args: &[String], env: &BTreeMap<String, String>) -> String {
```

理由写在它已有的 doc 注释里补一句：

```
/// 可见性提成 `pub(crate)` 是因为 `diff` 要拿它渲染 `update` 的 from/to——
/// 再写一个同样的渲染函数就等于有了两份「什么算同一个 MCP」的定义。
```

- [ ] **Step 2: 写失败的测试**

先在 `src/diff.rs` 的 `mod tests` 里加一个能喂账本的入口（`plan_a` 今天写死空账本）：

```rust
    fn plan_a_with_ledger(desired: &[Capability], inputs: &[HarnessInput], ledger: &Ledger) -> Plan {
        compute(desired, inputs, &no_degrade(), &ContentIndex::default(), ledger)
    }

    fn run_with_ledger(desired: &[Capability], adapter: FakeAdapter, ledger: &Ledger) -> HarnessPlan {
        let state = HarnessState {
            name: adapter.name().to_string(),
            detected: adapter.detect(),
            actual: adapter.read_state().unwrap(),
        };
        let inputs = vec![HarnessInput {
            state: &state,
            adapter: &adapter,
        }];
        plan_a_with_ledger(desired, &inputs, ledger)
            .harnesses
            .into_iter()
            .next()
            .unwrap()
    }
```

再把 `run` 改成委托，避免两份重复：

```rust
    fn run(desired: &[Capability], adapter: FakeAdapter) -> HarnessPlan {
        run_with_ledger(desired, adapter, &Ledger::default())
    }
```

新测试（放在 `changed_command_is_update_with_actual_then_desired` 附近）：

```rust
    fn mcp_entry(harness: &str, name: &str, hash: &str) -> LedgerEntry {
        LedgerEntry {
            harness: harness.into(),
            kind: Kind::Mcp,
            name: name.into(),
            path: PathBuf::from("/cfg").join(harness),
            source_id: format!("manifest:mcp/{name}"),
            content_hash: hash.into(),
            delivered_at_unix: 1_700_000_000,
        }
    }

    /// §9.6 表第一行：字节相同就不看账本。账本里有没有这条、是不是 henv 交付的，
    /// 都不改变「它已经是对的了」。
    #[test]
    fn an_identical_mcp_is_in_sync_even_without_a_ledger_entry() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "npx")]),
        );
        assert_eq!(labels(&hp), vec![("mcp/context7".into(), "in-sync".into())]);
    }

    /// §9.6 表第二行：不同 + 无账本 = 不是 henv 交付的 → conflict。
    /// 这条**取代**了 B1 时期那个「不同即 update」的旧结论（见下一条测试的说明）。
    #[test]
    fn a_differing_mcp_without_a_ledger_entry_is_a_conflict() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "other")]),
        );
        assert_eq!(labels(&hp), vec![("mcp/context7".into(), "conflict".into())]);
        match &hp.entries[0].action {
            Action::Conflict { reason } => assert!(
                reason.contains("不是 henv 交付的"),
                "理由要说清为什么不动手：{reason}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// §9.6 表第三行：不同 + 有账本 + 账本哈希 == 现状 → 是我们交付的旧版本，放心 update。
    /// 账本哈希算的是**现状**（从配置里读回来的那份），不是期望。
    #[test]
    fn a_differing_mcp_we_delivered_is_an_update() {
        let actual = FakeAdapter::mcp("context7", "old");
        let Spec::Mcp { command, args, env } = &actual.spec else {
            unreachable!()
        };
        let mut l = Ledger::default();
        l.record(mcp_entry(
            "codex",
            "context7",
            &crate::store::hash::hash_mcp_spec(command, args, env),
        ));

        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![actual]),
            &l,
        );
        match &hp.entries[0].action {
            Action::Update { from, to } => {
                assert_eq!(from, "old", "from = 现状");
                assert_eq!(to, "npx", "to = 期望");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(hp.entries[0].note, None, "没被手改过就不该有注");
    }

    /// §9.6 表第四行：账本哈希 != 现状 → 交付之后有人改了它。仍然 update
    /// （账本说这块地是我们的），但要把这件事说出来。
    #[test]
    fn a_delivered_mcp_edited_by_hand_gets_the_note() {
        let mut l = Ledger::default();
        l.record(mcp_entry("codex", "context7", "hash-of-something-else"));

        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "old")]),
            &l,
        );
        assert!(matches!(hp.entries[0].action, Action::Update { .. }));
        assert_eq!(hp.entries[0].note.as_deref(), Some("副本被手改过"));
    }

    /// 改造之后**不能再**拿期望的哈希去比账本：那样清单每改一次，
    /// 所有 MCP 都会假报「被手改过」——这正是本 Task 最容易写错的一处。
    /// 构造：账本记的是**旧期望**的哈希，而现状第三份内容。
    #[test]
    fn changing_the_manifest_does_not_fake_a_hand_edit() {
        let old_desired = FakeAdapter::mcp("context7", "npx");
        let Spec::Mcp { command, args, env } = &old_desired.spec else {
            unreachable!()
        };
        let mut l = Ledger::default();
        l.record(mcp_entry(
            "codex",
            "context7",
            &crate::store::hash::hash_mcp_spec(command, args, env),
        ));

        // 现状与账本哈希一致 → 没被手改过，尽管期望已经变了
        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("uvx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "npx")]),
            &l,
        );
        assert_eq!(hp.entries[0].note, None, "改了清单不是「被手改过」");
    }

    /// 同名的一件事：skill 与 mcp 的账本槽不能串（Task 1 的签名在 diff 里的兑现）。
    #[test]
    fn an_mcp_verdict_does_not_read_the_skill_ledger_slot() {
        let mut l = Ledger::default();
        // 只记了 skill 那条，mcp 那条没有
        l.record(LedgerEntry {
            harness: "codex".into(),
            kind: Kind::Skill,
            name: "context7".into(),
            path: PathBuf::from("/skills/context7"),
            source_id: "agents-dir".into(),
            content_hash: "skill-hash".into(),
            delivered_at_unix: 1,
        });

        let hp = run_with_ledger(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "other")]),
            &l,
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/context7".into(), "conflict".into())],
            "skill 的账本条目不许被当成 mcp 的交付凭证"
        );
    }
```

`PlanEntry` 是否有 `note` 字段？有（`verdict_note` 被赋给它）。若名字不是 `note`，按实际字段名改这两处断言。`PathBuf` / `LedgerEntry` 需在测试模块里 import（`use crate::store::provenance::LedgerEntry; use std::path::PathBuf;`）。

- [ ] **Step 3: 跑测试，确认它们失败**

Run: `cargo test --lib diff::tests::a_differing_mcp_without_a_ledger_entry_is_a_conflict`
Expected: FAIL——实际报 `update`（今天 `compare_versions` 的 Different 直接落成 Update），断言要的是 `conflict`。

- [ ] **Step 4: 实现 `mcp_verdict` 与分派**

在 `src/diff.rs` 里、`content_verdict` 之后加：

```rust
/// 取出 MCP 的三段载荷；不是 MCP 就 `None`。
fn mcp_fields(spec: &Spec) -> Option<(&str, &[String], &BTreeMap<String, String>)> {
    match spec {
        Spec::Mcp { command, args, env } => Some((command, args, env)),
        _ => None,
    }
}

/// 从账本层裁决一条 MCP（§9.6 的判定表）。
///
/// 「是谁放的」这件事，内容层（skill）从字节读得出来，MCP 读不出来——目标侧只有
/// 一份配置，没有来源痕迹。所以这里的证据是账本：**本次交付时记下的哈希**。
///
/// 返回 `None` = 这条不是 MCP（或现状不是），交给原来的 `compare_versions`。
fn mcp_verdict(
    harness: &str,
    cap: &Capability,
    actual: &Capability,
    ledger: &Ledger,
) -> Option<Verdict> {
    let (dc, da, de) = mcp_fields(&cap.spec)?;
    let (ac, aa, ae) = mcp_fields(&actual.spec)?;

    // 第一行：字节相同就不看账本。与 content_verdict 同一条道理——
    // 目标已经是对的了，它当初是谁放的不改变这个事实。
    if dc == ac && da == aa && de == ae {
        return Some(Verdict {
            action: Action::InSync,
            note: None,
        });
    }

    let to = mcp_summary(dc, da, de);
    let from = mcp_summary(ac, aa, ae);
    match ledger.find(harness, Kind::Mcp, &cap.id.name) {
        // 不同 + 无账本：目标上有一个同名、内容不同、又不是我们放的东西。
        // §9.3 不变式二：绝不覆盖，报出来让人自己决定。
        None => Some(Verdict {
            action: Action::Conflict {
                reason: format!("目标已存在同名 MCP 且不是 henv 交付的（{from}），apply 不会覆盖"),
            },
            note: None,
        }),
        // 哈希算的是**现状**（从配置里读回来的那份），不是期望。
        // 拿期望去比，改一次清单就会让所有 MCP 假报「被手改过」。
        Some(entry) if entry.content_hash == hash_mcp_spec(ac, aa, ae) => Some(Verdict {
            action: Action::Update { from, to },
            note: None,
        }),
        Some(_) => Some(Verdict {
            action: Action::Update { from, to },
            note: Some("副本被手改过".to_string()),
        }),
    }
}

/// 内容层判得动就内容层，判不动就账本层（§9.6）。
///
/// 两层的适用面互斥：`content_verdict` 只认 `Spec::Skill`（第一行 `content.sources.get`），
/// `mcp_verdict` 只认 `Spec::Mcp`——所以这个 `or_else` 不是「谁优先」的策略，
/// 是「按 kind 分派」的写法。
fn ownership_verdict(
    harness: &str,
    cap: &Capability,
    actual: Option<&Capability>,
    content: &ContentIndex,
    ledger: &Ledger,
) -> Option<Verdict> {
    content_verdict(harness, cap, content, ledger).or_else(|| {
        let actual = actual?;
        mcp_verdict(harness, cap, actual, ledger)
    })
}
```

- [ ] **Step 5: 把 `compute` 接上**

`src/diff.rs` 的 `compute` 里，`Support::Native | Support::Degraded { .. } =>` 那个 arm 现在是「先算 `verdict_action`，再 `match state.actual.iter().find(...)`」。把实际条目的查找**提到前面**，让两个裁决都能看到它：

```rust
                Support::Native | Support::Degraded { .. } => {
                    matched.insert(cap.id.clone());
                    // 实际条目先找出来：内容层（skill）与账本层（mcp）的裁决都需要它。
                    let actual = state.actual.iter().find(|a| a.id == cap.id);
                    let (verdict_action, verdict_note) =
                        match ownership_verdict(&state.name, cap, actual, content, ledger) {
                            Some(v) => (Some(v.action), v.note),
                            // 两层都判不了（不是 skill 也不是 mcp，或目标侧没扫到）不等于
                            // 「没什么好说的」：源这次是沿用没刷新的缓存时，「你看到的这份内容
                            // 没经过上游确认」照样成立，而且 install 是它最要紧的一条路——
                            // 要写盘的正是这些没确认过的字节。stale 是源的属性，不该因为
                            // 目标侧扫不到就消失。
                            None => (
                                None,
                                content.sources.get(&cap.id).and_then(stale_note),
                            ),
                        };
                    let action = match actual {
```

（`match actual` 取代原来的 `match state.actual.iter().find(|a| a.id == cap.id)`；下面 `None => Action::Install` / `Some(a) => match &a.spec` 两臂原样不动——`a` 现在是 `&Capability`，`&a.spec` 依旧成立。）

文件顶部补 `use crate::capability::mcp_summary;` 与 `use crate::store::hash::hash_mcp_spec;`。

- [ ] **Step 6: 改写被这次语义变更推翻的旧测试**

`src/diff.rs` 里 `changed_command_is_update_with_actual_then_desired` 现在**必然**失败：它用空账本断言 `Update`，而 §9.6 说不同 + 无账本 = conflict。删掉它，它的两个场景分别被上面新加的 `a_differing_mcp_without_a_ledger_entry_is_a_conflict`（无账本）与 `a_differing_mcp_we_delivered_is_an_update`（有账本）覆盖，且覆盖得**更严格**（旧测试的结论本身是错的）。

```bash
# 用 grep 确认没有别处还在断言这个旧结论
grep -rn "changed_command_is_update_with_actual_then_desired" src/ tests/
```

Expected: 只有 `src/diff.rs` 里那一处定义（已删）与可能的本计划文档。

- [ ] **Step 7: 跑全量测试**

Run: `cargo test`
Expected: 全绿。**留意 `src/lib.rs` 的两条 MCP 测试**（`desired_mcp_present_in_claude_json_is_in_sync_and_missing_one_is_install`、`fully_satisfied_manifest_renders_the_summary_line`）：它们的 spec 字节相同，走判定表第一行，不受影响——若它们红了，说明 `mcp_verdict` 的 InSync 分行写错了，不是「顺手改测试」的时候。

- [ ] **Step 8: Commit**

```bash
git add src/diff.rs src/capability.rs
git commit -m "feat(diff): decide MCP ownership by ledger (spec 9.6)"
```

---

### Task 4: `mcp_target()`——写路径的落点由 adapter 给

**Files:**
- Create: `src/apply/mcp.rs`（本 Task 只放类型与 `mcp_target` 的返回值定义）
- Modify: `src/adapters/mod.rs`（trait + 默认 `apply`）
- Modify: `src/adapters/claude_code.rs`、`src/adapters/codex.rs`、`src/adapters/kimi_code.rs`
- Modify: `src/testutil.rs`（`FakeAdapter`）
- Test: `src/adapters/mod.rs`、`src/testutil.rs`

**Interfaces:**
- Produces:
  - `pub struct McpTarget { pub path: PathBuf, pub mechanism: McpMechanism }`
  - `pub enum McpMechanism { ClaudeCli { binary: PathBuf }, CodexToml, KimiJson }`
  - `HarnessAdapter::mcp_target(&self) -> Option<McpTarget>`（**无默认实现**，同 `skills_target_dir`）
  - `FakeAdapter::with_mcp_target(mcp: McpTarget)`
- Consumes: `Home` 的 `claude_json()` / `codex_config()` / `kimi_mcp()`。

**为什么 `path` 对 claude 也要填：** claude 的字节由 CLI 写，我们不经手；但填上读路径那个文件，才让「写路径 == 读路径」这条约束在三个 adapter 上长得一样、被同一条测试看着。它同时也是账本 `LedgerEntry.path` 的值。

**为什么 `ClaudeCli { binary }` 不是测试后门：** 生产填裸的 `"claude"`（`Command::new` 对单组件相对路径走 PATH 查找，与用户手敲 `claude` 同一件事）。测试填一条 shim 的绝对路径。这一族接口本来就得能指定可执行文件，否则「CLI 不在 PATH 上」这条**真实故障**没法被表达成测试。

- [ ] **Step 1: 写失败的测试**

`src/adapters/mod.rs` 的 `mod tests` 里：

```rust
    /// §9.6：三家的 MCP 写目标必须**指向读路径读的那个文件**。
    /// 这条测试是「写进去了、读不出来」这个故障类的结构防线——
    /// 读路径用的是 `home.claude_json()` 等三个访问器，写路径拿到的必须是同一批。
    #[test]
    fn every_mcp_target_points_at_the_file_the_reader_reads() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        let by_name = |n: &str| {
            reg.iter()
                .find(|a| a.name() == n)
                .unwrap_or_else(|| panic!("{n} 不在 registry 里"))
                .mcp_target()
                .unwrap_or_else(|| panic!("{n} 应当有 MCP 写目标"))
        };

        let claude = by_name(CLAUDE_CODE);
        assert_eq!(claude.path, t.home.claude_json(), "claude 写在 ~/.claude.json");
        assert_eq!(
            claude.mechanism,
            McpMechanism::ClaudeCli {
                binary: PathBuf::from("claude")
            },
            "生产里必须是裸 `claude`：走 PATH，与用户手敲的是同一件事"
        );

        let codex = by_name(CODEX);
        assert_eq!(codex.path, t.home.codex_config());
        assert_eq!(codex.mechanism, McpMechanism::CodexToml);

        let kimi = by_name(KIMI_CODE);
        assert_eq!(kimi.path, t.home.kimi_mcp());
        assert_eq!(kimi.mechanism, McpMechanism::KimiJson);
    }
```

（常量名按 `src/adapters/mod.rs` 里实际的用；`McpMechanism` 需 `PartialEq`。）

- [ ] **Step 2: 跑测试，确认它失败**

Run: `cargo test --lib adapters::tests::every_mcp_target_points_at_the_file_the_reader_reads`
Expected: 编译失败，`no method named mcp_target` / `cannot find type McpMechanism`。

- [ ] **Step 3: 定义类型**

新建 `src/apply/mcp.rs`：

```rust
//! MCP 的交付（spec §9.6）。落点由 adapter 的 `mcp_target()` 给出，
//! **字节由这里写**——读路径与写路径指向同一个文件是结构性的，不靠测试兜。
//!
//! 三家三种机制，因为三家的配置文件性质不同：
//! - claude：`~/.claude.json` 被 harness 频繁整体重写，直接 merge 会与它并发写竞争，
//!   所以交给它自己的 CLI（§9.6 第 1 行）。
//! - codex：`config.toml` 是用户手写的 TOML，只能子树级 merge（`toml_edit`）。
//! - kimi：`mcp.json` 结构简单，顶层键 merge（serde_json）。

use std::path::PathBuf;

/// 一家的 MCP 写目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTarget {
    /// 落点文件。claude 也填：它由 CLI 写，但填上读路径那个文件才让
    /// 「写路径 == 读路径」在三个 adapter 上长得一样；它同时是账本里的 `path`。
    pub path: PathBuf,
    pub mechanism: McpMechanism,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpMechanism {
    /// shell out `claude mcp add-json -s user`（§9.6）。
    ///
    /// `binary` 生产里是裸的 `"claude"`（走 PATH）；测试里是一条 shim 的绝对路径。
    /// **不是测试后门**：这一族接口本来就要能指定可执行文件，否则「CLI 不在 PATH 上」
    /// 这条真实故障没法被表达成测试。
    ClaudeCli { binary: PathBuf },
    /// `toml_edit` 只改 `[mcp_servers]` 子树，其余字节原样保留。
    CodexToml,
    /// serde_json 改 `mcpServers` 键，其余顶层键保留。
    KimiJson,
}
```

在 `src/apply/mod.rs` 里加 `pub mod mcp;`（放在 `pub mod write;` 旁边）。

- [ ] **Step 4: 扩 trait**

`src/adapters/mod.rs`：

```rust
    /// 这家把 MCP 写在哪、用什么机制写（§9.6）。`None` = 这家没有用户级 MCP 配置。
    ///
    /// 与 `skills_target_dir` 一样**没有默认实现**：默认实现会让「新加的 adapter 忘了
    /// 说自己写在哪」编译通过，然后静默地什么都不交付。
    fn mcp_target(&self) -> Option<McpTarget>;
```

顶部 import：`use crate::apply::mcp::{McpMechanism, McpTarget};`。

- [ ] **Step 5: 三家实现**

`src/adapters/claude_code.rs`：

```rust
    fn mcp_target(&self) -> Option<McpTarget> {
        Some(McpTarget {
            path: self.home.claude_json(),
            // `-s user` 不可省：默认 scope 是 `local`，那会写进
            // `projects.<cwd>.mcpServers`——一个读路径故意不看的位置（§9.6）。
            mechanism: McpMechanism::ClaudeCli {
                binary: PathBuf::from("claude"),
            },
        })
    }
```

`src/adapters/codex.rs`：

```rust
    fn mcp_target(&self) -> Option<McpTarget> {
        Some(McpTarget {
            path: self.home.codex_config(),
            mechanism: McpMechanism::CodexToml,
        })
    }
```

`src/adapters/kimi_code.rs`：

```rust
    fn mcp_target(&self) -> Option<McpTarget> {
        Some(McpTarget {
            path: self.home.kimi_mcp(),
            mechanism: McpMechanism::KimiJson,
        })
    }
```

（三家都要 import `McpTarget` / `McpMechanism` 与 `PathBuf`，按现有 use 风格。）

- [ ] **Step 6: `FakeAdapter`**

`src/testutil.rs`：字段加 `pub mcp_target: Option<McpTarget>`，`native()` 里初值 `None`，并加：

```rust
    pub fn with_mcp_target(mut self, mcp: McpTarget) -> Self {
        self.mcp_target = Some(mcp);
        self
    }
```

`impl HarnessAdapter for FakeAdapter` 里加：

```rust
    fn mcp_target(&self) -> Option<McpTarget> {
        self.mcp_target.clone()
    }
```

`src/testutil.rs` 的 `mod tests` 里给 `fake_adapter_answers_from_its_own_fields` 补两行断言：

```rust
        assert_eq!(FakeAdapter::native("codex").mcp_target(), None, "默认没有写目标");
        let t = TempHome::new();
        assert_eq!(
            FakeAdapter::native("codex")
                .with_mcp_target(McpTarget {
                    path: t.path(".codex/config.toml"),
                    mechanism: McpMechanism::CodexToml,
                })
                .mcp_target()
                .unwrap()
                .path,
            t.path(".codex/config.toml")
        );
```

- [ ] **Step 7: 跑全量测试**

Run: `cargo test`
Expected: 全绿。

- [ ] **Step 8: Commit**

```bash
git add src/apply/mcp.rs src/apply/mod.rs src/adapters/ src/testutil.rs
git commit -m "feat(adapters): expose mcp_target for the three harnesses"
```

---

### Task 5: `write_config`——配置文件的落盘原语

**Files:**
- Modify: `src/apply/write.rs`
- Test: `src/apply/write.rs`（同文件 `mod tests`）

**Interfaces:**
- Produces: `pub fn write_config(target: &Path, body: &[u8]) -> anyhow::Result<()>`。**Task 6、Task 7 都用它落盘。**

**协议（§9.7）：** 同目录临时文件 → `fsync` → `rename`。备份是**单槽**的 `<file>.henv.bak`，只在它**还不存在**时写一次——第二次交付不会把「最初的那份」覆盖掉，那才是真的要留的东西。目标文件不存在时不备份。

- [ ] **Step 1: 写失败的测试**

在 `src/apply/write.rs` 的 `mod tests` 里（`use crate::testutil::TempHome;`）：

```rust
    fn bak_of(p: &Path) -> PathBuf {
        let name = p.file_name().expect("file name").to_string_lossy();
        p.with_file_name(format!("{name}.henv.bak"))
    }

    // 临时文件必须清干净——`~/.codex/` 里堆着 `.config.toml.henv-tmp-1234`
    // 不只是难看，harness 自己扫这个目录时可能读它们。
    // **复用同模块已有的 `tmp_leftovers`**（它同样按 `.henv-` 过滤），不另写一份
    // ——那会是同一段逻辑的第二份抄写，而两份过滤条件一旦漂移，这些断言就变成空转。

    #[test]
    fn writing_a_new_config_creates_no_backup() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let target = t.path(".codex/config.toml");
        write_config(&target, b"a = 1\n").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "a = 1\n");
        assert!(!bak_of(&target).exists(), "原本就没有文件，没什么可备份的");
        assert!(
            tmp_leftovers(&t.path(".codex")).is_empty(),
            "{:?}",
            tmp_leftovers(&t.path(".codex"))
        );
    }

    #[test]
    fn the_first_write_backs_up_the_original() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "user's own\n");
        write_config(&target, b"ours\n").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "ours\n");
        assert_eq!(
            std::fs::read_to_string(bak_of(&target)).unwrap(),
            "user's own\n",
            "备份要留住用户原来那份"
        );
        assert!(tmp_leftovers(&t.path(".codex")).is_empty());
    }

    /// 单槽：第一次之后就不该再动它。否则第二次交付会把「用户最初的配置」
    /// 换成「我们上一次写的」，备份就失去了意义。
    #[test]
    fn the_backup_is_single_slot_and_keeps_the_oldest() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "original\n");
        write_config(&target, b"first\n").unwrap();
        write_config(&target, b"second\n").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second\n");
        assert_eq!(
            std::fs::read_to_string(bak_of(&target)).unwrap(),
            "original\n",
            "第二次写不许覆盖备份"
        );
    }

    /// 写盘要经临时文件：目标文件在 rename 之前不许出现新内容。
    /// 这条用一个**同名目录**占住目标路径来间接证伪——rename 一个普通文件到已存在
    /// 的目录上必定失败（EISDIR），而临时文件仍要清干净（半截文件留在用户配置
    /// 目录里是最糟的形状）。
    ///
    /// `.bak` 必须先占上：目标是个目录时，备份那步会先去 `fs::copy` 这个目录而
    /// **先**失败，那样根本走不到 rename，测试名字里的承诺就成了空转。
    #[test]
    fn a_failed_rename_still_cleans_up_its_temp_file() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.mkdir(".codex/config.toml"); // 目录占位
        t.write(".codex/config.toml.henv.bak", "already backed up\n");
        write_config(&target, b"nope\n").unwrap_err();
        assert!(
            tmp_leftovers(&t.path(".codex")).is_empty(),
            "失败的写入也不能留临时文件：{:?}",
            tmp_leftovers(&t.path(".codex"))
        );
    }

    fn mode_of(p: &Path) -> u32 {
        fs::metadata(p).expect("stat").permissions().mode() & 0o777
    }

    /// 权限不许被我们**放宽**：用户或 harness 把这些配置设成 0600（里面有 token），
    /// 重写一遍不该把它变成 umask 默认的 0644。
    ///
    /// 选 `0o640` 而不是 `0o600` 是刻意的：0640 在任何 umask 下都不是 `File::create`
    /// 的默认产物，这条断言因此不会因为开发机 umask 恰好是 077 而空转。
    #[test]
    fn rewriting_a_config_keeps_its_permissions() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "a = 1\n");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();

        write_config(&target, b"a = 2\n").unwrap();

        assert_eq!(mode_of(&target), 0o640, "重写不许动用户设的权限");
    }

    /// 新建的配置是 owner-only：`~/.codex/config.toml`、`~/.kimi-code/mcp.json`
    /// 都可能装 token（`experimental_bearer_token` 之类），而 `~` 通常是
    /// `drwxr-x---`——组内其他用户能穿过去，0644 就等于把 token 递出去。
    #[test]
    fn a_new_config_is_owner_only() {
        let t = TempHome::new();
        let target = t.path(".kimi-code/mcp.json");
        write_config(&target, b"{}\n").unwrap();
        assert_eq!(mode_of(&target) & 0o077, 0, "同机其他用户不许读得到");
    }

    /// 备份是「用户原来那份」的忠实副本，权限也要跟着（`fs::copy` 在 Unix 上会带过
    /// 权限位——这条把它钉住，将来换成手动读写就不会悄悄丢掉）。
    #[test]
    fn the_backup_keeps_the_original_permissions() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "user's own\n");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        write_config(&target, b"ours\n").unwrap();

        assert_eq!(mode_of(&bak_of(&target)), 0o600, "备份不许比原文件更开放");
    }
```

`mod tests` 的 import 里要有 `use std::os::unix::fs::PermissionsExt;`（`mode_of` 与
`Permissions::from_mode` 都靠它；只在函数内部 `use` 会让另一处的 `from_mode` 编译失败）。

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib apply::write::tests::the_first_write_backs_up_the_original`
Expected: 编译失败，`cannot find function write_config`。

- [ ] **Step 3: 实现**

`src/apply/write.rs`（`sibling` 已存在，直接用）：

```rust
/// 写一个配置文件（§9.7）：同目录临时文件 → fsync → rename。
///
/// **为什么不经 `fs::write` 直接落到目标**：写一半被打断（断电、进程被杀、
/// 磁盘满）会留下一个语法坏掉的 `config.toml`，而那会连坐整个 codex——
/// 用户为了装一条 MCP 赔上整个 harness。rename 在同一文件系统内是原子的：
/// 目标要么是旧字节，要么是完整的新字节，没有中间态。
///
/// **备份是单槽的**：只在 `<file>.henv.bak` 不存在时写一次。第二次交付不覆盖它——
/// 要留的是「用户最初的配置」，不是「我们上一次写的那份」。
/// 目标文件不存在时不备份：没有可失去的东西。
pub fn write_config(target: &Path, body: &[u8]) -> anyhow::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{}: no parent directory", target.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("{}: cannot create parent directory", parent.display()))?;

    let name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{}: no file name", target.display()))?;

    // 备份先做：它读的是**改动前**的目标。
    // `symlink_metadata` 而不是 `Path::exists()`：后者跟随符号链接，
    // 一个指向不存在目标的链接会被判成「不存在」，于是我们不备份就把它换掉了。
    if std::fs::symlink_metadata(target).is_ok() {
        let bak = parent.join(format!("{}.henv.bak", name.to_string_lossy()));
        if std::fs::symlink_metadata(&bak).is_err() {
            std::fs::copy(target, &bak)
                .with_context(|| format!("{}: cannot back up", target.display()))?;
        }
    }

    // 权限**只许保留、不许放宽**：这些配置里可能有 token（`config.toml` 的
    // `experimental_bearer_token` 就是明文），而 `~` 通常是 `drwxr-x---`——
    // 组内其他用户能穿过去，umask 默认的 0644 就等于把 token 递出去。
    // 目标已存在就照抄它的权限位（用户或 harness 设过的，我们无权改）；
    // 不存在就用 0600，**不跟 umask 走**。
    let mode = std::fs::metadata(target)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o600);

    // 后缀用 `henv-tmp` 与 `deliver_skill` 同一家族：`.config.toml.henv-tmp-<pid>`。
    // 取别的名字（比如 `tmp`）会让测试里的 `.henv-` 过滤条件一个字节都匹配不到，
    // 「不留残骸」那几条断言随即变成空转。
    let tmp = parent.join(sibling(name, "henv-tmp"));
    let write = || -> anyhow::Result<()> {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("{}: cannot create temp file", tmp.display()))?;
        // 在写入前设权限：临时文件存在的每一刻都不该比目标更开放。
        f.set_permissions(std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("{}: cannot set permissions", tmp.display()))?;
        f.write_all(body)
            .with_context(|| format!("{}: cannot write", tmp.display()))?;
        // fsync：没有它，rename 可能先于数据落盘被持久化，掉电后目标是个空文件。
        f.sync_all()
            .with_context(|| format!("{}: cannot sync", tmp.display()))?;
        Ok(())
    };
    if let Err(err) = write().and_then(|()| {
        std::fs::rename(&tmp, target).with_context(|| format!("{}: cannot replace", target.display()))
    }) {
        // 失败也要收尾：半截临时文件留在用户的配置目录里，比不写更糟。
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}
```

检查 `src/apply/write.rs` 顶部是否已有 `use std::io::Write as _;` 与 `with_context`——`deliver_skill` 里若没有用到 `Write`，需要 `use std::io::Write;`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib apply::write`
Expected: 全绿。

- [ ] **Step 5: Commit**

```bash
git add src/apply/write.rs
git commit -m "feat(apply): add write_config with single-slot backup"
```

---

### Task 6: codex 的 `[mcp_servers]` 子树 merge

**Files:**
- Modify: `Cargo.toml`
- Create: `src/apply/mcp_toml.rs`
- Modify: `src/apply/mod.rs`（`pub mod mcp_toml;`）
- Test: `src/apply/mcp_toml.rs`（同文件 `mod tests`）

**Interfaces:**
- Consumes: `write_config`（Task 5）。
- Produces: `pub fn upsert_codex_mcp(target: &Path, name: &str, command: &str, args: &[String], env: &BTreeMap<String, String>) -> anyhow::Result<()>`。**Task 9 的 `deliver_mcps` 调它。**

**为什么要 `toml_edit`：** `config.toml` 是用户手写的，里面有注释、有格式、有别的键（`experimental_bearer_token`、`[projects."/Users/x"]`）。用 `toml` 反序列化再序列化会把注释和格式全丢掉——那是在改用户的文件，不是在加一个键。`toml_edit` 保字节。§9.12 已解禁。

- [ ] **Step 1: 加依赖**

```bash
cargo add toml_edit@0.25.15
cargo build
```

Expected: 编译通过。（`toml_edit = "0.25.15"` 一行进了 `[dependencies]`；任务书里要求的版本就是它，实测过键的自动引号与既有字节保留。）

- [ ] **Step 2: 写失败的测试**

新建 `src/apply/mcp_toml.rs`，先只写 `mod tests`：

```rust
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
        assert_eq!(doc["mcp_servers"]["context7"]["command"].as_str(), Some("uvx"));
        assert!(doc["mcp_servers"]["context7"].get("args").is_none(), "{text}");
        assert!(doc["mcp_servers"]["context7"].get("env").is_none(), "{text}");
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
        assert_eq!(doc["mcp_servers"]["context7"]["command"].as_str(), Some("npx"));
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
        assert_eq!(doc["mcp_servers"]["context7"]["command"].as_str(), Some("npx"));
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
        t.write(".codex/config.toml", "secret-token = \"sk-live-abc\"\n[[[broken\n");
        let before = std::fs::read(&p).unwrap();

        let err = upsert_codex_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(!msg.contains("sk-live-abc"), "错误消息不许回显配置内容：{msg}");
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
        assert_eq!(doc["mcp_servers"]["context7"]["env"]["K"].as_str(), Some("${K}"));
        assert_eq!(doc["mcp_servers"]["context7"]["env"]["W"].as_str(), Some("${A_B1}"));
    }
}
```

- [ ] **Step 3: 跑测试，确认它们失败**

Run: `cargo test --lib apply::mcp_toml`
Expected: 编译失败，`cannot find function upsert_codex_mcp`。

- [ ] **Step 4: 实现**

在 `src/apply/mcp_toml.rs` 顶部（`mod tests` 之前）：

```rust
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
```

`src/apply/mod.rs` 加 `pub mod mcp_toml;`。

若 `other.type_name()` 在 `toml_edit 0.25` 的 `Item` 上不存在，改用 `format!("{other:?}")` 的短形式或直接写 `"a non-table value"`——**不许回显内容**。

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib apply::mcp_toml`
Expected: 全绿。若 `an_inline_mcp_servers_table_is_updated_in_place` 或 `names_with_dots_spaces_and_quotes_round_trip` 红了，先跑一次 `cargo test --lib apply::mcp_toml -- --nocapture` 把落盘后的文本打出来看形状——`toml_edit` 的自动引号实测是过的，红的话多半是 `Item::Table` 插进了内联表。

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/apply/mcp_toml.rs src/apply/mod.rs
git commit -m "feat(apply): merge MCP servers into codex config.toml"
```

---

### Task 7: kimi 的 `mcpServers` 顶层键 merge

**Files:**
- Create: `src/apply/mcp_json.rs`
- Modify: `src/apply/mod.rs`（`pub mod mcp_json;`）
- Test: `src/apply/mcp_json.rs`（同文件 `mod tests`）

**Interfaces:**
- Consumes: `write_config`（Task 5）。
- Produces: `pub fn upsert_kimi_mcp(target: &Path, name: &str, command: &str, args: &[String], env: &BTreeMap<String, String>) -> anyhow::Result<()>`。**Task 9 的 `deliver_mcps` 调它。**

**已接受的代价（Ruling，写进 ledger）：** serde_json 默认的 `Map` 是 `BTreeMap`，所以写出去之后顶层键按字典序——用户的文件会被重排。开 `preserve_order` 会全局改变 `serde_json::Map` 的类型（`claude_code.rs` 的 MCP 读取遍历也用它，golden 会跟着动），代价远大于收益：`~/.kimi-code/mcp.json` 是机器写的配置文件，键序不是内容。**本计划不重排任何键的语义**，只保证「别的顶层键一个不少」。将来若有证据表明 kimi 侧在乎键序，再单独评估 `preserve_order`。

- [ ] **Step 1: 写失败的测试**

`src/apply/mcp_json.rs` 的 `mod tests`：与 Task 6 同形状（fresh file / 别的顶层键存活 / 整棵替换不留旧键 / 别的服务器不受影响 / 类型不对拒绝且文件不变 / 语法坏掉拒绝且不回显 / env 原样），再各加两条 JSON 特有的：

```rust
    /// 显式 `null` 是**正常状态**不是故障（读路径也这么认）：把它换成对象。
    #[test]
    fn an_explicit_null_mcp_servers_is_replaced_not_refused() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"mcpServers\": null, \"keep\": 1}\n");
        upsert_kimi_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap();

        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
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

    /// 点名的一条已知代价：顶层键会被字典序重排。这条测试不是「钉住这个行为」，
    /// 是让将来某天有人改了它时**知道自己在改什么**——两组键都要在。
    #[test]
    fn other_top_level_keys_survive_regardless_of_order() {
        let t = TempHome::new();
        let p = t.path(".kimi-code/mcp.json");
        t.write(".kimi-code/mcp.json", "{\"zeta\": 1, \"alpha\": {\"a\": true}}\n");
        upsert_kimi_mcp(&p, "context7", "npx", &[], &BTreeMap::new()).unwrap();

        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["zeta"], 1);
        assert_eq!(v["alpha"]["a"], true);
        assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
    }
```

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib apply::mcp_json`
Expected: 编译失败，`cannot find function upsert_kimi_mcp`。

- [ ] **Step 3: 实现**

```rust
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
        serde_json::from_str(&existing)
            .with_context(|| format!("{}: not valid JSON, refusing to touch it", target.display()))?
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
    let servers = servers.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("{}: `mcpServers` must be an object", target.display())
    })?;

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
```

`src/apply/mod.rs` 加 `pub mod mcp_json;`。

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib apply::mcp_json`
Expected: 全绿。

- [ ] **Step 5: Commit**

```bash
git add src/apply/mcp_json.rs src/apply/mod.rs
git commit -m "feat(apply): merge MCP servers into kimi mcp.json"
```

---

### Task 8: claude 的 shell out

**Files:**
- Create: `src/apply/mcp_claude.rs`
- Modify: `src/apply/mod.rs`（`pub mod mcp_claude;`）
- Modify: `src/testutil.rs`（新增 `FakeCli`）
- Modify: `tests/read_only_guard.rs`（`PROCESS_ALLOWLIST`）
- Test: `src/apply/mcp_claude.rs`（同文件 `mod tests`）

**Interfaces:**
- Produces:
  - `pub fn claude_add_json(binary: &Path, name: &str, spec_json: &str) -> anyhow::Result<()>`
  - `pub fn claude_remove(binary: &Path, name: &str) -> anyhow::Result<()>`
  - `FakeCli`（`src/testutil.rs`）：`FakeCli::new(&TempHome, rel) -> FakeCli`、`fn path(&self) -> PathBuf`、`fn invocations(&self) -> Vec<Vec<String>>`、`fn failing(t, rel, code, stderr) -> FakeCli`
- Consumes: 无（不碰任何配置文件——字节由 CLI 写）。

**为什么是 shell out：** `~/.claude.json` 被 harness 频繁整体重写（`numStartups` 之类），直接 merge 会与它并发写竞争，用户会丢失我们写的那个键或它写的那些键。§9.6 第 1 行。

**实测确认的 CLI 契约（本计划据此实现，全部在隔离 `HOME` 下实测过、真实 `~/.claude.json` 前后字节相同）：**
- `claude mcp add-json -s user -- <name> <json>`：`-s user` 不可省，默认 scope 是 `local` → `projects.<cwd>.mcpServers`，而读路径故意不看那里。
- **`--` 不可省**（计划缺陷 12）：没有它时，`-h` 这样的名字被读成 `--help`——打印帮助、**退出 0**、什么都不写。名字来自清单，而 `manifest::name_is_a_single_component` 放行 `-` 开头。
- 对**已存在**的名字再 `add-json` 直接失败（`MCP server <name> already exists in user config`），退出码非 0，**没有 `--force` 或覆盖开关**。所以 update 只能是两条命令：先 `remove` 再 `add-json`。
- `claude mcp remove -s user -- <name>`：对不存在的名字退出 1；对存在的名字退出 0，只摘掉 `mcpServers` 里那一个键，其余顶层键原样留下。
- `env` 写成 `{"K": "${K}"}`——裸引用原样保留，从不展开。

**为什么 argv 里没有密钥：** 清单侧 `manifest::env_refs` → `secrets::validate_env`/`parse_ref` 保证 env 值一律是裸 `${VAR}`（`src/secrets.rs:27`）。传给我们 JSON 里的值就是那个引用本身。

**为什么绝不经过 shell：** 用 `Command::args`，不经 `sh -c`，不做任何字符串拼接。`${VAR}` 一旦经过 shell 就会被展开成真密钥、并落进 CLI 的进程表。

- [ ] **Step 1: 写 `FakeCli`**

`src/testutil.rs`：

```rust
/// 一条记录 argv 的假 CLI（B3a 的 claude shell out 用）。
///
/// 它证明的是「我们发出的 argv 长什么样」——真 CLI 接不接受、写进哪里，
/// 由 `#[ignore]` 的那条冒烟测试证。两者缺一不可：假 shim 证不了真 CLI 的契约，
/// 真 CLI 证不了我们每一次调用都发了什么（它只说话，不留证据）。
///
/// **HOME 由调用方钉死**：`FakeCli` 只是可执行文件，不设置环境。测试若用它
/// 跑真 CLI 的 wrapper，wrapper 自己 export HOME。
pub struct FakeCli {
    path: PathBuf,
    log: PathBuf,
}

impl FakeCli {
    /// 造一条 shim：记录每次调用的 argv（一次调用一行 `---` 分隔），退出 0。
    pub fn new(t: &TempHome, rel: &str) -> Self {
        Self::with_exit(t, rel, 0, "")
    }

    /// 造一条会失败的 shim，用于证明失败被报出来。
    pub fn failing(t: &TempHome, rel: &str, code: i32, stderr: &str) -> Self {
        Self::with_exit(t, rel, code, stderr)
    }

    fn with_exit(t: &TempHome, rel: &str, code: i32, stderr: &str) -> Self {
        let path = t.path(rel);
        let log = t.path(&format!("{rel}.log"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir shim parent");
        }
        // stderr 先落成文件、再由 shim `cat` 出来，**不把它插进脚本**：
        // 未加引号的 `printf '%s\n' {stderr}` 会按参数逐个换行——
        // "MCP server n already exists" 于是变成五行，错误消息里再也找不到
        // 连续的 `already exists`（实测：这曾让 Task 8 的一条测试红）。
        // 走文件则任何内容都原样吐出，不必跟 shell 引号规则较劲。
        let err_file = t.path(&format!("{rel}.stderr"));
        if !stderr.is_empty() {
            std::fs::write(&err_file, format!("{stderr}\n")).expect("write shim stderr");
        }
        // 逐参数写一行：`"$*"` 会把参数用空格拼起来，丢了边界，
        // 于是「JSON 里带空格」这种真实输入会被验成「参数对不上」。
        let script = format!(
            "#!/bin/sh\nprintf -- '---\\n' >> {log}\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {log}; done\n\
             {}\nexit {code}\n",
            if stderr.is_empty() {
                String::new()
            } else {
                format!("cat {} >&2\n", err_file.display())
            },
            log = log.display()
        );
        std::fs::write(&path, script).expect("write shim");
        let mut perms = std::fs::metadata(&path).expect("stat").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        Self { path, log }
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    /// 每次调用的 argv，按调用顺序。`Vec<Vec<String>>` 里每一项是一次调用。
    pub fn invocations(&self) -> Vec<Vec<String>> {
        let text = match std::fs::read_to_string(&self.log) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<Vec<String>> = Vec::new();
        for line in text.lines() {
            if line == "---" {
                out.push(Vec::new());
            } else if let Some(last) = out.last_mut() {
                last.push(line.to_string());
            }
        }
        out
    }
}
```

`src/testutil.rs` 的 `mod tests` 里加两条钉子：

```rust
    #[test]
    fn the_fake_cli_records_argv_split_by_invocation() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        assert!(cli.path().is_file());

        // 直接起它：这是 fixture 自己的契约测试，不用生产代码
        std::process::Command::new(cli.path())
            .args(["mcp", "add-json", "-s", "user", "n", "{\"a\": \"b c\"}"])
            .status()
            .unwrap();
        std::process::Command::new(cli.path())
            .args(["mcp", "remove", "-s", "user", "n"])
            .status()
            .unwrap();

        assert_eq!(
            cli.invocations(),
            vec![
                vec!["mcp", "add-json", "-s", "user", "n", "{\"a\": \"b c\"}"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                vec!["mcp", "remove", "-s", "user", "n"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
            ],
            "参数边界与调用边界都要留住"
        );
    }

    #[test]
    fn a_failing_fake_cli_reports_its_code() {
        let t = TempHome::new();
        let cli = FakeCli::failing(&t, "bin/claude", 1, "boom");
        let out = std::process::Command::new(cli.path()).output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "boom");
    }
```

- [ ] **Step 2: 写失败的测试**

`src/apply/mcp_claude.rs` 的 `mod tests`：

```rust
    fn args_of(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn add_json_sends_the_exact_argv_and_the_json_verbatim() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        let json = r#"{"command":"npx","args":["-y","pkg"],"env":{"K":"${K}"}}"#;
        claude_add_json(&cli.path(), "context7", json).unwrap();

        let calls = cli.invocations();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            args_of(&calls[0]),
            vec!["mcp", "add-json", "-s", "user", "--", "context7", json],
            "`-s user` 不可省：默认 scope 是 local，读路径不看那里；`--` 见下面那条"
        );
        assert!(calls[0][6].contains("${K}"), "引用不许被展开");
    }

    #[test]
    fn remove_sends_the_exact_argv() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        claude_remove(&cli.path(), "context7").unwrap();
        assert_eq!(
            args_of(&cli.invocations()[0]),
            vec!["mcp", "remove", "-s", "user", "--", "context7"]
        );
    }

    /// **一个长得像旗标的名字，不能被 CLI 读成旗标。**（计划缺陷 12）
    ///
    /// 名字来自清单（`mcps:` 的键），而 `manifest::name_is_a_single_component`
    /// 只排除路径分隔符——`-h` 是合法的清单键。实测真 CLI（`claude 2.1.267`）：
    ///
    /// ```text
    /// $ claude mcp add-json -s user -h '{"command":"npx"}'
    /// Usage: claude mcp add-json [options] <name> <json>   ← 打印帮助，**退出 0**
    /// ```
    ///
    /// 退出 0 而一个字节没写，而 `run` 只看退出码——于是 `deliver_mcps` 会把这一条
    /// 推进 `delivered` 并记进账本：用户被告知「已交付」，盘上什么都没有。
    ///
    /// 修法是 `--`，与 `src/store/git.rs` 的 `clone_args` 同一形状。
    #[test]
    fn a_name_that_looks_like_a_flag_is_not_read_as_one() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        claude_add_json(&cli.path(), "-h", "{}").unwrap();
        claude_remove(&cli.path(), "-h").unwrap();

        let calls = cli.invocations();
        assert_eq!(
            args_of(&calls[0]),
            vec!["mcp", "add-json", "-s", "user", "--", "-h", "{}"]
        );
        assert_eq!(
            args_of(&calls[1]),
            vec!["mcp", "remove", "-s", "user", "--", "-h"]
        );
    }

    /// 一条**看起来像 shell 展开**的 env 值：若实现里用了 `sh -c` 或任何拼串，
    /// `${K}` 会被 shell 换掉（这里 K 不存在则变成空串），这条就红。
    /// 这是「密钥永不进 argv」的技术保障的一半——另一半是 manifest 只收裸引用。
    #[test]
    fn the_argv_is_not_interpreted_by_a_shell() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        // 值本身也参与：`;` 与 `$` 若经过 shell 会改变命令语义
        let json = r#"{"command":"npx","env":{"K":"${K}","SEMI":"${A};${B}"}}"#;
        claude_add_json(&cli.path(), "n", json).unwrap();
        assert_eq!(cli.invocations()[0][5], json);
    }

    #[test]
    fn a_failing_cli_becomes_an_error_that_names_the_server_and_carries_stderr() {
        let t = TempHome::new();
        let cli = FakeCli::failing(&t, "bin/claude", 1, "MCP server n already exists");
        let err = claude_add_json(&cli.path(), "n", "{}").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains('n'), "要能看出是哪一条：{msg}");
        assert!(msg.contains("already exists"), "要带上 CLI 的话：{msg}");
    }

    /// CLI 不在 PATH 上（用户没装 claude）：必须是错误，不许当成成功。
    /// 这是 Review Focus 第 1 条的机械钉子。
    #[test]
    fn a_missing_binary_is_an_error_not_a_silent_success() {
        let t = TempHome::new();
        let err = claude_add_json(&t.path("bin/nope"), "n", "{}").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("claude"), "要说清是起不来 CLI：{msg}");
    }

    /// 真 CLI 的冒烟：默认跳过。手动跑 `cargo test -- --ignored`。
    ///
    /// 它证的是假 shim 证不了的那一半：**真 CLI 接受我们这串 argv，而且写进去的东西
    /// 读路径真的看得到**（`-s user` 落在 `~/.claude.json` 顶层）。
    ///
    /// 名字故意取成**旗标形状**（`-henv-smoke`，计划缺陷 12）：这一条因此同时钉住
    /// `--` 被真 CLI 接受、且 `--` 之后的名字不会被读成选项。名字取普通串的话，
    /// 把 `--` 删掉这条照样绿——那样的冒烟证不了本次修复。
    ///
    /// HOME 由 wrapper 脚本钉死在 TempHome 里——这一条绝不能碰真实 `~`。
    #[test]
    #[ignore = "runs the real claude CLI; needs it on PATH"]
    fn the_real_cli_accepts_our_argv_and_the_reader_sees_it() {
        let t = TempHome::new();
        let Some(real) = which("claude") else {
            eprintln!("claude 不在 PATH 上，跳过");
            return;
        };
        // wrapper 而不是直接调用：写路径按继承的环境起进程，测试里没有
        // （也不该有）改进程全局环境的手段，所以把 HOME 写进脚本里。
        let wrapper = t.path("bin/claude");
        std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
        std::fs::write(
            &wrapper,
            format!("#!/bin/sh\nHOME={} exec {} \"$@\"\n", t.home.root().display(), real.display()),
        )
        .unwrap();
        let mut p = std::fs::metadata(&wrapper).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
        std::fs::set_permissions(&wrapper, p).unwrap();

        let json = r#"{"command":"npx","args":["-y","@upstash/context7-mcp"],"env":{"K":"${K}"}}"#;
        let name = "-henv-smoke";
        claude_add_json(&wrapper, name, json).unwrap();

        // 读路径（adapter 的 read_state）必须看见它——这就是「写路径 == 读路径」。
        let adapter = crate::adapters::claude_code::ClaudeCode::new(t.home.clone());
        let caps = adapter.read_state().unwrap();
        let got = caps
            .iter()
            .find(|c| c.id.name == name)
            .expect("读路径必须看得见刚写进去的那条");
        match &got.spec {
            crate::capability::Spec::Mcp { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args[0], "-y");
                assert_eq!(env["K"], "${K}", "引用原样保留");
            }
            other => panic!("{other:?}"),
        }

        // update 那条路：先 remove 再 add，两条都不许报错。
        claude_remove(&wrapper, name).unwrap();
        claude_add_json(&wrapper, name, r#"{"command":"uvx"}"#).unwrap();
        let caps = adapter.read_state().unwrap();
        let got = caps.iter().find(|c| c.id.name == name).unwrap();
        match &got.spec {
            crate::capability::Spec::Mcp { command, .. } => assert_eq!(command, "uvx"),
            other => panic!("{other:?}"),
        }
    }

    /// 在 PATH 里找一个可执行文件。**不用 `Command::new("which")`**：多一个
    /// shell-out 点没有好处，扫 PATH 就够。
    fn which(name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(name))
                .find(|p| std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false))
        })
    }
```

若 `ClaudeCode` 的构造器不是 `new(Home)` 或 `Home` 不是 `Clone`，按实际调整（`registry(&t.home)` + `lookup` 拿一个 `&dyn HarnessAdapter` 亦可，但 `read_state` 后要 drop 掉 registry，写法会绕；优先用构造器）。`t.home.clone()` 需要 `Home: Clone`——若它不是，改成从 `registry(&t.home)` 取 adapter 并在同一个作用域里用。

- [ ] **Step 3: 跑测试，确认它们失败**

Run: `cargo test --lib apply::mcp_claude`
Expected: 编译失败，`cannot find function claude_add_json`。

- [ ] **Step 4: 实现**

```rust
//! claude 的 MCP 写入：交给它自己的 CLI（spec §9.6 第 1 行）。
//!
//! **为什么不经手字节**：`~/.claude.json` 被 claude 自己频繁整体重写
//! （`numStartups` 之类），我们 merge 进去的键会和它下一次重写竞争——
//! 输的那个是用户。CLI 是唯一知道该文件当前形状的东西。
//!
//! **这是全项目第二个 shell out 点**（第一个是 `src/store/git.rs`）。
//! `tests/read_only_guard.rs` 的 `PROCESS_ALLOWLIST` 按文件豁免，本文件必须
//! 显式列进去并写理由——那张表不许退化成目录前缀。
//!
//! **绝不经过 shell**：`Command::args`，不是 `sh -c`，不做字符串拼接。
//! argv 里也没有密钥：清单的 env 值由 `secrets::parse_ref` 保证是裸 `${VAR}` 引用。

use anyhow::Context as _;
use std::path::Path;

/// `claude mcp add-json -s user -- <name> <json>`。
///
/// **`-s user` 不可省**：默认 scope 是 `local`，那会写进
/// `projects.<cwd>.mcpServers`——一个读路径故意不看的位置。
///
/// **`--` 也不可省**（计划缺陷 12）：名字来自清单，而
/// `manifest::name_is_a_single_component` 只挡路径分隔符——`-h` 是合法的清单键。
/// 没有 `--` 时真 CLI 把 `-h` 读成 `--help`，打印帮助并**退出 0**：`deliver_mcps`
/// 只信退出码，于是把一条从未写下的交付记进账本。详见 Task 8 的旗标形状测试。
pub fn claude_add_json(binary: &Path, name: &str, spec_json: &str) -> anyhow::Result<()> {
    run(
        binary,
        &["mcp", "add-json", "-s", "user", "--", name, spec_json],
        name,
    )
}

/// `claude mcp remove -s user -- <name>`。
///
/// `--` 的理由同 `claude_add_json`。
///
/// **update 必须先做这一步**：实测 `add-json` 对已存在的名字直接失败
/// （`MCP server <name> already exists in user config`），退出码非 0，
/// 没有 `--force` 之类的覆盖开关。所以「改一个已经交付过的 MCP」只能是
/// remove → add 两条命令。
///
/// 对不存在的名字它会退出 1——**不吞这个错**：走到 update 说明 diff 刚看到过它，
/// 这时 remove 失败是真故障，照 §9.7 报 partial。
pub fn claude_remove(binary: &Path, name: &str) -> anyhow::Result<()> {
    run(binary, &["mcp", "remove", "-s", "user", "--", name], name)
}

fn run(binary: &Path, args: &[&str], name: &str) -> anyhow::Result<()> {
    let out = std::process::Command::new(binary)
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "cannot run the claude CLI ({}); is it installed and on PATH?",
                binary.display()
            )
        })?;
    anyhow::ensure!(
        out.status.success(),
        "claude CLI failed for MCP `{name}` ({}): {}",
        out.status,
        // stderr 只可能是 CLI 自己的话——我们传进去的 JSON 里没有密钥（全是 ${VAR} 引用）。
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}
```

`src/apply/mod.rs` 加 `pub mod mcp_claude;`。

- [ ] **Step 5: 加进 shell out 允许位**

`tests/read_only_guard.rs:43`：

```rust
const PROCESS_ALLOWLIST: &[&str] = &[
    "src/store/git.rs",
    "src/testutil.rs",
    // B3a：claude 的 MCP 写入交给它自己的 CLI——`~/.claude.json` 被 harness
    // 频繁重写，直接 merge 会与它并发写竞争（§9.6 第 1 行）。
    "src/apply/mcp_claude.rs",
];
```

同时也更新那张表上方的注释（它今天说「B2 的 git 与 B3 的 `claude mcp add-json` 是第一次真的调用外部程序」——B3a 到了，把这句话落实）。

Run: `cargo test --test read_only_guard`
Expected: 全绿，含 `the_process_allowlist_names_files_not_directories`（新文件真的存在）与 `the_allowlist_is_not_the_whole_tree`（`src/capability.rs` 仍然零违规）。

- [ ] **Step 6: 跑测试**

Run: `cargo test --lib apply::mcp_claude && cargo test --test read_only_guard`
Expected: 全绿（`the_real_cli_...` 被 `#[ignore]` 跳过）。

- [ ] **Step 7: Commit**

```bash
git add src/apply/mcp_claude.rs src/apply/mod.rs src/testutil.rs tests/read_only_guard.rs
git commit -m "feat(apply): write claude MCP via its own CLI"
```

---

### Task 9: `deliver_mcps` 与执行层接线

**Files:**
- Modify: `src/apply/mcp.rs`（加 `deliver_mcps` 与分派）
- Modify: `src/apply/mod.rs`（`HarnessReport::absorb`、默认 `apply`、`execute` 签名）
- Modify: `src/adapters/mod.rs`（默认 `apply` 调 `deliver_mcps`）
- Modify: `src/lib.rs`（`Planned.desired`、`plan_for`、`run_apply`）
- Create: `tests/mcp_end_to_end.rs`

**Interfaces:**
- Consumes: `upsert_codex_mcp`（Task 6）、`upsert_kimi_mcp`（Task 7）、`claude_add_json`/`claude_remove`（Task 8）、`hash_mcp_spec`（Task 2）、`Ledger::record`（Task 1）、`McpTarget`（Task 4）。
- Produces:
  - `pub fn deliver_mcps(harness: &str, target: Option<&McpTarget>, slice: &HarnessPlan, desired: &[Capability], ledger: &mut Ledger) -> anyhow::Result<HarnessReport>`
  - `HarnessReport::absorb(&mut self, other: HarnessReport)`
  - `HarnessAdapter::apply(&self, slice, desired: &[Capability], content, ledger)`
  - `pub fn execute(plan: &Plan, desired: &[Capability], content: &ContentIndex, ledger: &mut Ledger, home: &Home) -> anyhow::Result<ApplyReport>`

**账本哈希记的是「我们写下去的三个字段」（Ruling，写进 ledger）：** skill 那条路（§9.3）记的是交付完成后在目标上**重算**的哈希，不是解析源时的值——因为源与目标之间隔着一次拷贝。MCP 这条没有那层：三个字段就在手里，写进去的就是它们，写入器要么成功要么报错，不存在「成功但写了别的」。所以这里记期望的哈希。这个选择由「apply 之后 diff 必须是 in-sync」这条**性质测试**兜底（见 Step 7 的端到端）：真 CLI 若规范化了某个字段，那条会红。代价：将来若发现某家 CLI 会改写内容，要在 `deliver_mcps` 里加一次读回。

- [ ] **Step 1: 写失败的测试（单元层）**

在 `src/apply/mcp.rs` 末尾新开 `mod tests`（Task 4 建这个文件时只放了类型，还没有测试块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Kind};
    use crate::diff::{Action, HarnessPlan, PlanEntry};
    use crate::store::provenance::Ledger;
    use crate::testutil::{FakeAdapter, FakeCli, TempHome};
    use std::collections::BTreeMap;

    fn plan_with_mcp(name: &str, action: Action) -> HarnessPlan {
        HarnessPlan {
            harness: "codex".into(),
            detected: true,
            entries: vec![PlanEntry::new(CapId::new(Kind::Mcp, name), action)],
            actual_by_kind: BTreeMap::new(),
        }
    }

    #[test]
    fn an_install_writes_the_mcp_and_records_it() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let d = FakeAdapter::mcp("context7", "npx");
        let mut l = Ledger::default();

        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan_with_mcp("context7", Action::Install),
            &[d.clone()],
            &mut l,
        )
        .unwrap();

        assert_eq!(r.delivered, vec!["mcp/context7"]);
        assert!(r.failure.is_none());
        let entry = l.find("codex", Kind::Mcp, "context7").expect("要记账");
        assert_eq!(entry.path, target.path);
        assert_eq!(entry.source_id, "manifest:mcp/context7");
        let Spec::Mcp { command, args, env } = &d.spec else {
            unreachable!()
        };
        assert_eq!(entry.content_hash, hash_mcp_spec(command, args, env));
    }

    /// 这家没有 MCP 写目标（`mcp_target()` 给 `None`）：空报告，不是失败。
    #[test]
    fn no_target_is_an_empty_report_not_a_failure() {
        let mut l = Ledger::default();
        let r = deliver_mcps(
            "kimi-code",
            None,
            &plan_with_mcp("context7", Action::Install),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();
        assert!(r.delivered.is_empty() && r.failure.is_none());
    }

    /// 单条失败不记账（§9.7：没交付成功就不该声称交付过），也不上抛。
    #[test]
    fn a_failed_write_is_recorded_as_a_failure_and_not_ledgered() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        t.write(".codex/config.toml", "mcp_servers = \"oops\"\n");
        let mut l = Ledger::default();

        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan_with_mcp("context7", Action::Install),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();

        assert!(r.delivered.is_empty());
        assert!(r.failure.as_deref().unwrap().contains("mcp/context7"));
        assert!(
            l.find("codex", Kind::Mcp, "context7").is_none(),
            "失败的不许记账"
        );
    }

    /// 失败隔离：一条坏了，同一次里别的一条照样交付。
    ///
    /// 坏的那条走「计划里有它、清单里没有」这条失败路径——机制不是重点，
    /// 重点是顺序执行里一条失败**不中断**这个切片的其余条目。
    /// （与 `execute` 的冲突即停不冲突：那是计划层的全停，这里是单条失败后
    /// 已成功的仍然留在盘上、账本上也仍然记着。）
    #[test]
    fn one_failure_does_not_stop_the_rest_of_this_harness() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let mut l = Ledger::default();
        let mut plan = plan_with_mcp("good", Action::Install);
        plan.entries
            .push(PlanEntry::new(CapId::new(Kind::Mcp, "bad"), Action::Install));

        // 清单里只有 good —— bad 因此失败。
        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan,
            &[FakeAdapter::mcp("good", "npx")],
            &mut l,
        )
        .unwrap();

        assert_eq!(r.delivered, vec!["mcp/good"], "好的一条要交付");
        assert!(
            r.failure.as_deref().unwrap().contains("mcp/bad"),
            "坏的一条要报出来：{:?}",
            r.failure
        );
    }

    /// 计划说要装、清单里却没有这一条 → 是要做的事没做成，不是「跳过」（§9.1）。
    #[test]
    fn a_planned_mcp_missing_from_the_manifest_is_a_failure() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let mut l = Ledger::default();
        let r = deliver_mcps(
            "codex",
            Some(&target),
            &plan_with_mcp("ghost", Action::Install),
            &[],
            &mut l,
        )
        .unwrap();
        assert!(r.delivered.is_empty());
        assert!(r.failure.as_deref().unwrap().contains("mcp/ghost"));
    }

    /// in-sync / conflict / drift / unknown 都不是「把内容放过去」，一条都不许写。
    #[test]
    fn only_install_and_update_write() {
        let t = TempHome::new();
        let target = McpTarget {
            path: t.path(".codex/config.toml"),
            mechanism: McpMechanism::CodexToml,
        };
        let mut l = Ledger::default();
        let d = FakeAdapter::mcp("context7", "npx");
        for action in [
            Action::InSync,
            Action::Conflict {
                reason: "x".into(),
            },
            Action::Drift,
            Action::Unknown,
            Action::Undeliverable {
                reason: "x".into(),
            },
        ] {
            let r = deliver_mcps(
                "codex",
                Some(&target),
                &plan_with_mcp("context7", action.clone()),
                &[d.clone()],
                &mut l,
            )
            .unwrap();
            assert!(r.delivered.is_empty(), "{action:?} 不该写盘");
            assert!(r.failure.is_none(), "{action:?} 不该报失败");
        }
        assert!(!t.path(".codex/config.toml").exists(), "一个字节都不该写");
    }

    /// **update 在 claude 上是两条命令，顺序不能反**（§9.6）。
    /// 假 shim 正好能证这件事：真 CLI 证「它接受这串 argv」，shim 证「我们每次发了什么」。
    #[test]
    fn a_claude_update_removes_first_then_adds() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        let target = McpTarget {
            path: t.home.claude_json(),
            mechanism: McpMechanism::ClaudeCli {
                binary: cli.path(),
            },
        };
        let d = FakeAdapter::mcp("context7", "npx");
        let mut l = Ledger::default();

        let r = deliver_mcps(
            "claude-code",
            Some(&target),
            &plan_with_mcp(
                "context7",
                Action::Update {
                    from: "old".into(),
                    to: "npx".into(),
                },
            ),
            &[d],
            &mut l,
        )
        .unwrap();

        assert_eq!(r.delivered, vec!["mcp/context7"]);
        // 先绑到局部变量：`invocations()` 返回的是自有 Vec，
        // 直接在临时值上 `.iter()` 借不出 `&str`（E0716）。
        let invocations = cli.invocations();
        let calls: Vec<Vec<&str>> = invocations
            .iter()
            .map(|v| v.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(calls.len(), 2, "update 是两条命令");
        // remove 那条比全：名字也在里面，钉住「删的是这一条」。
        assert_eq!(calls[0], ["mcp", "remove", "-s", "user", "--", "context7"]);
        // add-json 那条截到名字为止——末尾是 JSON 载荷，由 mcp_claude 自己的测试钉。
        assert_eq!(
            calls[1][..6],
            ["mcp", "add-json", "-s", "user", "--", "context7"]
        );
    }

    /// install 只有一条命令——`add-json` 对不存在的名字是能成的，
    /// 多来一次 `remove` 会失败（对不存在的名字退出 1），把 install 变成假故障。
    #[test]
    fn a_claude_install_is_a_single_add() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        let target = McpTarget {
            path: t.home.claude_json(),
            mechanism: McpMechanism::ClaudeCli {
                binary: cli.path(),
            },
        };
        let mut l = Ledger::default();
        deliver_mcps(
            "claude-code",
            Some(&target),
            &plan_with_mcp("context7", Action::Install),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();

        let calls = cli.invocations();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][1], "add-json");
    }

    /// remove 失败（CLI 说这个名字不存在）→ 真故障，报 partial，且**不许接着 add**：
    /// add 会覆盖掉一份我们并不认识的东西。
    #[test]
    fn a_failed_remove_stops_before_the_add() {
        let t = TempHome::new();
        let cli = FakeCli::failing(&t, "bin/claude", 1, "No MCP server named \"context7\"");
        let target = McpTarget {
            path: t.home.claude_json(),
            mechanism: McpMechanism::ClaudeCli {
                binary: cli.path(),
            },
        };
        let mut l = Ledger::default();
        let r = deliver_mcps(
            "claude-code",
            Some(&target),
            &plan_with_mcp(
                "context7",
                Action::Update {
                    from: "old".into(),
                    to: "npx".into(),
                },
            ),
            &[FakeAdapter::mcp("context7", "npx")],
            &mut l,
        )
        .unwrap();

        assert!(r.delivered.is_empty());
        assert!(r.failure.is_some());
        assert_eq!(cli.invocations().len(), 1, "remove 失败就不该再 add");
        assert!(l.find("claude-code", Kind::Mcp, "context7").is_none());
    }
```

（执行前扫描已核对：`HarnessPlan` 有四个字段，第四个 `actual_by_kind: BTreeMap<Kind, usize>` 空表即可；`PlanEntry::new(id, action)` 存在。`Action::Unknown` 是**无字段**变体。）

- [ ] **Step 2: 跑测试，确认它们失败**

Run: `cargo test --lib apply::mcp`
Expected: 编译失败，`cannot find function deliver_mcps`。

- [ ] **Step 3: 实现 `deliver_mcps`**

追加到 `src/apply/mcp.rs`：

```rust
/// 一家的 MCP 交付（§9.6）。与 [`crate::apply::deliver_skills`] 同一条失败隔离：
/// 单条失败不中断这一家剩下的条目，失败的条目不记账，返回 `Err` 只留给「压根没法开始」。
///
/// `target` 为 `None`（这家没有用户级 MCP 配置）时返回一份空报告——不是失败，是这家的事实。
pub fn deliver_mcps(
    harness: &str,
    target: Option<&McpTarget>,
    slice: &HarnessPlan,
    desired: &[Capability],
    ledger: &mut Ledger,
) -> anyhow::Result<HarnessReport> {
    let mut report = HarnessReport {
        harness: harness.to_string(),
        delivered: Vec::new(),
        failure: None,
    };
    let Some(target) = target else {
        return Ok(report);
    };

    let mut failures: Vec<String> = Vec::new();
    for e in &slice.entries {
        if e.id.kind != Kind::Mcp {
            continue;
        }
        // 只有 install / update 是「把内容放过去」。其余动作都是「不该放」。
        if !matches!(e.action, Action::Install | Action::Update { .. }) {
            continue;
        }
        // 清单里取回这条的三段载荷。计划说要装、清单里却没有 → 是要做的事没做成。
        let Some(d) = desired.iter().find(|c| c.id == e.id) else {
            failures.push(format!("{}：计划里有它、清单里没有，未交付", e.id.label()));
            continue;
        };
        let Spec::Mcp { command, args, env } = &d.spec else {
            failures.push(format!("{}：清单里这条不是 MCP 规格，未交付", e.id.label()));
            continue;
        };

        // update 在 claude 上要先摘再写（实测 add-json 对已存在的名字直接失败）。
        // 其余两家是整棵子树替换，天然就是 update。
        let replace = matches!(e.action, Action::Update { .. });
        match dispatch(target, &e.id.name, command, args, env, replace) {
            Ok(()) => {
                report.delivered.push(e.id.label());
                ledger.record(LedgerEntry {
                    harness: harness.to_string(),
                    kind: Kind::Mcp,
                    name: e.id.name.clone(),
                    path: target.path.clone(),
                    // B3a 的 MCP 来源只有清单一条，格式与 skill 的 `agents-dir` 并列。
                    source_id: format!("manifest:mcp/{}", e.id.name),
                    // 记的是我们写下去的三个字段。skill 那条路记的是交付完成后在目标上
                    // **重算**的哈希（源与目标之间隔着一次拷贝）；MCP 没有那层——
                    // 写入器要么成功要么报错，不存在「成功但写了别的」。
                    // 这个选择由「apply 之后 diff 必须是 in-sync」的性质测试兜底。
                    content_hash: hash_mcp_spec(command, args, env),
                    delivered_at_unix: super::now_unix(),
                });
            }
            Err(err) => failures.push(format!("{}: {err:#}", e.id.label())),
        }
    }
    report.delivered.sort();
    if !failures.is_empty() {
        report.failure = Some(failures.join("; "));
    }
    Ok(report)
}

/// 按机制分派到三家的写入器。
fn dispatch(
    target: &McpTarget,
    name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    replace: bool,
) -> anyhow::Result<()> {
    match &target.mechanism {
        McpMechanism::CodexToml => mcp_toml::upsert_codex_mcp(&target.path, name, command, args, env),
        McpMechanism::KimiJson => mcp_json::upsert_kimi_mcp(&target.path, name, command, args, env),
        McpMechanism::ClaudeCli { binary } => {
            if replace {
                // 先摘再写。remove 失败就停在这儿——add 会覆盖掉一份我们并不认识的东西。
                mcp_claude::claude_remove(binary, name)?;
            }
            mcp_claude::claude_add_json(binary, name, &claude_spec_json(command, args, env))
        }
    }
}

/// claude CLI 收的 `--json` 载荷。键名与它自己写出来的形状一致。
fn claude_spec_json(command: &str, args: &[String], env: &BTreeMap<String, String>) -> String {
    let mut v = serde_json::Map::new();
    v.insert("command".into(), command.into());
    if !args.is_empty() {
        v.insert("args".into(), serde_json::json!(args));
    }
    if !env.is_empty() {
        v.insert("env".into(), serde_json::json!(env));
    }
    serde_json::Value::Object(v).to_string()
}
```

顶部 import 补齐：`use crate::apply::write::...` 不需要；需要

```rust
use crate::apply::{HarnessReport, now_unix};
```
——不，`now_unix` 是 `apply::mod` 的私有 fn，从 `apply::mcp` 用 `super::now_unix()` 即可（Rust 的私有可见性包含子孙模块）。`HarnessReport` 用 `use crate::apply::HarnessReport;`。

另外需要 `use crate::apply::{mcp_claude, mcp_json, mcp_toml};`、`use crate::capability::{Capability, Kind, Spec};`、`use crate::diff::{Action, HarnessPlan};`、`use crate::store::hash::hash_mcp_spec;`、`use crate::store::provenance::{Ledger, LedgerEntry};`、`use std::collections::BTreeMap;`。

- [ ] **Step 4: 跑单元测试**

Run: `cargo test --lib apply::mcp`
Expected: 全绿。

- [ ] **Step 5: 加 `HarnessReport::absorb` 并接默认 `apply`**

`src/apply/mod.rs`（`impl HarnessReport` 若不存在就新建一个）：

```rust
impl HarnessReport {
    /// 把同一家的第二批交付并进第一批（§9.6：一家有两种写路径，报告只有一个）。
    ///
    /// `delivered` 合并后重排——「按 label 排好序」是这个类型的契约，
    /// 两份各自有序不保证合并后有序。
    pub fn absorb(&mut self, other: HarnessReport) {
        self.delivered.extend(other.delivered);
        self.delivered.sort();
        if let Some(f) = other.failure {
            self.failure = Some(match self.failure.take() {
                Some(prev) => format!("{prev}; {f}"),
                None => f,
            });
        }
    }
}
```

`src/adapters/mod.rs` 的默认 `apply`：

```rust
    /// 默认实现：先交付 skills，再交付 MCP（§9.2 第 2 步）。
    ///
    /// 两种写路径并进同一份报告——「claude-code 只做成了一半」是一件事，
    /// 不该因为它是 skills 那半还是 MCP 那半而分成两行。
    fn apply(
        &self,
        slice: &HarnessPlan,
        desired: &[Capability],
        content: &ContentIndex,
        ledger: &mut Ledger,
    ) -> anyhow::Result<HarnessReport> {
        let mut report = crate::apply::deliver_skills(
            self.name(),
            self.skills_target_dir().as_deref(),
            slice,
            content,
            ledger,
        )?;
        let mcps = crate::apply::mcp::deliver_mcps(
            self.name(),
            self.mcp_target().as_ref(),
            slice,
            desired,
            ledger,
        )?;
        report.absorb(mcps);
        Ok(report)
    }
```

（`use crate::capability::{..., Capability};` 若还没 import 就补。）

- [ ] **Step 6: `execute` 接收 `desired`**

`src/apply/mod.rs`：

```rust
pub fn execute(
    plan: &Plan,
    desired: &[Capability],
    content: &ContentIndex,
    ledger: &mut Ledger,
    home: &Home,
) -> anyhow::Result<ApplyReport> {
```

调用点改成 `adapter.apply(hp, desired, content, ledger)`。doc 注释补一句：

```
/// `desired` 是清单lower 后的能力表——MCP 的交付要拿它的三段载荷
/// （`plan` 里只有 id 与动作，没有要写进去的字节）。
```

`src/lib.rs`：
- `struct Planned` 加 `desired: Vec<Capability>`；
- `plan_for` 里在 `lower::lower(&manifest)` 之后把它一并放进 `Planned`；
- `run_apply` 改成 `apply::execute(&planned.plan, &planned.desired, &planned.content, &mut planned.ledger, home)`；
- `run_diff` 若也构造 `Planned`，加上 `desired` 字段（diff 不用它，填空 Vec 即可——但**不要**为它加参数，diff 不需要知道要写什么）。

- [ ] **Step 7: 端到端（codex 与 kimi）**

新建 `tests/mcp_end_to_end.rs`（风格照 `tests/git_source_end_to_end.rs`：**走公开入口**，断言渲染出来的报告，不用 mtime）：

```rust
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

fn temp_home_with_codex() -> TempHome {
    let t = TempHome::new();
    t.mkdir(".codex");
    t
}

fn run_apply_to_string(t: &TempHome) -> String {
    let mut out = Vec::new();
    let mut confirm = AssumeYes;
    henv::run_apply(&t.path("env.yaml"), &t.home, &mut out, &mut confirm).expect("run_apply");
    String::from_utf8(out).expect("utf8")
}

fn run_diff_to_string(t: &TempHome) -> String {
    let mut out = Vec::new();
    henv::run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
    String::from_utf8(out).expect("utf8")
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
    assert!(cfg.contains("${K}"), "引用不许展开：\n{cfg}");

    // 3) **性质**：apply 之后 codex 那一行必须是 in-sync。
    //
    //    断言落在**这家那一行**上，不落在整段输出上：渲染器把全同步的 harness
    //    收成一行汇总（`✓ in-sync  mcps:1`），逐条 label 不再打印——所以
    //    「输出里有 mcp/context7」在成功路径上反而为假。
    //
    //    这条证的是**读回的就是写下去的**（写入器与读取器对同一批字节的理解一致），
    //    不是账本哈希：`mcp_verdict` 的第一行是「字节相同 → InSync，不看账本」，
    //    因此把 `deliver_mcps` 记的哈希改坏，这条**照样绿**（实测）。账本哈希由
    //    `apply::mcp::tests::an_install_writes_the_mcp_and_records_it` 直接钉住
    //    （实测把哈希改成常量，那条立刻红）。
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
    assert!(!second.contains("delivered"), "无事可做不该再交付：\n{second}");

    // 5) 备份留着用户最初那份，且是单槽
    let bak = t.path(".codex/config.toml.henv.bak");
    let bak_text = std::fs::read_to_string(&bak).unwrap();
    assert!(bak_text.contains("# my codex config"), "{bak_text}");
    assert!(!bak_text.contains("context7"), "备份是改动前的那份：\n{bak_text}");
}

#[test]
fn a_kimi_mcp_lands_in_its_own_file_and_other_keys_survive() {
    let t = TempHome::new();
    t.mkdir(".kimi-code");
    t.write(
        ".kimi-code/mcp.json",
        "{\n  \"mcpServers\": {\n    \"user-own\": { \"command\": \"their-binary\" }\n  }\n}\n",
    );
    t.write("env.yaml", &manifest());

    run_apply_to_string(&t);

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(t.path(".kimi-code/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        v["mcpServers"]["user-own"]["command"], "their-binary",
        "别人那条要留着"
    );
    assert_eq!(v["mcpServers"]["context7"]["command"], "npx");
    assert_eq!(v["mcpServers"]["context7"]["env"]["K"], "${K}");

    // apply 之后读回来的就是我们写下去的那条：清单声明的 context7 不再需要
    // install/update，报告里剩下的唯一动作是**没写进清单的** `user-own` 的 drift
    // ——它正是上面第一条断言要留住的那个「别人的条目」。
    //
    // 不能断言「整段输出含 `in-sync`」：user-own 的 drift 会把 kimi 那一行压成
    // drift，全同步的汇总行因此根本不出现。缺的是断言，不是功能。
    let after = run_diff_to_string(&t);
    assert!(after.contains("mcp/user-own"), "{after}");
    assert!(
        after.contains("0 install, 0 update"),
        "apply 之后清单里那条不该还要装或改：\n{after}"
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
```

`Cargo.toml` 的 `[dev-dependencies]` 里若没有 `serde_json`，加上（`serde_json` 已是普通依赖，集成测试里要用得显式声明）。

- [ ] **Step 8: 跑全量测试**

Run: `cargo test`
Expected: 全绿。

- [ ] **Step 9: Commit**

```bash
git add src/apply/ src/adapters/mod.rs src/lib.rs tests/mcp_end_to_end.rs Cargo.toml
git commit -m "feat(apply): deliver MCPs to all three harnesses"
```

---

### Task 10: 文档收口与验收

**Files:**
- Modify: `docs/design.md` §9.6、§9.7
- 无代码改动

- [ ] **Step 1: §9.6 改名 `hash_spec` → `hash_mcp_spec` 并写理由**

找到 §9.6 里 `store::hash::hash_spec`（约 line 663）那句，改成：

```
§9.6 的账本哈希用 `store::hash::hash_mcp_spec(command, args, env)`。spec 早先写的是
`hash_spec`（对 spec 的规范化序列化取哈希），实现改成三个原生字段：`Spec` 没有
`Serialize`，为哈希去 derive serde 会把序列化形状变成对外契约；`&Spec` 版本还要为
另外五个变体写永远走不到的 arm。三个原生字段顺带让「拿一个 skill spec 算 mcp 哈希」
在类型上不可表达。**env 的值参与哈希**，与 `mcp_summary`（只渲染键名，§3 决策 3）不同。
```

- [ ] **Step 2: §9.7 补两条命名**

在 §9.7 的备份那段旁边补：

```
临时文件与备份的命名（各家 adapter 落点旁）：
- 临时文件 `.<name>.henv-tmp-<pid>`（与 skill 交付的 `sibling()` 同一形状），
  写完 fsync 后 rename 到目标；失败时删除，不留半截。
- 备份 `<name>.henv.bak`，**单槽**：只在它不存在时写一次。
  目标文件不存在时不备份。
```

- [ ] **Step 3: 全量验收**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --lib apply::mcp_claude -- --ignored
```

Expected:
- `fmt` 无输出。
- `clippy` 无 warning。
- `cargo test` 全绿。
- 最后一条是**真 CLI 的冒烟**：装了 claude 且已登录时应当通过；没装时测试自己打印「跳过」并返回（不算失败）。跑之前先确认手上的 `~/.claude.json` 前后没变——wrapper 把 HOME 钉在 TempHome 里，这一点应当自动成立。

- [ ] **Step 4: 手验一次「写路径 == 读路径」**

```bash
# 用真二进制在一次性 HOME 里跑一遍（HY 变量名按实际 CLI 改）
mkdir -p /tmp/henv-smoke/.codex
cd /tmp/henv-smoke && printf 'version: 1\nmcps:\n  smoke:\n    command: npx\n' > env.yaml
HOME=/tmp/henv-smoke <repo>/target/debug/henv apply --yes
HOME=/tmp/henv-smoke <repo>/target/debug/henv diff
cat /tmp/henv-smoke/.codex/config.toml
```

Expected: diff 报 `in-sync`，`config.toml` 里有 `[mcp_servers.smoke]`。跑完 `rm -rf /tmp/henv-smoke`。（`--yes` 之类的旗标按实际 CLI 的表面改；这条是人工确认，不进测试。）

- [ ] **Step 5: Commit**

```bash
git add docs/design.md
git commit -m "docs(design): pin B3a write-path names in 9.6/9.7"
```

---

## 自查（写完后对着 spec 过一遍）

**Spec 覆盖**

| §9.6/§9.7 的要求 | 落在哪个 Task |
| --- | --- |
| claude 走 shell out，`-s user` | Task 8 |
| claude 的 update = remove + add-json | Task 8（order 测试）、Task 9（分派） |
| codex 走 `toml_edit` 子树 merge | Task 6 |
| kimi 走 serde_json 顶层键 merge | Task 7 |
| `mcp_target()` | Task 4 |
| 账本判定表四行 | Task 3 |
| 账本键加 `kind` | Task 1 |
| `hash_mcp_spec` | Task 2 |
| 同目录 tmp → fsync → rename | Task 5 |
| 单槽 `.bak`，文件不存在时不备份 | Task 5 |
| 失败隔离（单条失败不记账、不中断别家） | Task 9 |
| 冲突即停（一个字节不写） | 已有（`execute` 的 `conflicts()` 前置检查，Task 9 不改） |
| 写 API 白名单 / shell out 允许位 | Task 8 |
| §9.10 merge-safe / 幂等 | Task 6、Task 7、Task 9 的端到端 |

**未覆盖（明确不在 B3a 范围）**：MCP 的**移除**（`--prune`）；remote MCP（只有 `url`、没有 `command`）的 `Spec` 建模——今天 adapter 把它归一成空 `Spec::Mcp`，diff 只按 name 比，B3a 不改这条（它会以「不同 + 无账本 → conflict」的形式出现，是**安全**方向）；plugin bundle 分解与 `degrade:`（B3b）；`--harness <name>`（B3c）。

**类型一致性抽查**：`McpTarget`/`McpMechanism`（Task 4 定义，Task 8/9 用）；`deliver_mcps` 五参数（Task 9 定义与调用处一致）；`hash_mcp_spec` 三参数（Task 2 定义，Task 3/9 用）；`Ledger::find` 三参数（Task 1 定义，Task 3/9 用）；`HarnessAdapter::apply` 四参数（Task 9 定义，`execute` 调用处一致）。

**Review Focus 的钉子在哪个 Task**：1→Task 8/9；2→Task 6；3→Task 6/7；4→Task 8；5→Task 1/3；6→Task 6；7→Task 8（`a_failed_remove_stops_before_the_add`）/Task 9。

---

## Execution Handoff

计划写完。请审阅，并选执行方式（子代理逐任务 / 本会话内联）。这一份任务间耦合比较紧（Task 1 的签名贯穿 Task 3/9，Task 4 的类型贯穿 Task 5–9），所以**内联执行**更省——少一份每任务都要重新读一遍代码库的代价，最后仍然有一个全新上下文的整体评审兜着。
