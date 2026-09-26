# Plan C2：`scan` 收养实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `henv scan` 成为 `apply` 的反向管道：读一遍本机现状，把**清单还描述不了、但本机确实装着**的能力（skill / MCP / plugin）变成 `env.yaml` 条目追加进去，让用户从「手抄现状」里解放出来。只追加、不改已有条目；`--dry-run` 只打印。

**Architecture:** 与 `diff`/`apply`/`doctor` **同一份观察**：复用 `plan_for` 里那一次逐家 `read_state()`（本计划把那段读盘循环提成一个共用helper，`plan_for` 改为调用它），拿到的 `Capability` 带全部载荷（`Spec` 里就有 command/args/env、marketplace/entry）。收养判定与条目生成是纯函数（`&Manifest` + `&[HarnessState]` → `AdoptionReport` → 文本），**I/O 只在两处**：读现状（复用）与写回（复用 `src/apply/write.rs` 的 `write_config`）。写回是**文本级插入**——清单是用户手写的 YAML，注释与格式必须原样保留（§9.1「`scan` 的产物是清单条目，不是形态改写」）。

**Tech Stack:** Rust 2024 / clap 4.6 / serde_yaml_ng。**不新增任何依赖**（YAML 只读不写：条目由我们自己的渲染器生成，避开 serde 的 quote 规则与注释丢失）。

**Spec:** `docs/design.md` §5（`scan（收养，反向管道）`：各 adapter `read_state()` → 归一化 → 与 env.yaml 比对 → 新发现的能力生成条目追加进 env.yaml（`Origin::Adopted`）→ 只追加不修改已有条目、`--dry-run` 只打印）、§3 决策 2（`scan` 是同一管道的反向）、§2.1 第 2/4 条（plugin cache 内的 skill 是 plugin 的分发产物，收养它会「降级而非升级」）、§9.1（「`scan` 的产物是清单条目，不是形态改写」）、§9.9（只读守卫的白名单）、§4（`env.yaml` schema：哪些条目表达得了）、§8（`.bak` 只给配置文件）。

## Global Constraints

- 对话中文；代码标识符、注释、commit message 一律英文。
- **仓库没有远程**：不 push、不创建远程、不改写历史。每个 Task 的 commit 只提交本 Task 的文件，**是否提交由用户决定**。
- **不新增写路径目录。** `scan` 的落盘复用既有白名单内的 `crate::apply::write::write_config`（`src/apply/` 已在 `WRITE_WHITELIST` 里）——`src/scan.rs` 里**不得出现**任何 `WRITE_API` 模式（`fs::write(` / `File::create(` / `fs::rename(` …）。`tests/read_only_guard.rs` 的 `WRITE_WHITELIST` 与 `PROCESS_ALLOWLIST` **一个字符都不改**。
- **收养绝不扩范围。** 条目 `targets:` 记**发现它的那几家**，不写 `all`——收养的语义是「把现状记下来」，不是「顺手让它铺到另外两家」。
- **清单只读一次、现状只读一次。** 判定用的 `Manifest` 与写回用的原文取自同一次 `read_to_string`；harness 现状取自 `plan_for` 那一个循环（helper 提取，不是第二条读路径）。
- **写回前先验证。** 插入后的文本必须先 `Manifest::parse` 成功才允许落盘——宁可 `Err` 也不写出一份自己都解不开的清单。
- **`render()` 的输出与 `src/snapshots/henv__render__tests__*.snap` 一个字节不动。** `scan` 用自己的 `render_scan`。
- **`diff` / `apply` / `doctor` 行为不变**：`plan_for` 的读盘循环只是提取成 helper，观察顺序、错误措辞、`--harness` 语义都不动；既有的 20 个 diff 快照会证明这一点。
- **rust-version 1.85**：**没有 let-chains**（`let ... &&` 要 1.88）。需要「两个条件同时成立」时写嵌套 `if`。
- 本仓库**今天就有既有 lint 债**（`cargo fmt --check` 在 `src/main.rs`、`src/store/git.rs`、`src/store/skills.rs`、`tests/git_source_end_to_end.rs` 上不过；`src/store/skills.rs` 有两条 `clippy::unnecessary_get_then_check`）。**本计划不清理那笔债**：`rustfmt` 只逐个文件跑，**不要**用 `src/lib.rs` 当入口（它会递归格式化整个模块树，把既有债一起改掉）。

## Review Focus

按「最可能先咬人」排序。

1. **写回不许毁掉用户的清单。** 插入是文本级的：段定位（顶层 `^[a-z_]+:` 行）与缩进必须精确；插入后必须 `Manifest::parse` 验证；落盘走 `write_config`（temp → rename，rename 前落 `.bak`）。任何一条不成立都要**报错不写**，而不是「尽力写一半」。→ Task 2。
2. **幂等：连跑两次，第二次 `0 adopted`。** 这是收养正确的标志——第一次写进去的条目第二次必须被「已在清单」认出来。名字带引号的条目（`foo:bar`）是这条最容易被漏掉的角落。→ Task 1、Task 3。
3. **不可收养的东西必须说出来，不许静默跳过。** `command`/`agent`/`hook` 在清单 schema 里表达不了；plugin cache 里的 skill 是 plugin 的分发产物（收养 = 降级）；断链与来源不明没有可记的真相；没有 marketplace 的 plugin 记不出 `source:`。每一条都要在报告里写清「为什么没收养」。→ Task 1。
4. **`targets:` 只记发现它的那几家。** 写 `all` 会让下一次 `apply` 把只装在一家的东西铺到另外两家——收养静默地改了「期望」，这是最危险的一种自作主张。→ Task 1。
5. **已有的条目不碰。** 同名同 kind 已在清单里（无论内容是否与现状一致）= 不收养。内容不一致是 `diff` 的 `update`/`drift` 领域，不是 `scan` 的。→ Task 1。
6. **plugin 的 `ref:` 是快照，不是判决。** 收养时把观测到的 version 记进 `ref:`（只在 version 形时；sha 形与 `unknown` 不记，写进注记）——§2.1 第 3 条：五种形态互不可比，sha 不能当身份。→ Task 1。

## File Structure

**新增**
- `src/scan.rs` —— 收养判定（`adoptions`）、条目渲染（`entry_text`）、报告（`render_scan`）、写回（`insert_entries`、`run_scan`）。一个文件：它是一条纯函数管道 + 一层复用既有原语的 I/O。
- `tests/scan_end_to_end.rs` —— 走公开入口与真实二进制的端到端：dry-run 不写、真写追加、`.bak`、幂等、不可收养的 kind、目标文本逐字节保留。
- `src/snapshots/henv__scan__tests__*.snap` —— 报告 golden（insta）。

**修改**
- `src/lib.rs` —— `pub mod scan;`；把 `plan_for` 里的逐家读盘循环提成 `read_harness_states(home, &manifest, only) -> Vec<diff::HarnessState>`（`plan_for` 改为调用它；`diff::HarnessState.actual` 已经带全部 `Capability` 载荷）。
- `src/main.rs` —— `Scan { manifest, dry_run }` 子命令 + 输出 routing + 模块头那行「`scan` 由 Plan C2 加」改成实际状态 + `long_about` 披露 `scan` 会写盘。
- `docs/design.md` —— 新增 `### 10.2 scan（收养）`。
- `examples/env.yaml` —— 只在必要时补注释（可选；若改，`diff` 的 golden 不受影响，注释不参与解析）。

---

### Task 1: 收养判定与报告（纯函数）

**Files:**
- Modify: `src/lib.rs`、`src/scan.rs`（新建，本 Task 只放纯函数部分）

**Interfaces:**
- Consumes: `crate::manifest::{Manifest, McpEntry, PluginEntry, SkillEntry, SkillSourceKind, PluginSourceKind}`、`crate::capability::{Capability, CapId, Kind, Origin, SkillSource, Spec, Targets}`、`crate::diff::HarnessState`。
- Produces:
  - `pub enum Section { Skills, Mcps, Plugins }`（`as_str()` → `"skills"` / `"mcps"` / `"plugins"`）
  - `pub struct Adoption { pub section: Section, pub name: String, pub body: String }` —— `body` 是该条目**字段行**的 YAML 文本，缩进相对条目头（2 空格），不含 `name:` 那一行。
  - `pub struct Skip { pub label: String, pub harness: String, pub reason: String }`
  - `pub struct AdoptionReport { pub adoptions: Vec<Adoption>, pub skips: Vec<Skip> }`
  - `pub fn adoptions(manifest: &Manifest, states: &[HarnessState]) -> AdoptionReport`
  - `pub fn render_scan(report: &AdoptionReport, dry_run: bool) -> String`

- [ ] **Step 1: 把读盘循环提取成 helper（不改行为）**

`src/lib.rs`：`plan_for` 里 `for a in &registry { … }` 那段（`is_enabled` + `only` 过滤 + `detect` + `read_state` + `HarnessState`）整段搬进

```rust
/// 逐家读现状。**只在这里发生**：`diff`/`apply`/`doctor`/`scan` 共用同一次观察。
/// registry 由调用方传进来（它是有序表，顺序决定 `pairs` 的配对，不要在函数里重建一份）。
/// 未安装的家不 `read_state`（对不存在的目录做无谓 IO），现状记空。
pub(crate) fn read_harness_states(
    registry: &[Box<dyn HarnessAdapter>],
    manifest: &Manifest,
    only: Option<&str>,
) -> anyhow::Result<Vec<diff::HarnessState>> {
    let mut states = Vec::new();
    for a in registry {
        if !is_enabled(manifest, a.name()) {
            continue;
        }
        if let Some(only) = only {
            if a.name() != only {
                continue;
            }
        }
        let detected = a.detect();
        let actual = if detected {
            a.read_state()
                .with_context(|| format!("cannot scan {} state", a.name()))?
        } else {
            Vec::new()
        };
        states.push(diff::HarnessState {
            name: a.name().to_string(),
            detected,
            actual,
        });
    }
    Ok(states)
}
```

`plan_for` 里改成：

```rust
let registry = adapters::registry(home);
let states = read_harness_states(&registry, &manifest, only)?;
// state 与它的 adapter 必须成对交给 compute：同名查找用既有的 adapters::lookup。
let pairs: Vec<(diff::HarnessState, &dyn HarnessAdapter)> = states
    .into_iter()
    .map(|s| {
        let a = adapters::lookup(&registry, &s.name);
        (s, a.expect("read_harness_states 只吐 registry 里的名字"))
    })
    .collect();
```

- [ ] **Step 2: 跑既有测试，确认提取是纯重构**

Run: `cargo test --all-features 2>&1 | grep -E "^test result"`
Expected: 与改动前**同样的条数、全绿**（lib 584 / 集成 38）。任何一条红都说明提取改变了观察顺序或错误措辞——先修回去，别往下走。

- [ ] **Step 3: 写失败的纯函数测试（`src/scan.rs` 的 `#[cfg(test)]`）**

四条，覆盖 Review Focus 的 2/3/4/5：

```rust
/// 现状里三家都认得出的一条 skill（`agents-dir`）→ 一条 `skills:` 条目，
/// `targets` 只记发现它的那两家（不是 `all`）。
#[test]
fn an_adopted_agents_dir_skill_remembers_only_the_harnesses_that_had_it() { … }

/// 已在清单里的 (kind, name) 一律不收——内容是否与现状一致是 diff 的事。
#[test]
fn a_capability_already_in_the_manifest_is_never_adopted() { … }

/// 清单 schema 表达不了的 kind（command/agent/hook）→ 进 `skips`，理由写清楚。
#[test]
fn kinds_the_manifest_cannot_express_are_skipped_with_a_reason() { … }

/// plugin cache 里的 skill 是 plugin 的分发产物：收养它等于把原生安装降级成散装 skill。
#[test]
fn plugin_cache_skills_are_skipped_because_adopting_would_downgrade_them() { … }
```

第一条的夹具用 `crate::testutil::FakeAdapter`（`src/adapters/mod.rs` 里有 `FakeAdapter::cap`，造的就是 `Origin::Adopted`）构造 `HarnessState`，不要自己去搭 adapter。

- [ ] **Step 4: 跑测试确认失败**

Run: `cargo test --lib scan:: 2>&1 | tail -20`
Expected: 编译失败（`adoptions` / `Section` / `Adoption` 不存在）。

- [ ] **Step 5: 实现判定**

判定规则（**这是本计划的契约，逐条对着写**）：

| 现状 | 收养成 | 备注 |
|---|---|---|
| `Skill`，`SkillSource::AgentsDir` | `skills: {source: agents-dir, targets: [发现它的家…]}` | 共享库是既有事实标准（§2.1 第 4 条） |
| `Skill`，`SkillSource::Local { path }` | `skills: {source: local, path: <path>, targets: […]}` | harness 自己目录里的真目录 |
| `Skill`，`SkillSource::PluginBundle` | **skip** | plugin 分发产物，收养 = 降级 |
| `Skill`，`SkillSource::Unknown` | **skip**，理由「来源不可判定（断链或指向库外）」 | `Spec::Skill` 只留 `content_path()`，断链与库外链接在这层**分不开**——不要为此去猜 |
| `Mcp` | `mcps: {command, args, env, targets: […]}` | `env` 值是 `${VAR}` 引用，**原样**写回（不展开，§3 决策 3） |
| `Plugin`，`marketplace: Some(m)` | `plugins: {source: claude-marketplace, marketplace: m, entry: e, ref: <version 形才记>, targets: […]}` | `ref` 是快照 |
| `Plugin`，`marketplace: None` | **skip**，理由「没有 marketplace，记不出 source」 | |
| `Command` / `Agent` / `Hook` | **skip**，理由「清单 schema 表达不了这个 kind」 | §4 |
| 已在清单的 `(kind, name)` | **skip**，理由「清单里已有同名条目」 | 三种 kind 都要判 |

同名去重：同一个 `(kind, name)` 在**多家**都扫到时，只产出一条 `Adoption`，`targets` 是发现它的那些家的并集（按 `KNOWN_HARNESSES` 顺序稳定输出）。同一个 `(kind, name)` 出现在不同的 manifest 段里是不可能的（kind 决定段）。

`body` 的渲染（`entry_text`）——**每个值都可能需要引号**，规则只有一条：先用 `plain_safe(s)` 判，安全就裸写，否则双引号 + 转义：

```rust
/// YAML 裸标量安全：非空、首字符是字母/数字/下划线、其余字符属于
/// `[A-Za-z0-9_.\-/]`、且不是 `true/false/null/yes/no` 这类会被隐式解析的词。
/// 不安全的（含 `:`、`#`、首尾空格、空串……）一律双引号包住并转义 `\` 与 `"`。
fn scalar(s: &str) -> String { … }
```

`scalar` 的测试要含：`foo:bar`、`lead`、`with space`、`#hash`、`""`、`a"b`、`back\slash`、`true`（要引），以及一个**往返**断言——`Manifest::parse` 出来的条目名与原串逐字符相等（这是「quote 规则写错」的唯一有效证据）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test --lib scan:: 2>&1 | tail -20`
Expected: PASS。

- [ ] **Step 7: 报告渲染 + golden**

`render_scan(report, dry_run)` 的形态（与 `doctor` 的段名风格一致，但不共用渲染器）：

```text
(adopt)       skill/trim-cot-leakage  → skills: source=local path=~/.claude/skills/trim-cot-leakage targets=[claude-code]
              mcp/kimi-cu             → mcps: command=uvx targets=[kimi-code]
(skip)        skill/foo@codex  plugin cache 里的 skill 是 plugin 的分发产物（收养会把原生安装降级成散装）
              hook/…          清单 schema 表达不了这个 kind

Scan: 2 to adopt, 3 skipped (dry run)
```

末行随模式变：dry run 写 `(dry run)`，真写模式写 `(appended to env.yaml)`，没有可收养时写 `(nothing to adopt)`。加一条 insta golden 钉住它，加一条断言空报告只有末行。

- [ ] **Step 8: 全量验证 + commit（等用户同意）**

Run: `cargo test --all-features 2>&1 | grep -E "^test result"`

```bash
git add src/lib.rs src/scan.rs src/snapshots
git commit -m "feat(scan): compute what the manifest cannot yet describe"
```

---

### Task 2: 写回 `env.yaml`（文本级插入）

**Files:**
- Modify: `src/scan.rs`、`src/main.rs`、`tests/scan_end_to_end.rs`（新建）

**Interfaces:**
- Produces: `pub fn insert_entries(text: &str, adoptions: &[Adoption], sections: &BTreeMap<Section, Vec<String>>) -> anyhow::Result<String>`、`pub fn run_scan(manifest_path: &Path, home: &Home, dry_run: bool, out: &mut dyn Write) -> anyhow::Result<AdoptionReport>`。
- Consumes: `crate::apply::write::write_config`、`crate::read_harness_states`、`crate::manifest::Manifest::parse`。

- [ ] **Step 1: 写失败的插入测试（纯函数层）**

```rust
/// 追加进已存在的段：原来的字节一个不动，新条目接在该段末尾。
#[test]
fn appending_into_an_existing_section_keeps_every_original_byte() { … }

/// 段不存在 → 在文件末尾新建；已有段与新建段之间恰好一个空行。
#[test]
fn a_missing_section_is_created_at_the_end() { … }

/// 插入结果必须解得出、且解出的条目与原清单条目**逐字段相等**（原有部分不变、新增部分就位）。
#[test]
fn the_inserted_text_parses_and_preserves_the_existing_entries() { … }

/// 段头写了内联值（`mcps: {}` / `mcps: [a]`）时拒绝：我们没有能力把一个 flow 形态
/// 展开成 block 还不改动用户的其他字节 → 报错不写。
#[test]
fn an_inline_section_header_is_refused_instead_of_corrupted() { … }

/// 名带 `:`、`#`、空格的条目插入后仍解得出原名（quote 规则的端到端证据）。
#[test]
fn names_that_need_quoting_survive_the_round_trip() { … }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib scan:: 2>&1 | tail -20`
Expected: 编译失败（`insert_entries` 不存在）。

- [ ] **Step 3: 实现插入**

契约：

- **段定位**：只认列 0 的 `^([A-Za-z0-9_-]+):` 行；`#` 开头的整行注释与空行不参与。段的范围 = 该行到**下一个**列 0 的顶层键行（或文件末尾）。
- **段头必须无内联值**：`mcps:` 之后除空白（与可选注释）外还有内容 → `Err`，消息里指出是哪个段、并建议用户把它改成 block 形态。
- **插入点**：段内最后一个非空、非注释行的下一行；在该点插入 `  <name>:` + 缩进 4 的 `body` 行。段不存在 → 文件末尾（必要时先补一个空行）追加 `mcps:` 段头再插。
- **不改动其他字节**：`insert_entries` 返回的字符串里，原有字符的**相对顺序与内容**不变（用「原文本是新文本的子序列」这条断言钉死）。
- **落盘前验证**：`run_scan` 里 `Manifest::parse(&new_text)` 失败就 `Err`，一个字节都不写。

`run_scan` 的顺序：

```rust
let text = std::fs::read_to_string(manifest_path)?;          // 唯一一次读原文
let manifest = Manifest::parse(&text)?;
let states = crate::read_harness_states(home, &manifest, None)?;
let report = adoptions(&manifest, &states);
write!(out, "{}", render_scan(&report, dry_run))?;
if !dry_run {
    if let Some(new_text) = insert_entries(&text, &report)? {   // None = 无可插入
        Manifest::parse(&new_text).context("refusing to write a manifest we cannot parse")?;
        crate::apply::write::write_config(manifest_path, new_text.as_bytes())?;
    }
}
Ok(report)
```

`--dry-run` 时**不读原文之外不碰任何东西**（含不落 `.bak`）。

- [ ] **Step 4: 接上 CLI**

`src/main.rs`：

```rust
/// 收养：把本机已有、清单还没描述的能力追加进清单（会写 env.yaml）
Scan {
    /// 清单路径
    #[arg(short, long, default_value = "env.yaml")]
    manifest: PathBuf,
    /// 只打印将要追加的条目，不写清单
    #[arg(long)]
    dry_run: bool,
},
```

`long_about` 里补一句写盘边界：`scan` 会写——它只把新条目**追加**进 `env.yaml`（已有条目一个字节不动），落盘前落 `.bak`；`--dry-run` 只打印。模块头那句「`scan` 由 Plan C2 加」改成实际状态。

退出码：`0` 成功（无论有没有新增）/ `2` 失败（清单读不了解不开、某家状态读不了、插入结果解不开）。**没有 `1`**——`scan` 不是门禁（对比 `doctor`）。错误走 stderr 的 `Error: {err:?}`，与 `doctor` 的 2 一致。

- [ ] **Step 5: 端到端测试（`tests/scan_end_to_end.rs`）**

夹具照抄 `tests/doctor_end_to_end.rs` 的形状（`TempHome` + `CARGO_BIN_EXE_henv` + `HENV_HOME`/`HOME` 指向夹具）。四条：

```rust
/// --dry-run：报告里有将要追加的条目，清单**逐字节不变**（也没有 .bak）。
#[test]
fn dry_run_prints_and_writes_nothing() { … }

/// 真写：清单里出现新条目、原字节保持、.bak 落了一份原文。
#[test]
fn a_real_run_appends_and_keeps_a_backup() { … }

/// 幂等：连跑两次，第二次 `0 to adopt` 且清单逐字节不变。
#[test]
fn a_second_run_has_nothing_left_to_adopt() { … }

/// harness 配置读不出来 → 2（沿用 doctor 的「工具没跑成」语义），清单一个字节不动。
#[test]
fn an_unreadable_harness_config_exits_two_and_leaves_the_manifest_alone() { … }
```

- [ ] **Step 6: 全量验证 + 真机一条（只读）**

Run: `cargo test --all-features 2>&1 | grep -E "^test result"`
Run（**先 dry-run**，真机）：`cp examples/env.yaml /tmp/env.yaml.bak && cargo run --quiet -- scan -m /tmp/env.yaml; echo "exit=$?"`
Expected: 真机上仍有一批可收养条目（`mcp/kimi-cu` 等 drift 里那些）；`exit=0`。**不要**对仓库里的 `examples/env.yaml` 跑真写，除非用户明确同意——那会把本机的私货写进仓库文件。

- [ ] **Step 7: commit（等用户同意）**

```bash
git add src/scan.rs src/main.rs tests/scan_end_to_end.rs src/snapshots docs/design.md
git commit -m "feat(scan): append adopted capabilities to the manifest"
```

---

### Task 3: 边界与验收（幂等、`targets` 不扩范围、guard 不动）

**Files:**
- Modify: `tests/scan_end_to_end.rs`、`docs/design.md`（新增 `### 10.2 scan（收养）`）

- [ ] **Step 1: 补边界测试**

```rust
/// 收养来的条目 `targets` 是发现它的那些家：写进去之后，`diff` 不该冒出
/// 「另外两家要装它」的新行。
#[test]
fn an_adopted_entry_only_targets_the_harnesses_that_had_it() { … }

/// 收养不改已有条目：清单里已经有一条同名同 kind 但内容与现状不同（drift/update 的领域），
/// scan 一个字节都不动它。
#[test]
fn scan_never_touches_an_existing_entry_that_drifted() { … }

/// 不可收养的 kind（hook/command/agent）出现在报告里，但清单里一个新段都没有。
#[test]
fn non_adoptable_kinds_are_reported_but_never_written() { … }
```

- [ ] **Step 2: 跑它们**

Run: `cargo test --test scan_end_to_end 2>&1 | tail -20`
Expected: PASS。

- [ ] **Step 3: 守卫与不变量的复核**

Run: `cargo test --all-features 2>&1 | grep -E "^test result"`
Run: `git diff --stat tests/read_only_guard.rs` → **必须无输出**（两张表一个字符没改）。
Run: `cargo test --test read_only_guard -- --nocapture 2>&1 | tail -15` → 8 passed。

- [ ] **Step 4: 落 §10.2**

`docs/design.md` 新增 `### 10.2 scan（收养）`：管道形状（复用同一次 `read_state`）、收养规则表（Task 1 Step 5 那张）、`targets` 只记发现它的家、只追加不改已有、文本级插入 + 落盘前 `Manifest::parse` 验证 + `.bak`、`--dry-run`、退出码 `0/2`、以及「清单 schema 表达不了的 kind 只报告不写」。

- [ ] **Step 5: 全量验证 + commit（等用户同意）**

```bash
git add tests/scan_end_to_end.rs docs/design.md
git commit -m "test(scan): pin idempotence, target scope and the read-only guard"
```

---

## 执行中的偏离（2026-09-27，Task 1 落地时）

1. **「已在清单里」是静默跳过，不出 `(skip)` 行。** 计划里的表把它写成 skip；实现时改成静默——
   它是**正常情况**（清单里每一条都会在现状里出现），报出来会把报告刷满，与 `doctor` 把
   `install`/`in-sync` 当安静行是同一条理由。`(skip)` 段只留「用户可能以为能收养、但收不了」
   的那几类：表达不了的 kind、随 plugin 分发的内容、来源不可判定、无 marketplace、装了但禁用。
2. **`Adoption` 多了 `label: String` 与 `targets: Vec<String>`。** 报告行首与 `Skip.label` 用
   同一套拼写（`skill/trim`，不是 `skills/trim`）；`targets` 单独存一份，渲染器不再去 body 里
   抠那一行（抠字符串是脆的）。
3. **`SkillSource::Git` 也进 skip**：现状侧读不到 `repo:`/`ref:`（adapter 只读 harness 目录），
   写一条只有 `source: git` 的空壳比不写更坏。计划里的表漏了这一行。
4. **`enabled == false` 的 plugin 不收养**，理由「清单不该记一个用户关掉的东西」。
5. **`scalar` 对数字开头的串一律加引号**（`ref: "6.3.0"`）：YAML 会把 `6.3.0`/`1.0` 这类读成
   隐式类型，多一对引号只是不好看，少一对会让清单解不开或解成别的值。判据钉在往返上。
6. `read_harness_states` 收 `registry` 参数而不是自己 `adapters::registry(home)` 再建一份
   ——它是有序表，重建一份就有了两个「顺序」的来源。

7. **plugin 一律不收养**（推翻 Task 1 Step 5 表里的 plugin 行，以及 Review Focus 6 的
   「`ref:` 记快照」规则）。实测：把收养来的 `plugin/chrome-devtools-mcp` 写成
   `targets: [claude-code]` + `source_harness: claude-code` 后跑真机 `diff`，得到的是
   `! undeliverable … 差集为空（目标只有源 harness 自己）`——即 `scan` 会**给环境造问题**；
   而 `targets: all` 会把这家 bundle 的内容拆到另外两家，是静默扩范围。清单里的 plugin 条目
   是**交付请求**不是快照，要记版本得先给清单加「只记录、不交付」的 source 形态（本轮不做）。
   真机 `scan --dry-run` 因此是 `18 to adopt, 75 skipped`，其中 30 条是该理由的 plugin。
8. **`insert_entries(text, adoptions)` 只收两个参数**（计划里的 `sections` 是多余的：段在
   `Adoption` 上）；`Adoption.body` 改成**相对缩进**（每级 2 空格），绝对缩进由插入器按**该段
   已有内容**的缩进补上——用户用 4 空格缩进条目，新条目也是 4 空格。段里的**注释**算段内容，
   新条目排在它下面；段头带内联值 → 报错拒绝写。
9. **备份名是 `.henv.bak`**（`write_config` 既有约定，不是计划里写的 `.bak`）；写回顺序是
   **先落盘后打印报告**——报告末行会说「已追加到 env.yaml」，那句话不能先于事实出现。
10. **不提问确认**：只追加 + 有 `.bak` + 用户显式敲的命令（对比 `apply` 会覆盖既有内容）。

## 验收（C2 完成时）

- `henv scan --dry-run` 在真机上打出可收养条目与不可收养的理由，**清单与 `.bak` 都不出现**。
- `henv scan` 把新条目**追加**进清单：原有字节保持（`insert_entries` 的子序列断言 + 端到端逐字节断言），`.bak` 是原文。
- **连跑两次，第二次 `0 to adopt`，清单逐字节不变**——幂等。
- 收养来的条目 `targets` 只含发现它的家；下一次 `diff` 不冒出「另外两家要装它」。
- 清单 schema 表达不了的 kind（command/agent/hook）、随 plugin 分发的内容、断链、**以及全部 plugin**：
  报告里逐条有理由，清单里零痕迹。
- `tests/read_only_guard.rs` 两张表一个字符未改，8 条守卫全绿；`render()` 的 golden 一个字节未动；`diff`/`apply`/`doctor` 既有测试条数与断言不变。
- 真机冒烟（`#[ignore]`）仍通过：`cargo test --test smoke_real_home -- --ignored --nocapture`。
