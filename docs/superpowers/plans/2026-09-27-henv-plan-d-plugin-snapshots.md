# Plan D：记录型 plugin（`source: snapshot`）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让清单能**只记录**一个原生安装的 plugin（身份 + 逐家版本），而不请求交付。它补上 Plan C2 唯一一处被整类跳过的缺口：`scan` 收养 plugin 时，清单里没有「只记录、不交付」这种形态，于是 19 条已装 plugin 只能永远报 `drift`（真机实测 66 条 drift 里 19 条是它）。

**Architecture:** 纯增量，**不动任何既有语义**。

- schema 多一个 `PluginSourceKind::Snapshot` + 一个 `versions:` 映射（harness → 观测到的版本）；
- `lower` 走**第三条旁路表**（`SnapshotTable`，先例是 `DegradeTable` / `SourceHarnessTable`）——**`Spec::Plugin` 一个字段都不加**（§2.2 第 3 条的约束仍然有效）；
- `compute` 见到记录型条目：**吸收**本家那份现状（于是它不再是 drift）、**永不产** `install`/`update`，只在「清单记的版本」与「本机版本」不符（或本机没装）时产一条新的 `Action::Snapshot`；
- `Action::Snapshot` **不是可执行动作**：`apply` 只认 `install`/`update`/`remove`（`apply/mod.rs` 的 filter），一个新变体天然落在它外面；`doctor` 把它算**警告**（记录与现实不符是提醒，不是门禁）。

**Tech Stack:** Rust 2024 / serde_yaml_ng。**不新增任何依赖。**

**Spec:** `docs/design.md` §2 非目标（「marketplace plugin 以 `scan` 快照记录版本」——本计划兑现它）、§2.1 第 3 条 + §2.2 第 7 条（版本判据：只有两边都是 version 形才可比）、§2.2 第 6 条（同一 plugin 在本机可以有 2 个版本 ⇒ 版本必须**逐家**记）、§4 决策 5（没写 `source_harness:` 的条目只做版本对账、不再假装能装）、§9.5（`source_harness:` 与 `targets` 的语义）、§10.1（doctor 的问题/警告划分）、§10.2（`scan` 的收养判据）。

**与原 Plan C2 的关系:** C2 的 Review Focus 第 6 条本来就写了「收养时把观测到的 version 记进 `ref:`」，Task 落地时发现：没有 `source_harness:` 的 plugin 条目仍然会产 `install`/`update`（→ 改判 `undeliverable`），没有任何写法能表达「我只是记着它」。于是 C2 把 plugin **整类**跳过（§10.2 的「plugin 不收养」）。本计划把那一类补回来，并把 `ref:` 换成逐家的 `versions:`（`ref:` 是 git 源的锁，一个 plugin 一个版本在两家的现实面前不够用）。

## Global Constraints

- 对话中文；代码标识符、注释、commit message 一律英文。
- **仓库没有远程**：不 push、不创建远程、不改写历史。commit **由用户决定**（本计划不 commit）。
- **`Spec::Plugin` 不加字段**（§2.2 第 3 条的约束）。记录型的载荷走旁路表，与 `DegradeTable`/`SourceHarnessTable` 同一个先例。
- **`render()` 的既有输出一个字节不动。** 汇总行只在 `snapshot > 0` 时才追加 `, N snapshot`——这一条是硬约束（`src/snapshots/henv__render__*.snap`）。
- **`apply` 行为不变**：记录型条目一个字节都不写，`apply` 的 filter 不认新变体。
- **`tests/read_only_guard.rs` 的 `WRITE_WHITELIST` / `PROCESS_ALLOWLIST` 一个字符不改。**
- **rust-version 1.85**：没有 let-chains。
- `rustfmt` **逐个文件**跑；不要拿 `src/lib.rs` 当入口（会递归格式化整个模块树、把既有债一起改掉）。
- 不顺手清理既有 fmt/clippy 债（`src/main.rs:143/186`、`src/store/git.rs`、`src/store/skills.rs:194/483/498`、`tests/git_source_end_to_end.rs:89`）。
- insta golden 手写时**不留 `assertion_line:`**。

## Review Focus

按「最可能先咬人」排序。

1. **记录型条目绝不许变成交付请求。** 它在 `compute` 里必须在 `Support`/版本比较**之前**被吸收掉：一旦漏掉，`apply` 会去写一个它写不了的东西（§9.5 的「假成功」）。判据：一个只有记录型条目的清单，`diff` 报 `0 install / 0 update / 0 undeliverable`。→ Task 2。
2. **版本必须逐家比。** §2.2 第 6 条实测：`superpowers` 在 claude 是 6.4.1、在 kimi 是 6.3.0。单值 `version:` 必然有一家永远对不上、且**改不掉**。所以是 `versions:` 映射。→ Task 1、Task 2。
3. **不可比的版本不报假 drift。** 比对必须复用 `normalize_version`（§2.1 第 3 条：只有两边都是 version 形才可比）。sha 形目录名、分支名、哨兵、codex 的「版本恒 None」都要安静，而不是报一条永远修不掉的 warning。→ Task 2。
4. **`scan` 的「已在清单」判据要按 plugin 身份，不是清单 key。** plugin 身份是 `entry`（缺省才是 key）。`{key: superpowers-bundle, entry: superpowers}` 与扫到的 `superpowers` 是**同一条**；按 key 判会收养出一条重复条目。→ Task 4。
5. **只记 version 形的版本。** scan 观测到的 sha 形目录名（`db467cc56673`）记进去就是一条 diff 说「不可比」的死值。不记，并在条目的报告行里说明。→ Task 4。
6. **doctor 里它是警告，不是问题。** 记录与现实不符不是事故（本机就没装这个 plugin 也可以是正常状态）。进 `problems` 会让 doctor 在每台新机器上恒红。→ Task 3。

## File Structure

**新增**
- `src/snapshots/henv__scan__tests__*.snap`（若报告 golden 变化）
- `tests/plugin_snapshot_end_to_end.rs` —— 记录型条目的端到端：不写盘、drift 消失、版本对账行、`scan` 收养出 `source: snapshot` 条目、再跑一次幂等。

**修改**
- `src/manifest.rs` —— `PluginSourceKind::Snapshot` + `PluginEntry.versions: Option<BTreeMap<String,String>>` + 校验规则。
- `src/lower.rs` —— `SnapshotTable`（旁路表）+ `lower` 的 marketplace 映射。
- `src/capability.rs` —— `RecordedMatch` + `compare_recorded_version`（与 `normalize_version` 同源）。
- `src/diff.rs` —— `Action::Snapshot`、`rank()`、`Counts::snapshot`、`compute` 的参数与吸收分支。
- `src/lib.rs` —— 建表并传进 `compute`。
- `src/render.rs` —— 新分支 + **条件**汇总行。
- `src/doctor.rs` —— 新分支（警告）。
- `src/scan.rs` —— 收养 plugin（`adoptable`/`shape`/`declared_ids`/`found` 的形状）。
- `docs/design.md` —— §4（schema 与校验）、§5（scan 管道那句）、§10.1（警告清单）、§10.2（plugin 那两行的改写）、新增 §10.3。

---

### Task 1: schema 与校验

**Files:** `src/manifest.rs`

**Interfaces:**
- Produces: `PluginSourceKind::Snapshot`（`source: snapshot`）；`PluginEntry { versions: Option<BTreeMap<String, String>>, .. }`。

**校验规则（全部在 `Manifest::validate` 里，风格与既有条目一致）：**

| 规则 | 理由 |
|---|---|
| `source: snapshot` 不许带 `marketplace` / `repo` / `ref` / `source_harness` | 这四格都在描述「从哪儿拿内容」，记录型条目不拿内容 |
| `source: snapshot` 允许 `entry`（缺省 = key）、`targets`、`versions` | `entry` 是身份，`targets` 是记录覆盖的家，`versions` 是逐家版本 |
| `versions` 的键必须是已知 harness | 拼错要在校验期骂，而不是静默不比对 |
| `versions` 的键必须 ⊆ `targets`（`targets` 缺省 = `all`） | 「记了版本却不认它是记录范围」是自相矛盾 |
| `versions` 的值不许空白（`unknown` 哨兵**允许**） | 空白是打错；`unknown` 是既有的「没有版本」哨兵（§2.2 第 7 条） |
| 非 snapshot 来源写了 `versions` → 报错 | 只有记录型条目读它，写了就是误解 |

**Steps:**
- [ ] `PluginSourceKind` 加 `Snapshot` 变体（`rename_all = "kebab-case"` ⇒ `snapshot`）。
- [ ] `PluginEntry` 加 `versions: Option<BTreeMap<String, String>>`（`#[serde(default)]`）。
- [ ] `validate` 加上面六条。
- [ ] 单测：合法形态解析通过（含 `versions` 与缺省）；六条反例各自报出对应错误。

### Task 2: 旁路表 + 对账判据 + `Action::Snapshot`

**Files:** `src/lower.rs`、`src/capability.rs`、`src/diff.rs`、`src/lib.rs`

**Interfaces:**
- `lower::SnapshotTable::new(&Manifest) -> Self`；`get(&self, plugin: &str) -> Option<&BTreeMap<String, String>>`
  ——「**在表里**」= 这条 plugin 条目是记录型（哪怕 `versions` 是空的）。
- `capability::RecordedMatch { Same, Different, Incomparable }`
- `capability::compare_recorded_version(recorded: &str, actual: Option<&str>) -> RecordedMatch`
- `diff::Action::Snapshot { recorded: String, actual: Option<String> }`（`actual: None` = 本机没装）
- `diff::compute(desired, inputs, degrade, content, ledger, sources, snapshots)` —— 多一个参数。

**判据（顺序即优先级）：**

1. `cap` 是 `Kind::Plugin` 且在 `SnapshotTable` 里 → **吸收**（`matched.insert`，于是本家那份现状不再是 drift）；`continue`。
2. 目标家不在 `cap.targets` 里 → 不吸收（照旧报 drift：「清单说只记在 claude」而 kimi 也有，那是该看见的事）。
3. `versions` 里没有本家 → 安静（只记了身份，没有可对账的版本）。
4. 本机没装 → `Snapshot { recorded, actual: None }`。
5. `compare_recorded_version`：`Different` → `Snapshot { recorded, actual: Some(本机版本原值) }`；`Same`/`Incomparable` → 安静。

**Steps:**
- [ ] `SnapshotTable`（`lower.rs`，仿 `SourceHarnessTable` 的注释与形状）。
- [ ] `RecordedMatch` + `compare_recorded_version`（`capability.rs`，复用 `normalize_version`）。
- [ ] `Action::Snapshot` + `rank()`（排在 `Unknown` 之后、`InSync` 之前）+ `Counts::snapshot`。
- [ ] `compute` 加参数、加吸收分支（放在 `Kind::Plugin` 那条 `source_harness` 分支**之前**）。
- [ ] `plan_for` 建表并传参；`diff.rs` 里既有测试的 `compute` 调用补参数。
- [ ] 单测：`0 install/0 update/0 undeliverable`（Review Focus 1）、drift 消失、`targets` 不含的家照旧 drift、逐家版本（一家 Same 一家 Different）、`Incomparable` 安静、本机没装产 `actual: None`。

### Task 3: 渲染与 doctor

**Files:** `src/render.rs`、`src/doctor.rs`

- `render`：`~ snapshot plugin/x (只记录不交付；清单记 a，本机 b)` / `(…；清单记 a，本机没装)`；汇总行**只在非零时**追加 `, N snapshot`。
- `doctor`：`Action::Snapshot` → `CheckKind::Warning`，前缀 `~ snapshot`，文案同 `render`（doctor 是扁平报告，带 `@harness`）。
- `is_clean()` **不看** `snapshot`（记录不是待办；`apply` 对它无事可做）。

**Steps:**
- [ ] `render_entries` 新分支 + 汇总行条件拼接。
- [ ] `doctor::plan_checks` 新分支。
- [ ] 单测：`snapshot = 0` 时汇总行逐字节不变（既有 golden 就是这条证据）；`snapshot > 0` 时才多一节；doctor 里它是 warning（`healthy()` 仍为 true 单条记录不符时……**否**：`healthy()` 只看 `problems()`，warning 不影响 ⇒ 断言 `problems() == 0 && warnings() >= 1`）。

### Task 4: `scan` 收养 plugin

**Files:** `src/scan.rs`、`tests/plugin_snapshot_end_to_end.rs`

**Steps:**
- [ ] `adoptable`：`Spec::Plugin` → `Ok(())`（删掉「交付请求」那条理由）。
- [ ] `declared_ids`：plugin 段按 `lower::plugin_identity(key, entry)` 收，不按 key（Review Focus 4）。
- [ ] `found` 的值类型从 `(BTreeSet<String>, Capability)` 改成 `BTreeMap<String, Capability>`（harness → 观测到的那份），这样 `shape` 能拿到**逐家**的 `Spec::Plugin.version`。
- [ ] `shape` 的 plugin 分支：`source: snapshot` + 有 version 形的家才写 `versions:`（Review Focus 5），非 version 形的家只进 `targets:` 并在 `summary` 里说明。
- [ ] 端到端：一个 fixture HOME（假 HOME，绝不碰真实 `~`）里放 2 个 plugin（一家有 version、一家没有）→ `scan --dry-run` 报告里两条都可收养；真跑 → 清单里出现 `source: snapshot` 条目；再跑 `diff` → **drift 里没有它们**；再跑 `scan` → `nothing to adopt`（幂等）。
- [ ] 反例：清单里已有 `{key: alias, entry: <同名>}` 时不收养（Review Focus 4）。

### Task 5: 文档与验收

- [ ] `docs/design.md` §4：schema 例子里加一段 `source: snapshot` 的注解 + 校验规则表。
- [ ] §5 的 `scan（收养，反向管道）` 那句改成与 §10.2 一致的描述（条目 `targets` 记发现它的那几家、文本级插入、退出码 0/2）。
- [ ] §10.1：警告清单加「记录型 plugin 的版本与记录不符 / 本机没装」。
- [ ] §10.2：plugin 表格行与「plugin 不收养」那两段改写（表格行指向 §10.3）。
- [ ] 新增 §10.3：记录型 plugin 的 schema、判据表、为什么 `Spec::Plugin` 不加字段、为什么逐家版本。
- [ ] 全量验收。

## 验收（Plan D 完成时）

- 一个只有记录型 plugin 条目的清单：`diff` 报 `0 install / 0 update / 0 undeliverable`，那几条 plugin 不再出现在 `drift` 里。
- 版本不符 / 本机没装 → 一条 `~ snapshot` 行；doctor 里是**警告**，退出码仍 0。
- 逐家版本：claude 对上、kimi 对不上 → **只有 kimi 那一行**。
- 不可比的版本（sha 形、codex 的 None）→ 安静，没有永远修不掉的 warning。
- `scan` 在真机（只读 `--dry-run`）上把 19 条 plugin drift 都报成可收养；`source: snapshot` 条目里的 `versions:` 只含 version 形的值。
- `render()` 的既有 golden 一个字节未动；`diff`/`apply`/`doctor` 既有测试条数与断言不变；`tests/read_only_guard.rs` 8 条全绿。
- 真机只读：`diff` 的 66 drift 里那 19 条 plugin **仍然报 drift**（清单里还没收它们——这是**预期**），但把它们收养进清单后 drift → 0（在临时副本上验证，不动仓库里的 `examples/env.yaml`）。

## 执行中的偏离（2026-09-27 落地时）

1. **`Action::Snapshot` 用 `~ snapshot` 一个标记**，不分「版本不符」与「本机没装」两种
   前缀（提示语里写清）。理由：两者是同一件事的两面——「记录与现实不符」；多一个前缀就多
   一组渲染/分类的格子，而读者要的信息在括号里。
2. **`snapshot` 只在非零时进汇总行**（`Plan: …, 0 unknown, 2 snapshot`）。硬约束是既有
   golden 一个字节不动，而汇总行是它的一部分。
3. **`SnapshotTable` 是第三张旁路表**（`Spec::Plugin` 不加字段，§2.2 第 3 条）。
   代价是 `diff::compute` 多一个参数，既有测试调用点补 `SnapshotTable::default()`
   （Plan A 的「内容层缺席」路径仍然逐字节相同）。
4. **`enabled: false` 的 plugin 照样被收养、也照样按版本对账**，不做区分。记录的是
   「盘上有什么」；禁用让一条**交付请求**无法满足（那条路照旧报 `undeliverable`），
   却不让一条**记录**失真。写进 §10.3 的「已知边界」。
5. **`versions:` 的值经 `scalar()` 渲染，数字开头的会被加引号**（`"6.4.1"`）。
   这是既有的「宁严勿宽」规则，不是本计划的新决定；解出来仍是同一个串。
6. **`manifest.rs` 里那条「unknown variant, expected one of …」的既有断言补上 `snapshot`**
   ——schema 变了，serde 的错误文本跟着变，这是必然结果，不是顺手改测试。
7. **顺带修掉一处诚实性问题**：`scan` 零收养时原来仍然打印 `(appended to env.yaml)`，
   而那一支根本不会走到 `write_config`（清单与 `.bak` 都不动）。改成 `(nothing written)`。
   真机验收第二次跑时正好撞上这句话。（`tests/scan_end_to_end.rs` 的幂等测试钉住它。）
8. **`is_clean()` 不数 `snapshot`**：它不是待办——`apply` 对它无事可做（filter 只认
   `install`/`update`/`remove`）。`counts()` 数它，因为 `render` 的汇总行要看得见。
