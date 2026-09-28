# Plan E：清单编辑子命令（`add` / `remove` / `upgrade`）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 补上「命令式手感」的三个子命令。现在改清单只能手写 YAML，`scan` 又只做追加；`add`/`remove`/`upgrade` 把「改清单 → 应用」这一圈收进一条命令，而清单仍是唯一事实源。

**Architecture:** 三个命令都是**编排层**，不新增任何交付/撤销逻辑。

- 改清单走**文本级编辑**（保留注释与排版），复用 `scan.rs` 已有的定位原语；
- 交付走既有的 `plan_for` + `execute` 路径，撤销走既有的 `prune_plan`——「绝不覆盖」「只删账本记着的」「失败隔离」三类不变式仍然只有一个实现点；
- 落盘走既有的 `apply::write::write_config`（原子写 + 单槽 `.bak`），写前 `Manifest::parse` 自检。

**Tech Stack:** Rust 2024 / serde_yaml_ng / clap。**不新增任何依赖。**

**Spec:** `docs/design.md` §9.2（规划与执行分离）、§9.3（不变式一/二）、§9.8（CLI 面）、§9.12（git 源）。

## 已确认的产品决策

1. **`remove` 默认撤销**本机副本（只删账本里记着的），`--keep` 只改清单。
2. **`add` / `upgrade` 默认交付**，落盘前问一次 y/N；`--no-apply` 只改清单。
3. **`upgrade` 只认 version 形 tag**（去前导 `v` 后含小数点）。没有这种 tag 时报错并提示用 `--to <ref>`，**不猜**默认分支。

## Global Constraints

- 对话中文；代码标识符、注释、commit message 一律英文。
- **不主动 commit**。commit 由用户决定。
- **`diff` / `apply` / `scan` / `doctor` 的既有行为与渲染输出一个字节不动。** 三个新命令只是新增入口。
- **`tests/read_only_guard.rs` 的 `WRITE_WHITELIST` / `PROCESS_ALLOWLIST` 尽量一个字符不改。** 清单编辑落在 `src/` 下新的模块里时，若它调用写 API，必须落在白名单目录内——本计划把编辑原语放进 `src/manifest_edit.rs` 时，写盘仍统一委托 `src/apply/write.rs`，编辑模块本身**只做纯文本变换、不碰盘**。
- **`rust-version 1.85`**：没有 let-chains。
- `rustfmt` 逐个文件跑，不要拿 `src/lib.rs` 当入口。
- 不顺手清理既有 fmt/clippy 债。

## Review Focus

按「最可能先咬人」排序。

1. **改清单必须保留用户的字节。** 清单是给人读的：`examples/env.yaml` 的注释、条目间距、缩进都不能被重写。任何「解析成结构再序列化」的做法都会让这条失守。→ Task 1。
2. **验证源在改清单之前。** `add` 与 `upgrade` 都要先确认新内容真取得到（git ref 存在、`path` 有内容、local 路径存在），失败时**一个字都不写**。否则清单会指向不存在的东西，之后每次 `diff` 都报错，而用户不知道为什么。→ Task 3、Task 5。
3. **`upgrade` 不许猜。** 没有 version 形 tag 时不升默认分支——那会把「ref 就是锁」变成「ref 随上游漂移」。→ Task 5。
4. **重名与不存在都不静默。** `add` 撞已有条目要报错并指出替代命令；`remove` 删不存在的条目要报错退出。静默成功会让脚本以为生效了。→ Task 3、Task 4。
5. **幂等。** `upgrade` 已是最新时不写清单、不留 `.bak`；`add`/`remove` 失败时不留半成品。
6. **`remove` 的撤销顺序。** 必须先改清单再跑 prune——prune 的判据是「账本里有、清单里不要」。顺序反了会找不到目标。

## File Structure

**新增**
- `src/manifest_edit.rs` —— 纯文本清单编辑原语：`insert_entry` / `remove_entry` / `set_entry_field`（自 `scan.rs` 抽出并扩展）。
- `tests/manifest_edit_end_to_end.rs` —— 三个子命令的端到端：假 HOME + 本地 git 仓库。

**修改**
- `src/scan.rs` —— `insert_entries`/`insert_section`/`render_entries` 等原语迁到 `manifest_edit.rs`，scan 改为调用；行为不变。
- `src/store/git.rs` —— 新增 `latest_version_tag`（`git ls-remote --tags --refs`，不 clone）。
- `src/lib.rs` —— `run_add` / `run_remove` / `run_upgrade`。
- `src/main.rs` —— 三个 clap 子命令与 `--help` 文案（披露写盘边界）。
- `docs/design.md` —— §9.8 补三个命令的命令面。

## Tasks

### Task 1：抽出 `src/manifest_edit.rs`

- [x] 把 `scan.rs` 的 `line_ranges` / `top_level_key` / `is_nonblank` / `is_indented_content` / `insert_section` / `render_entries` / `append_section` / `scalar` 迁进新模块，`scan.rs` 改为 `use crate::manifest_edit::…`。
- [x] 把 `Section` 枚举一并迁过去（它是清单段落的概念，不是 scan 的概念），`scan.rs` re-export 以保持既有引用不变。
- [x] 新增 `insert_entry(text, section, name, body) -> Result<Option<String>>`（单条版本，`insert_entries` 变成它的循环）。
- [x] 新增 `remove_entry(text, section, name) -> Result<Option<String>>`：定位列 0 的 `<name>:` 条目行 → 删到下一个同级条目或段尾；条目自己的续行跟着删；条目之间的空行保留一个。
- [x] 新增 `set_entry_field(text, section, name, field, value) -> Result<Option<String>>`：只替换该条目内 `<field>:` 那一行的值，其它字节不动；字段不存在时返回 `Ok(None)`。
- [x] 迁移后 `cargo test` 全绿（`scan` 的既有 golden 不变）。

### Task 2：`store/git.rs` 的 `latest_version_tag`

- [x] `pub fn latest_version_tag(repo: &str, cache: &Path) -> anyhow::Result<Option<String>>`：跑 `git ls-remote --tags --refs <url>`，解析出 tag 名，过滤出 **version 形**（去前导 `v` 后含小数点、且各段是数字）的，取最大。
- [x] 比较用「数字分段比较」而不是字典序（`1.10.0 > 1.9.0`）。
- [x] 没有 version 形 tag → `Ok(None)`（调用方据此报错并提示 `--to`）。
- [x] 网络/命令失败 → `Err`，由调用方按失败隔离处理。
- [x] 单测：混合 tag 列表的挑选、无 version 形、`v` 前缀等价。

### Task 3：`run_add`

- [x] `pub fn run_add(name, source, repo, path, git_ref, targets, manifest_path, home, out, confirm, no_apply) -> Result<()>`。
- [x] 构造条目 body 文本（2 空格相对缩进），与 `scan` 的 `shape()` 输出同形。
- [x] 校验：复用 `manifest.rs` 的规则（不能另写一套）——做法是把新文本插进去后 `Manifest::parse` 自检，失败则原样报错、不落盘。
- [x] 重名检测：`Manifest::parse` 后查 `skills` 段是否已有该 key，有则报错并提示 `upgrade`/`remove`。
- [x] **先验证源**：`git` 源走 `store::git::resolve(..., Fetch)` 确认 ref + path 取得到；`local` 源确认路径存在；`agents-dir` 源确认 `~/.agents/skills/<name>` 存在。
- [x] 写清单（`write_config` + `.bak` + 自检），打印变更摘要。
- [x] 非 `--no-apply` 时接着跑 `run_apply` 的路径（渲染计划 → 确认 → 交付）。

### Task 4：`run_remove`

- [x] `pub fn run_remove(names, manifest_path, home, out, confirm, keep) -> Result<()>`。
- [x] 每个名字先确认清单里存在，缺一个就整次报错退出、不写盘。
- [x] `remove_entry` 逐个删除，`Manifest::parse` 自检后落盘。
- [x] `--keep` 之外走 `run_apply(prune = true)`：只删账本里记着的副本，用户手放的 drift 只报不删。
- [x] 撤销失败按失败隔离报告（账本还在，重跑 `apply --prune` 是修复路径）。

### Task 5：`run_upgrade`

- [x] `pub fn run_upgrade(names, manifest_path, home, out, confirm, to, no_apply) -> Result<()>`。
- [x] 选定条目：给了名字就只升那些（不存在则报错）；否则升全部 `source: git` 条目。
- [x] 每个条目：`--to` 直接用它；否则 `latest_version_tag` 查上游。
- [x] 目标与现状相同 → 报 `already up to date`，不写。
- [x] 没有 version 形 tag 且没有 `--to` → 该条目报错并提示 `--to`（其它条目继续，失败隔离）。
- [x] **先验证新 ref 的内容取得到**，再 `set_entry_field` 改文本。
- [x] 全部处理完后自检 + 落盘；无任何变更时一个字节都不写。
- [x] 非 `--no-apply` 时接着跑 apply 路径。

### Task 6：CLI 接线

- [x] `main.rs` 加三个子命令，`long_about` 里写清写盘边界（与既有子命令同风格）。
- [x] 公共开关：`-m/--manifest`、`--dry-run`、`--no-apply`（`remove` 为 `--keep`）、`--yes`、`--harness`。
- [x] 新增 `tests`：clap 解析（`--source` 值域、`--to` 只在 upgrade 上）。

### Task 7：端到端测试

- [x] `tests/manifest_edit_end_to_end.rs`：假 HOME + 本地 git 仓库（复用 `testutil`）。
- [x] `add` 后：清单里出现条目、格式保留、`diff` 能看到 install、`apply` 后盘上有内容。
- [x] `add` 撞重名 → 报错且清单不变。
- [x] `remove` 后：清单条目消失、盘上 henv 交付的副本被撤销、用户手放的副本仍在。
- [x] `upgrade`：本地仓库打两个 tag，跑一次后 `ref:` 变成新 tag；再跑一次幂等且不留新 `.bak`。
- [x] `upgrade` 无 version 形 tag → 报错并提示 `--to`，清单不变。

### Task 8：文档

- [x] `docs/design.md` §9.8 补三个命令的命令面与写盘边界。
- [x] 本文件的 checkbox 全部销掉。

## 验证

- `cargo test` 全绿（含既有 306+ 条）。
- 在真实 HOME 上跑一次 `add` + `remove` 的 dry-run，确认不改任何字节。
