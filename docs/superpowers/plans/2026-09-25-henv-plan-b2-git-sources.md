# Plan B2：git 源与 `henv skills list` 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `henv` 能真的把清单里 `source: git` 的 skill 交付到 harness 目录，并给出 `henv skills list --repo <url>` 这个「先看一眼仓库里有什么」的入口。

**Architecture:** 内容解析（§9.2 的「源解析，全局一次」）是唯一在 `compute` 之前做 I/O 的地方，B2 把 git 源接进这一层：`store/git.rs` 用 `git` 子进程把仓库同步到 `~/.cache/henv/git/<repo-hash>/repo`，`store/skills.rs` 把该仓库的一个子目录解析成 `ResolvedContent`，其后的判定、渲染、交付全部沿用 B1 已有路径（内容哈希比字节、账本判归属、temp-dir-then-rename 交付）。新增的一条纪律是**判定模式**：`diff` 只读缓存不联网（拿不到内容就诚实报「判不了」），`apply` 才联网 fetch，且计划里绝不出现「说要装、内容却是空的」条目。

**Tech Stack:** Rust 2024 edition / rust-version 1.85；clap 4.6 derive、serde 1、serde_yaml_ng 0.10、serde_json、toml 1.1、anyhow、sha2 0.10；dev-deps `insta` 1.48、`tempfile` 3。**B2 不新增任何依赖**（§9.12：git 一律 shell out 系统 `git`，不引 `git2`）。

**Spec:** `docs/design.md` §9（尤其 §9.2 源解析、§9.3 两条不变式、§9.4 staleness、§9.7 失败隔离、§9.8 CLI 面、§9.9 只读守卫的去向、§9.10 测试策略、§9.12 git 与依赖），另见 §4 的清单示例（`repo: obra/superpowers`）。

## Global Constraints

以下每一条对本计划的每个任务都成立，写代码前先读一遍。

- **语言**：标识符与 commit message 用英文；注释与用户可见文案用中文，跟随各文件既有风格（不重写、不翻译已有注释）。
- **依赖**：不新增运行时依赖。`tempfile` **只是 dev-dependency**，生产代码不得引用它（夹具一律放 `src/testutil.rs`）。
- **平台**：POSIX-only（`src/apply/write.rs` 用 `std::os::unix::fs::symlink`）。计划里不引入 Windows 兜底。
- **只读守卫（§9.9）**：写 API 只允许出现在 `src/apply/` 与 `src/store/` 下；**外部进程调用（`Command::new(`）按具体文件开允许位**，允许位是 `src/store/git.rs` 与 `src/testutil.rs` 两个文件，不是目录。守卫本身不许被删、不许被放宽成目录级豁免。
- **测试绝不碰真实 `~`**（§9.10）：所有夹具走 `TempHome`。git 夹具必须是**本地 `file://` 仓库**且 env 密闭（`GIT_CONFIG_NOSYSTEM=1`、`GIT_CONFIG_GLOBAL=/dev/null`、固定 author/committer），绝不触网、绝不读用户 git 配置。
- **不变式一（§9.3）**：henv 绝不在 harness 的 skills 目录里建软链，永远是真拷贝。
- **不变式二（§9.3）**：绝不覆盖不是 henv 交付的东西；冲突时在任何写入发生前停手。
- **失败隔离（§9.7）**：单条失败不牵连同批；失败的条目不进账本；重跑就是修复路径。
- **离线要能用（§9.4）**：fetch 不到时沿用缓存内容并把条目标 `stale`，**不报失败**。
- **报告要诚实（§9.1）**：任何「计划说了要做、实际做不了」的情况必须在报告里写清楚，不许静默跳过。
- **网络与副作用**：实现过程不联网（除测试里 git 操作的是本地 `file://` 仓库）；不 commit、不 push、不建远端——**是否逐任务 commit 见「需要用户裁定的一处」第 5 条**，未裁定前执行者一律不 commit。

## Review Focus

下面五类输入/失效模式是 spec 没有逐条点名、但真实用户一定会撞上的。列在最前面的最可能咬人；每一类都由指定任务的测试钉住（括号内为该任务号）。

1. **网络不可达，或机器上根本没有 `git`**（Task 4 / 5）。用户期望：「离线也给出正确的计划，说清楚哪些是缓存里的旧内容」；没有 git 时期望一句能看懂的话，而不是 `No such file or directory`。**测试：Task 5 Step 3 的 `git_source_is_deferred_when_the_cache_is_missing_in_cached_mode` 与 Task 4 Step 7 的 `a_git_that_cannot_be_started_is_an_error_not_a_silent_skip`。**
2. **畸形的仓库定位符**：`path: ../../etc`、`repo` 为空、`ref` 是拼错的 tag（Task 3 / 4）。用户期望：清单被拒或源被判为取不到，**绝不能**把缓存目录以外的目录当成内容读出来。**测试：Task 3 Step 8 的 `a_git_sub_path_may_not_escape_the_checkout`、Task 4 Step 7 的 `an_unknown_ref_is_a_failure_not_a_fallback_to_head`。**
3. **缓存损坏**（`.git` 目录存在但不是仓库、clone 半途被打断）（Task 4）。用户期望：报错并重建或提示重跑，而不是把半截目录当内容拷进 harness。**测试：Task 4 Step 7 的 `a_corrupt_cache_directory_is_reported_not_used`。**
4. **本地源目录不存在**（Task 6）。用户期望：计划说 `+ install` 而 apply 一个字节都写不出来时，报告必须说出来。**测试：Task 6 Step 1 的 `a_failed_source_turns_a_promised_install_into_undeliverable` 与 Task 6 Step 5 的 `a_planned_delivery_without_content_is_a_reported_failure`。**
5. **用真实 git 配置/凭据的私有仓库**（Task 4）。用户期望：henv 不吞掉用户的 git 配置（`~/.ssh`、credential helper 要能用），所以生产路径**继承环境**；而测试路径必须密闭。**测试：Task 4 Step 5 的 `the_git_runner_inherits_the_environment_by_default`。**

---

## File Structure

| 文件 | 动作 | 职责 |
| --- | --- | --- |
| `tests/read_only_guard.rs` | 修改 | 把 `FORBIDDEN` 拆成「写 API」（目录级白名单可豁免）与「外部进程调用」（**文件级**允许位）。 |
| `src/home.rs` | 修改 | 新增 `henv_cache_dir()` / `henv_git_cache()`：henv 自己的可写缓存区（§9.12）。 |
| `src/store/hash.rs` | 修改 | 新增 `hash_str(&str) -> String`（字符串指纹，缓存键用）。 |
| `src/store/git.rs` | **新建**（Task 1 只建空壳：允许位里写的文件必须真实存在，见 Task 1 Step 3；Task 3 起填实现） | 仓库定位符的解释（URL 归一化、缓存键）、`git` 子进程原语、`resolve`（fetch → sha → 子目录 → 内容目录）、`list_skills`。**唯一允许 shell out 的生产文件。** |
| `src/manifest.rs` | 修改 | `source: git` 的 `path` 校验：必须是仓库内相对路径，不得逃出 checkout。 |
| `src/store/skills.rs` | 修改 | 新增 `SourceMode`、`Unresolved`；`resolve_sources` 接 git 源并按模式分类「延期」与「失败」。 |
| `src/store/mod.rs` | 修改 | 导出 `git` 子模块。 |
| `src/diff.rs` | 修改 | 消费 `ContentIndex.unresolved` 与 `ResolvedContent.stale`：失败 → 可见的 `undeliverable`，延期/stale → 行尾注记。 |
| `src/render.rs` | 修改 | 让带注记的 `install` 不被合并、带注记的 `in-sync` 不被汇总——否则 §9.4 的 `stale` 无处显示。 |
| `src/apply/mod.rs` | 修改 | 「计划说装、内容却缺席」不再静默 `continue`，改为报失败（§9.1）。 |
| `src/lib.rs` | 修改 | `plan_for` 带上 `SourceMode`；`run_diff` 用 `Cached`、`run_apply` 用 `Fetch`。 |
| `src/main.rs` | 修改 | 新增 `henv skills list --repo <url> [--ref <r>] [--path <p>]`（§9.8）。 |
| `src/testutil.rs` | 修改 | 新增密闭本地 git 仓库夹具（`GitFixture`）。**第二个允许 shell out 的文件。** |
| `docs/design.md` | 修改 | §9.8 的 `skills list` 那一行按裁定改掉（落在缓存子目录，不是系统临时目录）；§9 进度段补 B2。 |

---

### Task 1: 只读守卫拆表——外部进程调用按文件开口子

`tests/read_only_guard.rs` 现在是「白名单目录内的文件整体豁免 `FORBIDDEN`」。B2 要 shell out 到 `git`，而 `Command::new(` 埋在同一张 `FORBIDDEN` 表里——**目录级豁免会连带把 shell out 也放行**（`src/store/git.rs` 正好在白名单目录内），绊线就此失效。所以先拆表：写 API 仍按目录豁免；外部进程调用按**具体文件**豁免，且这份名单本身受测试保护。

**Files:**
- Modify: `tests/read_only_guard.rs`
- Test: `tests/read_only_guard.rs`（该文件本身就是测试）

**Interfaces:**
- Consumes: 无。
- Produces: `WRITE_API: &[&str]`（原 `FORBIDDEN` 去掉 `"Command::new("`）、`PROCESS_CALLS: &[&str]`、`PROCESS_ALLOWLIST: &[&str]`、`offenders_of_text(text: &str) -> Vec<String>`。Task 4 的 `src/store/git.rs` 与 Task 4/8 的 `src/testutil.rs` 依赖 `PROCESS_ALLOWLIST` 里有自己。

- [ ] **Step 1: 写失败测试——允许位之外的文件里的 `Command::new(` 必须被拦下**

在 `tests/read_only_guard.rs` 的测试模块里加：

```rust
#[test]
fn a_shell_out_outside_the_allowlist_is_refused() {
    // 绊线本身要可证伪：拿一段合成文本，证明扫描器真的会认出 Command::new(。
    let synthetic = "fn f() { let _ = Command::new(\"git\"); }";
    assert_eq!(offenders_of_text(synthetic), vec![PROCESS_CALLS[0].to_string()]);
}

#[test]
fn the_process_allowlist_names_files_not_directories() {
    // 允许位是「这一个文件」，不是「这一片目录」——目录级豁免正是 B2 要避免的那件事。
    for p in PROCESS_ALLOWLIST {
        assert!(p.ends_with(".rs"), "{p} 不是具体文件");
        assert!(!p.ends_with('/'), "{p} 看着像目录");
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(p).exists(),
            "{p} 不存在：允许位不允许指向幽灵文件"
        );
    }
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --test read_only_guard`
Expected: 编译失败，`cannot find function `offenders_of_text`` / `cannot find value `PROCESS_CALLS``（这三个名字还不存在）。

- [ ] **Step 3: 拆表并重构扫描器**

把现有的 `const FORBIDDEN: &[&str] = &[...]` 换成两张表，并把 `offenders(path: &Path)` 拆成「读文件」+「扫文本」两层：

```rust
/// 写 API：只允许出现在 `WRITE_WHITELIST` 下面的目录里。
const WRITE_API: &[&str] = &[
    "fs::write(",
    "File::create(",
    "OpenOptions",
    "fs::remove_file(",
    "fs::remove_dir",
    "fs::create_dir",
    "fs::rename(",
    "fs::copy(",
    "fs::symlink(",
    "fs::hard_link(",
    "fs::set_permissions(",
];

/// 外部进程调用：**不按目录豁免**，只按具体文件豁免（§9.9）。
///
/// 为什么单独一张表：`src/store/` 允许写文件，而 shell out 比写文件强得多——
/// 它能做这个进程权限内的任何事。目录级豁免会把「这个目录可以写文件」悄悄
/// 升级成「这个目录可以起进程」。B2 的 git 与 B3 的 `claude mcp add-json`
/// 是这个项目第一次真的调用外部程序，绊线必须收紧到文件名。
const PROCESS_CALLS: &[&str] = &["Command::new("];

/// 允许 shell out 的文件，白名单精确到路径全等。
///
/// - `src/store/git.rs`：git 源解析，整个模块的存在理由就是调 git（§9.12）。
/// - `src/testutil.rs`：测试夹具要造本地 git 仓库，但它只在 cfg(test) 下编译。
const PROCESS_ALLOWLIST: &[&str] = &["src/store/git.rs", "src/testutil.rs"];

fn offenders_of_text(text: &str) -> Vec<String> {
    let stripped = strip_cfg_test_blocks(text);
    let mut found: Vec<String> = Vec::new();
    for pat in WRITE_API.iter().chain(PROCESS_CALLS.iter()) {
        // 保留原有语义：每个模式最多报一次，报模式本身。
        if stripped.contains(pat) && !found.contains(&pat.to_string()) {
            found.push(pat.to_string());
        }
    }
    found
}
```

**同时创建 `src/store/git.rs` 的空壳**（这一步在允许位之后、在 Task 3 填实现之前）：允许位里写的两个文件必须**真的存在**，否则 `the_process_allowlist_names_files_not_directories` 会在 Step 4 就红——而把「存在性」这条断言去掉，允许位就退化成一张可以随便写幽灵文件的名单。所以由本任务建壳，Task 3 往里填内容：

```rust
//! git 源解析（§9.12）：一律 shell out 系统 `git`，不引 `git2`。
//!
//! 这个模块是项目里唯一允许调外部进程的生产代码（见 `tests/read_only_guard.rs`
//! 的 `PROCESS_ALLOWLIST`）。内容在 Task 3 起填入。
```

`src/store/mod.rs` 加一行 `pub mod git;`（`hash` / `provenance` / `skills` 已在那里）。

同时把原 `offenders(path)` 改成薄壳：

```rust
fn offenders(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(text) => offenders_of_text(&text),
        Err(_) => Vec::new(),
    }
}
```

并把原 `write_api_is_allowed_only_inside_the_whitelist` 的过滤逻辑改成对两张表分别判定：

```rust
fn violations(path: &Path) -> Vec<String> {
    let found = offenders(path);
    let rel = rel_path(path);
    found
        .into_iter()
        .filter(|pat| {
            if PROCESS_CALLS.contains(&pat.as_str()) {
                // 外部进程调用：只有名字在允许位里的文件能过。
                !PROCESS_ALLOWLIST.contains(&rel.as_str())
            } else {
                // 写 API：目录级白名单照旧。
                !is_whitelisted(path)
            }
        })
        .collect()
}
```

（把既有的 `strip_cfg_test_blocks`——原 `offenders` 里做括号深度跟踪的那段——原样提成独立函数；只搬位置，不改逻辑。）

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --test read_only_guard`
Expected: `test result: ok.` 全部通过（原 5 条 + 新增 2 条）。

- [ ] **Step 5: 用真实文件证明绊线还带电**

在测试模块里再加一条，防止以后有人把允许位放宽成 `src/`：

```rust
#[test]
fn the_allowlist_is_not_the_whole_tree() {
    for p in PROCESS_ALLOWLIST {
        assert_ne!(p, &"src/");
        assert_ne!(p, &"src/store/");
    }
    // 允许位之外的真实文件，扫到 Command::new( 必须被判违规（用合成路径验证判定函数）。
    let tmp = Path::new("src/capability.rs");
    if tmp.exists() {
        assert!(
            violations(tmp).is_empty(),
            "既有文件里不该有 shell out —— 出现了就要显式加进 PROCESS_ALLOWLIST"
        );
    }
}
```

Run: `cargo test --test read_only_guard`
Expected: `test result: ok.`（8 条通过）

- [ ] **Step 6: 全量回归（守卫收紧了，确认没有既有代码被误伤）**

Run: `cargo test`
Expected: `test result: ok.`（lib 383 条、guard 8 条、smoke 1 ignored，数字只增不减）

---

### Task 2: `Home` 的 git 缓存路径

§9.12 规定缓存落在 `~/.cache/henv/git/<repo-hash>/`。它必须在 `Home` 上有名字，且必须在任何 harness 内容目录之外——否则「只读」的承诺和 `diff` 不写盘的边界就说不清了。

**Files:**
- Modify: `src/home.rs`
- Test: `src/home.rs`（mod tests）

**Interfaces:**
- Consumes: 无。
- Produces: `Home::henv_cache_dir() -> PathBuf`（`root/.cache/henv`）、`Home::henv_git_cache() -> PathBuf`（`root/.cache/henv/git`）。Task 4/5/8 消费。

- [ ] **Step 1: 写失败测试**

在 `src/home.rs` 的 `mod tests` 里加：

```rust
#[test]
fn the_git_cache_lives_under_henv_own_cache_directory() {
    let t = TempHome::new();
    assert_eq!(t.home.henv_cache_dir(), t.path(".cache/henv"));
    assert_eq!(t.home.henv_git_cache(), t.path(".cache/henv/git"));
}

#[test]
fn the_git_cache_is_not_inside_any_harness_directory() {
    // 「diff 不写 Home 下任何文件」这条承诺在 B2 之后要收窄成
    // 「除 henv 自己的 cache 外不写」——所以缓存路径必须能被单独指认出来。
    let t = TempHome::new();
    let cache = t.home.henv_git_cache();
    for d in t.home.harness_dirs() {
        assert!(
            !cache.starts_with(&d),
            "缓存 {} 落在了 harness 目录 {} 里",
            cache.display(),
            d.display()
        );
    }
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --lib home::`
Expected: 编译失败，`no method named `henv_cache_dir``。

- [ ] **Step 3: 实现**

在 `src/home.rs` 里 `henv_state_dir()` 附近加（`henv_state_dir` 是 `root/.local/state/henv`，注释里说明与它的分工）：

```rust
    /// henv 自己的缓存根（§9.12）。放在 `root/.cache` 而不是 `.local/state`：
    /// 状态目录存账本（丢了就分不清东西是谁放的），缓存目录存可以从上游重取的内容
    /// （丢了只是下次慢一点）。两者的丢失代价不同，所以不放在一起。
    pub fn henv_cache_dir(&self) -> PathBuf {
        self.root.join(".cache/henv")
    }

    /// git 源的内容缓存：`<henv_cache>/git/<repo-hash>/repo` 是一次 clone。
    pub fn henv_git_cache(&self) -> PathBuf {
        self.henv_cache_dir().join("git")
    }
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --lib home::`
Expected: `test result: ok.`

---

### Task 3: git 定位符的解释——URL 归一化、缓存键、子路径校验

三件事都是「清单里那个字符串到底指哪里」，所以同一个任务里做完：`obra/superpowers` 这种 owner/repo 简写要展开成 git 能 clone 的 URL（§4 的清单示例就是这么写的）；缓存键要由归一化后的 URL 决定（同一仓库的不同写法指向同一份缓存）；清单里的 `path:` 必须是仓库内的相对路径，**不许逃出 checkout**。

**Files:**
- Modify: `src/store/git.rs`（空壳已在 Task 1 建好，本任务开始填实现）
- Modify: `src/store/hash.rs`（加 `hash_str`）
- Modify: `src/manifest.rs`（git 的 `path` 校验）
- Test: 各文件 mod tests

**Interfaces:**
- Consumes: `crate::store::hash::hash_str`（本任务新增）。
- Produces:
  - `store::hash::hash_str(s: &str) -> String`（64 位十六进制 SHA-256）
  - `store::git::repo_url(repo: &str) -> String`
  - `store::git::repo_key(repo: &str) -> String`（归一化 URL 的 SHA-256 前 16 位十六进制）
  - `store::git::GITHUB_SHORTHAND_HOST: &str`
  - `manifest.rs` 内部的 `fn git_sub_path_is_safe(s: &str) -> bool`

- [ ] **Step 1: 写失败测试——`hash_str`**

在 `src/store/hash.rs` 的 `mod tests` 里加：

```rust
#[test]
fn hash_str_is_stable_and_sized_like_a_sha256() {
    assert_eq!(hash_str("https://github.com/obra/superpowers.git").len(), 64);
    assert_eq!(hash_str("a"), hash_str("a"), "同一输入必须同一指纹");
    assert_ne!(hash_str("a"), hash_str("b"));
    assert!(hash_str("").len() == 64, "空串也有指纹，不返回 None");
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --lib store::hash`
Expected: 编译失败，`cannot find function `hash_str``。

- [ ] **Step 3: 实现 `hash_str`**

在 `src/store/hash.rs` 里复用已有的私有 `hex()`：

```rust
/// 字符串指纹。给缓存键这类「路径不好直接当目录名」的场合用：
/// 归一化后的 URL 里有 `/`、`:`，直接拼进路径既难看又有歧义。
pub fn hash_str(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex(&hasher.finalize())
}
```

Run: `cargo test --lib store::hash`
Expected: `test result: ok.`

- [ ] **Step 4: 写失败测试——URL 归一化与缓存键**

在 `src/store/git.rs` 的 `mod tests` 里加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_repo_shorthand_expands_to_a_cloneable_url() {
        assert_eq!(
            repo_url("obra/superpowers"),
            "https://github.com/obra/superpowers.git"
        );
        assert_eq!(
            repo_url("obra/superpowers.git"),
            "https://github.com/obra/superpowers.git",
            "已经写了 .git 不重复追加"
        );
    }

    #[test]
    fn explicit_urls_and_local_paths_are_left_alone() {
        for s in [
            "https://github.com/obra/superpowers.git",
            "http://example.com/x.git",
            "ssh://git@example.com/x.git",
            "git@github.com:obra/superpowers.git",
            "git://example.com/x.git",
            "file:///tmp/repo.git",
            "/tmp/repo",
            "./repo",
            "~/repos/x",
        ] {
            assert_eq!(repo_url(s), s, "{s} 不该被改写");
        }
    }

    #[test]
    fn the_cache_key_follows_the_normalized_url() {
        // 同一仓库的两种写法（简写与全 URL）必须落进同一份缓存——
        // 否则同一个源会在盘上有两份内容，账本却只有一个 hash。
        assert_eq!(repo_key("obra/superpowers"), repo_key("obra/superpowers.git"));
        assert_eq!(repo_key("obra/superpowers").len(), 16);
        assert_ne!(repo_key("obra/superpowers"), repo_key("obra/other"));
    }
}
```

- [ ] **Step 5: 运行，确认失败**

Run: `cargo test --lib store::git`
Expected: 编译失败，`cannot find function `repo_url``。

- [ ] **Step 6: 实现 `repo_url` 与 `repo_key`**

在 `src/store/git.rs` 顶部写模块文档，再实现：

```rust
//! git 源解析（§9.12）：一律 shell out 系统 `git`，不引 `git2`。
//!
//! 这个模块是项目里唯一允许调外部进程的生产代码（见 `tests/read_only_guard.rs`
//! 的 `PROCESS_ALLOWLIST`）。它的输出只是「一份内容在盘上的哪个目录」——
//! 判定与交付都在别处，所以这里不需要聪明，只需要准确。

use std::path::{Path, PathBuf};

use crate::store::hash::hash_str;

/// owner/repo 简写展开到哪个主机。§4 的清单示例 `repo: obra/superpowers`
/// 是 GitHub 简写；git 自己不认识这种写法（它会当成相对路径），所以必须由
/// 我们展开。这是一个**约定**，不是发现：写别家的简写请在清单里写全 URL。
pub const GITHUB_SHORTHAND_HOST: &str = "https://github.com";

/// 把清单里的 `repo` 变成 git 能 clone 的定位符。
///
/// 只有一种情况需要改写：`owner/repo`（恰好一个斜杠、两侧是非空标识符、不含 scheme）。
/// 其余一律原样——**宁可让 git 去报错，也不要猜**：猜错了用户看到的是一个
/// 能跑通的、内容却来自别处的仓库。
pub fn repo_url(repo: &str) -> String {
    if !is_owner_repo_shorthand(repo) {
        return repo.to_string();
    }
    let trimmed = repo.strip_suffix(".git").unwrap_or(repo);
    format!("{GITHUB_SHORTHAND_HOST}/{trimmed}.git")
}

fn is_owner_repo_shorthand(s: &str) -> bool {
    if s.contains("://") || s.contains(':') || s.starts_with(['/', '.', '~']) {
        return false;
    }
    let body = s.strip_suffix(".git").unwrap_or(s);
    let mut parts = body.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) => {
            !owner.is_empty()
                && !name.is_empty()
                && [owner, name].iter().all(|p| {
                    p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                })
        }
        _ => false,
    }
}

/// 缓存目录名（§9.12 的 `<repo-hash>`）：归一化 URL 的 SHA-256 前 16 位。
pub fn repo_key(repo: &str) -> String {
    hash_str(&repo_url(repo)).chars().take(16).collect()
}
```

- [ ] **Step 7: 运行，确认通过**

Run: `cargo test --lib store::git`
Expected: `test result: ok.`（3 条）

- [ ] **Step 8: 写失败测试——git 的 `path` 不许逃出 checkout**

在 `src/manifest.rs` 的 `mod tests` 里加（沿用该文件既有的 `Manifest::parse` 写法）：

```rust
#[test]
fn a_git_sub_path_may_not_escape_the_checkout() {
    // 逃出去的 path 会把缓存目录以外的内容当成 skill 内容读到、并拷进 harness。
    // 用户的清单里写 `local` 源是明说「读这个目录」，写 `git` 源却指定 `../..`
    // 就是清单自相矛盾——拒绝它，而不是照做。
    for bad in ["../other", "skills/../../escape", "/etc", "a/../b"] {
        let text = format!(
            "skills:\n  s1:\n    source: git\n    repo: obra/superpowers\n    ref: main\n    path: {bad}\n    targets: [claude-code]\n"
        );
        let err = Manifest::parse(&text).expect_err("{bad} 应当被拒");
        assert!(
            err.to_string().contains("path"),
            "报错要点名 path，实际：{err}"
        );
    }
}

#[test]
fn a_plain_git_sub_path_is_accepted() {
    let text = "skills:\n  s1:\n    source: git\n    repo: obra/superpowers\n    ref: main\n    path: skills/foo\n    targets: [claude-code]\n";
    assert!(Manifest::parse(text).is_ok());
}
```

- [ ] **Step 9: 运行，确认失败**

Run: `cargo test --lib manifest::tests::a_git_sub_path`
Expected: FAIL —— `expect_err` 没拿到错误（今天 `path: ../other` 会被接受）。

- [ ] **Step 10: 实现校验**

在 `src/manifest.rs` 的 `SkillSourceKind::Git` 校验分支里，紧接既有的 `reject_blank` 之后加：

```rust
/// git 源的 `path` 是**仓库内**的子目录：相对、不带 `..`、不以 `/` 开头。
///
/// 这不是防越权（读哪个目录用户在本机本来就有权限决定），是防清单自相矛盾：
/// `source: git` 说「内容在这个仓库里」，`path: ../../x` 说的是「内容在这个仓库外」。
/// 后者应当写成 `source: local`。
fn git_sub_path_is_safe(s: &str) -> bool {
    if s.starts_with('/') {
        return false;
    }
    Path::new(s)
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
}
```

并在 Git 分支：

```rust
if let Some(p) = path {
    anyhow::ensure!(
        git_sub_path_is_safe(p),
        "skill `{name}` 的 path 必须是仓库内的相对路径（不能是绝对路径，也不能含 `..`）：{p}"
    );
}
```

（`reject_blank` 已保证非空；这里只管形状。`name` 用该分支已有的变量名。）

- [ ] **Step 11: 写失败测试——mcp 与 plugin 的键也不能是一条路径**

一次外部安全复查指出 `src/apply/mod.rs` 的 `target_dir.join(&e.id.name)` 是路径穿越。核对结论：**那一行不成立**（能到那里的名字全是清单里的 skill 键，而 skill 键已在 `name_is_a_single_component` 挡过，见 `src/manifest.rs:496`），但同一类问题在**另外两条循环上是真的开着的**——`mcps`（:525）与 `plugins`（:537）的键只查了空白。这两个名字正是 B3 要拿去拼路径的（plugin 的安装目录、`claude mcp add-json <name>`）。趁 B2 正在改 `validate()` 这一带，把它补上。

```rust
#[test]
fn every_capability_key_is_a_single_path_component() {
    // skill 一直有这个约束（B1 加的），mcp 与 plugin 没有——而它们的名字同样
    // 会被 henv 拿去拼路径。键里带 `/` 是清单层次写错了，不是「名字里就有斜杠」：
    // 身份由 `marketplace`/`entry`/`repo` 那些字段承载，不靠键。
    for bad in ["a/b", "..", ".", "", "a\\b", "/etc"] {
        let mcp = format!("mcps:\n  '{bad}':\n    command: npx\n");
        let plugin = format!(
            "plugins:\n  '{bad}':\n    source: claude-marketplace\n    marketplace: obra/superpowers\n    entry: superpowers\n"
        );
        for (kind, text) in [("mcps", mcp), ("plugins", plugin)] {
            assert!(
                Manifest::parse(&text).is_err(),
                "{kind} 的键 {bad:?} 应当被拒"
            );
        }
    }
    // 反面的绊线：约束不能宽到把正常名字也拒了。
    let ok = "mcps:\n  context7:\n    command: npx\nplugins:\n  superpowers:\n    source: claude-marketplace\n    marketplace: obra/superpowers\n    entry: superpowers\n";
    assert!(Manifest::parse(ok).is_ok(), "正常名字要照收");
}
```

（`plugins` 那几条的字段照抄该文件既有 plugin 夹具的形状；若 `enabled` 之类必填项与上面写的不符，按夹具补齐。）

- [ ] **Step 12: 运行，确认失败**

Run: `cargo test --lib manifest::tests::every_capability_key`
Expected: FAIL —— `mcps`/`plugins` 的坏键今天会被接受。

- [ ] **Step 13: 实现——把同一条约束用到三条循环上**

在 `validate()` 的 `mcps` 与 `plugins` 循环开头，照 `skills` 那条加同一段检查（同一句话，措辞照抄）：

```rust
            if !name_is_a_single_component(name) {
                e.push(format!(
                    "{at}: name must be a single path component (no `/`, no `\\`, not `.`/`..`, not empty)"
                ));
            }
```

并把 `name_is_a_single_component` 的文档注释从「skill 的名字」改成它现在的通用含义（三类能力的键），提示 POSIX-only 的那段保留。

- [ ] **Step 14: 运行，确认通过**

Run: `cargo test --lib manifest::`
Expected: `test result: ok.`

- [ ] **Step 15: 全量回归**

Run: `cargo test`
Expected: `test result: ok.`

---

### Task 4: shell out 到系统 `git`，把仓库同步到缓存

这一任务交付 `git::resolve`：给定清单里的 `(repo, path, ref)` 与模式，返回「内容在盘上的哪个目录 + 那个提交的 sha（或「用了没刷新的缓存」）」。**所有 git 调用都经过一个函数**，这样「生产继承环境、测试密闭环境」可以分别钉在两个地方。

**Files:**
- Modify: `src/store/git.rs`
- Modify: `src/testutil.rs`（新增 `GitFixture`）
- Modify: `src/store/skills.rs`（只为引入 `SourceMode` 到 `git.rs` 的签名——类型定义在本任务的 `git.rs` 里，Task 5 才改 `skills.rs` 的调用）

**Interfaces:**
- Consumes: `TempHome`（`src/testutil.rs` 既有）、`Home::henv_git_cache()`（Task 2）、`repo_url`/`repo_key`（Task 3）。
- Produces:
  - `store::git::SourceMode { Cached, Fetch }`（`#[derive(Clone, Copy, PartialEq, Eq, Debug)]`）
  - `store::git::GitSource { pub path: PathBuf, pub sha: String, pub stale: Option<String> }`
  - `store::git::resolve(repo: &str, sub: Option<&str>, git_ref: &str, cache: &Path, mode: SourceMode) -> anyhow::Result<Option<GitSource>>`
  - `store::git::list_skills(repo: &str, sub: Option<&str>, git_ref: Option<&str>, cache: &Path) -> anyhow::Result<Vec<(String, String)>>`（名字, 仓库内相对路径）——Task 8 消费
  - `testutil::GitFixture { pub dir: PathBuf, pub url: String }`，方法 `write`、`commit`、`tag`

- [ ] **Step 1: 写失败测试——夹具能造出一个密闭的本地仓库**

在 `src/testutil.rs` 的 `impl TempHome` 后追加夹具，**先写它的测试**：

```rust
#[test]
fn the_git_fixture_creates_a_hermetic_local_repo() {
    // §9.10：git 源不联网，夹具必须是本地的；也绝不读用户 git 配置。
    let t = TempHome::new();
    let repo = t.git_repo("fixtures/upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    let first = repo.commit("init");
    assert_eq!(first.len(), 40, "commit 返回完整 sha");

    repo.write("skills/foo/SKILL.md", "# foo v2");
    let second = repo.commit("v2");
    assert_ne!(first, second);
    repo.tag("v1");
    assert!(repo.url.starts_with("file://"), "必须是本地 URL：{}", repo.url);
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --lib testutil::tests::the_git_fixture`
Expected: 编译失败，`no method named `git_repo``。

- [ ] **Step 3: 实现夹具**

`src/testutil.rs` 里加（该文件已在 Task 1 的 `PROCESS_ALLOWLIST` 内）：

```rust
/// 一个本地 git 仓库夹具（§9.10：git 源不联网）。
///
/// 每一条 git 调用都带**显式配置**，因为这些测试跑在开发机上：用户自己的
/// `~/.gitconfig` 里可能有 `commit.gpgsign = true`、`init.defaultBranch = master`
/// 之类的设置，那会让「同样的 fixture 在不同机器上产出不同的 sha」。测试要的是
/// 可复现，不是「在我机器上能过」。
pub struct GitFixture {
    pub dir: PathBuf,
    /// `file://` 形式的 clone 地址——绝对路径直接当 URL 传给 git 会被当成
    /// 「本地路径优化」，行为与远端 clone 有差异，file:// 才是忠实的模拟。
    pub url: String,
}

impl GitFixture {
    fn git(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "henv-fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "henv-fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00+0000")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00+0000")
            .output()
            .expect("git 必须可用：本项目要求开发机装有 git");
        assert!(
            out.status.success(),
            "git {args:?} 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn write(&self, rel: &str, body: &str) {
        let p = self.dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("夹具路径要有父目录")).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// 提交并返回 sha。返回 sha 是为了让测试能断言「账本记的是这个提交」。
    pub fn commit(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }

    pub fn tag(&self, name: &str) {
        self.git(&["tag", name]);
    }
}

impl TempHome {
    /// 在临时 home 里造一个本地仓库。放在 home 下是因为夹具的一切都该随 TempDir 消失。
    pub fn git_repo(&self, rel: &str) -> GitFixture {
        let dir = self.path(rel);
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("file://{}", dir.display());
        let f = GitFixture { dir, url };
        f.git(&["init", "-q"]);
        f
    }
}
```

（`use std::path::PathBuf;` 若该文件尚未引入则补上。）

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --lib testutil::`
Expected: `test result: ok.`

- [ ] **Step 5: 写失败测试——`git` 运行器继承环境**

`resolve`/`list_skills` 要走生产路径（继承用户环境，私有仓库与 SSH 凭据才能用），夹具走密闭路径。两条路径的区别必须由测试钉住，否则以后有人图省事把夹具的环境清空加进生产函数，用户会得到一个「读不到 ~/.ssh」的 henv。

```rust
#[test]
fn the_git_runner_inherits_the_environment_by_default() {
    // 生产路径必须能用用户自己的 git 配置与凭据（§9.12 只说 shell out，
    // 没说隔离）。隔离是**测试夹具**的责任，不是这里的。
    let out = git_output(Path::new("."), &["--version"]).expect("git 要在 PATH 上");
    assert!(out.starts_with("git version"), "实际：{out}");
}

#[test]
fn the_git_runner_surfaces_stderr_when_the_command_fails() {
    let err = git_output(Path::new("."), &["rev-parse", "no-such-ref-henv"])
        .expect_err("不存在的 ref 应当失败");
    let msg = format!("{err:#}");
    assert!(msg.contains("rev-parse"), "报错要带上命令行，实际：{msg}");
    assert!(
        !msg.contains('\n') || msg.lines().count() <= 8,
        "stderr 要摘要，不要整段灌进报告：{msg}"
    );
}
```

- [ ] **Step 6: 运行，确认失败**

Run: `cargo test --lib store::git`
Expected: 编译失败，`cannot find function `git_output``。

- [ ] **Step 7: 写失败测试——`resolve` 的诸分流**

在 `src/store/git.rs` 的 `mod tests` 里加（夹具在 `src/testutil.rs`，用 `crate::testutil::TempHome`）：

```rust
#[test]
fn cached_mode_never_touches_the_network_and_reports_none_when_has_no_cache() {
    let t = TempHome::new();
    // 一个不存在的本地路径：Cached 模式下连 clone 都不该尝试——
    // 真去 clone 就会失败报错，而这里期望的是安静的 None。
    let got = resolve(
        "../nowhere/henv-nonexistent",
        None,
        "main",
        &t.home.henv_git_cache(),
        SourceMode::Cached,
    )
    .expect("Cached 模式不该报错");
    assert!(got.is_none());
}

#[test]
fn fetch_mode_clones_then_reads_the_requested_sub_directory() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    let sha = repo.commit("init");

    let got = resolve(
        &repo.url,
        Some("skills/foo"),
        "main",
        &t.home.henv_git_cache(),
        SourceMode::Fetch,
    )
    .expect("clone 应当成功")
    .expect("应当解析出内容");
    assert_eq!(got.sha, sha, "sha 要能对上那个提交");
    assert_eq!(got.stale, None);
    assert_eq!(
        std::fs::read_to_string(got.path.join("SKILL.md")).unwrap(),
        "# foo"
    );
}

#[test]
fn fetch_mode_is_stale_but_usable_when_the_remote_is_gone() {
    // §9.4：网络不可达则沿用缓存内容、把条目标 stale、不报失败——离线也要能用。
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");

    let cache = t.home.henv_git_cache();
    resolve(&repo.url, Some("skills/foo"), "main", &cache, SourceMode::Fetch).unwrap();

    // 上游消失（删掉整个仓库目录），缓存还在。
    std::fs::remove_dir_all(&repo.dir).unwrap();
    let got = resolve(&repo.url, Some("skills/foo"), "main", &cache, SourceMode::Fetch)
        .expect("fetch 失败不该让整次解析报错")
        .expect("缓存还在，内容就该给出来");
    assert!(got.stale.is_some(), "没刷新成功就必须标 stale");
    assert!(got.stale.unwrap().contains("未能刷新"));
}

#[test]
fn an_unknown_ref_is_a_failure_not_a_fallback_to_head() {
    // 拼错的 ref 绝不能悄悄退回默认分支：那会把另一个提交的内容当成用户要的。
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");
    let got = resolve(
        &repo.url,
        None,
        "no-such-branch",
        &t.home.henv_git_cache(),
        SourceMode::Fetch,
    )
    .expect("这不是错误");
    assert!(got.is_none(), "解不出 ref 就该是 None，而不是 HEAD");
}

#[test]
fn a_git_that_cannot_be_started_is_an_error_not_a_silent_skip() {
    // 跑不起来 git = 这个功能根本不可用。静默返回 None 会让用户以为
    // 自己的清单写错了，而不是机器上缺东西。
    //
    // 本机一定有 git，没法真把 PATH 清空；这里用不存在的 cwd 让 spawn 失败，
    // 钉住的是**我们自己的措辞**：错误要能看懂，且绝不能退化成 None。
    let err = git_output(Path::new("/nonexistent/henv-cwd"), &["status"])
        .expect_err("cwd 不存在时 spawn 就该失败");
    let msg = format!("{err:#}");
    assert!(msg.contains("无法执行 git"), "实际：{msg}");
}

#[test]
fn a_corrupt_cache_directory_is_reported_not_used() {
    // clone 被打断、或用户手贱动了缓存：`.git` 在但不是仓库。
    // 必须报错或重建，绝不能把半截目录当内容读出去拷进 harness。
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");

    let cache = t.home.henv_git_cache();
    let dir = cache.join(repo_key(&repo.url)).join("repo");
    std::fs::create_dir_all(dir.join(".git")).unwrap(); // 空壳 .git

    let got = resolve(&repo.url, Some("skills/foo"), "main", &cache, SourceMode::Fetch);
    // 期望：要么报错，要么给出内容——但给的必须是真内容，不能是空目录。
    match got {
        Err(e) => assert!(format!("{e:#}").contains("git"), "报错要能看懂：{e:#}"),
        Ok(Some(s)) => assert!(
            s.path.join("SKILL.md").exists(),
            "给出来的内容目录里必须有真东西"
        ),
        Ok(None) => panic!("不该是 None：缓存里那个目录是坏的，不是没有"),
    }
}
```

- [ ] **Step 8: 运行，确认失败**

Run: `cargo test --lib store::git`
Expected: 编译失败，`cannot find function `resolve``（`git_output` 已在 Step 5 的 RED/GREEN 里写好，此刻缺的是 `resolve` 与两个类型）。

- [ ] **Step 9: 实现运行器与 `resolve`**

在 `src/store/git.rs` 加：

```rust
/// 缓存目录是否是一个**可用**的仓库。只看 `.git` 存在是不够的：
/// clone 被打断会留下一个空的 `.git`，之后的 fetch/checkout 会以各种
/// 难懂的方式失败。`a_corrupt_cache_directory_is_reported_not_used` 钉住这条。
fn cache_is_usable(dir: &Path) -> bool {
    dir.join(".git").is_dir() && git_output(dir, &["rev-parse", "--git-dir"]).is_ok()
}

/// 跑一条 git 命令，返回 stdout。失败时把**命令行与 stderr 摘要**放进错误。
///
/// 环境原样继承：用户的 credential helper、`~/.ssh/config`、代理设置
/// 都是他访问私有仓库的凭据来源，隔离掉它们等于让 henv 只能克隆公开仓库。
/// 测试要的密闭性由 `testutil::GitFixture` 自己保证。
fn git_output(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| anyhow::anyhow!("无法执行 git（{args:?}）：{e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git {args:?} 失败：{}", summarize(&stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// stderr 可能很长（fetch 的进度、网络的多次重试），报告里只留前几行。
fn summarize(s: &str) -> String {
    let mut lines = s.lines().filter(|l| !l.trim().is_empty());
    let head: Vec<&str> = lines.by_ref().take(3).collect();
    let more = lines.next().is_some();
    let mut out = head.join(" / ");
    if more {
        out.push_str(" / …");
    }
    out
}

/// 判定模式（§9.2 与 §9.4 的接口）：决定这次解析会不会联网。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceMode {
    /// 只读缓存，不联网（`diff`）。拿不到内容就是「这次判不了」，不是失败。
    Cached,
    /// fetch 并把内容同步到缓存（`apply`）——**唯一**会改盘的调用方。
    Fetch,
}

pub struct GitSource {
    /// 仓库里那个子目录在缓存中的路径。
    pub path: PathBuf,
    /// 这次解析到的提交。
    pub sha: String,
    /// `Some(原因)` 表示内容来自没能刷新的缓存（§9.4 的 stale）。
    pub stale: Option<String>,
}

/// 把 `(repo, sub, ref)` 解析成本地内容目录。
///
/// 返回 `Ok(None)` 表示「这次拿不到，而且不是错误」：
/// `Cached` 模式下缓存里没有、或者 `Fetch` 模式下 fetch 失败且连缓存都没有。
/// 两者的区别由调用方按模式归类成 deferred / failed（见 `skills::Unresolved`）。
///
/// 这是这个项目里唯一会改盘的解析路径（`Fetch` 模式下的 fetch 与 checkout）。
pub fn resolve(
    repo: &str,
    sub: Option<&str>,
    git_ref: &str,
    cache: &Path,
    mode: SourceMode,
) -> anyhow::Result<Option<GitSource>> {
    let dir = cache.join(repo_key(repo)).join("repo");
    let url = repo_url(repo);

    let mut stale_reason: Option<String> = None;

    if !cache_is_usable(&dir) {
        if mode == SourceMode::Cached {
            return Ok(None); // 没缓存（或缓存是坏的），而这次不联网。
        }
        // 缓存坏了就整个丢掉重建：半截目录留着只会让 fetch/checkout
        // 以难懂的方式失败（`a_corrupt_cache_directory_is_reported_not_used`）。
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        // clone 到临时目录再改名：半途失败不能留下一个看似可用的缓存目录。
        let tmp = dir.with_extension(format!("cloning-{}", std::process::id()));
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::create_dir_all(tmp.parent().expect("缓存根要有父目录"))?;
        if let Err(e) = git_output(cache, &["clone", "--quiet", &url, tmp.to_str().unwrap()]) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e.context(format!("克隆 {url} 失败")));
        }
        std::fs::rename(&tmp, &dir)?;
    } else if mode == SourceMode::Fetch {
        // fetch 失败不是致命的：§9.4 要求「离线也要能用」。
        if let Err(e) = git_output(&dir, &["fetch", "--quiet", "--tags", "--force", "origin"]) {
            stale_reason = Some(format!("未能刷新上游（{}）", summarize(&format!("{e:#}"))));
        }
    }

    // **[2026-09-26 执行期修正]** 原稿在这里写的是裸 `rev-parse <ref>`，那是错的：
    // `git fetch` 只更新 `origin/*`、**不移动本地分支**，所以对着本地 `main` 解出来的
    // 是 clone 那天的提交——上游前进了也报「新鲜」（§9.4 要防的静默过期），而非默认
    // 分支根本没被 clone 建成本地分支，永远解不出来。改为优先解 `origin/<ref>`，
    // 回退裸 `<ref>` 覆盖 tag / 裸 sha / 只有本地引用的缓存。
    // 见 `store::git::resolve_commit` 与 `a_second_resolve_picks_up_an_upstream_commit`。
    let Some(sha) = resolve_commit(&dir, git_ref) else {
        return Ok(None); // ref 不存在（拼错、或缓存里还没取到这个 ref）
    };

    // 工作区必须停在这个提交上，否则读到的会是另一个 ref 的内容。
    let head = git_output(&dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    if head != sha {
        if mode == SourceMode::Cached {
            return Ok(None); // 缓存里没有这个 ref 的内容，而这次不联网。
        }
        git_output(&dir, &["checkout", "--quiet", "--detach", &sha])?;
    }

    let content = match sub {
        Some(s) => dir.join(s),
        None => dir.clone(),
    };
    if !content.is_dir() {
        // 仓库里没有这个子目录：这是清单错了，不是网络问题。
        anyhow::bail!("仓库 {url} 的 {git_ref} 里没有目录 `{}`", sub.unwrap_or(""));
    }
    Ok(Some(GitSource {
        path: content,
        sha,
        stale: stale_reason,
    }))
}
```

- [ ] **Step 10: 运行，确认通过**

Run: `cargo test --lib store::git`
Expected: `test result: ok.`（11 条）。

- [ ] **Step 11: 全量回归**

Run: `cargo test`
Expected: `test result: ok.`

---

### Task 5: 判定模式穿线，`resolve_sources` 接 git 源

`resolve_sources` 是全链路唯一的 I/O 入口（§9.2），B2 在这里做两件事：把 git 源解析成内容；把「这次为什么拿不到内容」分类成**延期**（不联网而已，`apply` 会去取）与**失败**（再试也取不到）。分类是判定模式的责任，所以模式必须从 CLI 一路传到这一层。

**Files:**
- Modify: `src/store/skills.rs`
- Modify: `src/lib.rs`（`plan_for` 加参、两个调用方各传各的模式）
- Test: 两文件的 mod tests

**Interfaces:**
- Consumes: `git::{resolve, SourceMode, GitSource}`（Task 4）、`Home::henv_git_cache()`（Task 2）。
- Produces:
  - `store::skills::SourceMode`——**直接复用** `git::SourceMode`（`pub use crate::store::git::SourceMode;`），不另立一个同形类型
  - `store::skills::Unresolved { Deferred(String), Failed(String) }`
  - `store::skills::ResolvedContent { path, hash, source_id, stale: Option<String> }`（新增 `stale`）
  - `store::skills::ContentIndex { sources, targets, unresolved: BTreeMap<CapId, Unresolved> }`
  - `store::skills::resolve_sources(desired: &[Capability], home: &Home, mode: SourceMode) -> anyhow::Result<ContentIndex>`
  - `lib::plan_for(manifest_path: &Path, home: &Home, mode: SourceMode) -> anyhow::Result<Planned>`

- [ ] **Step 1: 写失败测试——模式与分类**

在 `src/store/skills.rs` 的 `mod tests` 里加：

```rust
#[test]
fn a_git_source_resolves_in_fetch_mode_and_records_repo_ref_sha() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    let sha = repo.commit("init");

    let idx = resolve_sources(
        &[desired(
            "foo",
            SkillSource::Git {
                repo: repo.url.clone(),
                path: Some("skills/foo".into()),
                git_ref: "main".into(),
            },
        )],
        &t.home,
        SourceMode::Fetch,
    )
    .unwrap();

    let r = idx.get(&CapId::new(Kind::Skill, "foo")).expect("应当解析出来");
    assert_eq!(r.hash.len(), 64);
    assert_eq!(r.stale, None);
    assert_eq!(
        r.source_id,
        format!("{}@main@{}", repo.url, sha),
        "来源标识（§9.3）要写成人能读懂、且跨机器可复现的 `repo@ref@sha`"
    );
}

#[test]
fn a_git_source_is_deferred_when_the_cache_is_missing_in_cached_mode() {
    // 用户选的裁定：diff 只读缓存、不联网。这条断言就是那个裁定在代码里的样子。
    let t = TempHome::new();
    let idx = resolve_sources(
        &[desired(
            "from-git",
            SkillSource::Git {
                repo: "https://example.invalid/x".into(),
                path: None,
                git_ref: "main".into(),
            },
        )],
        &t.home,
        SourceMode::Cached,
    )
    .unwrap();

    assert!(idx.sources.get(&CapId::new(Kind::Skill, "from-git")).is_none());
    match idx.unresolved.get(&CapId::new(Kind::Skill, "from-git")) {
        Some(Unresolved::Deferred(reason)) => assert!(
            reason.contains("不联网"),
            "延期要说明原因是这次不联网，实际：{reason}"
        ),
        other => panic!("应当是 Deferred，实际 {other:?}"),
    }
}

#[test]
fn a_missing_local_directory_is_a_failure_not_a_deferral() {
    // 本地目录不存在，联网也救不了：它是 Failed，不是 Deferred。
    let t = TempHome::new();
    let idx = resolve_sources(
        &[desired("bad-local", SkillSource::Local { path: t.path("nope") })],
        &t.home,
        SourceMode::Fetch,
    )
    .unwrap();
    assert!(matches!(
        idx.unresolved.get(&CapId::new(Kind::Skill, "bad-local")),
        Some(Unresolved::Failed(_))
    ));
}

#[test]
fn a_stale_git_source_still_produces_content_and_carries_the_reason() {
    // §9.4：离线要能用——内容照给，只是标了 stale。
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");
    let cap = desired(
        "foo",
        SkillSource::Git {
            repo: repo.url.clone(),
            path: Some("skills/foo".into()),
            git_ref: "main".into(),
        },
    );
    resolve_sources(std::slice::from_ref(&cap), &t.home, SourceMode::Fetch).unwrap();

    std::fs::remove_dir_all(&repo.dir).unwrap();
    let idx = resolve_sources(std::slice::from_ref(&cap), &t.home, SourceMode::Fetch).unwrap();
    let r = idx.get(&CapId::new(Kind::Skill, "foo")).expect("缓存还在，内容要给");
    assert!(r.stale.is_some());
    assert!(!idx.unresolved.contains_key(&CapId::new(Kind::Skill, "foo")));
}
```

并把既有的 `an_unresolvable_source_is_simply_absent` 改造：`resolve_sources(..., SourceMode::Fetch)`；`from-git` 那一条改成断言 `unresolved` 里有它（B2 之后它不再「安静地缺席」，而是有名有姓地拿不到）；`agents-dir` 与 `local` 两条保持 `sources` 缺席。

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --lib store::skills`
Expected: 编译失败，`this function takes 2 arguments but 3 were supplied` / 找不到 `Unresolved`。

- [ ] **Step 3: 实现类型与解析**

`src/store/skills.rs`：

```rust
pub use crate::store::git::SourceMode;

/// 源没解析出来时的两种含义（§9.4 与 §9.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// 这次不联网（`diff`）而缓存里没有——不是失败，`apply` 会去取。
    /// 判定的后果：保留 Plan A 的动作，行尾注明为什么这次判不了。
    Deferred(String),
    /// 取不到，再试也取不到（源目录不存在、ref 不存在、fetch 失败且无缓存）。
    /// 判定的后果：如果计划本来会写盘，就把动作换成可见的 undeliverable。
    Failed(String),
}

pub struct ResolvedContent {
    pub path: PathBuf,
    pub hash: String,
    pub source_id: String,
    /// `Some(原因)`：内容来自没能刷新的缓存（§9.4 的 stale）。
    pub stale: Option<String>,
}
```

`resolve_sources` 的 git 分支（替换掉今天那个 `_ => continue`）：

```rust
for cap in desired {
    let Some(source) = skill_source_of(cap) else { continue };
    match source {
        SkillSource::AgentsDir => {
            resolve_one(&mut out, cap, home.agents_skills().join(&cap.id.name), "agents-dir".to_string())
        }
        SkillSource::Local { path } => {
            resolve_one(&mut out, cap, path.clone(), format!("local:{}", path.display()))
        }
        SkillSource::Git { repo, path, git_ref } => {
            match git::resolve(repo, path.as_deref(), git_ref, &home.henv_git_cache(), mode)? {
                Some(src) => {
                    let source_id = format!("{repo}@{git_ref}@{}", src.sha);
                    match hash_dir(&src.path)? {
                        Some(hash) => {
                            out.sources.insert(cap.id.clone(), ResolvedContent {
                                path: src.path,
                                hash,
                                source_id,
                                stale: src.stale,
                            });
                        }
                        None => {
                            out.unresolved.insert(
                                cap.id.clone(),
                                Unresolved::Failed(format!("仓库里 {path:?} 那份内容读不出来")),
                            );
                        }
                    }
                }
                None if mode == SourceMode::Cached => {
                    out.unresolved.insert(
                        cap.id.clone(),
                        Unresolved::Deferred("diff 不联网；缓存里没有这个源的内容（apply 会去取）".into()),
                    );
                }
                None => {
                    out.unresolved.insert(
                        cap.id.clone(),
                        Unresolved::Failed(format!("取不到 {repo} 的 {git_ref}（缓存里也没有）")),
                    );
                }
            }
        }
        // PluginBundle（B3）：不是今天的事，安静跳过。
        SkillSource::PluginBundle { .. } => {}
        SkillSource::Unknown => {}
    }
}
```

其中 `resolve_one`、`skill_source_of` 是把今天那段 `match source` 里的两件事抽出来（`hash_dir` 拿到就给 `ResolvedContent`；`None` 就写 `unresolved` 的 `Failed("源目录不存在…")`）。`skill_source_of(cap)` 返回 `Option<&SkillSource>`，只对 `Spec::Skill` 给值（今天那个 `continue` 的语义）。

`ContentIndex` 加字段与 Default：

```rust
#[derive(Debug, Default)]
pub struct ContentIndex {
    pub sources: BTreeMap<CapId, ResolvedContent>,
    pub targets: BTreeMap<String, BTreeMap<String, TargetContent>>,
    /// 没解析出内容的源，以及为什么（§9.1：报告里必须写清楚）。
    pub unresolved: BTreeMap<CapId, Unresolved>,
}
```

既有的 `get`/`is_empty`/`keys` 三个便捷方法保持原样（`is_empty` 只看 `sources`——它的语义是「内容层有没有东西可比」）。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --lib store::skills`
Expected: `test result: ok.`（既有断言用的几个构造点需要跟着加 `stale: None` 或改调用签名，一并改掉）

- [ ] **Step 5: 写失败测试——`plan_for` 的模式穿线**

在 `src/lib.rs` 的 `mod tests` 里加（沿用该文件既有的 `TempHome` + `plan_for` 用法）：

```rust
#[test]
fn diff_plans_without_network_and_apply_plans_with_it() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");
    t.write(
        "env.yaml",
        &format!(
            "skills:\n  foo:\n    source: git\n    repo: {}\n    ref: main\n    path: skills/foo\n    targets: [claude-code]\n",
            repo.url
        ),
    );
    let m = t.path("env.yaml");

    let cached = plan_for(&m, &t.home, SourceMode::Cached).unwrap();
    assert!(
        cached.content.unresolved.contains_key(&CapId::new(Kind::Skill, "foo")),
        "diff 不联网：第一次跑时缓存是空的，源应当报「这次判不了」"
    );

    let fetched = plan_for(&m, &t.home, SourceMode::Fetch).unwrap();
    assert!(
        fetched.content.sources.contains_key(&CapId::new(Kind::Skill, "foo")),
        "apply 会去取，所以同样一份清单在 Fetch 模式下应当解析出内容"
    );
}
```

- [ ] **Step 6: 实现穿线并运行**

`src/lib.rs`：`plan_for(manifest_path, home, mode)`，把 `mode` 传给 `resolve_sources`；`run_diff` 传 `SourceMode::Cached`，`run_apply` 传 `SourceMode::Fetch`。模块文档里那句「`diff` 只读……全链路不写 `Home` 下任何文件」按新事实收窄：

```rust
//! `diff` 全链路不写 `Home` 下的任何文件，也不联网：git 源只读 henv 自己的
//! 缓存（`~/.cache/henv/git`），缓存里没有就如实报「这次判不了」（§9.4）。
//! 唯一会改盘的是 `apply`（写 harness 目录、写账本、刷新 git 缓存）。
```

Run: `cargo test --lib lib::`
Expected: `test result: ok.`

- [ ] **Step 7: 全量回归**

Run: `cargo test`
Expected: `test result: ok.`（既有测试里 `plan_for` 的调用点需补 `SourceMode` 参数）

---

### Task 6: 拿不到的源不许被静默跳过

B1 的 `apply::deliver_skills` 对「计划说要装、内容层却没有它」的条目是 `continue`——一个字节都不写，也一个字都不说。plan 阶段被拒、部分交付、离线……用户看到的都是「我说了 y，然后什么都没发生」。这条 B1 遗留缺陷在 B2 会从罕见变成常态（离线是 git 源的常见状态），所以必须修：**计划里就不能出现这种条目**，且交付层再加一道保险。

**Files:**
- Modify: `src/diff.rs`（消费 `unresolved`）
- Modify: `src/apply/mod.rs`（`deliver_skills` 的 `continue` 改成报告失败）
- Test: 两文件的 mod tests + `tests/` 下的 CLI 级测试

**Interfaces:**
- Consumes: `ContentIndex.unresolved`、`Unresolved::{Deferred, Failed}`（Task 5）。
- Produces: 无新 API（行为变更）。

- [ ] **Step 1: 写失败测试——`Failed` 把承诺的 install 变成可见的 undeliverable**

在 `src/diff.rs` 的 `mod tests` 里加（该文件已有构造 `ContentIndex` 的辅助；若没有，直接用 `ContentIndex { sources: ..., targets: ..., unresolved: ... }`）：

```rust
#[test]
fn a_failed_source_turns_a_promised_install_into_undeliverable() {
    let mut content = ContentIndex::default();
    content.unresolved.insert(
        CapId::new(Kind::Skill, "foo"),
        Unresolved::Failed("源目录不存在：/nope".into()),
    );
    let plan = compute(&[skill("foo")], &inputs(), &DegradeTable::default(), &content, &Ledger::default());
    let e = entry_for(&plan, "claude-code", "foo");
    match &e.action {
        Action::Undeliverable { reason } => assert!(reason.contains("源目录不存在")),
        other => panic!("计划说了要装却装不了，必须报 undeliverable，实际 {other:?}"),
    }
}

#[test]
fn a_deferred_source_keeps_the_plan_a_verdict_and_only_adds_a_note() {
    // diff 不联网时，动作仍按 Plan A 判（这里目标是空的 → install），
    // 但行尾必须说清「这次判不了」。**不能**报 undeliverable：
    // apply 会去取，报 undeliverable 是在吓唬人。
    let mut content = ContentIndex::default();
    content.unresolved.insert(
        CapId::new(Kind::Skill, "foo"),
        Unresolved::Deferred("diff 不联网；缓存里没有这个源的内容（apply 会去取）".into()),
    );
    let plan = compute(&[skill("foo")], &inputs(), &DegradeTable::default(), &content, &Ledger::default());
    let e = entry_for(&plan, "claude-code", "foo");
    assert!(matches!(e.action, Action::Install));
    assert!(e.note.as_deref().unwrap_or_default().contains("不联网"));
}

#[test]
fn a_failed_source_does_not_override_a_verdict_that_asks_for_no_write() {
    // 目标已经是 in-sync：没有要写的东西，所以没有「承诺了做不到」的问题。
    // 此时只加注记，不改动作。
    let mut content = ContentIndex::default();
    content.targets.insert(
        "claude-code".into(),
        BTreeMap::from([("foo".to_string(), TargetContent::Has("deadbeef".into()))]),
    );
    content.unresolved.insert(
        CapId::new(Kind::Skill, "foo"),
        Unresolved::Failed("取不到".into()),
    );
    let mut actual = skill("foo");
    actual.origin = Origin::Adopted { harness: "claude-code".into() };
    let plan = compute(&[actual], &inputs_with("claude-code"), &DegradeTable::default(), &content, &Ledger::default());
    let e = entry_for(&plan, "claude-code", "foo");
    assert!(!matches!(e.action, Action::Undeliverable { .. }), "实际 {:?}", e.action);
    assert!(e.note.is_some(), "但原因要写出来");
}
```

（`skill()`、`inputs()`、`inputs_with()`、`entry_for()` 是本文件测试模块里的辅助函数——`compute` 的既有测试一定已有等价物，按它们现有的名字与形状改写这几条，不要新造一套。）

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --lib diff::tests::a_failed_source`
Expected: FAIL —— 动作是 `Install` 而不是 `Undeliverable`。

- [ ] **Step 3: 实现判定**

在 `src/diff.rs` 的 `Support::Native | Support::Degraded` 分支里，动作算出之后、`PlanEntry::new` 之前插入：

```rust
                // 源拿不到，而这条动作会写盘 → 换成可见的 undeliverable。
                // §9.1：计划承诺了要做的事，实际做不到，就必须在报告里说出来。
                // 只在 Install/Update 上换：in-sync/drift/conflict 本来就不写盘，
                // 换掉它们会把一个准确的判定改成一个吓人的名词。
                let action = match (content.unresolved.get(&cap.id), &action) {
                    (Some(Unresolved::Failed(reason)), Action::Install | Action::Update { .. }) => {
                        Action::Undeliverable { reason: reason.clone() }
                    }
                    _ => action,
                };
                // 注记：内容层的裁决优先（它说的是「盘上这东西怎么了」），
                // 其次是「源这次拿不到」的原因。
                let note = verdict_note.or_else(|| match content.unresolved.get(&cap.id) {
                    Some(Unresolved::Deferred(r)) => Some(r.clone()),
                    Some(Unresolved::Failed(r)) => Some(r.clone()),
                    None => None,
                });
                // 已经是 undeliverable 的行自带 reason，不再挂注记（同一句话说两遍）。
                let note = match &action {
                    Action::Undeliverable { .. }
                        if matches!(content.unresolved.get(&cap.id), Some(Unresolved::Failed(_))) =>
                    {
                        verdict_note
                    }
                    _ => note,
                };
```

把这里的三段合成一段读得懂的写法是执行者的自由（例如先算 `override_action`，再决定 note），只要三条测试的语义都成立：`Failed` + 写入动作 → `Undeliverable{reason}` 且无重复注记；`Deferred` → 动作不变 + 注记；非写入动作 → 动作不变 + 注记。

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --lib diff::`
Expected: `test result: ok.`

- [ ] **Step 5: 写失败测试——交付层不再有静默跳过**

在 `src/apply/mod.rs` 的 `mod tests` 里加：

```rust
#[test]
fn a_planned_delivery_without_content_is_a_reported_failure() {
    // 计划说要装，内容层却没有它。今天这里是 `continue`——静默跳过。
    // 就算 diff 已经把这种条目变成 undeliverable，这一层也要自己兜住：
    // 万一以后有人改了 compute，坏消息必须仍有一个出口。
    let t = TempHome::new();
    let adapter = FakeAdapter::native("claude-code").with_skills_dir(".claude/skills");
    let plan = plan_with_entries(vec![PlanEntry::new(
        CapId::new(Kind::Skill, "ghost"),
        Action::Install,
    )]);
    let report = execute(&plan, &ContentIndex::default(), &mut Ledger::default(), &t.home);
    let r = report.for_harness("claude-code");
    assert!(
        r.failure.as_deref().unwrap_or_default().contains("ghost"),
        "缺席的内容必须报成失败并点名是哪一条，实际 {r:?}"
    );
    assert!(!t.path(".claude/skills/ghost").exists(), "什么都没写才对");
}

#[test]
fn a_plugin_install_without_content_is_not_a_failure() {
    // 这条钉住上一条的作用范围。内容层只解析 skill，所以 plugin / mcp 的
    // install 条目「没有内容」是正常状态（它们的交付路径在 B3）。
    // 若把那段 `continue` 无差别改成失败，每一次带 plugin 的 apply 都会
    // 刷出一串假故障——这条测试就是那个改动的绊线。
    let t = TempHome::new();
    let plan = plan_with_entries(vec![PlanEntry::new(
        CapId::new(Kind::Plugin, "superpowers"),
        Action::Install,
    )]);
    let report = execute(&plan, &ContentIndex::default(), &mut Ledger::default(), &t.home);
    assert!(
        report.for_harness("claude-code").failure.is_none(),
        "plugin 的交付还没做，那不是失败，实际 {:?}",
        report.for_harness("claude-code")
    );
}
```

（`plan_with_entries`、`for_harness` 按该测试模块已有的构造手法与访问器写；`execute` 的签名以实际为准——若它接受 `&[HarnessInput]` 之外的形状，照实调整。）

- [ ] **Step 6: 运行，确认失败**

Run: `cargo test --lib apply::tests::a_planned_delivery_without_content`
Expected: FAIL —— `failure` 是 `None`。

- [ ] **Step 7: 实现**

把 `deliver_skills` 里那段「计划要装、`content.sources` 里没有」的 `continue` 改成记一条失败——**但只对 skill 生效**：

```rust
    let Some(src) = content.sources.get(&e.id) else {
        // 内容层只解析 skill（`Spec::Skill` 是唯一的来源形状），所以 mcp 与 plugin
        // 的 install 条目走到这里时「没有内容」是**正常**的：它们的交付路径还没做
        // （B3）。把那种情况报成失败，会让每一次带 plugin 的 apply 都刷出一串
        // 假故障，真正该被看见的 skill 失败反而被淹掉。
        if e.id.kind == Kind::Skill {
            // skill 计划说要交付、内容层却没有它：这不是「跳过」，是「要做的没做成」。
            // 重跑是修复路径（§9.7），但重跑的前提是用户知道出事了。
            failures.push(format!("{}：源没有解析出内容，未交付", e.id.label()));
        }
        continue;
    };
```

（用该函数已有的累计方式与措辞风格；失败最终要落到 `HarnessReport.failure`，与既有的单条失败合并，不要覆盖。`Kind` 已在该文件的作用域里——`actual_by_kind` 就在用它。）

- [ ] **Step 8: 运行，确认通过**

Run: `cargo test --lib apply::`
Expected: `test result: ok.`

- [ ] **Step 9: 全量回归**

Run: `cargo test`
Expected: `test result: ok.`

---

### Task 7: `stale` 的可见性——带注记的行不再被合并或汇总

§9.4 要求把沿用缓存的条目标 `stale`。`diff` 今天把连续的 `install` 合并成一行、把 `in-sync` 汇总成一行，两处都会把注记吃掉——「不报失败」于是变成「什么都不说」，静默过期又回来了。这一任务让注记在渲染上真的有位置。

**Files:**
- Modify: `src/diff.rs`（把 `stale` 变成注记）
- Modify: `src/render.rs`（带注记的 install 不合并、带注记的 in-sync 不汇总）
- Test: 两文件的 mod tests

**Interfaces:**
- Consumes: `ResolvedContent.stale`（Task 5）、`PlanEntry.note`（既有）。
- Produces: 无新 API（行为变更）。

- [ ] **Step 1: 写失败测试——`stale` 变成注记**

在 `src/diff.rs` 的 `mod tests` 里加：

```rust
#[test]
fn a_stale_source_marks_every_entry_it_touches() {
    // §9.4：沿用缓存内容时条目要标 stale，且**不报失败**。
    let mut content = ContentIndex::default();
    content.sources.insert(
        CapId::new(Kind::Skill, "foo"),
        ResolvedContent {
            path: PathBuf::from("/cache/foo"),
            hash: "a".repeat(64),
            source_id: "file:///up@main@abc".into(),
            stale: Some("未能刷新上游".into()),
        },
    );
    let plan = compute(&[skill("foo")], &inputs(), &DegradeTable::default(), &content, &Ledger::default());
    let e = entry_for(&plan, "claude-code", "foo");
    assert!(matches!(e.action, Action::Install), "stale 不改动作");
    assert!(
        e.note.as_deref().unwrap_or_default().contains("未能刷新"),
        "实际 note：{:?}",
        e.note
    );
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --lib diff::tests::a_stale_source`
Expected: FAIL —— note 是 `None`。

- [ ] **Step 3: 实现——`content_verdict` 认得 `stale`**

`content_verdict` 有三个出口都在构造 `Verdict { note: None }`（`TargetContent::NoContent`、`Has(h) if h == &src.hash`、`Has(h)` 且账本吻合）。把注记的来源收成一处：

```rust
/// 源这次是「沿用缓存」时给行尾的注记（§9.4）。它不参与任何判定，
/// 只在报告里把「你看到的这份内容没经过上游确认」说出来。
fn stale_note(src: &ResolvedContent) -> Option<String> {
    src.stale.clone()
}
```

把 `content_verdict` 里每个 `note: None` 换成 `note: stale_note(src)`；`Has(h)` 那条已有 `Some("副本被手改过")` 的出口改成把两件事拼起来（两者可以同时成立）：

```rust
                Some(_) => Some(Verdict {
                    action,
                    note: Some(match stale_note(src) {
                        Some(s) => format!("副本被手改过；{s}"),
                        None => "副本被手改过".to_string(),
                    }),
                }),
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --lib diff::`
Expected: `test result: ok.`

- [ ] **Step 5: 写失败测试——渲染不再吃掉注记**

在 `src/render.rs` 的 `mod tests` 里加（沿用该文件既有的 `Plan` 构造手法）：

```rust
#[test]
fn a_noted_install_is_not_merged_into_the_coalesced_line() {
    // `+ install  a, b, c` 这一行的价值是短；但当其中一条带着
    // 「内容没经过上游确认」时，短就是错。带注记的那条自己一行。
    let plan = plan_with(vec![
        PlanEntry::new(CapId::new(Kind::Skill, "a"), Action::Install),
        PlanEntry::new(CapId::new(Kind::Skill, "b"), Action::Install)
            .with_note(Some("未能刷新上游".into())),
        PlanEntry::new(CapId::new(Kind::Skill, "c"), Action::Install),
    ]);
    let out = render(&plan);
    assert!(out.contains("+ install") && out.contains("a, c"), "实际：\n{out}");
    assert!(out.contains("b [未能刷新上游]"), "带注记的条目要单独一行：\n{out}");
}

#[test]
fn a_noted_in_sync_is_not_folded_into_the_summary() {
    // 汇总行 `✓ in-sync skills:1` 装不下注记；一条 stale 的 in-sync
    // 恰恰是「看着没事、其实没确认过」，最需要被看见。
    let plan = plan_with(vec![PlanEntry::new(
        CapId::new(Kind::Skill, "a"),
        Action::InSync,
    )
    .with_note(Some("未能刷新上游".into()))]);
    let out = render(&plan);
    assert!(out.contains("in-sync") && out.contains("a"), "实际：\n{out}");
    assert!(out.contains("未能刷新上游"), "注记必须显示出来：\n{out}");
}
```

- [ ] **Step 6: 运行，确认失败**

Run: `cargo test --lib render::tests::a_noted`
Expected: 两条都 FAIL（install 被合并进 `a, b, c`；in-sync 被折成汇总行）。

- [ ] **Step 7: 实现渲染改动**

`render_harness` 里：

```rust
    // InSync 只在带注记时才逐条渲染：注记是「内容可能过时 / 未能确认」
    // 这类必须被看见的信息，汇总成一行就丢了（§9.4 的 stale）。
    let pending: Vec<&PlanEntry> = h
        .entries
        .iter()
        .filter(|e| !matches!(e.action, Action::InSync) || e.note.is_some())
        .collect();
```

install 的合并循环加上注记判据，并补一条自己的分支：

```rust
            Action::Install if e.note.is_none() => {
                let mut labels = vec![e.id.label()];
                while i < pending.len()
                    && matches!(pending[i].action, Action::Install)
                    && pending[i].note.is_none()
                {
                    labels.push(pending[i].id.label());
                    i += 1;
                }
                let _ = writeln!(out, "{lead}{}{}", action_cell("+ install"), labels.join(", "));
            }
            Action::Install => {
                let line = format!("{lead}{}+ install  {}", action_cell(""), e.id.label());
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
```

（对齐列宽用该文件已有的 `action_cell`/`indent` 手法，别新造一套；把 `+ install` 那一格的宽度算对。）

`Action::InSync => {}` 那条空臂改成真的渲染：

```rust
            Action::InSync => {
                let line = format!("{lead}{}{}", action_cell("✓ in-sync"), e.id.label());
                let _ = writeln!(out, "{}", with_note(line, e.note.as_ref()));
            }
```

并把 `with_note` 的文档注释里那句「install 的合并行不用它」改成现在的规矩（合并行只用在不带注记的那些行上）。

- [ ] **Step 8: 运行，确认通过**

Run: `cargo test --lib render::`
Expected: `test result: ok.`（既有渲染测试全绿：既有 fixture 里没有 git 源，也就没有注记，渲染输出不变）

- [ ] **Step 9: 全量回归 + 快照**

Run: `cargo test`
Expected: `test result: ok.`，且 `insta` 快照**无变更**（`cargo insta review` 无待处理）。若有 golden 变化，先停下核对：那说明某条既有 fixture 意外产生了注记。

---

### Task 8: `henv skills list`

§9.8 的入口：用户给出一个 git 地址，第一件事是「看看里面有什么」。它必须是**只读**的——不写 harness 目录、不写账本、不碰用户的 `~`。裁定（第 3 条）把它 clone 到 henv 自己缓存下的唯一子目录并在结束后删掉，而不是系统临时目录：§9.12 已经把缓存位置定在这里，多一个 `/tmp` 只会多一处要解释的东西。

**Files:**
- Modify: `src/store/git.rs`（`list_skills`）
- Modify: `src/lib.rs`（`run_skills_list`）
- Modify: `src/main.rs`（`Command::Skills`）
- Test: `src/store/git.rs`、`tests/` 下的 CLI 级测试

**Interfaces:**
- Consumes: `git_output`、`repo_url`、`repo_key`（Task 3/4）。
- Produces:
  - `store::git::list_skills(repo: &str, sub: Option<&str>, git_ref: Option<&str>, cache: &Path) -> anyhow::Result<Vec<(String, String)>>`（(名字, 仓库内相对路径)，按路径排序）
  - `lib::run_skills_list(repo: &str, git_ref: Option<&str>, sub: Option<&str>, home: &Home) -> anyhow::Result<String>`（返回要打印的整段文本，便于测试断言）

- [ ] **Step 1: 写失败测试——`list_skills` 找到所有含 `SKILL.md` 的目录**

在 `src/store/git.rs` 的 `mod tests` 里加：

```rust
#[test]
fn list_skills_finds_every_directory_holding_a_skill_md() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.write("skills/bar/SKILL.md", "# bar");
    repo.write("skills/bar/nested/SKILL.md", "# nested");
    repo.write("skills/not-a-skill/README.md", "nope");
    repo.commit("init");

    let got = list_skills(&repo.url, None, None, &t.home.henv_git_cache()).unwrap();
    assert_eq!(
        got,
        vec![
            ("bar".to_string(), "skills/bar".to_string()),
            ("nested".to_string(), "skills/bar/nested".to_string()),
            ("foo".to_string(), "skills/foo".to_string()),
        ],
        "按仓库内相对路径排序，名字取目录名"
    );
}

#[test]
fn list_skills_stays_inside_the_requested_sub_path() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.write("other/baz/SKILL.md", "# baz");
    repo.commit("init");

    let got = list_skills(&repo.url, Some("skills"), None, &t.home.henv_git_cache()).unwrap();
    assert_eq!(got, vec![("foo".to_string(), "skills/foo".to_string())]);
}

#[test]
fn list_skills_cleans_up_after_itself() {
    // 只读的意思是「除了 henv 自己缓存里的那个唯一子目录，什么都不留下」。
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");

    let cache = t.home.henv_git_cache();
    list_skills(&repo.url, None, None, &cache).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(&cache)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "列完就该删干净，实际剩下 {leftovers:?}");
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test --lib store::git::tests::list_skills`
Expected: 编译失败，`cannot find function `list_skills``。

- [ ] **Step 3: 实现 `list_skills`**

```rust
/// 列出一个仓库里所有含 `SKILL.md` 的目录（§9.8）。
///
/// 只读：它只写 henv 自己缓存下的一个**进程独占**子目录，并在返回前删掉。
/// 放缓存下而不是系统临时目录，是因为 §9.12 已经把 henv 的缓存定在这里，
/// 少一处要解释的路径；独占名字（repo-key + pid）让两个并发的 list 不打架。
pub fn list_skills(
    repo: &str,
    sub: Option<&str>,
    git_ref: Option<&str>,
    cache: &Path,
) -> anyhow::Result<Vec<(String, String)>> {
    let scratch = cache.join(format!("list-{}-{}", repo_key(repo), std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch)?;
    }
    std::fs::create_dir_all(&scratch)?;

    let result = (|| -> anyhow::Result<Vec<(String, String)>> {
        let url = repo_url(repo);
        git_output(&scratch, &["clone", "--quiet", &url, "repo"])?;
        let dir = scratch.join("repo");
        if let Some(r) = git_ref {
            git_output(&dir, &["checkout", "--quiet", r])?;
        }
        let root = match sub {
            Some(s) => dir.join(s),
            None => dir.clone(),
        };
        anyhow::ensure!(root.is_dir(), "仓库里没有目录 `{}`", sub.unwrap_or(""));

        let mut found: Vec<(String, String)> = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d)? {
                let p = entry?.path();
                if p.is_dir() {
                    // 跳过 .git：里面不会有 SKILL.md，但遍历它既慢又容易踩到怪东西。
                    if p.file_name().is_some_and(|n| n == ".git") {
                        continue;
                    }
                    stack.push(p);
                }
            }
            if d.join("SKILL.md").is_file() {
                let rel = d
                    .strip_prefix(&dir)
                    .unwrap_or(&d)
                    .to_string_lossy()
                    .to_string();
                let name = d
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                found.push((name, rel));
            }
        }
        found.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(found)
    })();

    // 无论成败都删掉这次用的临时目录：留着的话下次要靠 pid 撞不上去，
    // 而残留的半个 clone 会让人以为缓存里有东西。
    let _ = std::fs::remove_dir_all(&scratch);
    result
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --lib store::git`
Expected: `test result: ok.`

- [ ] **Step 5: 写失败测试——渲染与 CLI**

在 `src/lib.rs` 的 `mod tests` 里加：

```rust
#[test]
fn skills_list_prints_a_name_padded_to_the_longest_one() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.write("skills/longer-name/SKILL.md", "# longer");
    repo.commit("init");

    let out = run_skills_list(&repo.url, None, None, &t.home).unwrap();
    assert_eq!(
        out,
        "foo          skills/foo\nlonger-name  skills/longer-name\n"
    );
}

#[test]
fn skills_list_writes_nothing_outside_henv_cache() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");

    run_skills_list(&repo.url, None, None, &t.home).unwrap();
    for rel in [".claude", ".codex", ".kimi", ".agents", ".local"] {
        assert!(!t.path(rel).exists(), "{rel} 不该被创建");
    }
}
```

- [ ] **Step 6: 运行，确认失败**

Run: `cargo test --lib lib::tests::skills_list`
Expected: 编译失败，`cannot find function `run_skills_list``。

- [ ] **Step 7: 实现 `run_skills_list` 与 CLI**

`src/lib.rs`：

```rust
/// `henv skills list`：给一个 git 地址，列出里面所有含 `SKILL.md` 的目录（§9.8）。
///
/// 只读，且不要求清单。名字对齐到最长的那个，好看且便于 `awk '{print $2}'`。
pub fn run_skills_list(
    repo: &str,
    git_ref: Option<&str>,
    sub: Option<&str>,
    home: &Home,
) -> anyhow::Result<String> {
    let found = store::git::list_skills(repo, sub, git_ref, &home.henv_git_cache())?;
    if found.is_empty() {
        return Ok(format!("没有找到含 SKILL.md 的目录（看了 {repo}）\n"));
    }
    let width = found.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
    let mut out = String::new();
    for (name, rel) in found {
        let _ = writeln!(out, "{name}{}  {rel}", " ".repeat(width - name.chars().count()));
    }
    Ok(out)
}
```

`src/main.rs`：加

```rust
    /// 列出一个 git 仓库里所有包含 SKILL.md 的目录（§9.8）。
    ///
    /// 只读：不写 harness 目录、不写账本。唯一的写入是 henv 自己缓存下的
    /// 一个临时子目录，命令结束前删掉。
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
```

以及

```rust
#[derive(Subcommand)]
pub enum SkillsCmd {
    /// 列出仓库里的 skill（名字 + 仓库内相对路径）。
    List {
        /// 仓库地址，或 GitHub 的 owner/repo 简写。
        #[arg(long)]
        repo: String,
        /// 分支 / tag / 提交。缺省用仓库的默认分支。
        // [2026-09-26 执行期修正] `#[arg(long)]` 会按字段名生出 `--git-ref`，与 §9.8
        // 承诺的 `--ref` 不符（照规格敲会得到「unexpected argument」）。值名也一并
        // 对齐，免得帮助里显示 `<GIT_REF>`。
        #[arg(long = "ref", value_name = "REF")]
        git_ref: Option<String>,
        /// 只看仓库里的这个子目录。
        #[arg(long)]
        path: Option<String>,
    },
}
```

分派到 `run_skills_list(&repo, git_ref.as_deref(), path.as_deref(), &home)` 并 `print!("{out}")`。

- [ ] **Step 8: 运行，确认通过**

Run: `cargo test --lib && cargo test`
Expected: `test result: ok.`

- [ ] **Step 9: 手工验收一次真实 CLI（用本地夹具仓，不联网）**

Run:
```bash
mkdir -p /tmp/henv-smoke && cd /tmp/henv-smoke && git init -q r && cd r && mkdir -p skills/foo && echo '# foo' > skills/foo/SKILL.md && git add -A && git commit -qm init && cd .. && cargo run --manifest-path /Users/nava/Code/tools/henv/Cargo.toml -- skills list --repo "file:///tmp/henv-smoke/r"
```
Expected: 打印 `foo  skills/foo`，退出码 0。（`/tmp` 下的这个仓库是**手工冒烟**用的，不进测试；测试一律走 `TempHome`。）

- [ ] **Step 10: 清理冒烟残留**

Run: `rm -rf /tmp/henv-smoke`
Expected: 无输出。

---

### Task 9: 端到端验收与文档收口

把 B2 的四条承诺在一条完整链路上验一遍，并把设计文档改成与代码一致。

**Files:**
- Modify: `docs/design.md`（§9.8 的 `skills list` 那一行、§9 的进度段）
- Test: `tests/` 下的 CLI 级端到端测试（沿用 B1 的 `tests/smoke.rs` 或同级文件）

**Interfaces:**
- Consumes: 前面全部任务。
- Produces: 无。

- [ ] **Step 1: 写端到端测试——git 源的完整闭环**

新建 `tests/git_source_end_to_end.rs`：

```rust
//! §9.10 的四条性质在 git 源上的样子：不联网、冲突即停、幂等、roundtrip。

use henv::testutil::TempHome;

#[test]
fn a_git_skill_goes_from_absent_to_delivered_to_in_sync() {
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");
    t.write("env.yaml", &manifest(&repo.url));

    // 1) diff 不联网：缓存是空的，报「这次判不了」，且不改盘。
    let before = henv::run_diff(&t.path("env.yaml"), &t.home, /* detail */ false).unwrap().text;
    assert!(before.contains("unknown"), "实际：\n{before}");
    assert!(!t.path(".claude/skills/foo").exists(), "diff 不许写盘");

    // 2) apply 会去取，并真的交付。
    henv::run_apply(&t.path("env.yaml"), &t.home, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
        "# foo"
    );

    // 3) 幂等：再跑一次 diff，计划里没有待办。
    let after = henv::run_diff(&t.path("env.yaml"), &t.home, false).unwrap().text;
    assert!(!after.contains("install"), "第二次应当无事可做：\n{after}");

    // 4) 账本记着来源：`<repo>@<ref>@<sha>`（§9.3）。
    let ledger = std::fs::read_to_string(t.home.henv_provenance()).unwrap();
    let expect = format!("{}@main@", repo.url);
    let at = ledger.find(&expect).unwrap_or_else(|| panic!("账本里没有 {expect}：{ledger}"));
    let after = &ledger[at + expect.len()..];
    assert!(
        after.len() >= 40 && after[..40].chars().all(|c| c.is_ascii_hexdigit()),
        "`@main@` 后面要跟一个 40 位的提交 sha，实际：{after:.60}"
    );

    // 5) 上游不动、缓存丢不掉：再 apply 一次仍然 in-sync，内容不被重写。
    let mtime = std::fs::metadata(t.path(".claude/skills/foo/SKILL.md")).unwrap().modified().unwrap();
    henv::run_apply(&t.path("env.yaml"), &t.home, true).unwrap();
    assert_eq!(std::fs::metadata(t.path(".claude/skills/foo/SKILL.md")).unwrap().modified().unwrap(), mtime);
}
```

（`manifest(&url)` 是本文件里的一个小辅助；`run_diff`/`run_apply` 的返回形状按 `src/lib.rs` 的实际签名写；`sha_prefix` 若不好写就直接断言 `@main@` 后面跟着 40 位十六进制——用 `ledger.contains("@main@")` 加一段正则式的循环断言即可，**不要**为了这条测试给生产代码加新 API。）

- [ ] **Step 2: 运行，确认它真的在验行为**

Run: `cargo test --test git_source_end_to_end`
Expected: `test result: ok.`（若这一步直接通过，逐条改动上面五个断言之一再跑，确认每条都真的会失败——一条不会失败的断言等于没有。）

- [ ] **Step 3: 写端到端测试——离线时的诚实**

```rust
#[test]
fn an_offline_rerun_reports_stale_instead_of_failing() {
    // §9.4 的核心承诺：离线也要能用，且不假装成功。
    let t = TempHome::new();
    let repo = t.git_repo("upstream");
    repo.write("skills/foo/SKILL.md", "# foo");
    repo.commit("init");
    t.write("env.yaml", &manifest(&repo.url));
    henv::run_apply(&t.path("env.yaml"), &t.home, true).unwrap();

    // 上游没了（离线的最强形式）。
    std::fs::remove_dir_all(&repo.dir).unwrap();
    // 目标被清空，迫使计划里出现「要装」这条 —— stale 才有地方显示。
    std::fs::remove_dir_all(t.path(".claude/skills/foo")).unwrap();

    let out = henv::run_apply(&t.path("env.yaml"), &t.home, true).unwrap();
    let text = format!("{out}");
    assert!(
        text.contains("未能刷新"),
        "沿用缓存时要标出来，实际：\n{text}"
    );
    assert!(
        std::fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap() == "# foo",
        "离线也要真的把内容装上去"
    );
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test --test git_source_end_to_end`
Expected: `test result: ok.`

- [ ] **Step 5: 改设计文档**

`docs/design.md` §9.8：

- 把 `` `skills list`：clone 到临时目录，列出所有含 `SKILL.md` 的目录（名字 + 仓库内相对路径）。**只读**，不写任何东西。`` 改成与裁定一致的措辞：clone 落在 henv 自己缓存下的一个进程独占子目录（`~/.cache/henv/git/list-<repo-hash>-<pid>/`），返回前删除；除这个临时目录外不写任何东西。
- 在 §9 的进度段补 B2：git 源（`source: git`）已交付；`henv skills list` 已交付；判定模式（`diff` 不联网 / `apply` fetch）与 §9.4 的 `stale` 已落；B3（MCP 写入、bundle 分解、`degrade:` 生效、`--prune`）未做。

- [ ] **Step 6: 全量回归**

Run: `cargo test`
Expected: `test result: ok.`（lib 若干 + guard 8 + 端到端若干 + smoke 1 ignored）

- [ ] **Step 7: 只读守卫的最终核对（人工读一遍，不是跑测试）**

Run: `cargo test --test read_only_guard -- --nocapture`
Expected: 全绿；并确认 `PROCESS_ALLOWLIST` 仍然只有 `src/store/git.rs` 与 `src/testutil.rs` 两项——若实现过程中为了别的文件加过第三项，那是一次需要写进报告的让步。

---

## Self-Review

**1. Spec coverage（对着 §9 逐条点）**

| Spec | 落在哪 |
| --- | --- |
| §9.2 源解析是唯一的前置 I/O | Task 5（`resolve_sources` 是唯一入口）；`plan_for` 加模式参 |
| §9.2 git 源解析出「本地内容目录 + sha」 | Task 4 `resolve` |
| §9.3 来源标识 `repo@ref@sha` | Task 5 Step 1 的断言 |
| §9.3 两条不变式 | 不变式一/二在 B1 已实现；B2 不改交付层，只在 Task 6 补失败上报（不碰既有覆盖规则） |
| §9.4 staleness + 离线可用 | Task 4 `stale`、Task 5 传播、Task 7 可见性、Task 9 Step 3 端到端 |
| §9.7 失败隔离 | Task 6（失败上报进 `HarnessReport.failure`，不牵连同批） |
| §9.8 `skills list` | Task 8（含 CLI 形状 `--repo/--ref/--path`） |
| §9.8 除 apply 外不提问 | 无交互代码新增；`skills list` 无确认 |
| §9.9 守卫收窄 | Task 1 |
| §9.10 测试策略 | Task 4/8 的密闭夹具、Task 9 的端到端 |
| §9.12 shell out 系统 git、不引 git2、缓存位置 | Task 3/4（`Cargo.toml` 无改动即证据） |
| §4 的 `obra/superpowers` 简写 | Task 3 `repo_url` |
| 能力键必须是单个路径分量（安全复查带出，非 spec 明文） | Task 3 Step 11–14：skill 已有一条，补上 mcp / plugin |

**2. Placeholder scan**：无「TBD / 稍后补 / 类似 Task N」。每个代码步骤都是可粘贴的实体。Task 6 Step 3 与 Task 7 Step 7 给了语义约束 + 等价实现建议（那里的最终形状取决于该文件既有的辅助函数名），这是刻意的：写死一份与真实文件不合的代码比写清约束更糟。

**3. Type consistency**：`SourceMode` 只有一份定义（`store::git`），`skills.rs` 用 `pub use` 重导出，Task 5/8 的调用方都用 `SourceMode::{Cached, Fetch}`；`ResolvedContent.stale: Option<String>` 在 Task 5 定义、Task 7 消费；`Unresolved::{Deferred, Failed}` 在 Task 5 定义、Task 6 消费；`repo_key` 返回 16 位十六进制、`hash_str` 返回 64 位，Task 3 的断言与 Task 4 的路径拼接一致。

**4. Review Focus**：五类逐条有测试（见 Review Focus 段末尾的粗体指针），分布 Task 3/4/5/6。

---

## 需要用户裁定的七处

前三条是**已经问过、你已裁定**的（记录在此，执行者照办，不重新讨论）：

1. **`diff` 遇到 git 源联不联网** → 你选「diff 只读缓存，不联网」。落地：`SourceMode::Cached` 下缓存里没有就报 `Deferred`，行尾注记写明「diff 不联网；apply 会去取」。**代价**：新机器上第一次 `diff` 对每个 git 源都显示「判不了」，直到跑过一次 `apply`。
2. **只读守卫怎么给 shell out 开口子** → 你选「拆表，按具体文件开口子」。落地：Task 1，`PROCESS_ALLOWLIST = ["src/store/git.rs", "src/testutil.rs"]`，且名单受测试保护（必须是存在的具体文件、不得是目录）。
3. **`skills list` 的 clone 落在哪** → 你选「henv 缓存下的唯一子目录」。落地：Task 8，`~/.cache/henv/git/list-<repo-hash>-<pid>/`，返回前删除。

后四条是我在计划里替你做了的判断，**现在提出来给你否掉的机会**：

4. **`owner/repo` 简写硬编码展开到 GitHub**。§4 的示例 `repo: obra/superpowers` 是 GitHub 简写，git 自己不认（会当相对路径），所以必须有个约定。我选 `https://github.com/<owner>/<repo>.git` 作为唯一默认，写别家请在清单里写全 URL。**代价**：若你以后常用 GitLab 简写，那会是另一个任务（加主机名白名单或让清单显式写主机）。
5. **把「B1 遗留缺陷：未解析的源被静默跳过」并进 B2**（Task 6）。理由：离线是 git 源的常态，这个缺陷在 B2 会从罕见变常见，放着不管等于 B2 自带一个静默失败。**代价**：B2 的 diff 从 5 条变成 9 条，范围比原计划大一点。
6. **是否逐任务 commit**。B1 的做法是全程不 commit、由你决定时机；B2 的每个任务结尾我都留了「不 commit」的默认。若你希望这次逐任务落 commit（每个任务一条英文 conventional commit），在开工前说一声，我按「Task N: …」的粒度提交。
7. **把 mcp / plugin 的键校验补上（Task 3 Step 11–14）**。起因是一次外部安全复查指着 `src/apply/mod.rs` 的 `target_dir.join(&e.id.name)` 说路径穿越。核对下来**那一行不成立**：能到那里的名字全部来自清单里的 skill 键，而 skill 键已被 `name_is_a_single_component` 挡住（`src/manifest.rs:496`）。但同一条约束**只加在 skills 上**，`mcps` 与 `plugins` 的键没查——而 B3 正是要拿这两个名字拼路径。所以我把这三行校验补进 B2，理由是同一次 `validate()` 改动、同一类问题。**代价**：清单里带 `/` 的 mcp / plugin 键会从「今天接受」变成「报错」（没有既有用户，实际代价接近零；如果你的清单里真有这种键，说一声我改成只警告）。

---

## Execution Handoff

计划已保存到 `docs/superpowers/plans/2026-09-25-henv-plan-b2-git-sources.md`。请先过一遍，尤其是「需要用户裁定的七处」的后四条。

选执行方式：

- **Native（本会话内联执行，推荐）** —— B2 的九个任务是一条链：Task 1 的允许位被 Task 4 消费，Task 3 的 `repo_key` 被 Task 4/8 消费，Task 5 的类型被 Task 6/7 消费，Task 4 的夹具被 Task 5/8/9 消费。接口咬得紧，内联执行加一份账本（ledger）能在同一个上下文里守住这些接口，而且最后仍有一次全新 reviewer 的整体复查。成本最低、最快。
- **Subagent-driven** —— 每个任务一个全新 subagent 实现、一个全新 reviewer 复查，再整体复查。最彻底；代价是每个任务与每次复查各付一份全新上下文，且跨任务的接口要靠 brief 转述，而 B2 恰恰是接口密集型。

推荐 **Native**。请确认计划是否符合你要的，以及选哪种执行方式。

（另：B0+B1 那份改动仍在分支 `plan-b-apply` 上，27 个文件已 `git add`、零提交，工作树没动。你说一句「提交」我就提交；也可以你自己 `! git commit -F -`；想退回原状就 `git checkout main` + `git branch -d plan-b-apply`。B2 的开工不依赖它，但两者同时改 `src/diff.rs`、`src/lib.rs`、`src/store/skills.rs` —— 先落 B0+B1 再开 B2 会顺很多。）
