# henv Plan B1 — apply 骨架 + skills 交付 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `henv apply` 能把清单里 `agents-dir` / `local` 两种来源的 skill **真拷贝**交付到 claude-code 与 codex，并把「交付了什么」记进账本，使得重跑可判、幂等、且绝不覆盖不是自己放的东西。

**Architecture:** 在 `diff::compute` 这个既有纯函数上加一层**内容层**入参（`ContentIndex` + `Ledger`），而不是另写一个 `plan_apply`。`diff` 与 `apply` 因此共用同一个规划函数——两个规划器互相不一致，正是这个项目存在要消灭的「假同步」。执行层收在新的 `src/apply/` 里，是全项目唯一落盘的地方；内容解析收在 `src/store/`。只读守卫从「哪里都不许写」收窄成白名单「只许 `src/apply/` 与 `src/store/` 写」。

**Tech Stack:** Rust 2024 edition / rust-version 1.85、clap 4.6 (derive)、serde + serde_json、anyhow；**新增依赖 `sha2 0.10`**（内容哈希，见 Task 1 说明）。测试用 insta 1.48 + tempfile 3。

**Spec:** `docs/design.md` §9（写路径），重点 §9.1 范围、§9.2 分层、§9.3 两条不变式与账本、§9.4 staleness、§9.7 事务、§9.8 CLI、§9.9 只读守卫、§9.10 验收测试、§9.12 选型。§2.2 第 1 条（断链）是 §9.3 自愈行为的实测依据。

---

## Global Constraints

- **不变式一（§9.3）**：henv 绝不在 harness 的 skills 目录里创建软链，一律**真拷贝**。
- **不变式二（§9.3）**：henv 只动**自己账本里记着的**条目；遇到同名冲突**停下报错，绝不覆盖**。
- **判定单位是内容等值，不是形态相同（§9.3）**：一条指向源目录的软链与那个源目录是同一个东西，必须判 in-sync 并原样留着。
- **只读守卫白名单（§9.9）**：`src/` 下只有 `src/apply/` 与 `src/store/` 允许出现写 API，其余（含 `testutil.rs` 之外的一切）一律不许。`FORBIDDEN` 里的 `"Command::new("` **必须保留**并拿到白名单，不许从列表里删掉。
- **失败隔离（§5、§9.4）**：文件缺失 → 空值/空表，**不是**错误；文件存在但解不开 → 报错。这条适用于账本、源目录、目标目录。
- **单一交付顺序（§9.2）**：先全局来源解析（`store::skills`，唯一 I/O 前置），再逐 harness 交付（`apply`）。`diff::compute` 是纯函数，不得读盘。
- **事务（§9.7）**：skills 交付走「同层临时目录 → rename 到位」，**不落 `.bak`**（`.bak` 只给配置文件，B3 才用）；单个条目失败不中断其余条目（失败隔离），**没有回滚命令，重跑就是修复路径**。
- **选型（§9.12）**：不引 `git2`（git 走 shell out，B2）、不 shell out 到 `npx`。B1 完全不 shell out。
- **测试**：绝不碰真实 `~`，一律 `TempHome`/`TempDir`。
- **不改无关代码**：不顺手重构、不格式化、不重命名。`Cargo.lock` 随 `Cargo.toml` 一起改。
- 标识符、commit message 用英文；注释与文档串沿用本仓库现状（中文）。

## Review Focus

计划里的测试**没有**覆盖、但最可能咬到真实使用者的五类输入/失效模式。每一条都在所属任务的步骤里补了测试：

1. **交付到一半进程被杀** → 目标目录处于「已换出、未换入」的中间态，或留下 `.henv-tmp-*` / `.henv-old-*` 残骸。使用者期望：下次 `apply` 照常交付并清掉残骸，而不是永久卡住或往里叠。（Task 6）
2. **源目录在解析与交付之间被改动（TOCTOU）** → 账本里记的哈希与盘上内容不符，于是下一次 `diff` 报出一个没人能解释的 `update`。使用者期望：账本记的是**交付完成后重算**的哈希。（Task 7）
3. **skill 目录名含空格或非 ASCII** → 临时目录名与 rename 路径拼接出错，交付把名字写坏。使用者期望：交付后目录名逐字节不变。（Task 6）
4. **清单里同一个 skill 名出现两次、或名字与某个已存在的实目录同名** → `BTreeMap` 静默吞掉重复项 / 冲突判定失效。使用者期望：要么清单层拒绝，要么报可见的冲突。（Task 4）
5. **目标 skills 目录本身不存在**（claude 装了但从没建过 `.claude/skills`）→ apply 直接失败，而不是建目录后交付。使用者期望：交付是「让目标变成清单的样子」，缺目录不算错误。（Task 6）

---

## File Structure

**新建：**

| 文件 | 职责 |
|---|---|
| `src/store/mod.rs` | 落盘层的模块声明与边界说明 |
| `src/store/hash.rs` | `hash_dir`：目录内容指纹。§9.3「内容等值」的度量 |
| `src/store/provenance.rs` | `Ledger` / `LedgerEntry` / `Kind` 的 serde 映射：交付账本 |
| `src/store/skills.rs` | `resolve_sources` / `target_content` / `ContentIndex`：内容层 |
| `src/apply/mod.rs` | `execute` / `ApplyReport` / `deliver_skills`：执行编排 |
| `src/apply/write.rs` | `deliver_skill` / `copy_tree` / `remove_any`：唯一的字节写原语 |

**修改：**

| 文件 | 改动 |
|---|---|
| `Cargo.toml` / `Cargo.lock` | 加 `sha2` |
| `src/capability.rs` | `Kind` 加 serde derive（+ 往返测试） |
| `src/home.rs` | `henv_state_dir()` / `henv_provenance()` |
| `src/diff.rs` | `Action::Conflict`、`PlanEntry.note`、`Plan::conflicts()`、`compute` 新签名、内容层裁决 |
| `src/render.rs` | conflict 行、note 后缀、footer 加 conflict 计数 |
| `src/lib.rs` | `run_apply`、`Confirm` 门面、`run_diff` 接线、`pub mod` 两项、footer 断言 |
| `src/main.rs` | `henv apply` 子命令 |
| `src/adapters/mod.rs` | trait 加 `skills_target_dir()` 与 `apply()`（带默认实现） |
| `src/adapters/claude_code.rs` / `codex.rs` / `kimi_code.rs` | 各实现 `skills_target_dir()` |
| `src/testutil.rs` | `FakeAdapter` 加 `skills_dir` 字段与 builder；`AssumeYes`/`RefuseAll` 测试替身 |
| `tests/read_only_guard.rs` | 白名单改造 + 非空性反证 |
| `src/snapshots/henv__render__tests__spec_example_matches_design_doc.snap` | footer 行 |
| `docs/design.md` | §9 状态标注、§5 示例 footer（顺带修 Minor-2） |

**关键分层：** `store` 回答「内容是什么、谁交付的」；`apply` 回答「往哪写」；`diff` 谁也不问，只比较。**只有 `apply/write.rs` 直接调写 API。**

---

## Task 1: 只读守卫收窄成白名单

**Files:**
- Modify: `tests/read_only_guard.rs`
- Test: `tests/read_only_guard.rs`（本文件内的 `#[test]`）

**Interfaces:**
- Consumes: 无
- Produces: `const WRITE_WHITELIST: &[&str]`、`fn is_whitelisted(path: &Path) -> bool`。后续任务不调用它们，但**必须**落在这两个前缀下才会被守放行。

**为什么先做：** 白名单指向的两个目录此刻还不存在，`for` 循环自然扫不到——所以这一步单独就能全绿；等 Task 6 真的往里写字节时，闸门已经在位。

- [ ] **Step 1: 写失败的测试**

在 `tests/read_only_guard.rs` 末尾追加（`offenders` 与 `src_files` 已存在，直接用）：

```rust
/// §9.9：Plan A 的「哪里都不许写」收窄成「只许在这两个目录写」。
/// 这条测试钉的是**机制**本身——它今天就是绿的（那两个目录还不存在，扫不到东西），
/// 意义在于：Task 6/7 往里放写 API 时放错目录会被它抓住，而放对目录不会被它拦。
#[test]
fn write_api_is_allowed_only_inside_the_whitelist() {
    let mut all = Vec::new();
    for f in src_files() {
        if is_whitelisted(&f) {
            continue;
        }
        if f.file_name().and_then(|n| n.to_str()) == Some("testutil.rs") {
            continue;
        }
        all.extend(offenders(&f));
    }
    assert!(
        all.is_empty(),
        "白名单外的生产代码出现写 API：\n{}\n（写路径只允许出现在 {} 下）",
        all.join("\n"),
        WRITE_WHITELIST.join(" / ")
    );
}

/// 白名单前缀必须**真能**匹配，且必须**不**误放行别的目录。
/// 少了这条，把 `WRITE_WHITELIST` 写成 `&["src/"]` 也能让上一条全绿——
/// 那等于把守卫整个删掉还留着一条「通过」的记录。
#[test]
fn whitelist_matches_by_directory_prefix() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(is_whitelisted(&root.join("src/store/provenance.rs")));
    assert!(is_whitelisted(&root.join("src/apply/write.rs")));
    assert!(
        !is_whitelisted(&root.join("src/diff.rs")),
        "diff 是纯函数，不许写盘"
    );
    assert!(
        !is_whitelisted(&root.join("src/store.rs")),
        "前缀必须带尾斜杠：`src/store` 不该放行一个叫 src/store.rs 的文件"
    );
    assert!(!is_whitelisted(&root.join("src/adapters/codex.rs")));
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --test read_only_guard`
Expected: 编译错误 `cannot find function is_whitelisted in this scope` / `cannot find value WRITE_WHITELIST`。两条新测试都编不过 —— 这是本 Task 预期的 RED（与 Plan A 的 `Action::Unknown` 那次同类）。

- [ ] **Step 3: 改文件头注释与常量**

把 `tests/read_only_guard.rs` 顶部的模块注释与 `FORBIDDEN` 上方注释替换为：

```rust
//! 写路径守卫：`src/` 下只有白名单目录（`src/apply/`、`src/store/`）允许出现文件写 API。
//!
//! Plan A 的形态是「哪里都不许写」；Plan B 落地后收窄成白名单（spec §9.9）。
//! 收窄的方式是**加一层允许**，不是从 `FORBIDDEN` 里删模式——`Command::new(` 尤其
//! 必须留着（B2 的 git 与 B3 的 `claude mcp add-json` 都要 shell out，那是这个项目
//! 第一次真的调用外部程序），谁把某条模式从列表里删掉，`FORBIDDEN` 就少一道闸。

use std::path::{Path, PathBuf};

/// 唯一允许落盘的目录前缀（相对仓库根，带尾斜杠）。spec §9.9。
const WRITE_WHITELIST: &[&str] = &["src/apply/", "src/store/"];
```

`FORBIDDEN` 数组**一个字节都不要改**。

- [ ] **Step 4: 实现 is_whitelisted**

插在 `offenders` 之前：

```rust
/// 相对仓库根的路径，一律用 `/` 分隔——白名单按这个形态匹配。
fn rel_path(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_whitelisted(path: &Path) -> bool {
    let rel = rel_path(path);
    WRITE_WHITELIST.iter().any(|w| rel.starts_with(w))
}
```

- [ ] **Step 5: 删掉旧的名字与断言**

把 `#[test] fn plan_a_source_has_no_write_api_outside_testutil()` 整个函数**删除**（它已被 `write_api_is_allowed_only_inside_the_whitelist` 取代）。`scanner_is_not_vacuous` **原样保留**——它直接调 `offenders(testutil)`，绕开白名单，仍然在证明扫描器不空转。

- [ ] **Step 6: 跑测试确认全绿**

Run: `cargo test --test read_only_guard`
Expected: `4 passed`（`write_api_is_allowed_only_inside_the_whitelist`、`whitelist_matches_by_directory_prefix`、`production_code_never_calls_the_lossy_skill_scanner`、`scanner_is_not_vacuous`）。

- [ ] **Step 7: 跑全量套件**

Run: `cargo test`
Expected: lib 306 passed / 集成 4 passed / 1 ignored / 0 failed。

- [ ] **Step 8: Commit**

```bash
git add tests/read_only_guard.rs
git commit -m "test(guard): narrow the read-only guard to a write whitelist"
```

---

## Task 2: store 模块骨架 + 内容指纹 hash_dir

**Files:**
- Create: `src/store/mod.rs`, `src/store/hash.rs`
- Modify: `Cargo.toml`, `Cargo.lock`, `src/lib.rs`（`pub mod store;`）

**Interfaces:**
- Consumes: 无
- Produces:
  - `pub fn hash_dir(path: &Path) -> anyhow::Result<Option<String>>`（`crate::store::hash`）
  - `pub mod store;` 在 `lib.rs` 里对全 crate 可见

**为什么用 sha2：** 交付后拿到的哈希是「这东西还是不是我放的那个」的唯一证据。用非加密哈希（fxhash 等）意味着一次碰撞就产生**静默假同步**——正是项目要消灭的东西；shell out 到 `shasum` 被 `FORBIDDEN` 的 `Command::new(` 禁止且平台差异大。`sha2` 是纯 Rust、无传递依赖负担的既成事实标准。

- [ ] **Step 1: 加依赖**

```bash
cargo add sha2@0.10
```

Expected: `Cargo.toml` 的 `[dependencies]` 出现 `sha2 = "0.10"`，`Cargo.lock` 随之更新（`Cargo.lock` 是提交物，一起改）。

- [ ] **Step 2: 写失败的测试**

创建 `src/store/hash.rs`，先只放测试模块骨架与一条测试（生产代码留空）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    #[test]
    fn missing_path_has_no_content_hash() {
        let t = TempHome::new();
        assert_eq!(
            hash_dir(&t.path(".agents/skills/nope")).unwrap(),
            None,
            "不存在的路径没有内容可交付，不是错误"
        );
    }

    #[test]
    fn a_dangling_link_has_no_content_hash() {
        let t = TempHome::new();
        t.symlink(".claude/skills/ghost", "../../.agents/skills/ghost");
        assert_eq!(
            hash_dir(&t.path(".claude/skills/ghost")).unwrap(),
            None,
            "断链没有内容——这正是 §2.2 第 1 条那种形态"
        );
    }

    #[test]
    fn a_link_to_a_tree_hashes_as_that_tree() {
        // §9.3 的核心断言：本机那 14 条软链必须被判成「内容与源相同」，
        // 而不是「形态不对，待交付」。形态无关的判定单位就落在这一条上。
        let t = TempHome::new();
        t.write(".agents/skills/real-one/SKILL.md", "# hi")
            .symlink(".claude/skills/real-one", "../../.agents/skills/real-one");
        let src = hash_dir(&t.path(".agents/skills/real-one")).unwrap();
        let via_link = hash_dir(&t.path(".claude/skills/real-one")).unwrap();
        assert!(src.is_some(), "真目录必须有哈希");
        assert_eq!(src, via_link, "软链与其目标必须同哈希");
    }

    #[test]
    fn identical_trees_hash_identically_wherever_they_live() {
        let t = TempHome::new();
        t.write("a/x.md", "content").write("a/sub/y.md", "more");
        t.write("b/x.md", "content").write("b/sub/y.md", "more");
        assert_eq!(
            hash_dir(&t.path("a")).unwrap(),
            hash_dir(&t.path("b")).unwrap(),
            "哈希只看树内相对路径与内容，不看树在哪"
        );
    }

    #[test]
    fn content_or_layout_change_changes_the_hash() {
        let t = TempHome::new();
        t.write("a/x.md", "content").write("a/y.md", "a");
        let base = hash_dir(&t.path("a")).unwrap();

        let t2 = TempHome::new();
        t2.write("b/x.md", "content").write("b/y.md", "b");
        assert_ne!(base, hash_dir(&t2.path("b")).unwrap(), "内容变了");

        let t3 = TempHome::new();
        t3.write("c/x.md", "content");
        assert_ne!(base, hash_dir(&t3.path("c")).unwrap(), "少了一个文件");
    }

    #[test]
    fn entry_boundaries_are_not_ambiguous() {
        // 不做长度前缀分隔的话 ("ab","c") 与 ("a","bc") 会喂进同一串字节。
        let a = TempHome::new();
        a.write("t/ab", "c");
        let b = TempHome::new();
        b.write("t/a", "bc");
        assert_ne!(hash_dir(&a.path("t")).unwrap(), hash_dir(&b.path("t")).unwrap());
    }

    #[test]
    fn a_nested_link_contributes_its_target_not_its_content() {
        // 树内部的软链按「链接目标字符串」参与哈希、不跟随：跟随会让一棵树的内容
        // 哈希绑到树外，也让一个指回祖先的链接变成死循环。
        let t = TempHome::new();
        t.write("src/real.md", "x").write("src/keep.md", "y");
        t.symlink("src/link.md", "real.md");
        let src = hash_dir(&t.path("src")).unwrap();

        let t2 = TempHome::new();
        t2.write("copy/real.md", "x")
            .write("copy/keep.md", "y")
            .symlink("copy/link.md", "real.md");
        assert_eq!(src, hash_dir(&t2.path("copy")).unwrap(), "同样的链接目标");

        let t3 = TempHome::new();
        t3.write("other/real.md", "x")
            .write("other/keep.md", "y")
            .symlink("other/link.md", "../elsewhere.md");
        assert_ne!(src, hash_dir(&t3.path("other")).unwrap(), "链接目标变了");
    }

    #[test]
    fn an_empty_dir_is_not_an_empty_file() {
        let t = TempHome::new();
        t.mkdir("d");
        t.write("f", "");
        assert_ne!(
            hash_dir(&t.path("d")).unwrap(),
            hash_dir(&t.path("f")).unwrap(),
            "空目录与空文件不是同一个东西"
        );
    }

    #[test]
    fn a_regular_file_also_hashes() {
        let t = TempHome::new();
        t.write("one.md", "hello");
        let h = hash_dir(&t.path("one.md")).unwrap().expect("文件有内容");
        assert_eq!(h.len(), 64, "SHA-256 的十六进制是 64 字符，实际 {h}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
```

同时创建 `src/store/mod.rs`：

```rust
//! 落盘层（spec §9.2/§9.3）。
//!
//! 本模块与 `apply` 是全项目**仅有的**两个允许调写 API 的目录（§9.9 白名单）。
//! `store` 回答「内容是什么、谁交付的」，`apply` 回答「往哪写」——分开是为了让
//! 「决定交什么」这件事可以被纯函数测出来，而不必真的落盘。

pub mod hash;
```

并在 `src/lib.rs` 的 `pub mod skills_dir;` 之后加一行：

```rust
pub mod store;
```

- [ ] **Step 3: 跑测试确认它失败**

Run: `cargo test --lib store::hash`
Expected: 编译错误 `cannot find function hash_dir in this scope`。这是预期的 RED。

- [ ] **Step 4: 实现 hash_dir**

在 `src/store/hash.rs` 的测试模块**之前**插入：

```rust
//! 目录内容指纹。spec §9.3 把「需不需要交付」的判定单位定成**内容是否等价**、
//! 不是形态是否相同——所以这里要回答的是「这两坨字节一不一样」。

use anyhow::Context as _;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// 目录（或单文件）的内容指纹，十六进制 SHA-256。
///
/// 三态契约（与 `Home::try_canonicalize`、adapter `read_state` 同一条失败隔离）：
/// - 路径不存在 → `Ok(None)`
/// - 路径存在但解不开（断链）→ `Ok(None)`
/// - 读得出内容 → `Ok(Some(hash))`
/// - 存在、能解，但读的过程中出错（权限等）→ `Err`
///
/// 顶层**跟随**软链：`~/.claude/skills/dingtalk → ~/.agents/skills/dingtalk`
/// 必须与真目录同哈希，否则 §9.3 承诺的「那 14 条软链原样留着」会全变成待交付项。
/// 树**内部**的软链按链接目标字符串参与哈希、不跟随——跟随会把哈希绑到树外，
/// 也让一个指回祖先的链接变成死循环。
pub fn hash_dir(path: &Path) -> anyhow::Result<Option<String>> {
    let root = match std::fs::canonicalize(path) {
        Ok(p) => p,
        // canonicalize 把「不存在」与「断链」都报成 NotFound，对调用方而言它们等价：
        // 都没有内容可交付。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::Error::new(e)
                .context(format!("cannot resolve {}", path.display())));
        }
    };
    let mut hasher = Sha256::new();
    hash_entry(&root, &root, &mut hasher)?;
    Ok(Some(hex(hasher.finalize().as_slice())))
}

fn hash_entry(root: &Path, path: &Path, hasher: &mut Sha256) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?;
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)
            .with_context(|| format!("cannot read link {}", path.display()))?;
        feed(hasher, b'l', &rel, target.as_os_str().as_encoded_bytes());
    } else if meta.is_dir() {
        feed(hasher, b'd', &rel, b"");
        let mut kids: Vec<PathBuf> = std::fs::read_dir(path)
            .with_context(|| format!("cannot list {}", path.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()
            .with_context(|| format!("cannot list {}", path.display()))?;
        // 排序是契约不是优化：readdir 顺序随文件系统而变，不排的话同一棵树
        // 两次哈希可能不同，「内容没变」会被报成漂移。
        kids.sort();
        for k in kids {
            hash_entry(root, &k, hasher)?;
        }
    } else {
        let bytes =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        feed(hasher, b'f', &rel, &bytes);
    }
    Ok(())
}

/// 逐条喂进 hasher：类型字符、相对路径、长度、内容，之间用 `\0` 分隔。
/// 长度前缀不可省——否则 `("ab","c")` 与 `("a","bc")` 喂出同一串字节。
fn feed(hasher: &mut Sha256, kind: u8, rel: &str, body: &[u8]) {
    hasher.update([kind, 0]);
    hasher.update(rel.as_bytes());
    hasher.update([0]);
    hasher.update((body.len() as u64).to_le_bytes());
    hasher.update(body);
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// 让 `OsStr` 的扩展方法在作用域里——`as_encoded_bytes` 是 std 的固有方法，
// 这行 import 只为文档可读性保留；若 clippy 报 unused，删掉它。
```

**注意：** 上面最后那行 `use std::ffi::OsStr;` 如果 `cargo clippy -D warnings` 报 unused（`as_encoded_bytes` 是固有方法，不需要 trait 在作用域），**把它删掉**——本计划不允许为消警告加 `#[allow]`。

- [ ] **Step 5: 跑测试确认它通过**

Run: `cargo test --lib store::hash`
Expected: `8 passed`。

- [ ] **Step 6: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: lib 314 passed / 集成 4 passed / 1 ignored / 0 failed；clippy 零警告。

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/store/mod.rs src/store/hash.rs
git commit -m "feat(store): add content hashing for form-agnostic skill equivalence"
```

---

## Task 3: Home 的 state 路径 + 交付账本

**Files:**
- Modify: `src/home.rs`
- Create: `src/store/provenance.rs`
- Modify: `src/capability.rs`（`Kind` 的 serde 映射）, `src/store/mod.rs`, `src/lib.rs`（`Cargo.toml` 已含 serde_json）
- Test: `src/store/provenance.rs` 内的 `#[test]`、`src/home.rs` 的 `joins_harness_paths_under_root`

**Interfaces:**
- Consumes: `crate::store::hash::hash_dir`（不直接调，账本只存字符串）
- Produces:
  - `Home::henv_state_dir() -> PathBuf`、`Home::henv_provenance() -> PathBuf`
  - `pub const LEDGER_VERSION: u32 = 1`
  - `pub struct Ledger { pub version: u32, pub entries: Vec<LedgerEntry> }`，`Default` 给出 `version = LEDGER_VERSION`
  - `pub struct LedgerEntry { harness: String, kind: Kind, name: String, path: PathBuf, source_id: String, content_hash: String, delivered_at_unix: u64 }`
  - `Ledger::load(&Home) -> anyhow::Result<Ledger>`、`Ledger::save(&self, &Home) -> anyhow::Result<()>`、`Ledger::find(&self, &str, &str) -> Option<&LedgerEntry>`、`Ledger::record(&mut self, LedgerEntry)`

- [ ] **Step 1: 写失败的测试**

在 `src/home.rs` 的测试模块里追加：

```rust
#[test]
fn state_paths_live_outside_any_content_directory() {
    let h = Home::new("/fake/root");
    assert_eq!(
        h.henv_state_dir(),
        PathBuf::from("/fake/root/.local/state/henv")
    );
    assert_eq!(
        h.henv_provenance(),
        PathBuf::from("/fake/root/.local/state/henv/provenance.json")
    );
    // §9.3：账本刻意放在任何内容目录之外，这样它比被它记录的那些目录活得久。
    for d in h.harness_dirs() {
        assert!(!h.henv_state_dir().starts_with(d), "{d:?}");
    }
    assert!(!h.henv_state_dir().starts_with(h.agents_skills()));
}
```

创建 `src/store/provenance.rs`，先只放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Kind;
    use crate::testutil::TempHome;

    fn entry(harness: &str, name: &str, hash: &str) -> LedgerEntry {
        LedgerEntry {
            harness: harness.into(),
            kind: Kind::Skill,
            name: name.into(),
            path: PathBuf::from("/somewhere").join(name),
            source_id: "agents-dir".into(),
            content_hash: hash.into(),
            delivered_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn a_missing_ledger_is_empty_not_an_error() {
        // 这台机器还没 apply 过。缺失不是错误——与 adapter read_state 同一条契约。
        let t = TempHome::new();
        assert_eq!(Ledger::load(&t.home).unwrap(), Ledger::default());
        assert_eq!(Ledger::load(&t.home).unwrap().version, LEDGER_VERSION);
    }

    #[test]
    fn a_saved_ledger_round_trips() {
        let t = TempHome::new();
        let mut l = Ledger::default();
        l.record(entry("claude-code", "foo", "abc"));
        l.save(&t.home).unwrap();

        let back = Ledger::load(&t.home).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.find("claude-code", "foo").unwrap().name, "foo");
        assert!(back.find("claude-code", "bar").is_none());
        assert!(back.find("codex", "foo").is_none(), "同名不同家不能串");
    }

    #[test]
    fn save_writes_entries_in_a_stable_order() {
        // 账本是人也会读的文件。按记录顺序落盘的话，两台机器上的同一份状态会
        // 写出两个不同的文件——纯粹是噪音。
        let t = TempHome::new();
        let mut l = Ledger::default();
        l.record(entry("codex", "zeta", "h3"));
        l.record(entry("claude-code", "beta", "h2"));
        l.record(entry("claude-code", "alpha", "h1"));
        l.save(&t.home).unwrap();

        let text = std::fs::read_to_string(t.home.henv_provenance()).unwrap();
        let pos = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle} 不在账本里"));
        assert!(pos("\"alpha\"") < pos("\"beta\""), "同名 harness 内按名字排");
        assert!(pos("\"claude-code\"") < pos("\"codex\""), "harness 之间按名字排");
    }

    #[test]
    fn record_overwrites_the_same_harness_and_name() {
        let t = TempHome::new();
        let mut l = Ledger::default();
        l.record(entry("codex", "foo", "old"));
        l.record(entry("codex", "foo", "new"));
        assert_eq!(l.entries.len(), 1, "同 harness+name 是 upsert，不是追加");
        assert_eq!(l.find("codex", "foo").unwrap().content_hash, "new");
    }

    #[test]
    fn a_malformed_ledger_is_an_error_not_an_empty_one() {
        // 「存在但解不开」必须报错。静默成空账本会让下一次 apply 把 henv 自己的
        // 交付物当成「别人建的」而拒绝更新，或者更糟：当成无主目录覆盖。
        let t = TempHome::new();
        t.write(".local/state/henv/provenance.json", "{ not json");
        let err = format!("{:#}", Ledger::load(&t.home).unwrap_err());
        assert!(err.contains("provenance.json"), "{err}");
    }

    #[test]
    fn a_ledger_from_a_future_version_is_refused() {
        let t = TempHome::new();
        t.write(
            ".local/state/henv/provenance.json",
            "{\"version\": 999, \"entries\": []}",
        );
        let err = format!("{:#}", Ledger::load(&t.home).unwrap_err());
        assert!(err.contains("999"), "{err}");
    }

    #[test]
    fn an_unknown_entry_field_is_refused() {
        // deny_unknown_fields：账本格式的演进必须走 version 升版，
        // 而不是让新字段在旧二进制眼里静默消失。
        let t = TempHome::new();
        t.write(
            ".local/state/henv/provenance.json",
            "{\"version\": 1, \"entries\": [{\"harness\":\"codex\",\"kind\":\"skill\",\
             \"name\":\"a\",\"path\":\"/p\",\"source_id\":\"s\",\"content_hash\":\"h\",\
             \"delivered_at_unix\":1,\"future\":true}]}",
        );
        assert!(Ledger::load(&t.home).is_err());
    }
}
```

并在 `src/capability.rs` 的测试模块里追加：

```rust
/// 账本把 `Kind` 序列化成 `as_str()` 的那个串（§9.3）。serde 的 rename 与
/// `as_str()`/`parse()` 是**独立的第三处拼写**，必须钉成相等——否则账本里
/// 会出现一个 `parse()` 认不出、而 serde 认得出的 kind。
#[test]
fn kind_serde_uses_the_same_spelling_as_as_str() {
    for k in Kind::all() {
        let json = serde_json::to_string(&k).expect("serialize");
        assert_eq!(json, format!("\"{}\"", k.as_str()), "{k:?}");
        let back: Kind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, k);
    }
    assert!(serde_json::from_str::<Kind>("\"nonsense\"").is_err());
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib store::provenance home::tests::state_paths capability::tests::kind_serde`
Expected: 编译错误——`cannot find function henv_state_dir`、`cannot find type Ledger`、`Kind: Serialize` 未满足。这是预期的 RED。

- [ ] **Step 3: 给 Kind 加 serde 映射**

在 `src/capability.rs` 里，把 `Kind` 的 derive 行改成：

```rust
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
```

（`Kind` 的六个值全是单个小写单词：skill / mcp / command / agent / hook / plugin，所以 `lowercase` 与 `as_str()` 恰好一致，由上面那条测试钉住。）

- [ ] **Step 4: 加 Home 的 state 路径**

在 `src/home.rs` 的 `harness_dirs()` 之后、`impl` 结束之前插入：

```rust
    /// henv 自己的状态目录（spec §9.3）。刻意放在任何**内容目录之外**——
    /// 账本要能活过它所记录的那些目录，放进 `.claude/` 就等于把自己的记忆
    /// 交给被记录方保管。用 XDG 的 state 位置（相对 root，所以 $HENV_HOME 一路有效）。
    pub fn henv_state_dir(&self) -> PathBuf {
        self.root.join(".local").join("state").join("henv")
    }

    /// 交付账本。**不是** §1 非目标里那个 lockfile：它只记「henv 在这里放过什么」，
    /// 不解版本、不做依赖求解、不参与任何求解过程。
    pub fn henv_provenance(&self) -> PathBuf {
        self.henv_state_dir().join("provenance.json")
    }
```

- [ ] **Step 5: 实现 Ledger**

在 `src/store/provenance.rs` 的测试模块**之前**插入：

```rust
//! 交付账本（spec §9.3）。它只回答一个问题：**这个目标是不是 henv 放那儿的**。
//! 「是不是」决定了 `apply` 有没有资格动它——不变式二是「只碰账本里记着的条目」。

use crate::capability::Kind;
use crate::home::Home;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 账本格式版本。与 `manifest::SUPPORTED_VERSION` **分开**：清单格式与账本格式是
/// 两个独立演进面（清单变了不必动账本，反之亦然），共用一个常量会让一边被迫跟版。
pub const LEDGER_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<LedgerEntry>,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger {
            version: LEDGER_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// 目标 harness 名（`adapters::CLAUDE_CODE` 等三个之一）。
    pub harness: String,
    /// 能力种类。B1 只写 `Skill`；字段在是因为 §9.3 的账本要覆盖 B3 的 MCP 写入，
    /// 那时候补字段得升 version，不如现在就留好。
    pub kind: Kind,
    /// 能力名（skill 是目录名）。
    pub name: String,
    /// 交付到的绝对路径。
    pub path: PathBuf,
    /// 来源标识（§9.3）：B1 是 `agents-dir` 或 `local:<dir>`；B2/B3 再加
    /// `<repo>@<ref>@<sha>` 与 `bundle@<from>@<version>@<sha>`。
    /// 它**不参与比较**，是给人读的——所以同一个来源在不同机器上应当写出同一个串。
    pub source_id: String,
    /// 交付完成后在目标上重算的 `hash_dir` 值（不是解析源时的值，见 `apply`）。
    /// §9.4 的 staleness 判定与 §9.3 的「副本被手改」标注都读它。
    pub content_hash: String,
    /// 交付时间，Unix 秒。不用格式化时间串是为了不引日期库进依赖树。
    pub delivered_at_unix: u64,
}

impl Ledger {
    /// 读账本。**缺失 → 空账本，不是错误**（这台机器还没 apply 过）；
    /// **存在但解不开 → 报错**——与 adapter `read_state` 同一条失败隔离契约。
    pub fn load(home: &Home) -> anyhow::Result<Ledger> {
        let path = home.henv_provenance();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("cannot read ledger {}", path.display())));
            }
        };
        let ledger: Ledger = serde_json::from_str(&text)
            .with_context(|| format!("ledger {} is not valid json", path.display()))?;
        anyhow::ensure!(
            ledger.version == LEDGER_VERSION,
            "ledger {} has version {}, this build only understands {LEDGER_VERSION}",
            path.display(),
            ledger.version
        );
        Ok(ledger)
    }

    /// 写账本。这是 `store` 里唯一的写点，且只被 `apply::execute` 调用。
    ///
    /// §9.7 的事务形态：同目录临时文件 → fsync → rename。**不落 `.bak`**——
    /// 账本是本工具自己的状态文件，被覆盖是它的正常工作方式，不是用户的手写配置。
    pub fn save(&self, home: &Home) -> anyhow::Result<()> {
        let path = home.henv_provenance();
        let dir = path
            .parent()
            .expect("provenance.json always has a parent directory");
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;

        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| {
            (&a.harness, &a.kind, &a.name).cmp(&(&b.harness, &b.kind, &b.name))
        });
        let body = serde_json::to_vec_pretty(&Ledger {
            version: self.version,
            entries,
        })
        .context("cannot serialize ledger")?;

        let tmp = dir.join("provenance.json.henv-tmp");
        std::fs::write(&tmp, &body)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::File::open(&tmp)
            .with_context(|| format!("cannot reopen {}", tmp.display()))?
            .sync_all()
            .with_context(|| format!("cannot fsync {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }

    pub fn find(&self, harness: &str, name: &str) -> Option<&LedgerEntry> {
        self.entries
            .iter()
            .find(|e| e.harness == harness && e.name == name)
    }

    /// 记一条（同 harness+name 覆盖旧的）。`apply` 交付成功后调用。
    pub fn record(&mut self, entry: LedgerEntry) {
        match self
            .entries
            .iter_mut()
            .find(|e| e.harness == entry.harness && e.name == entry.name)
        {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }
}
```

在 `src/store/mod.rs` 里加：

```rust
pub mod provenance;
```

- [ ] **Step 6: 跑测试确认它通过**

Run: `cargo test --lib store::provenance store::hash home::tests capability::tests::kind_serde`
Expected: 全绿（provenance 7 条 + hash 8 条 + home 的 1 条新增 + kind_serde 1 条）。

- [ ] **Step 7: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: lib 323 passed / 集成 4 passed / 1 ignored / 0 failed；clippy 零警告。

- [ ] **Step 8: Commit**

```bash
git add src/home.rs src/capability.rs src/store/mod.rs src/store/provenance.rs
git commit -m "feat(store): add the delivery ledger at the XDG state path"
```

---

## Task 4: store::skills —— 源解析与目标内容扫描

**Files:**
- Create: `src/store/skills.rs`
- Modify: `src/store/mod.rs`

**Interfaces:**
- Consumes: `crate::store::hash::hash_dir`
- Produces:
  - `pub struct ResolvedContent { pub path: PathBuf, pub hash: String, pub source_id: String }`
  - `pub enum TargetContent { Has(String), NoContent }`
  - `pub struct ContentIndex { pub sources: BTreeMap<CapId, ResolvedContent>, pub targets: BTreeMap<String, BTreeMap<String, TargetContent>> }`（`Default` 空表）
  - `pub fn resolve_sources(desired: &[Capability], home: &Home) -> anyhow::Result<BTreeMap<CapId, ResolvedContent>>`
  - `pub fn target_content(actual: &[Capability]) -> anyhow::Result<BTreeMap<String, TargetContent>>`

**为什么只解析两种源：** §9.1 把 B1 的范围钉死在 `agents-dir` / `local`。git 源（B2）与 bundle 拆解（B3）**不进** `sources`，于是 `compute` 对它们落回 Plan A 的 `compare_versions`，行为与今天逐字节相同——这是既有 306 条测试能继续全绿的前提。

- [ ] **Step 1: 写失败的测试**

创建 `src/store/skills.rs`，先只放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Origin, SkillSource, Spec, Targets};
    use crate::testutil::{FakeAdapter, TempHome};

    /// 清单侧的一条 skill（与 `lower::lower` 的产物同形：`path: None`）。
    fn desired(name: &str, source: SkillSource) -> Capability {
        Capability {
            id: CapId::new(Kind::Skill, name),
            origin: Origin::Manifest,
            spec: Spec::Skill { source, path: None },
            targets: Targets::All,
        }
    }

    /// 现状侧的一条 skill（带 adapter 填入的路径）。
    fn actual(name: &str, path: PathBuf) -> Capability {
        let mut c = FakeAdapter::cap(
            Kind::Skill,
            name,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(path),
            },
        );
        c.origin = Origin::Adopted {
            harness: "claude-code".into(),
        };
        c
    }

    #[test]
    fn agents_dir_source_resolves_to_root_agents_skills() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        let idx = resolve_sources(&[desired("foo", SkillSource::AgentsDir)], &t.home).unwrap();
        let r = idx.get(&CapId::new(Kind::Skill, "foo")).expect("must resolve");
        assert_eq!(r.path, t.path(".agents/skills/foo"));
        assert_eq!(r.source_id, "agents-dir");
        assert_eq!(r.hash.len(), 64);
    }

    #[test]
    fn local_source_resolves_to_the_declared_path_and_records_it() {
        let t = TempHome::new();
        let dir = t.path("elsewhere/bar");
        t.write("elsewhere/bar/SKILL.md", "# bar");
        let idx = resolve_sources(
            &[desired("bar", SkillSource::Local { path: dir.clone() })],
            &t.home,
        )
        .unwrap();
        let r = idx.get(&CapId::new(Kind::Skill, "bar")).expect("must resolve");
        assert_eq!(r.path, dir);
        assert_eq!(
            r.source_id,
            format!("local:{}", dir.display()),
            "来源标识要能告诉人这份内容从哪个目录来的"
        );
    }

    #[test]
    fn an_unresolvable_source_is_simply_absent() {
        // 三件事都落在这条上：源目录还不存在、git 源（B2 的范围）、bundle 源（B3）。
        // 「absent」的后果是 compute 落回 compare_versions —— 即 Plan A 的行为。
        let t = TempHome::new();
        let wanted = vec![
            desired("missing", SkillSource::AgentsDir),
            desired(
                "from-git",
                SkillSource::Git {
                    repo: "https://example.invalid/x".into(),
                    path: None,
                    git_ref: "main".into(),
                },
            ),
            desired(
                "from-bundle",
                SkillSource::PluginBundle {
                    plugin: "superpowers".into(),
                },
            ),
            desired("bad-local", SkillSource::Local { path: t.path("nope") }),
        ];
        let idx = resolve_sources(&wanted, &t.home).unwrap();
        assert!(idx.is_empty(), "一个都不该解析出来，实际 {:?}", idx.keys());
    }

    #[test]
    fn non_skill_capabilities_are_ignored() {
        let t = TempHome::new();
        let mcp = FakeAdapter::mcp("context7", "npx");
        assert!(resolve_sources(&[mcp], &t.home).unwrap().is_empty());
    }

    #[test]
    fn target_content_reads_the_copy_or_reports_it_has_none() {
        let t = TempHome::new();
        t.write(".claude/skills/real/SKILL.md", "# real");
        t.symlink(".claude/skills/ghost", "../../.agents/skills/ghost");
        t.mkdir(".codex/skills");
        let caps = vec![
            actual("real", t.path(".claude/skills/real")),
            actual("ghost", t.path(".claude/skills/ghost")),
        ];
        let map = target_content(&caps).unwrap();
        assert!(matches!(map.get("real"), Some(TargetContent::Has(h)) if h.len() == 64));
        assert_eq!(
            map.get("ghost"),
            Some(&TargetContent::NoContent),
            "盘上有条目但内容解不开 → NoContent"
        );
        assert!(map.get("absent").is_none(), "没扫到就是没扫到");
    }

    #[test]
    fn a_skill_without_a_path_is_not_in_the_target_map() {
        // adapter 没填路径 = 不知道它在哪。凭空补一个猜测路径会让 compute 对一个
        // 根本没读到的位置下内容判决——这正是要防的假判定。
        let mut c = actual("no-path", PathBuf::from("/irrelevant"));
        c.spec = Spec::Skill {
            source: SkillSource::AgentsDir,
            path: None,
        };
        assert!(target_content(&[c]).unwrap().is_empty());
    }

    #[test]
    fn a_link_to_the_source_hashes_the_same_as_the_source() {
        // 这一条是 §9.3 那 14 条软链不被动的前提，跨 resolve/target 两侧验证。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../.agents/skills/foo");
        let src = resolve_sources(&[desired("foo", SkillSource::AgentsDir)], &t.home).unwrap();
        let tgt = target_content(&[actual("foo", t.path(".claude/skills/foo"))]).unwrap();
        let ResolvedContent { hash, .. } = src.values().next().unwrap();
        assert_eq!(
            tgt.get("foo"),
            Some(&TargetContent::Has(hash.clone())),
            "源与指向源的软链必须同哈希，否则那 14 条软链会被反复判成待交付"
        );
    }

    #[test]
    fn the_index_default_is_two_empty_tables() {
        // compute 的 Plan A 行为靠这个 Default 复现，它是既有 306 条测试的前提。
        let idx = ContentIndex::default();
        assert!(idx.sources.is_empty());
        assert!(idx.targets.is_empty());
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib store::skills`
Expected: 编译错误 `cannot find function resolve_sources` / `cannot find type ContentIndex`。预期的 RED。

- [ ] **Step 3: 实现**

在 `src/store/skills.rs` 的测试模块**之前**插入：

```rust
//! 内容层（spec §9.2 第 1 步 + §9.3）：把「清单要什么」与「本机现在是什么」
//! 都翻译成内容指纹，交给 `diff::compute` 做纯函数比较。
//!
//! 本模块是 `store` 里唯一的 I/O 入口，且只在 `compute` **之前**跑——
//! `compute` 之后就是纯函数，`apply` 之后才落盘。

use crate::capability::{CapId, Capability, Kind, SkillSource, Spec};
use crate::home::Home;
use crate::store::hash::hash_dir;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 清单里一条 skill 的本地内容来源，已解析成真实路径 + 指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedContent {
    /// 源所在目录（`Local` 是清单声明的路径，`AgentsDir` 是 `~/.agents/skills/<name>`）。
    pub path: PathBuf,
    /// 源内容的 [`hash_dir`] 值。
    pub hash: String,
    /// 写进账本的来源标识（§9.3）。
    pub source_id: String,
}

/// 目标侧一个 skill 的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetContent {
    /// 目标存在且读得到内容（真目录，或能解析的软链——§9.3 不看形态）。
    Has(String),
    /// 盘上有这个条目，但它解不开（断链）。**它没有内容，所以没有东西可被破坏**——
    /// `apply` 会把它换成真拷贝（§9.3：旧的脆弱形态在重跑中自愈）。
    NoContent,
}

/// `compute` 的内容层入参。
///
/// 两个空表的 `Default` 让 Plan A 的行为**逐字节**保持——那是既有 306 条测试的前提，
/// 也是本计划能小步落地的基础。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentIndex {
    /// 清单侧：能力 → 本地源内容。
    pub sources: BTreeMap<CapId, ResolvedContent>,
    /// 目标侧：harness 名 → skill 名 → 目标内容。
    pub targets: BTreeMap<String, BTreeMap<String, TargetContent>>,
}

/// §9.2 第 1 步：全局来源解析。
///
/// B1 只解析 `agents-dir` 与 `local`（§9.1 的范围）。解析不出来的条目**不进**表，
/// 于是 `compute` 对它们落回 Plan A 的 `compare_versions`（对 `AgentsDir`/`Local`
/// 是 `Unknown`，对 `Git` 是比 ref）——行为与今天一致。
pub fn resolve_sources(
    desired: &[Capability],
    home: &Home,
) -> anyhow::Result<BTreeMap<CapId, ResolvedContent>> {
    let mut out = BTreeMap::new();
    for cap in desired {
        if cap.id.kind != Kind::Skill {
            continue;
        }
        let Spec::Skill { source, .. } = &cap.spec else {
            continue;
        };
        let (path, source_id) = match source {
            SkillSource::AgentsDir => (
                home.agents_skills().join(&cap.id.name),
                "agents-dir".to_string(),
            ),
            SkillSource::Local { path } => (path.clone(), format!("local:{}", path.display())),
            // Git（B2）与 PluginBundle（B3）：不是今天的事，安静跳过。
            _ => continue,
        };
        // None = 源不存在或断链。没有源就没有内容可交付，也不该让整次运行失败。
        if let Some(hash) = hash_dir(&path)? {
            out.insert(
                cap.id.clone(),
                ResolvedContent {
                    path,
                    hash,
                    source_id,
                },
            );
        }
    }
    Ok(out)
}

/// 目标侧内容扫描：从 adapter 扫到的 `Spec::Skill.path` 上算内容。
///
/// §9.3 的「不看它是真目录、软链还是别的」在这里落地——`hash_dir` 顶层跟随软链，
/// 所以一条指向源目录的软链与那个源目录哈希相同，被判 in-sync 而不是待交付。
///
/// adapter 没填 `path` 的能力不进表：`compute` 于是落回 `compare_versions`，
/// 而不是对一个我们根本没读到的位置下内容判决。
pub fn target_content(
    actual: &[Capability],
) -> anyhow::Result<BTreeMap<String, TargetContent>> {
    let mut out = BTreeMap::new();
    for cap in actual {
        if cap.id.kind != Kind::Skill {
            continue;
        }
        let Spec::Skill { path: Some(p), .. } = &cap.spec else {
            continue;
        };
        if out.contains_key(&cap.id.name) {
            continue;
        }
        let content = match hash_dir(p)? {
            Some(h) => TargetContent::Has(h),
            None => TargetContent::NoContent,
        };
        out.insert(cap.id.name.clone(), content);
    }
    Ok(out)
}
```

在 `src/store/mod.rs` 里加：

```rust
pub mod skills;
```

- [ ] **Step 4: 跑测试确认它通过**

Run: `cargo test --lib store::skills`
Expected: `8 passed`。

- [ ] **Step 5: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: lib 331 passed / 集成 4 passed / 1 ignored / 0 failed；clippy 零警告。

- [ ] **Step 6: Commit**

```bash
git add src/store/mod.rs src/store/skills.rs
git commit -m "feat(store): resolve agents-dir and local skill sources into content"
```

---

## Task 5: diff 的内容层 —— Action::Conflict、PlanEntry.note、compute 新签名

**Files:**
- Modify: `src/diff.rs`
- Test: `src/diff.rs` 的测试模块

**Interfaces:**
- Consumes: `crate::store::skills::{ContentIndex, TargetContent}`、`crate::store::provenance::Ledger`
- Produces:
  - `Action::Conflict { reason: String }`（新增变体，`rank()` 里排 **0**，高于 Install）
  - `PlanEntry { pub id: CapId, pub action: Action, pub note: Option<String> }` + `PlanEntry::new(CapId, Action)` + `PlanEntry::with_note(Option<String>)`
  - `Counts { …, pub conflict: usize }`（既有字段全是 `usize`，新的跟着走）
  - `Plan::conflicts(&self) -> Vec<&PlanEntry>`
  - `compute(desired: &[Capability], inputs: &[HarnessInput], degrade: &DegradeTable, content: &ContentIndex, ledger: &Ledger) -> Plan`

**判定顺序（§9.3，写进代码注释）：**

| 目标内容 | 在账本里 | 动作 |
|---|---|---|
| 与源相等 | 不查 | `InSync` —— **本机那 14 条软链落这一格，原样留着** |
| 不等 | 账本哈希 = 当前目标哈希 | `Update` |
| 不等 | 账本哈希 ≠ 当前目标哈希 | `Update` + note「副本被手改过」 |
| 不等 | 不在账本里 | **`Conflict`** —— 不是我们放的东西，绝不覆盖 |
| 没有内容（断链） | 不查 | `Update` —— 没有内容可破坏，§9.3 要的正是这个自愈 |

- [ ] **Step 1: 写失败的测试**

在 `src/diff.rs` 的测试模块里追加：

```rust
    use crate::store::provenance::{Ledger, LedgerEntry};
    use crate::store::skills::{ContentIndex, ResolvedContent, TargetContent};

    /// 一条清单侧 skill 的能力（desired 形态）。
    fn wanted_skill(name: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Skill, name),
            origin: Origin::Manifest,
            spec: Spec::Skill {
                source: SkillSource::Local {
                    path: PathBuf::from(format!("/src/{name}")),
                },
                path: None,
            },
            targets: Targets::All,
        }
    }

    /// 说得出名字的源内容，绕过磁盘（内容层是纯函数，这里就该纯着测）。
    fn content_with(
        name: &str,
        source_hash: &str,
        target: TargetContent,
    ) -> ContentIndex {
        let id = CapId::new(Kind::Skill, name);
        let mut idx = ContentIndex::default();
        idx.sources.insert(
            id,
            ResolvedContent {
                path: PathBuf::from(format!("/src/{name}")),
                hash: source_hash.into(),
                source_id: "local:/src".into(),
            },
        );
        idx.targets
            .entry("claude-code".to_string())
            .or_default()
            .insert(name.to_string(), target);
        idx
    }

    fn ledger_claiming(name: &str, hash: &str) -> Ledger {
        let mut l = Ledger::default();
        l.record(LedgerEntry {
            harness: "claude-code".into(),
            kind: Kind::Skill,
            name: name.into(),
            path: PathBuf::from("/target").join(name),
            source_id: "local:/src".into(),
            content_hash: hash.into(),
            delivered_at_unix: 1,
        });
        l
    }

    fn content_plan(
        actual: Vec<Capability>,
        content: &ContentIndex,
        ledger: &Ledger,
    ) -> Plan {
        let fake = FakeAdapter::native("claude-code").with_caps(actual);
        let state = HarnessState {
            name: "claude-code".into(),
            detected: true,
            actual: fake.caps.clone(),
        };
        let inputs = vec![HarnessInput {
            state: &state,
            adapter: &fake,
        }];
        compute(
            &[wanted_skill("foo")],
            &inputs,
            &DegradeTable::new(&Manifest::parse("version: 1\n").unwrap()),
            content,
            ledger,
        )
    }

    fn first_entry(plan: &Plan) -> &PlanEntry {
        &plan.harnesses[0].entries[0]
    }

    #[test]
    fn equal_content_is_in_sync_even_without_a_ledger_entry() {
        // §9.3 的核心：本机那 14 条用户自建的软链，内容与源相同 → 原样留着。
        // 若这里判成 Conflict 或 Update，那 14 条会被无端改写。
        let t = TempHome::new();
        let actual = vec![FakeAdapter::cap(
            Kind::Skill,
            "foo",
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
        )];
        let content = content_with("foo", "aaaa", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &Ledger::default());
        assert!(matches!(first_entry(&plan).action, Action::InSync));
        assert!(first_entry(&plan).note.is_none());
    }

    #[test]
    fn differing_content_in_the_ledger_is_an_update() {
        let t = TempHome::new();
        let actual = vec![FakeAdapter::cap(
            Kind::Skill,
            "foo",
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
        )];
        let content = content_with("foo", "bbbb", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &ledger_claiming("foo", "aaaa"));
        match &first_entry(&plan).action {
            Action::Update { from, to } => {
                assert_eq!(from, "aaaa", "from 是实有，短哈希");
                assert_eq!(to, "bbbb", "to 是清单要的");
            }
            other => panic!("期望 update，实际 {other:?}"),
        }
        assert!(
            first_entry(&plan).note.is_none(),
            "账本记的就是盘上这个哈希 → 没被手改过"
        );
    }

    #[test]
    fn a_hand_modified_copy_is_an_update_with_a_visible_note() {
        // 账本说这里该是 cccc，盘上却是 aaaa —— 有人动过。apply 会覆盖它
        // （它终究是 henv 放的），但报告必须说出来。
        let t = TempHome::new();
        let actual = vec![FakeAdapter::cap(
            Kind::Skill,
            "foo",
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
        )];
        let content = content_with("foo", "bbbb", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &ledger_claiming("foo", "cccc"));
        assert!(matches!(first_entry(&plan).action, Action::Update { .. }));
        assert_eq!(
            first_entry(&plan).note.as_deref(),
            Some("副本被手改过"),
            "手改必须可见，不能静默覆盖"
        );
    }

    #[test]
    fn differing_content_outside_the_ledger_is_a_conflict() {
        // §9.3 不变式二：同名冲突停下报错，绝不覆盖。
        let t = TempHome::new();
        let actual = vec![FakeAdapter::cap(
            Kind::Skill,
            "foo",
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
        )];
        let content = content_with("foo", "bbbb", TargetContent::Has("aaaa".into()));
        let plan = content_plan(actual, &content, &Ledger::default());
        match &first_entry(&plan).action {
            Action::Conflict { reason } => {
                assert!(reason.contains("henv"), "理由要能读，实际 {reason}");
            }
            other => panic!("期望 conflict，实际 {other:?}"),
        }
        assert_eq!(plan.conflicts().len(), 1);
        assert!(!plan.is_clean(), "有冲突就不能算干净");
    }

    #[test]
    fn a_dangling_link_is_an_update_whatever_the_ledger_says() {
        // §9.3：断链没有内容，覆盖它不破坏任何东西——旧形态在重跑中自愈。
        let t = TempHome::new();
        let actual = vec![FakeAdapter::cap(
            Kind::Skill,
            "foo",
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
        )];
        let content = content_with("foo", "bbbb", TargetContent::NoContent);
        let plan = content_plan(actual, &content, &Ledger::default());
        match &first_entry(&plan).action {
            Action::Update { from, .. } => assert_eq!(from, "断链"),
            other => panic!("期望 update，实际 {other:?}"),
        }
        assert!(
            plan.conflicts().is_empty(),
            "断链不是冲突：它没有内容可被破坏"
        );
    }

    #[test]
    fn an_empty_content_index_reproduces_plan_a_exactly() {
        // 这个 Default 是既有 306 条测试的前提：内容层缺席时，判决完全由
        // compare_versions 给出。agents-dir/local 源在那条路上落到 Unknown。
        let t = TempHome::new();
        let actual = vec![FakeAdapter::cap(
            Kind::Skill,
            "foo",
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
        )];
        let plan = content_plan(actual, &ContentIndex::default(), &Ledger::default());
        assert!(
            matches!(first_entry(&plan).action, Action::Unknown),
            "内容层缺席时必须退回 Plan A 的判定（AgentsDir 是 Unknown）"
        );
    }

    #[test]
    fn a_source_with_no_target_content_verdict_falls_back() {
        // 源解析出来了，但这个 harness 上没扫到同名条目 → 表里没有 → 落回
        // compare_versions，而不是凭空判一个 in-sync。
        let t = TempHome::new();
        let actual = vec![FakeAdapter::cap(
            Kind::Skill,
            "foo",
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(t.path(".claude/skills/foo")),
            },
        )];
        let content = content_with("other", "bbbb", TargetContent::Has("bbbb".into()));
        let plan = content_plan(actual, &content, &Ledger::default());
        assert!(matches!(first_entry(&plan).action, Action::Unknown));
    }

    #[test]
    fn conflicts_sort_first_and_are_counted() {
        assert_eq!(Action::Conflict { reason: "x".into() }.rank(), 0);
        assert!(Action::Conflict { reason: "x".into() }.rank() < Action::Install.rank());
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib diff::tests::conflicts_sort_first`
Expected: 编译错误 —— `no variant named Conflict found for enum diff::Action`、`no field note on type PlanEntry`、`this function takes 3 arguments but 5 were supplied`。预期的 RED。

- [ ] **Step 3: 加 Action::Conflict、改 rank**

在 `Action` enum 里加（放在 `Install` 之前，与 rank 顺序一致）：

```rust
    /// 目标位置已存在一个**内容不同、又不是 henv 交付的**同名条目（§9.3 不变式二）。
    /// `diff` 把它显式报出来，好让人自己决定；`apply` 见到它就**停下报错、一个字节都不写**。
    ///
    /// 它排在 `rank()` 最前：冲突是阻断性的，其余动作都是「能做什么」，
    /// 只有它决定「能不能做」。
    Conflict {
        reason: String,
    },
```

并在 `rank()` 里把 `Conflict` 映射成 `0`，其余各项依次 +1（`Install` 1、`Update` 2、`Undeliverable` 3、`Drift` 4、`Unknown` 5、`InSync` 6）。

- [ ] **Step 4: 改 PlanEntry**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub id: CapId,
    pub action: Action,
    /// 可选的附加说明，渲染成行尾的 ` [note]`。内容层用它标注 §9.3 的
    /// 「副本被手改过」——动作本身仍是 `update`，但人要知道这次覆盖掉了什么。
    pub note: Option<String>,
}

impl PlanEntry {
    pub fn new(id: CapId, action: Action) -> Self {
        PlanEntry {
            id,
            action,
            note: None,
        }
    }

    /// 没有注记时是空操作——内容层缺席的路径因此不必写 `if let`。
    pub fn with_note(mut self, note: Option<String>) -> Self {
        self.note = note;
        self
    }
}
```

把 `src/diff.rs` 里三处 `PlanEntry { id, action }` 字面构造（约 162/187/209 行）换成 `PlanEntry::new(id, action)`；`src/render.rs:160` 的测试 helper `pe()` 同样换成 `PlanEntry::new(kind, name, action)` 的实现体里那一行。

- [ ] **Step 5: 加 Counts.conflict 与 Plan::conflicts**

`Counts` 加字段 `pub conflict: usize`（放在 `install` 之前，与既有六个字段同型），`counts()` 里加一臂 `Action::Conflict { .. } => c.conflict += 1,`。

`is_clean()` 是**六项全零的合取**（`c.install == 0 && c.update == 0 && c.remove == 0 && c.undeliverable == 0 && c.drift == 0 && c.unknown == 0`），加第七项：

```rust
            && c.conflict == 0
```

注意别把它改成只查 `conflict`/`unknown` 两项——那会**放宽**这个判定，一个有待交付内容的计划会被念成「干净」。新加的那条测试（`differing_content_outside_the_ledger_is_a_conflict` 里的 `assert!(!plan.is_clean())`）正好钉住这一点：不加这一项时 `counts()` 给出六项全零，`is_clean()` 返回 true，断言红。

```rust
impl Plan {
    /// 全部同名冲突（§9.3 不变式二）。非空即「不许交付」——
    /// `apply` 在任何一次落盘之前必须先问这里。
    pub fn conflicts(&self) -> Vec<&PlanEntry> {
        self.harnesses
            .iter()
            .flat_map(|h| h.entries.iter())
            .filter(|e| matches!(e.action, Action::Conflict { .. }))
            .collect()
    }
}
```

- [ ] **Step 6: 加内容层裁决并改 compute 签名**

在 `src/diff.rs` 里加：

```rust
/// 内容层给出的裁决：动作 + 可选的注记。
struct Verdict {
    action: Action,
    note: Option<String>,
}

/// §9.3 的内容裁决点。只有**清单侧解析出了内容**、且**目标侧扫到了这个 skill**
/// 时才走这里；任一条件不满足返回 `None`，`compute` 落回 Plan A 的 `compare_versions`。
///
/// 判定顺序不是随手排的：先判「内容相等」且**不看账本**，是因为本机那 14 条用户自建
/// 的软链内容与源一模一样，必须原样留着而不是被"修"成拷贝；把账本判断提前会把它们
/// 全变成冲突。
fn content_verdict(
    harness: &str,
    cap: &Capability,
    content: &ContentIndex,
    ledger: &Ledger,
) -> Option<Verdict> {
    let src = content.sources.get(&cap.id)?;
    let target = content.targets.get(harness)?.get(&cap.id.name)?;
    match target {
        TargetContent::NoContent => Some(Verdict {
            action: Action::Update {
                from: "断链".to_string(),
                to: short_hash(&src.hash),
            },
            note: None,
        }),
        TargetContent::Has(h) if h == &src.hash => Some(Verdict {
            action: Action::InSync,
            note: None,
        }),
        TargetContent::Has(h) => {
            let action = Action::Update {
                from: short_hash(h),
                to: short_hash(&src.hash),
            };
            match ledger.find(harness, &cap.id.name) {
                // 账本记的就是盘上这个哈希：内容是 henv 放的，也没人动过。
                Some(entry) if &entry.content_hash == h => Some(Verdict { action, note: None }),
                // 账本说这里该是别的哈希：有人手改过，或上次交付只成功了一半。
                // 两种情况都照常覆盖（它终究是 henv 放的），但报告要显式说出来。
                Some(_) => Some(Verdict {
                    action,
                    note: Some("副本被手改过".to_string()),
                }),
                // 盘上有内容、不是我们放的、又和清单要的不一样 → 不动它。
                None => Some(Verdict {
                    action: Action::Conflict {
                        reason: format!(
                            "目标已存在同名条目且不是 henv 交付的（{}），apply 不会覆盖",
                            short_hash(h)
                        ),
                    },
                    note: None,
                }),
            }
        }
    }
}

/// 报告里只显示哈希前 8 位：完整 64 位在终端里是噪音，而人要看的只是
/// 「两端是不是同一个东西」，8 位十六进制足够区分。
fn short_hash(h: &str) -> String {
    h.chars().take(8).collect()
}
```

`compute` 的签名改成：

```rust
pub fn compute(
    desired: &[Capability],
    inputs: &[HarnessInput],
    degrade: &DegradeTable,
    content: &ContentIndex,
    ledger: &Ledger,
) -> Plan
```

在 `Support::Native | Degraded` 那段里，把动作判定改成：

```rust
            let (verdict_action, verdict_note) =
                match content_verdict(&state.name, cap, content, ledger) {
                    Some(v) => (Some(v.action), v.note),
                    None => (None, None),
                };
            let action = match state.actual.iter().find(|a| a.id == cap.id) {
                None => Action::Install,
                Some(a) => match &a.spec {
                    Spec::Plugin { enabled: false, .. } => Action::Undeliverable {
                        reason: "plugin 已安装但未启用".to_string(),
                    },
                    // 内容层的裁决盖过版本比较：在能读到内容的地方，
                    // 「字节一不一样」比「版本串一不一样」是更强的证据。
                    _ => match verdict_action {
                        Some(a) => a,
                        None => match compare_versions(&cap.spec, &a.spec) {
                            VersionMatch::Same => Action::InSync,
                            VersionMatch::Different { from, to } => Action::Update { from, to },
                            // 判不了不再折成 in-sync：报可见的 unknown（spec §2.1 第 3 条）
                            VersionMatch::Unknown => Action::Unknown,
                        },
                    },
                },
            };
            entries.push(PlanEntry::new(cap.id.clone(), action).with_note(verdict_note));
```

- [ ] **Step 7: 修 compute 的调用点**

`src/lib.rs:80` 与 `src/diff.rs` 的测试 helper（约 267 行，以及 509/542/715/734/753/785/821 处的测试）都要补两个实参。全仓先按最省事的形态补上，Task 8 再把 `lib.rs` 接真值：

```rust
diff::compute(&desired, &inputs, &degrade, &ContentIndex::default(), &Ledger::default())
```

diff.rs 测试里的调用点，在测试模块顶部加一个 helper 后用：

```rust
    /// 既有测试的默认入参：内容层缺席 = Plan A 行为。
    fn plan_a(desired: &[Capability], inputs: &[HarnessInput]) -> Plan {
        compute(
            desired,
            inputs,
            &DegradeTable::new(&Manifest::parse("version: 1\n").unwrap()),
            &ContentIndex::default(),
            &Ledger::default(),
        )
    }
```

- [ ] **Step 8: 跑测试确认它通过**

Run: `cargo test --lib diff`
Expected: 全绿（既有 diff 测试 + 9 条新增）。

- [ ] **Step 9: 处理 render 的编译错误**

`render_harness` 的 `match &e.action` 现在缺 `Conflict` 臂 → 编译错误。**先加一个最小的臂让它编过**，渲染细节留给 Task 6：

```rust
            Action::Conflict { reason } => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} ({reason})",
                    action_cell("! conflict"),
                    e.id.label()
                );
            }
```

- [ ] **Step 10: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: **全绿**。本 Task 只加 `Counts.conflict` 字段、不改 `render()` 的格式串，所以渲染输出逐字节不变——既有的两条 footer 断言与 snapshot 都还是对的。格式串到 Task 6 Step 3 才改，那 4 处失配（2 条 lib.rs 断言 + .snap + design.md §5）也在 Task 6 Step 5 一并同步。**不要把 footer 的改动提前到这里**：那会让本任务以红套件结束。

- [ ] **Step 11: Commit**

```bash
git add src/diff.rs src/render.rs src/lib.rs
git commit -m "feat(diff): decide skills by content, and report name conflicts"
```

---

## Task 6: render —— conflict 行、note 后缀、footer 计数

**Files:**
- Modify: `src/render.rs`, `src/lib.rs`（footer 断言）, `src/snapshots/henv__render__tests__spec_example_matches_design_doc.snap`, `docs/design.md`（§5 示例 footer）

**Interfaces:**
- Consumes: `Action::Conflict`、`PlanEntry.note`、`Counts.conflict`（Task 5）
- Produces: 无新 API；输出格式变化。

- [ ] **Step 1: 写失败的测试**

在 `src/render.rs` 的测试模块里追加：

```rust
    #[test]
    fn footer_counts_come_from_plan_and_include_conflicts() {
        let plan = Plan {
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    pe(Kind::Skill, "a", Action::Install),
                    pe(
                        Kind::Skill,
                        "b",
                        Action::Conflict {
                            reason: "目标已存在同名条目且不是 henv 交付的".into(),
                        },
                    ),
                ],
                vec![(Kind::Skill, 2)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.ends_with(
                "Plan: 1 conflict, 1 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("! conflict  skill/b (目标已存在同名条目且不是 henv 交付的)\n"),
            "{out}"
        );
    }

    /// 冲突行前缀必须与别的动作一样对齐到 11 列，否则报告会错位。
    #[test]
    fn conflict_prefix_is_padded_like_every_other_action() {
        assert_eq!(action_cell("! conflict"), "! conflict ");
        assert_eq!(action_cell("! conflict").chars().count(), ACTION_WIDTH);
    }

    /// 内容层给的注记渲染成行尾的 ` [note]`（§9.3「副本被手改过」）。
    #[test]
    fn an_update_note_is_rendered_at_the_end_of_the_line() {
        let plan = Plan {
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![PlanEntry::new(
                    CapId::new(Kind::Skill, "a"),
                    Action::Update {
                        from: "aaaa".into(),
                        to: "bbbb".into(),
                    },
                )
                .with_note(Some("副本被手改过".into()))],
                vec![(Kind::Skill, 1)],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("~ update    skill/a (aaaa → bbbb) [副本被手改过]\n"),
            "{out}"
        );
    }

    /// 同一行里既有 update 又有 install 时，install 的合并逻辑不能被 note 打乱。
    #[test]
    fn a_note_does_not_break_install_coalescing() {
        let plan = Plan {
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    pe(Kind::Skill, "a", Action::Install),
                    PlanEntry::new(
                        CapId::new(Kind::Skill, "b"),
                        Action::Update {
                            from: "1".into(),
                            to: "2".into(),
                        },
                    )
                    .with_note(Some("副本被手改过".into())),
                    pe(Kind::Skill, "c", Action::Install),
                ],
                vec![(Kind::Skill, 3)],
            )],
        };
        let out = render(&plan);
        assert!(out.contains("+ install    skill/a, skill/c\n"), "{out}");
        assert!(out.contains(" [副本被手改过]\n"), "{out}");
    }

    /// 冲突是阻断性的，必须在最前——一行 `+ install` 出现在冲突之前会让人
    /// 以为改动已经可以做了。
    #[test]
    fn conflicts_render_before_installs() {
        let plan = Plan {
            harnesses: vec![hp(
                "claude-code",
                true,
                vec![
                    pe(
                        Kind::Skill,
                        "z-last-name",
                        Action::Conflict {
                            reason: "r".into(),
                        },
                    ),
                    pe(Kind::Skill, "a-first-name", Action::Install),
                ],
                vec![(Kind::Skill, 2)],
            )],
        };
        let out = render(&plan);
        let conflict_at = out.find("! conflict").expect("conflict line");
        let install_at = out.find("+ install").expect("install line");
        assert!(conflict_at < install_at, "{out}");
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib render`
Expected: footer 与 note 的断言红（`Plan: 1 install, …` 少 `1 conflict`；note 没被渲染）。`conflict_prefix_is_padded_like_every_other_action` 与 `conflicts_render_before_installs` 可能已绿——`action_cell` 与 `rank()` 在 Task 5 已经对了，留着它们是为把这两条契约钉在 render 这一侧。

- [ ] **Step 3: 改 footer**

```rust
    let _ = writeln!(
        out,
        "Plan: {} conflict, {} install, {} update, {} remove, {} undeliverable, {} drift, {} unknown",
        c.conflict, c.install, c.update, c.remove, c.undeliverable, c.drift, c.unknown
    );
```

- [ ] **Step 4: 把非 Install 的臂改成先攒行再加注记**

`render_harness` 里，`Action::Install` 那一臂**原样不动**（note 只可能出现在 update 上；install 没有目标可注记）。其余四臂改成：

```rust
            Action::Update { from, to } => {
                let mut line = format!(
                    "{lead}{}{} ({from} → {to})",
                    action_cell("~ update"),
                    e.id.label()
                );
                if let Some(note) = &e.note {
                    let _ = write!(line, " [{note}]");
                }
                let _ = writeln!(out, "{line}");
            }
```

`Undeliverable` / `Drift` / `Unknown` / `Conflict` 四臂同形——末尾都接同一段 `if let Some(note)`。若嫌重复，抽一个小 helper：

```rust
/// 行尾注记（§9.3）。install 的合并行不用它——note 只出现在 update/conflict 上。
fn with_note(mut line: String, note: Option<&String>) -> String {
    if let Some(n) = note {
        let _ = write!(line, " [{n}]");
    }
    line
}
```

- [ ] **Step 5: 更新既有断言与 golden**

- `src/lib.rs` 里两处 footer 断言（`empty_home_reports_all_three_as_skipped`、`desired_mcp_present_in_claude_json_is_in_sync_and_missing_one_is_install`）补上 `0 conflict, `。
- `src/snapshots/henv__render__tests__spec_example_matches_design_doc.snap` 的末行同样补 `0 conflict, `。**按 Plan A 的既有做法手改 .snap 后直接跑**，不走 `INSTA_UPDATE`。
- `docs/design.md` §5 的示例 footer 同步（顺带把 Plan A 终审留下的 Minor-2 一并处理：那个示例块本就缺 `, 0 unknown`）。**只改 footer 这一行**，示例块里另外两处既有失实（install 顺序、多余的 `ref drift:` 前缀）留给文档对齐时单独处理。

- [ ] **Step 6: 跑测试确认它通过**

Run: `cargo test`
Expected: lib 全绿（含 5 条新增）/ 集成 4 passed / 1 ignored / 0 failed。

- [ ] **Step 7: Commit**

```bash
git add src/render.rs src/lib.rs src/snapshots/ docs/design.md
git commit -m "feat(render): show conflicts, hand-edit notes, and the conflict count"
```

---

## Task 7: apply::write —— 唯一的落盘原语

**Files:**
- Create: `src/apply/mod.rs`, `src/apply/write.rs`
- Modify: `src/lib.rs`（`pub mod apply;`）
- Test: `src/apply/write.rs` 的测试模块、`tests/read_only_guard.rs`（加非空性反证）

**Interfaces:**
- Consumes: `crate::store::hash::hash_dir`（测试用）
- Produces:
  - `pub fn deliver_skill(target: &Path, source: &Path) -> anyhow::Result<()>`（`crate::apply::write`）

**Review Focus 1、3、5 就落在这个 Task：** 残骸自愈、非 ASCII / 带空格的名字、目标目录不存在。

- [ ] **Step 1: 写失败的测试**

创建 `src/apply/write.rs`，先放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;
    use std::fs;

    fn tmp_leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("read target parent")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".henv-"))
            .collect()
    }

    #[test]
    fn delivery_is_a_real_copy_not_a_link() {
        // §9.3 不变式一，最硬的一条。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo").write("src/foo/sub/x.md", "x");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(
            meta.file_type().is_dir() && !meta.file_type().is_symlink(),
            "交付物必须是真目录，实际 {:?}",
            meta.file_type()
        );
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# foo");
        assert_eq!(fs::read_to_string(target.join("sub/x.md")).unwrap(), "x");
        assert!(tmp_leftovers(target.parent().unwrap()).is_empty(), "不留残骸");
    }

    #[test]
    fn delivery_creates_a_missing_target_directory() {
        // Review Focus 5：claude 装了但没建过 .claude/skills 是常态。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        let target = t.path(".claude/skills/foo");
        assert!(!t.path(".claude").exists());
        deliver_skill(&target, &t.path("src/foo")).unwrap();
        assert!(target.join("SKILL.md").exists());
    }

    #[test]
    fn re_delivering_replaces_wholesale_and_is_idempotent() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# v1").write("src/foo/extra.md", "e");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        // 改源、删掉一个文件，再交付一次：目标必须整体等于新源，
        // 而不是旧内容上面叠新内容。
        t.write("src/foo/SKILL.md", "# v2");
        fs::remove_file(t.path("src/foo/extra.md")).unwrap();
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# v2");
        assert!(
            !target.join("extra.md").exists(),
            "整体替换：源里删掉的文件不能在目标里活下来"
        );
        assert!(tmp_leftovers(target.parent().unwrap()).is_empty());
    }

    #[test]
    fn a_dangling_link_is_replaced_by_a_real_copy() {
        // §9.3 的自愈：断链没有内容，覆盖它不破坏任何东西。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../src/gone");
        let target = t.path(".claude/skills/foo");
        assert!(fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        deliver_skill(&target, &t.path("src/foo")).unwrap();
        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# foo");
    }

    #[test]
    fn a_working_link_is_replaced_by_a_real_copy_too() {
        // apply 只在内容层判成 update 时才走到这儿。走到这儿就意味着
        // 「这个位置的形态要变成真拷贝」——§9.3 不变式一没有例外。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# v1")
            .write("other/foo/SKILL.md", "# v2")
            .symlink(".claude/skills/foo", "../../other/foo");
        deliver_skill(&t.path(".claude/skills/foo"), &t.path("src/foo")).unwrap();
        let target = t.path(".claude/skills/foo");
        assert!(!fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# v1");
    }

    #[test]
    fn a_source_that_is_itself_a_link_delivers_real_content() {
        // 源侧软链（本机那 14 条 agents-dir skill 就长这样被声明）不能让交付物
        // 也变成一条链——否则不变式一从源那头绕过去了。
        let t = TempHome::new();
        t.write("real/foo/SKILL.md", "# real")
            .symlink("src/foo", "../real/foo");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();
        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# real");
    }

    #[test]
    fn a_nested_link_is_copied_as_a_link() {
        // 树内部的软链按链接目标字符串照搬：`hash_dir` 也是这么算的，两边
        // 一致才能保证「交付完再算一次哈希 == 源哈希」，否则带内部软链的 skill
        // 会被永久判成待交付。
        let t = TempHome::new();
        t.write("src/foo/real.md", "x").symlink("src/foo/link.md", "real.md");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        let link = target.join("link.md");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("real.md"));
        assert_eq!(
            hash_dir(&target).unwrap(),
            hash_dir(&t.path("src/foo")).unwrap(),
            "交付前后内容哈希必须相同，否则每次 diff 都会重报这条 skill"
        );
    }

    #[test]
    fn awkward_names_are_delivered_verbatim() {
        // Review Focus 3：临时目录名是从目标名派生的，带空格/非 ASCII 的名字
        // 一旦被拼错，交付出来的目录名就不是清单里那个。
        let t = TempHome::new();
        for name in ["with space", "中文名字", "dash-and_underscore.v2"] {
            let src = format!("src/{name}");
            t.write(&format!("{src}/SKILL.md"), name);
            let target = t.path(&format!(".claude/skills/{name}"));
            deliver_skill(&target, &t.path(&src)).unwrap();
            assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), name);
            assert!(
                fs::symlink_metadata(&target).unwrap().file_type().is_dir(),
                "{name} 必须是真目录"
            );
        }
        assert!(tmp_leftovers(&t.path(".claude/skills")).is_empty());
    }

    #[test]
    fn leftovers_from_a_half_finished_run_are_cleaned_and_replaced() {
        // Review Focus 1：上次跑到一半被杀，临时目录留在那儿。重跑必须自愈，
        // 而不是把新内容往里叠。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        let target = t.path(".claude/skills/foo");
        let tmp = t.path(&format!(".claude/skills/.foo.henv-tmp-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("stale.md"), "leftover").unwrap();

        deliver_skill(&target, &t.path("src/foo")).unwrap();
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# foo");
        assert!(
            !target.join("stale.md").exists(),
            "残骸不能成为交付的一部分"
        );
        assert!(tmp_leftovers(target.parent().unwrap()).is_empty());
    }

    #[test]
    fn a_missing_source_is_an_error_and_leaves_the_target_alone() {
        let t = TempHome::new();
        t.write(".claude/skills/foo/SKILL.md", "# keep me");
        let err = deliver_skill(&t.path(".claude/skills/foo"), &t.path("src/gone"))
            .expect_err("源不存在必须报错");
        assert!(format!("{err:#}").contains("gone"), "{err:#}");
        assert_eq!(
            fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
            "# keep me",
            "失败不许碰目标"
        );
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib apply::write`
Expected: 编译错误 `cannot find function deliver_skill in this scope`。预期的 RED。

- [ ] **Step 3: 实现**

创建 `src/apply/mod.rs`：

```rust
//! 执行层（spec §9.2/§9.7）。**这是全项目唯一落盘的地方**（§9.9 白名单）。

pub mod write;

pub use write::deliver_skill;
```

`src/apply/write.rs` 的生产部分（插在测试模块之前）：

```rust
//! 落盘原语。`apply` 里所有真的动字节的操作都在这里，且只有这几个。

use anyhow::Context as _;
use std::ffi::OsString;
use std::path::Path;

/// 把一个 skill 目录**真拷贝**到目标位置（§9.3 不变式一：henv 绝不在 harness 的
/// skills 目录里创建软链，一律真拷贝）。
///
/// 落盘顺序（§9.7）：同层临时目录 → 装完 → rename 到位。同名旧条目先 rename 走、
/// 新的进来、最后删旧的——**不落 `.bak`**：skills 目录是构建产物，不是用户手写的
/// 配置（§9.7 只给配置文件留 .bak）。
///
/// 已经存在的条目**整体替换**，不做逐文件 merge：源里删掉的文件必须在目标里消失，
/// 否则「交付完的目标==源」这个等式不成立，`diff` 会永远报这条 skill 待交付。
///
/// 失败时目标保持原样：源读不了、临时目录装不进去、rename 失败，都在动到旧条目
/// 之前或之后回退。没有回滚命令——重跑就是修复路径。
pub fn deliver_skill(target: &Path, source: &Path) -> anyhow::Result<()> {
    // 顶层跟随软链：源本身是一条链时，交付物必须是它指向的真内容，
    // 否则不变式一从源那头被绕过去。
    let source = std::fs::canonicalize(source)
        .with_context(|| format!("cannot resolve source {}", source.display()))?;

    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target {} has no parent", target.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;

    let name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("target {} has no file name", target.display()))?;

    let tmp = parent.join(sibling(name, "henv-tmp"));
    // 上次跑到一半留下的残骸：先清掉，否则拷贝会往里叠。
    remove_any(&tmp)?;
    copy_tree(&source, &tmp)
        .with_context(|| format!("cannot copy {} to {}", source.display(), tmp.display()))?;

    let old = parent.join(sibling(name, "henv-old"));
    remove_any(&old)?;
    let had_old = match std::fs::rename(target, &old) {
        Ok(()) => true,
        // rename 不跟随软链：断链也能这样换走。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            remove_any(&tmp)?;
            return Err(anyhow::Error::new(e)
                .context(format!("cannot move aside {}", target.display())));
        }
    };

    if let Err(e) = std::fs::rename(&tmp, target) {
        // 放不进去就把旧的放回来，别把用户的东西弄丢。
        if had_old {
            let _ = std::fs::rename(&old, target);
        }
        remove_any(&tmp)?;
        return Err(anyhow::Error::new(e)
            .context(format!("cannot put {} in place", target.display())));
    }
    if had_old {
        remove_any(&old)?;
    }
    Ok(())
}

/// `.<name>.henv-<suffix>-<pid>`：与目标同层，且带上进程号——
/// 同一个目标并发跑两次时互相踩不到对方的临时目录。
fn sibling(name: &std::ffi::OsStr, suffix: &str) -> OsString {
    let mut s = OsString::from(".");
    s.push(name);
    s.push(format!(".{suffix}-{}", std::process::id()));
    s
}

/// 删掉一个条目，**不跟随软链**：断链要能被删掉，目录要整棵删。
/// 本来就不存在不是错误。
fn remove_any(path: &Path) -> anyhow::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("cannot remove dir {}", path.display()))?;
    } else {
        std::fs::remove_file(path)
            .with_context(|| format!("cannot remove {}", path.display()))?;
    }
    Ok(())
}

/// 递归拷贝。**树内部的软链照搬为软链**（不跟随）：这样交付后的内容哈希与源相同
/// ——`hash_dir` 也是按链接目标字符串算的。跟随会让带内部软链的 skill 每次都被
/// 判成待交付，也让一条指回祖先的链接变成死循环。
fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(src)
        .with_context(|| format!("cannot stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        let target =
            std::fs::read_link(src).with_context(|| format!("cannot read link {}", src.display()))?;
        // 本项目只跑 macOS/Linux（spec §1），unix 扩展足够。
        std::os::unix::fs::symlink(&target, dst)
            .with_context(|| format!("cannot link {}", dst.display()))?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(dst)
            .with_context(|| format!("cannot create {}", dst.display()))?;
        let mut kids: Vec<_> = std::fs::read_dir(src)
            .with_context(|| format!("cannot list {}", src.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()
            .with_context(|| format!("cannot list {}", src.display()))?;
        kids.sort();
        for k in kids {
            let file_name = k.file_name().expect("read_dir entry has a name");
            copy_tree(&k, &dst.join(file_name))?;
        }
    } else {
        std::fs::copy(src, dst)
            .with_context(|| format!("cannot copy {} to {}", src.display(), dst.display()))?;
    }
    Ok(())
}
```

在 `src/lib.rs` 的 `pub mod adapters;` 之前加：

```rust
pub mod apply;
```

- [ ] **Step 4: 跑测试确认它通过**

Run: `cargo test --lib apply::write`
Expected: `10 passed`。

- [ ] **Step 5: 给守卫加非空性反证**

白名单现在有真内容了。在 `tests/read_only_guard.rs` 里追加：

```rust
/// 白名单不能是摆设：它必须**真的**放行了一批写 API。少了这条，
/// 把白名单指向一个空目录、或者哪天把写路径整体挪出白名单，守卫都会照常绿。
#[test]
fn the_whitelist_actually_covers_real_write_code() {
    let mut hits = Vec::new();
    for f in src_files() {
        if is_whitelisted(&f) {
            hits.extend(offenders(&f));
        }
    }
    assert!(
        hits.iter().any(|h| h.contains("fs::write(") || h.contains("fs::create_dir")),
        "白名单目录里一个写 API 都没有，说明白名单已经和写路径脱节了：{hits:?}"
    );
}
```

- [ ] **Step 6: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: lib 350 passed / 集成 5 passed / 1 ignored / 0 failed；clippy 零警告。

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs src/apply/mod.rs src/apply/write.rs tests/read_only_guard.rs
git commit -m "feat(apply): add the only byte-writing primitive, as a real copy"
```

---

## Task 8: adapter 的 skills_target_dir 与 apply

**Files:**
- Modify: `src/adapters/mod.rs`（trait + 默认实现）、`src/adapters/claude_code.rs`、`src/adapters/codex.rs`、`src/adapters/kimi_code.rs`、`src/testutil.rs`
- Modify: `src/apply/mod.rs`（`deliver_skills`）
- Test: `src/adapters/mod.rs` 的测试模块、`src/apply/mod.rs` 的测试模块

**Interfaces:**
- Consumes: `crate::apply::deliver_skill`、`crate::store::skills::ContentIndex`、`crate::store::provenance::{Ledger, LedgerEntry}`
- Produces:
  - `impl HarnessAdapter { fn skills_target_dir(&self) -> Option<PathBuf>; fn apply(&self, slice: &HarnessPlan, content: &ContentIndex, ledger: &mut Ledger) -> anyhow::Result<HarnessReport> }`
  - `pub struct HarnessReport { pub harness: String, pub delivered: Vec<String>, pub failure: Option<String> }`
  - `pub fn deliver_skills(harness: &str, target_dir: Option<&Path>, slice: &HarnessPlan, content: &ContentIndex, ledger: &mut Ledger) -> anyhow::Result<HarnessReport>`

- [ ] **Step 1: 写失败的测试**

在 `src/adapters/mod.rs` 的测试模块里追加：

```rust
    /// 三家各自的目标目录（§9.1：kimi 没有用户级 skills 目录）。
    /// 这条把「谁有目录」钉死，否则一个把 kimi 也填上的实现会让 kimi 的散装 skill
    /// 静默走进交付路径，而它本该被 `supports` 判成 Unsupported。
    #[test]
    fn exactly_claude_and_codex_have_a_skills_target_dir() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        let got: Vec<(&str, bool)> = reg
            .iter()
            .map(|a| (a.name(), a.skills_target_dir().is_some()))
            .collect();
        assert_eq!(
            got,
            vec![(CLAUDE_CODE, true), (CODEX, true), (KIMI_CODE, false)]
        );
    }

    #[test]
    fn skills_target_dirs_point_at_each_harnesss_own_directory() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        assert_eq!(
            lookup(&reg, CLAUDE_CODE).unwrap().skills_target_dir().unwrap(),
            t.home.claude_skills()
        );
        assert_eq!(
            lookup(&reg, CODEX).unwrap().skills_target_dir().unwrap(),
            t.home.codex_skills()
        );
        // 必须是**各家自己的**目录：两处都返回 agents_skills() 这种错法会让
        // 「交付到 claude」把文件放进共享目录，codex 那边看着像已交付。
        assert_ne!(
            lookup(&reg, CLAUDE_CODE).unwrap().skills_target_dir(),
            Some(t.home.agents_skills())
        );
    }

    /// 默认的 `apply` 必须把活交给 `apply::deliver_skills`——包括 kimi 那次
    /// 「什么都不做但不是错误」的调用。这条用 FakeAdapter 驱动，不碰真实配置树。
    #[test]
    fn the_default_apply_delivers_through_the_shared_helper() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        let cap = FakeAdapter::cap(
            crate::capability::Kind::Skill,
            "foo",
            crate::capability::Spec::Skill {
                source: crate::capability::SkillSource::AgentsDir,
                path: None,
            },
        );
        let mut content = crate::store::skills::ContentIndex::default();
        content.sources.insert(
            cap.id.clone(),
            crate::store::skills::ResolvedContent {
                path: t.path("src/foo"),
                hash: "h".into(),
                source_id: "agents-dir".into(),
            },
        );
        content
            .targets
            .entry("claude-code".into())
            .or_default()
            .insert("foo".into(), crate::store::skills::TargetContent::NoContent);

        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            entries: vec![PlanEntry::new(cap.id.clone(), Action::Install)],
            actual_by_kind: Default::default(),
        };
        let mut ledger = crate::store::provenance::Ledger::default();
        let fake = FakeAdapter::native("claude-code").with_skills_dir(t.path(".claude/skills"));
        let report = fake
            .apply(&slice, &content, &mut ledger)
            .expect("default apply must work");
        assert_eq!(report.delivered, vec!["skill/foo".to_string()]);
        assert!(t.path(".claude/skills/foo/SKILL.md").exists());
        assert_eq!(ledger.find("claude-code", "foo").unwrap().name, "foo");

        // 没有目标目录的那家：不交付、不报错、不写账本。
        let kimi = FakeAdapter::native("kimi-code");
        let mut ledger2 = crate::store::provenance::Ledger::default();
        let report = kimi.apply(&slice, &content, &mut ledger2).expect("kimi 不报错");
        assert!(report.delivered.is_empty());
        assert!(report.failure.is_none());
        assert!(ledger2.entries.is_empty());
    }
```

在 `src/apply/mod.rs` 的测试模块里追加（先建测试模块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Kind, SkillSource, Spec};
    use crate::diff::{Action, HarnessPlan, PlanEntry};
    use crate::store::provenance::{Ledger, LEDGER_VERSION};
    use crate::store::skills::{ContentIndex, ResolvedContent, TargetContent};
    use crate::testutil::TempHome;

    /// 一份「清单要 foo、目标不存在」的最小场景。
    fn scenario(t: &TempHome) -> (HarnessPlan, ContentIndex) {
        t.write("src/foo/SKILL.md", "# foo");
        let id = CapId::new(Kind::Skill, "foo");
        let mut content = ContentIndex::default();
        content.sources.insert(
            id.clone(),
            ResolvedContent {
                path: t.path("src/foo"),
                hash: "ignored-on-purpose".into(),
                source_id: "agents-dir".into(),
            },
        );
        content
            .targets
            .entry("claude-code".into())
            .or_default()
            .insert("foo".into(), TargetContent::NoContent);
        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            entries: vec![PlanEntry::new(id, Action::Install)],
            actual_by_kind: Default::default(),
        };
        (slice, content)
    }

    #[test]
    fn a_delivered_skill_is_recorded_with_the_hash_recomputed_after_delivery() {
        // Review Focus 2（TOCTOU）：账本记的必须是**交付完成后目标上**的哈希。
        // 这里故意喂一个错的源哈希——若实现偷懒存了 sources 里的值，这条会红。
        let t = TempHome::new();
        let (slice, content) = scenario(&t);
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();

        assert_eq!(report.delivered, vec!["skill/foo".to_string()]);
        assert!(report.failure.is_none());
        let entry = ledger.find("claude-code", "foo").expect("must be recorded");
        assert_eq!(entry.source_id, "agents-dir");
        assert_eq!(entry.path, t.path(".claude/skills/foo"));
        assert_ne!(entry.content_hash, "ignored-on-purpose");
        assert_eq!(
            Some(entry.content_hash.clone()),
            hash_dir(&t.path(".claude/skills/foo")).unwrap(),
            "账本哈希必须等于交付后目标内容哈希"
        );
        assert_eq!(ledger.version, LEDGER_VERSION);
        assert!(entry.delivered_at_unix > 0);
    }

    #[test]
    fn a_failing_entry_does_not_stop_the_others() {
        // §9.7 失败隔离：单条失败不中断其余条目，失败要报出来。
        let t = TempHome::new();
        t.write("src/ok/SKILL.md", "# ok");
        let id_bad = CapId::new(Kind::Skill, "bad");
        let id_ok = CapId::new(Kind::Skill, "ok");
        let mut content = ContentIndex::default();
        // 一个不存在的源路径：哈希字段有没有都不影响，deliver 会在 canonicalize 处失败。
        content.sources.insert(
            id_bad.clone(),
            ResolvedContent {
                path: t.path("src/does-not-exist"),
                hash: "h".into(),
                source_id: "agents-dir".into(),
            },
        );
        content.sources.insert(
            id_ok.clone(),
            ResolvedContent {
                path: t.path("src/ok"),
                hash: "h".into(),
                source_id: "agents-dir".into(),
            },
        );
        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            // 按 label 排序：bad 在前，证明后面的 ok 仍然被交付。
            entries: vec![
                PlanEntry::new(id_bad, Action::Install),
                PlanEntry::new(id_ok, Action::Install),
            ],
            actual_by_kind: Default::default(),
        };
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();

        assert_eq!(report.delivered, vec!["skill/ok".to_string()]);
        let failure = report.failure.expect("失败必须被报出来");
        assert!(failure.contains("skill/bad"), "{failure}");
        assert!(t.path(".claude/skills/ok/SKILL.md").exists());
        assert!(ledger.find("claude-code", "bad").is_none(), "失败的条目不记账");
    }

    #[test]
    fn entries_that_are_not_deliverable_are_skipped() {
        let t = TempHome::new();
        let (mut slice, content) = scenario(&t);
        slice.entries = vec![
            PlanEntry::new(
                CapId::new(Kind::Skill, "foo"),
                Action::Undeliverable {
                    reason: "x".into(),
                },
            ),
            PlanEntry::new(CapId::new(Kind::Skill, "foo"), Action::InSync),
            PlanEntry::new(CapId::new(Kind::Skill, "foo"), Action::Unknown),
            PlanEntry::new(CapId::new(Kind::Skill, "foo"), Action::Drift),
        ];
        let mut ledger = Ledger::default();
        let report = deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();
        assert!(report.delivered.is_empty());
        assert!(report.failure.is_none());
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn the_delivered_content_matches_the_source() {
        // 3、4 两步合起来就是 §9.10 的 roundtrip 断言：交付完的内容是清单要的。
        let t = TempHome::new();
        let (slice, content) = scenario(&t);
        let mut ledger = Ledger::default();
        deliver_skills(
            "claude-code",
            Some(&t.path(".claude/skills")),
            &slice,
            &content,
            &mut ledger,
        )
        .unwrap();
        assert_eq!(
            hash_dir(&t.path(".claude/skills/foo")).unwrap(),
            hash_dir(&t.path("src/foo")).unwrap()
        );
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib apply:: adapters::`
Expected: 编译错误 —— `no method named skills_target_dir`、`no method named with_skills_dir`、`cannot find function deliver_skills`、`cannot find type HarnessReport`。预期的 RED。

- [ ] **Step 3: 加 trait 方法**

在 `src/adapters/mod.rs` 的 `HarnessAdapter` trait 里，`supports` 之后加：

```rust
    /// 本家交付 skill 的目标目录。`None` = 这家**没有**用户级 skills 目录
    /// （kimi-code，spec §9.1：它的散装 skill 只能随 plugin bundle 分发）。
    ///
    /// 返回 `None` 不是错误，是这家的事实；「拒绝」的判定在 `supports` 里做，
    /// 这个方法只回答「往哪写」。
    ///
    /// **没有默认实现**：新加一家 harness 时必须显式回答这个问题，
    /// 否则一个默认的 `None` 会让它的 skill 交付静默变成空操作。
    fn skills_target_dir(&self) -> Option<PathBuf>;

    /// 执行层的第 2 步（spec §9.2）：把**已经解析好的**内容交付到本家。
    /// 不碰网络、不解析来源——那些在 `store` 的前置步骤里已经做完了。
    ///
    /// 默认实现覆盖三家在当前范围内的全部差异：它们交付 skill 的方式一模一样
    /// （拷进各自的 skills 目录），差异只体现在 [`skills_target_dir`] 上。
    /// B3 的 MCP 写入会给 claude 覆盖这个方法（它走 shell out `claude mcp add-json`）。
    ///
    /// 这**不是** Plan A 里那种「放个桩只能靠期望 panic 来测」的方法：三条真实路径
    /// （claude/codex 各交付一次、kimi 交付零次）都有测试钉着。
    ///
    /// [`skills_target_dir`]: HarnessAdapter::skills_target_dir
    fn apply(
        &self,
        slice: &HarnessPlan,
        content: &ContentIndex,
        ledger: &mut Ledger,
    ) -> anyhow::Result<HarnessReport> {
        crate::apply::deliver_skills(
            self.name(),
            self.skills_target_dir().as_deref(),
            slice,
            content,
            ledger,
        )
    }
```

模块顶部的 import 补：

```rust
use crate::diff::HarnessPlan;
use crate::store::provenance::Ledger;
use crate::store::skills::ContentIndex;
use std::path::PathBuf;

use crate::apply::HarnessReport;
```

- [ ] **Step 4: 三家各自实现 skills_target_dir**

- `src/adapters/claude_code.rs`：在 `supports` 之前加
  ```rust
      fn skills_target_dir(&self) -> Option<PathBuf> {
          Some(self.home.claude_skills())
      }
  ```
- `src/adapters/codex.rs`：同样，`Some(self.home.codex_skills())`。
- `src/adapters/kimi_code.rs`：加一段带理由的实现
  ```rust
      /// kimi-code 没有用户级 skills 目录（§9.1）：它的散装 skill 只能随 plugin
      /// bundle 落地。`supports` 已经对散装 `Kind::Skill` 返回 `Unsupported`，
      /// 这里返回 `None` 是与之一致的事实陈述，不是第二处独立的拒绝判定。
      fn skills_target_dir(&self) -> Option<PathBuf> {
          None
      }
  ```

各文件顶部的 `use` 里补 `PathBuf`（若已有则不动）。

- [ ] **Step 5: 给 FakeAdapter 加 skills_dir**

在 `src/testutil.rs` 的 `FakeAdapter` 加字段与 builder：

```rust
    /// `None` = 这家没有用户级 skills 目录（kimi 的真实形态）。
    pub skills_dir: Option<PathBuf>,
```

`native()` 里初始化 `skills_dir: None`，并加：

```rust
    pub fn with_skills_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.skills_dir = Some(dir.into());
        self
    }
```

`impl HarnessAdapter for FakeAdapter` 里加：

```rust
    fn skills_target_dir(&self) -> Option<PathBuf> {
        self.skills_dir.clone()
    }
```

（`apply` 用 trait 的默认实现，不覆盖——测试替身与生产路径走同一段代码。）

- [ ] **Step 6: 实现 deliver_skills 与 HarnessReport**

在 `src/apply/mod.rs` 的生产部分加：

```rust
use crate::capability::CapId;
use crate::diff::{Action, HarnessPlan};
use crate::store::hash::hash_dir;
use crate::store::provenance::{Ledger, LedgerEntry};
use crate::store::skills::ContentIndex;
use anyhow::Context as _;
use std::path::Path;

/// 一家 harness 的交付结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessReport {
    pub harness: String,
    /// 本次真正交付的条目 label（如 `skill/foo`），按 label 排好序。
    pub delivered: Vec<String>,
    /// `Some` = 这家只做成了一部分（§9.7 失败隔离）。没有回滚，重跑就是修复路径。
    pub failure: Option<String>,
}

/// 一家 harness 的 skill 交付（§9.2 第 2 步）。
///
/// `target_dir` 为 `None`（这家没有用户级 skills 目录）时返回一份空报告——
/// 那不是失败，是这家的事实。
///
/// 失败隔离（§9.7）：单条失败**不中断**这一家剩下的条目，也不影响别家；
/// 失败的条目**不记账**（没交付成功就不该声称交付过）。
/// 返回 `Err` 只留给「这家压根没法开始」的情况（目标目录建不出来）。
pub fn deliver_skills(
    harness: &str,
    target_dir: Option<&Path>,
    slice: &HarnessPlan,
    content: &ContentIndex,
    ledger: &mut Ledger,
) -> anyhow::Result<HarnessReport> {
    let mut report = HarnessReport {
        harness: harness.to_string(),
        delivered: Vec::new(),
        failure: None,
    };
    let Some(target_dir) = target_dir else {
        return Ok(report);
    };

    let mut failures: Vec<String> = Vec::new();
    for e in &slice.entries {
        // 只有 install / update 是「把内容放过去」。其余动作都是「不该放」
        // ——冲突与不可交付由上层拦，in-sync / unknown / drift 没有内容要放。
        if !matches!(e.action, Action::Install | Action::Update { .. }) {
            continue;
        }
        // 源没解析出来（git / bundle / 源不存在）→ 内容层给不出路径，交给 Plan A 的
        // 判定去报告，这里不猜路径也不失败。
        let Some(src) = content.sources.get(&e.id) else {
            continue;
        };
        let target = target_dir.join(&e.id.name);
        match deliver_skill(&target, &src.path) {
            Ok(()) => {
                report.delivered.push(e.id.label());
                // §9.3：账本记的是**交付完成后目标上**的哈希，不是解析源时的值。
                // 用后者的话，「源在解析与交付之间被改动」会让账本与盘上内容不符，
                // 下一次 diff 报出一个没人能解释的 update。
                let content_hash = hash_dir(&target)
                    .with_context(|| format!("cannot hash {}", target.display()))?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{} 刚交付完却读不出内容——交付与账本会因此不一致",
                            target.display()
                        )
                    })?;
                ledger.record(LedgerEntry {
                    harness: harness.to_string(),
                    kind: e.id.kind,
                    name: e.id.name.clone(),
                    path: target,
                    source_id: src.source_id.clone(),
                    content_hash,
                    delivered_at_unix: now_unix(),
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

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
```

`CapId` 与 `hash_dir` 若在 `mod.rs` 里没被直接用到（`CapId` 只在类型路径里出现），按 clippy 的 unused import 报错**删掉多余的 import**。

- [ ] **Step 7: 跑测试确认它通过**

Run: `cargo test --lib apply:: adapters::`
Expected: 全绿（apply 4 条 + adapters 3 条新增）。

- [ ] **Step 8: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: lib 358 passed / 集成 5 passed / 1 ignored / 0 failed；clippy 零警告。

- [ ] **Step 9: Commit**

```bash
git add src/apply/mod.rs src/adapters/ src/testutil.rs
git commit -m "feat(adapters): deliver skills through one shared copy path"
```

---

## Task 9: apply::execute —— 逐 harness 执行与失败隔离

**Files:**
- Modify: `src/apply/mod.rs`
- Test: `src/apply/mod.rs` 的测试模块

**Interfaces:**
- Consumes: `HarnessAdapter::apply`（Task 8）、`Plan::conflicts()`（Task 5）
- Produces:
  - `pub struct ApplyReport { pub harnesses: Vec<HarnessReport> }`，含 `pub fn partial(&self) -> Vec<(&str, &str)>`、`pub fn delivered_total(&self) -> usize`
  - `pub fn execute(plan: &Plan, content: &ContentIndex, ledger: &mut Ledger, home: &Home) -> anyhow::Result<ApplyReport>`

- [ ] **Step 1: 写失败的测试**

在 `src/apply/mod.rs` 的测试模块里追加：

```rust
    use crate::adapters::CLAUDE_CODE;
    use crate::diff::Plan;

    /// 在一个 TempHome 上真的跑一次 execute：claude 装了、codex 装了、kimi 没装。
    fn end_to_end(t: &TempHome) -> (Plan, ContentIndex) {
        t.write("src/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");
        let manifest = crate::manifest::Manifest::parse("version: 1\n").unwrap();
        let desired = vec![crate::capability::Capability {
            id: CapId::new(Kind::Skill, "foo"),
            origin: crate::capability::Origin::Manifest,
            spec: Spec::Skill {
                source: SkillSource::Local {
                    path: t.path("src/foo"),
                },
                path: None,
            },
            targets: crate::capability::Targets::All,
        }];
        let registry = crate::adapters::registry(&t.home);
        let states: Vec<crate::diff::HarnessState> = registry
            .iter()
            .map(|a| crate::diff::HarnessState {
                name: a.name().to_string(),
                detected: a.detect(),
                actual: a.read_state().expect("read_state"),
            })
            .collect();
        let content = ContentIndex {
            sources: crate::store::skills::resolve_sources(&desired, &t.home).unwrap(),
            targets: Default::default(),
        };
        let inputs: Vec<crate::diff::HarnessInput> = states
            .iter()
            .zip(registry.iter())
            .map(|(state, adapter)| crate::diff::HarnessInput {
                state,
                adapter: &**adapter,
            })
            .collect();
        let plan = crate::diff::compute(
            &desired,
            &inputs,
            &crate::lower::DegradeTable::new(&manifest),
            &content,
            &Ledger::default(),
        );
        (plan, content)
    }

    #[test]
    fn execute_delivers_to_both_harnesses_that_have_a_skills_directory() {
        let t = TempHome::new();
        let (plan, content) = end_to_end(&t);
        let mut ledger = Ledger::default();
        let report = execute(&plan, &content, &mut ledger, &t.home).unwrap();

        let names: Vec<&str> = report
            .harnesses
            .iter()
            .map(|h| h.harness.as_str())
            .collect();
        assert_eq!(
            names,
            vec![CLAUDE_CODE, "codex"],
            "kimi 没装 → 不进报告；未安装的 harness 不该被跑"
        );
        assert!(report.partial().is_empty(), "没有失败就不该有 partial");
        assert_eq!(report.delivered_total(), 2);
        assert!(t.path(".claude/skills/foo/SKILL.md").exists());
        assert!(t.path(".codex/skills/foo/SKILL.md").exists());
        assert!(!t.path(".kimi-code/skills").exists(), "kimi 没有这个目录");
        assert!(ledger.find(CLAUDE_CODE, "foo").is_some());
        assert!(ledger.find("codex", "foo").is_some());
    }

    #[test]
    fn execute_refuses_the_whole_plan_when_there_is_a_conflict() {
        // §9.3 不变式二 + 「停下」的字面意思：一个字节都不许写，
        // 包括别的 harness 那一份。
        let t = TempHome::new();
        let (mut plan, content) = end_to_end(&t);
        // 给 claude 造一个冲突，给 codex 造一个正常的 install。
        let conflict = PlanEntry::new(
            CapId::new(Kind::Skill, "taken"),
            Action::Conflict {
                reason: "目标已存在同名条目且不是 henv 交付的".into(),
            },
        );
        plan.harnesses[0].entries.push(conflict);
        let mut ledger = Ledger::default();
        let err = format!(
            "{:#}",
            execute(&plan, &content, &mut ledger, &t.home).unwrap_err()
        );
        assert!(err.contains("skill/taken"), "{err}");
        assert!(
            !t.path(".claude/skills/foo").exists() && !t.path(".codex/skills/foo").exists(),
            "冲突必须阻断全部交付，而不是做到一半停下"
        );
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn one_harnesss_failure_leaves_the_other_alone() {
        // §9.7 失败隔离跨 harness 也成立。
        let t = TempHome::new();
        let (plan, mut content) = end_to_end(&t);
        // 把源换成不存在的路径：两家都会失败，但都只报 partial，不整体报错。
        for src in content.sources.values_mut() {
            src.path = t.path("src/gone");
        }
        let mut ledger = Ledger::default();
        let report = execute(&plan, &content, &mut ledger, &t.home).unwrap();
        assert_eq!(report.harnesses.len(), 2);
        assert_eq!(report.partial().len(), 2, "两家各自报自己的失败");
        assert_eq!(report.delivered_total(), 0);
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn execute_skips_an_undetected_harness() {
        // 没装的那家不跑：它的 skills 目录不存在，跑它等于无端建目录
        // ——那不是「让目标变成清单的样子」，是凭空造出一家的配置树。
        let t = TempHome::new();
        let (mut plan, content) = end_to_end(&t);
        plan.harnesses[0].detected = false;
        let mut ledger = Ledger::default();
        let report = execute(&plan, &content, &mut ledger, &t.home).unwrap();
        assert_eq!(report.harnesses.len(), 1);
        assert!(!t.path(".claude/skills/foo").exists());
        assert!(t.path(".codex/skills/foo").exists(), "另一家照常交付");
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib apply::tests::execute`
Expected: 编译错误 `cannot find function execute in this scope`。预期的 RED。

- [ ] **Step 3: 实现 execute**

```rust
/// 全部 harness 的交付结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyReport {
    pub harnesses: Vec<HarnessReport>,
}

impl ApplyReport {
    /// 只做成一半的 harness：名字 + 失败原因（§9.7 的 `partial`）。
    pub fn partial(&self) -> Vec<(&str, &str)> {
        self.harnesses
            .iter()
            .filter_map(|h| h.failure.as_deref().map(|f| (h.harness.as_str(), f)))
            .collect()
    }

    pub fn delivered_total(&self) -> usize {
        self.harnesses.iter().map(|h| h.delivered.len()).sum()
    }
}

/// 执行计划（§9.2 第 2 步）。**全项目唯一会往 `Home` 下写东西的函数。**
///
/// 冲突检查在**任何**落盘之前做完：§9.3 的「停下报错」如果做成「做到一半停下」，
/// 另一半就已经被改动了，而不变式二承诺的是「绝不覆盖」。
///
/// harness 级的失败进 `HarnessReport.failure` 而不上抛（失败隔离，§9.7）；
/// 返回 `Err` 只留给冲突与「压根没法开始」。没有回滚命令——重跑就是修复路径。
pub fn execute(
    plan: &Plan,
    content: &ContentIndex,
    ledger: &mut Ledger,
    home: &Home,
) -> anyhow::Result<ApplyReport> {
    let conflicts = plan.conflicts();
    anyhow::ensure!(
        conflicts.is_empty(),
        "refusing to apply, {} name conflict(s) found:\n{}",
        conflicts.len(),
        conflicts
            .iter()
            .map(|e| format!("  {} ({})", e.id.label(), match &e.action {
                Action::Conflict { reason } => reason.as_str(),
                _ => "",
            }))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let registry = crate::adapters::registry(home);
    let mut report = ApplyReport::default();
    for hp in &plan.harnesses {
        // 未安装的那家不跑：它的配置树不存在，跑它等于凭空造出一家的目录。
        if !hp.detected {
            continue;
        }
        let Some(adapter) = crate::adapters::lookup(&registry, &hp.harness) else {
            continue;
        };
        match adapter.apply(hp, content, ledger) {
            Ok(r) => report.harnesses.push(r),
            // adapter 级失败（连目标目录都建不出来）——也走 partial，不上抛。
            Err(e) => report.harnesses.push(HarnessReport {
                harness: hp.harness.clone(),
                delivered: Vec::new(),
                failure: Some(format!("{e:#}")),
            }),
        }
    }
    Ok(report)
}
```

`mod.rs` 顶部补 `use crate::home::Home;` 与 `use crate::diff::Plan;`（若未在作用域内）。

- [ ] **Step 4: 跑测试确认它通过**

Run: `cargo test --lib apply::`
Expected: `8 passed`。

- [ ] **Step 5: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: lib 362 passed / 集成 5 passed / 1 ignored / 0 failed；clippy 零警告。

- [ ] **Step 6: Commit**

```bash
git add src/apply/mod.rs
git commit -m "feat(apply): execute per harness with failures isolated"
```

---

## Task 10: henv apply CLI 与端到端验收

**Files:**
- Modify: `src/lib.rs`（`Confirm`、`run_apply`、`run_diff` 接线、`Planned` helper）、`src/main.rs`、`src/testutil.rs`（测试替身）
- Test: `src/lib.rs` 的测试模块

**Interfaces:**
- Consumes: 前面全部
- Produces:
  - `pub trait Confirm { fn ask(&mut self, prompt: &str) -> bool }`
  - `pub struct AssumeYes;` `pub struct StdinConfirm;`
  - `pub fn run_apply(manifest_path: &Path, home: &Home, out: &mut dyn Write, confirm: &mut dyn Confirm) -> anyhow::Result<()>`
  - CLI：`henv apply [-m <manifest>] [--yes]`

**为什么要 `Confirm` 这个 trait：** §9.8 的交互边界是「henv 什么都不问，除了 apply 的 y/N 确认」。它抽成 trait 才能让 `run_apply` 在测试里不读真的 stdin。

- [ ] **Step 1: 写失败的测试**

在 `src/lib.rs` 的测试模块里追加：

```rust
    use crate::testutil::{AssumeYes, RefuseAll};

    /// 在 TempHome 上跑一次 run_apply，返回渲染文本。
    fn apply(t: &TempHome, manifest: &str, confirm: &mut dyn Confirm) -> anyhow::Result<String> {
        t.write("env.yaml", manifest);
        let mut out = Vec::new();
        run_apply(&t.path("env.yaml"), &t.home, &mut out, confirm)?;
        Ok(String::from_utf8(out).expect("utf8"))
    }

    /// 最省事的「一个 agents-dir skill」清单。
    fn one_skill_manifest() -> &'static str {
        "version: 1\nskills:\n  foo:\n    source: agents-dir\n"
    }

    #[test]
    fn apply_delivers_real_copies_and_records_them() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).expect("apply");
        assert!(out.contains("~ update") || out.contains("+ install"), "{out}");

        for dir in [".claude/skills/foo", ".codex/skills/foo"] {
            let p = t.path(dir);
            let meta = std::fs::symlink_metadata(&p).expect("delivered");
            assert!(
                meta.file_type().is_dir() && !meta.file_type().is_symlink(),
                "{dir} 必须是真拷贝"
            );
            assert_eq!(std::fs::read_to_string(p.join("SKILL.md")).unwrap(), "# foo");
        }

        // §9.3：账本落在任何内容目录之外。
        let ledger = crate::store::provenance::Ledger::load(&t.home).unwrap();
        assert!(ledger.find(CLAUDE_CODE, "foo").is_some());
        assert!(ledger.find(CODEX, "foo").is_some());
        assert_eq!(ledger.find(CLAUDE_CODE, "foo").unwrap().source_id, "agents-dir");
        assert!(t.path(".local/state/henv/provenance.json").exists());
    }

    #[test]
    fn a_second_apply_changes_nothing() {
        // §9.10 的幂等验收：重跑是空操作，且报告说得出这句话。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");
        apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();

        let before = std::fs::read_to_string(t.path(".local/state/henv/provenance.json")).unwrap();
        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        let after = std::fs::read_to_string(t.path(".local/state/henv/provenance.json")).unwrap();

        assert!(out.contains("✓ in-sync"), "{out}");
        assert!(
            out.ends_with("Plan: 0 conflict, 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift, 0 unknown\n"),
            "{out}"
        );
        assert_eq!(before, after, "没有交付就不该改账本");
    }

    #[test]
    fn a_form_different_but_content_equal_target_is_left_alone() {
        // §9.3 + §9.10：本机那 14 条软链的形态。内容相同 → in-sync，
        // 而且**形态不许被迁移**——apply 不能把它"修"成拷贝。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../.agents/skills/foo");
        t.mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(
            !out.contains("~ update") && !out.contains("! conflict"),
            "内容相同就不该有任何动作：{out}"
        );
        let link = t.path(".claude/skills/foo");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "内容相同就不该动它——形态无关的判定单位正是为了这个"
        );
        // codex 那边没有条目 → 照常交付真拷贝。
        assert!(std::fs::symlink_metadata(t.path(".codex/skills/foo"))
            .unwrap()
            .file_type()
            .is_dir());
    }

    #[test]
    fn a_dangling_link_is_healed_into_a_real_copy() {
        // §2.2 第 1 条 + §9.3 的自愈：断链没有内容，apply 把它换成真拷贝。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../.agents/skills/gone");
        t.mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(out.contains("(断链 →"), "{out}");
        let target = t.path(".claude/skills/foo");
        let meta = std::fs::symlink_metadata(&target).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(target.join("SKILL.md")).unwrap(), "# foo");
    }

    #[test]
    fn a_foreign_copy_with_different_content_stops_the_run() {
        // §9.3 不变式二：不是 henv 放的东西，绝不覆盖——而且整次 apply 一个字节都不写。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# mine")
            .write(".claude/skills/foo/SKILL.md", "# theirs")
            .mkdir(".codex");

        let err = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap_err();
        assert!(format!("{err:#}").contains("skill/foo"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
            "# theirs",
            "别人的东西一个字节都不许动"
        );
        assert!(
            !t.path(".codex/skills/foo").exists(),
            "冲突阻断全部交付，包括别家"
        );
        assert!(!t.path(".local/state/henv/provenance.json").exists());
    }

    #[test]
    fn refusing_the_confirmation_writes_nothing() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".claude").mkdir(".codex");

        let out = apply(&t, one_skill_manifest(), &mut RefuseAll).unwrap();
        assert!(out.contains("cancelled"), "{out}");
        assert!(!t.path(".claude/skills/foo").exists());
        assert!(!t.path(".codex/skills/foo").exists());
        assert!(!t.path(".local/state/henv/provenance.json").exists());
    }

    #[test]
    fn a_hand_modified_copy_is_overwritten_and_reported() {
        // §9.3：账本记着它是我们放的，盘上哈希与账本不符 → 覆盖，但要说出来。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# v2");
        t.mkdir(".claude").mkdir(".codex");
        apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();

        t.write(".claude/skills/foo/SKILL.md", "# hand edited");
        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(out.contains("[副本被手改过]"), "{out}");
        assert_eq!(
            std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
            "# v2",
            "被手改的副本要恢复成清单要的内容"
        );
    }

    #[test]
    fn run_diff_still_reports_the_unknown_for_an_unresolvable_source() {
        // 接线之后 diff 的既有行为不变：local 源指向不存在的目录 → 落回 Plan A。
        let t = TempHome::new();
        t.mkdir(".claude");
        let out = diff(&t, "version: 1\nskills:\n  foo:\n    source: local\n    path: /tmp/definitely-not-here\n");
        assert!(out.contains("? unknown  skill/foo"), "{out}");
    }

    #[test]
    fn kimi_reports_a_loose_skill_as_undeliverable_and_apply_says_so() {
        // §9.1：kimi 的散装 skill 交付不了，报告要说清楚，且 apply 不许因此失败。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        t.mkdir(".kimi-code").mkdir(".claude").mkdir(".codex");
        let out = apply(&t, one_skill_manifest(), &mut AssumeYes).unwrap();
        assert!(out.contains("! undeliverable  skill/foo"), "{out}");
        assert!(out.contains("kimi-code 无用户级 skills 目录"), "{out}");
        assert!(!t.path(".kimi-code/skills").exists());
    }
```

在 `src/testutil.rs` 里加两个替身：

```rust
/// `Confirm` 的测试替身：恒同意 / 恒拒绝。
pub struct AssumeYes;
pub struct RefuseAll;

impl crate::Confirm for AssumeYes {
    fn ask(&mut self, _prompt: &str) -> bool {
        true
    }
}

impl crate::Confirm for RefuseAll {
    fn ask(&mut self, _prompt: &str) -> bool {
        false
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test --lib tests::apply_`
Expected: 编译错误 `cannot find function run_apply`、`cannot find trait Confirm`。预期的 RED。

- [ ] **Step 3: 加 Confirm 与运行路径**

在 `src/lib.rs` 的 `run_diff` 之前插入：

```rust
/// 交互闸门（§9.8）：apply 是 henv 唯一会问人一句话的地方。
/// 抽成 trait 是为了让 `run_apply` 能被测——否则测一次 apply 就要喂真 stdin。
pub trait Confirm {
    /// `prompt` 已含问句本身。返回 true 才继续。
    fn ask(&mut self, prompt: &str) -> bool;
}

/// `--yes`：不提问，恒同意（§9.8）。
pub struct AssumeYes;

impl Confirm for AssumeYes {
    fn ask(&mut self, _prompt: &str) -> bool {
        true
    }
}

/// 生产默认：问题写 stderr，读 stdin 一行，`y`/`Y` 开头算同意。
/// 默认是「不同意」——`--yes` 之外的任何输入都必须指向「什么都没发生」。
pub struct StdinConfirm;

impl Confirm for StdinConfirm {
    fn ask(&mut self, prompt: &str) -> bool {
        use std::io::BufRead as _;
        eprint!("{prompt} [y/N] ");
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        line.trim_start().starts_with(['y', 'Y'])
    }
}
```

把 `run_diff` 里「算计划」的那一段抽成共用函数，并接上内容层：

```rust
/// 一次规划的全部产物。`diff` 与 `apply` 共用**同一个**规划函数——
/// 两个规划器能互相不一致，正是这个项目存在要消灭的「假同步」。
struct Planned {
    plan: diff::Plan,
    content: store::skills::ContentIndex,
    ledger: store::provenance::Ledger,
}

fn plan_for(manifest_path: &Path, home: &Home) -> anyhow::Result<Planned> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("cannot read manifest: {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", manifest_path.display()))?;

    let desired = lower::lower(&manifest);
    let degrade = lower::DegradeTable::new(&manifest);

    // §9.2 第 1 步：全局来源解析。唯一在 compute 之前的 I/O。
    let sources = store::skills::resolve_sources(&desired, home)?;

    let registry = adapters::registry(home);
    let mut pairs: Vec<(diff::HarnessState, &dyn HarnessAdapter)> = Vec::new();
    for a in &registry {
        if !is_enabled(&manifest, a.name()) {
            continue;
        }
        let detected = a.detect();
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

    // 目标侧内容扫描：逐家算，按 harness 名收进一张表。
    let mut targets: std::collections::BTreeMap<String, _> = Default::default();
    for (state, _) in &pairs {
        targets.insert(state.name.clone(), store::skills::target_content(&state.actual)?);
    }

    let inputs: Vec<diff::HarnessInput> = pairs
        .iter()
        .map(|(state, adapter)| diff::HarnessInput {
            state,
            adapter: *adapter,
        })
        .collect();

    let content = store::skills::ContentIndex { sources, targets };
    let ledger = store::provenance::Ledger::load(home)?;
    let plan = diff::compute(&desired, &inputs, &degrade, &content, &ledger);
    Ok(Planned {
        plan,
        content,
        ledger,
    })
}
```

`run_diff` 变成：

```rust
pub fn run_diff(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<()> {
    let planned = plan_for(manifest_path, home)?;
    write!(out, "{}", render::render(&planned.plan))?;
    Ok(())
}
```

`run_apply` 加在后面：

```rust
/// 交付：把清单要的 skill 真拷贝到各家，并把「放了什么」记进账本（§9.2/§9.3）。
///
/// 顺序是刻意的：先算计划 → 冲突检查 → 渲染 → 问人 → 才落盘。
/// 冲突在问人之前就返回错误——让人对着一个不可能执行的计划回答 y 没有意义。
///
/// `apply` 是 henv 唯一的写路径；它不做计划以外的事（§9.1 的范围）。
pub fn run_apply(
    manifest_path: &Path,
    home: &Home,
    out: &mut dyn Write,
    confirm: &mut dyn Confirm,
) -> anyhow::Result<()> {
    let mut planned = plan_for(manifest_path, home)?;
    write!(out, "{}", render::render(&planned.plan))?;

    let conflicts = planned.plan.conflicts();
    anyhow::ensure!(
        conflicts.is_empty(),
        "refusing to apply, {} name conflict(s) found:\n{}",
        conflicts.len(),
        conflicts
            .iter()
            .map(|e| {
                format!(
                    "  {} ({})",
                    e.id.label(),
                    match &e.action {
                        diff::Action::Conflict { reason } => reason.as_str(),
                        _ => "",
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    // 「无事可做」的判定**不在这里**：`execute` 遍历计划，对没有 install/update
    // 条目的 harness 返回空报告，`delivered_total() == 0` 时既不写账本也不打印。
    // 这里再判一次就多出一份会与它漂移的判定。
    //
    // 只在已安装的家里问：一个都没装的话，问「要不要向 (空) 交付」没有意义。
    let pending: Vec<&str> = planned
        .plan
        .harnesses
        .iter()
        .filter(|h| h.detected)
        .map(|h| h.harness.as_str())
        .collect();
    if pending.is_empty() {
        let _ = writeln!(out, "没有已安装的 harness，无事可做。");
        return Ok(());
    }

    if !confirm.ask(&format!("将向 {} 交付。继续？", pending.join(", "))) {
        let _ = writeln!(out, "cancelled");
        return Ok(());
    }

    let report = apply::execute(&planned.plan, &planned.content, &mut planned.ledger, home)?;
    for h in &report.harnesses {
        for id in &h.delivered {
            let _ = writeln!(out, "{:<13} delivered {}", h.harness, id);
        }
    }
    // 账本只在真的交付过东西之后写：一次空跑不该改动 henv 的状态文件。
    if report.delivered_total() > 0 {
        planned.ledger.save(home)?;
    }
    let partial = report.partial();
    if !partial.is_empty() {
        let _ = writeln!(out, "partial:");
        for (harness, why) in partial {
            let _ = writeln!(out, "  {harness}: {why}");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: 加 CLI 子命令**

`src/main.rs` 的 `Command` enum 加：

```rust
#[derive(Subcommand)]
enum Command {
    /// 只读比对：清单为期望，本机为现状。
    Diff {
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
    },
    /// 交付：把清单里的能力真拷贝到各家，并记录在账本里（会写文件）。
    Apply {
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
        /// 不问确认直接执行（用于脚本与 CI）。
        #[arg(long)]
        yes: bool,
    },
}
```

`match` 里加：

```rust
        Command::Apply { manifest, yes } => {
            let mut confirm: Box<dyn Confirm> = if yes {
                Box::new(AssumeYes)
            } else {
                Box::new(StdinConfirm)
            };
            henv::run_apply(&manifest, &home, &mut out, confirm.as_mut())
        }
```

`main.rs` 的 `long_about` 里那句「Plan A 只读」要改：现在它只说对了一半，一个会写文件的子命令已经存在。**必须**改成明确说清 `diff` 只读、`apply` 会写。

- [ ] **Step 5: 跑测试确认它通过**

Run: `cargo test --lib tests::`
Expected: 全绿（10 条新增）。

- [ ] **Step 6: 把 design.md 的 §9 状态标注更新**

`docs/design.md` §9 开头那句「状态：已与用户逐节确认」后面加一行，写明 **B1 已实现的部分**（`agents-dir`/`local` 两种源的 skill 交付、账本、只读守卫白名单、`henv apply`）与**尚未实现的部分**（B2 的 git 源与 `skills list`、B3 的 MCP 写入、bundle 拆解、`degrade:` 生效、`--prune`）。**只加这一段**，不动 §9 的其余文字。

- [ ] **Step 7: 跑全量套件 + clippy**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: lib 372 passed / 集成 5 passed / 1 ignored / 0 failed；clippy 零警告。

- [ ] **Step 8: 手工在真实 home 上验一次形态（只读观察，不改 `~`）**

Run: `cargo run --quiet -- diff -m examples/env.yaml | tail -5`
Expected: 命令正常输出，末尾 footer 含 `0 conflict`。**不要**在真实 home 上跑 `apply`——那是这个项目第一次真的改用户配置树，留给用户自己决定时机。

- [ ] **Step 9: Commit**

```bash
git add src/lib.rs src/main.rs src/testutil.rs docs/design.md
git commit -m "feat(cli): add henv apply with a confirmation gate"
```

---

## Self-Review

**1. Spec 覆盖**

| Spec | 落在哪个 Task |
|---|---|
| §9.1 范围：agents-dir/local 的 skill 交付 | Task 4（解析）、Task 7/8（交付）、Task 10（验收） |
| §9.1 范围：kimi 散装 skill 保持 undeliverable 且报告说清 | Task 10（`kimi_reports_a_loose_skill_as_undeliverable_and_apply_says_so`） |
| §9.1 **不做**：plugin 原生安装 / scan / doctor / bundle 拆解 / MCP 写入 / --prune | 计划外（B2/B3），Task 10 Step 6 在 design.md 里显式标注 |
| §9.2 分层：源解析 → 纯函数规划 → 执行 | Task 4（store::skills）、Task 5（compute 新签名）、Task 9（execute） |
| §9.2 `plan_apply` 的名字 | **裁定**：不新起 `plan_apply`，扩展现有 `diff::compute`（见下方裁定 1） |
| §9.3 不变式一：绝不造软链 | Task 7（`delivery_is_a_real_copy_not_a_link`、`a_source_that_is_itself_a_link_delivers_real_content`） |
| §9.3 不变式二：只碰账本条目、冲突停下 | Task 5（`differing_content_outside_the_ledger_is_a_conflict`）、Task 9（`execute_refuses_the_whole_plan_when_there_is_a_conflict`）、Task 10 |
| §9.3 账本位置与内容 | Task 3（`state_paths_live_outside_any_content_directory`、`a_saved_ledger_round_trips`） |
| §9.3 判定单位是内容等值 | Task 2（`a_link_to_a_tree_hashes_as_that_tree`）、Task 4（`a_link_to_the_source_hashes_the_same_as_the_source`）、Task 10（`a_form_different_but_content_equal_target_is_left_alone`） |
| §9.3 断链自愈成真拷贝 | Task 5（`a_dangling_link_is_an_update_whatever_the_ledger_says`）、Task 10（`a_dangling_link_is_healed_into_a_real_copy`） |
| §9.4 staleness（B1 部分） | **部分**：账本记交付后哈希（Task 8）；git 源与 bundle 的 staleness 是 B2/B3 |
| §9.7 事务：temp + rename、skills 不落 .bak | Task 7 |
| §9.7 失败隔离 / partial / 重跑即修复 | Task 8（`a_failing_entry_does_not_stop_the_others`）、Task 9（`one_harnesss_failure_leaves_the_other_alone`） |
| §9.8 CLI 形状 | Task 10（`henv apply [-m] [--yes]`）；`skills list` 是 B2 |
| §9.8 交互边界：只问 apply 的确认 | Task 10（`Confirm`），且 `reports` 全走 `out` 不交互 |
| §9.9 守卫白名单 + `Command::new(` 保留 | Task 1、Task 7 Step 5 |
| §9.10 验收测试 | Task 7（真拷贝、幂等、整体替换）、Task 8（roundtrip）、Task 10（端到端 9 条） |
| §9.12 选型：无 git2、无 npx | Task 1-10 全程未引；B1 不 shell out |

**缺口（有意留白，非计划缺陷）：** §9.4 的 git/bundle staleness、§9.6 的 `toml_edit` 子树合并、§9.7 的配置文件 `.bak` 与 `claude mcp add-json` shell out、§9.8 的 `--prune` 与 `skills list` —— 全部属 B2/B3，§9.1 明确划在本次范围外。

**2. Placeholder 扫描**

无 `TBD`/`TODO`/「稍后实现」/「类似 Task N」。每个代码步骤都给了可照抄的代码。Task 10 Step 3 里那段「实现时简化成」是**显式指令**（删掉两个过早返回的分支），不是占位符——它给出了删除的理由与替换后的形状。

**3. 类型一致性**

- `ContentIndex { sources, targets }`：Task 4 定义，Task 5/8/9/10 消费，字段名一致。
- `TargetContent::{Has, NoContent}`：Task 4 定义，Task 5 匹配，两处拼写一致。
- `ResolvedContent { path, hash, source_id }`：Task 4 定义，Task 8 读 `path`/`source_id`。
- `deliver_skills(harness, target_dir, slice, content, ledger)`：Task 8 定义，Task 8 的 trait 默认实现按同一顺序传参。
- `PlanEntry::new(id, action)` / `.with_note(Option<String>)`：Task 5 定义，Task 6/8/9 使用。
- `compute(desired, inputs, degrade, content, ledger)`：Task 5 改签名，Task 5/10 调用点一致。
- `HarnessReport { harness, delivered, failure }`：Task 8 定义，Task 9 构造，字段名一致。
- `Ledger::find/record/load/save`：Task 3 定义，Task 5/8/10 使用。

**4. Review Focus**

五条都在下方对应任务的步骤里有测试：1 → Task 7 `leftovers_from_a_half_finished_run_are_cleaned_and_replaced`；2 → Task 8 `a_delivered_skill_is_recorded_with_the_hash_recomputed_after_delivery`；3 → Task 7 `awkward_names_are_delivered_verbatim`；4 → Task 5 `conflicts_sort_first_and_are_counted` + Task 10 的冲突端到端；5 → Task 7 `delivery_creates_a_missing_target_directory`。

---

## 需要用户裁定的四处

执行前请逐条确认。每一条都会在执行时写进 ledger 的 `Ruling:` 行：

1. **不新起 `plan_apply`，扩展 `diff::compute`。** §9.2 把规划器命名为 `plan_apply`，但项目里已经有一个做同一件事的纯函数。并存两个规划器意味着它们可以互相不一致——那正是这个项目存在要消灭的「假同步」。做法：`compute` 加 `content: &ContentIndex` 与 `ledger: &Ledger` 两个入参，两个 `Default` 空表逐字节复现 Plan A 行为。代价若要返工：把内容层从 `compute` 里摘出去另起一个函数，是一次机械搬移。

2. **新增依赖 `sha2`。** 内容等值的判定必须有防碰撞的指纹；非加密哈希的一次碰撞会产生静默假同步。代价若要返工：换实现是改一个函数体。

3. **新增 `Action::Conflict`。** §9.3 说冲突要「在报告里列出来供人决定」，但今天 `diff` 的动作集合里没有这一格——冲突会被折成 `update`，于是 `diff`（只读）会暗示一件 `apply` 会拒绝的事。代价若要返工：删一个变体与一个渲染臂。

4. **不变式二压过 §9.3 对断链的那句承诺，但只压一半。** §9.3 说断链「会被修成真拷贝」，同时又说「同名冲突时停下报错，绝不覆盖」。裁定：**断链没有内容，覆盖它不破坏任何东西 → 照常 healed**；**内容不同的实目录/实文件不是我们放的 → 冲突停下**。这样两句话都成立，且没有一处会丢数据。

---

## Execution Handoff

计划已存至 `docs/superpowers/plans/2026-09-25-henv-plan-b1-apply-skeleton.md`。
