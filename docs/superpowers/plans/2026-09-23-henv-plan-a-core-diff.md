# henv Plan A — 只读 diff 管道 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付可用的 `henv diff`——解析 `env.yaml`、识别本机三家 harness 现状、输出「切 harness 时什么会失效」的只读报告，全程零写入。

**Architecture:** 单 crate，`lib.rs` + 薄 `main.rs`。清单解析（`manifest` → `lower`）产出归一化 `Vec<Capability>`；三家 adapter 各自 `read_state()` 把真实配置树归一化成同一模型；`diff::compute` 是纯函数 `f(desired, actual) → Plan`；`render` 单独负责终端输出。所有 adapter 接受可注入的 `Home` 根目录，测试用 `TempDir` 假 HOME，绝不触碰真实 `~`。

**Tech Stack:** Rust 1.85+（edition 2024）、clap 4.6 derive、serde 1、serde_yaml_ng 0.10、serde_json 1.0、toml 1.1、anyhow 1.0；dev: insta 1.48、tempfile 3.27。

**Spec:** `docs/design.md` — 特别注意 **§2.1 实测修正**。该节经本机盘点写成，与上文冲突处**以 §2.1 为准**（claude-code 的 MCP 在 `~/.claude.json` 而非 `settings.json`；plugin 身份比较 SHA 优先；kimi-code 无用户级 skills 目录；`serde_yaml` 已弃用）。

## Global Constraints

- **本计划只读。** 不创建、不修改、不删除 `Home` 根目录下任何文件。写入路径（skills store / apply / merge-safe writer / `.bak` / `claude mcp add-json`）属 Plan B。
- 依赖锁定：`anyhow = "1"`、`clap = { version = "4.6", features = ["derive"] }`、`serde = { version = "1", features = ["derive"] }`、`serde_json = "1"`、`serde_yaml_ng = "0.10"`、`toml = "1.1"`；dev：`insta = "1.48"`、`tempfile = "3"`。**不得**引入 `serde_yaml`（已弃用）、`regex`、`git2`、`toml_edit`、`dirs`、`similar`——本计划都不需要。
- `rust-version = "1.85"`，edition `2024`。
- **HOME 必须可注入。** 任何模块都不得直接调用 `std::env::home_dir` / `dirs::home_dir`；一律经 `Home`。`Home::resolve()` 优先 `$HENV_HOME`，其次 `$HOME`。
- harness 名常量只有三个：`"claude-code"`、`"codex"`、`"kimi-code"`。
- **密钥语法即门禁**（spec §4 决策 2）：`mcps.*.env` 的值必须严格匹配 `${VAR}`，任何字面量在 `Manifest::parse` 阶段报错。密钥永不展开进任何产物，错误信息不得回显值。
- **plugin 身份 = entry 名**（spec §2.1 第 3 条）。`superpowers@claude-plugins-official`、`superpowers@openai-api-curated`、kimi 的 `id: superpowers` 三者身份同为 `superpowers`；marketplace 是 Spec 细节，不参与身份。
- **版本比较 SHA 优先、version 兜底**；一侧有 SHA 一侧只有 version → `Unknown`，diff 按 in-sync 处理（只读工具不误报）。
- 所有集合遍历必须**确定性排序**（`BTreeMap` / 显式 `sort`），否则 insta 快照与 diff 输出会抖动。
- 错误一律 `anyhow::Result`，用 `.context()` 带上文件路径。**配置文件缺失不是错误**（返回空），语法错误才是。`resolve()` 这类不打开文件的错误不适用后半句——它没有路径可带，只要消息本身点明了缺哪个环境变量即可。
- **RED 必须真的红。** 各 Task 把「在 `src/lib.rs` 声明模块」排在写测试之后，但 Rust 只编译被声明的模块：模块没进 `lib.rs` 时 `cargo test --lib foo::` 连测试文件都不构建，报 `0 passed; 0 failed` 并以 0 退出——那是**假绿**，不是预期的红。所以**写第一个测试的同一步骤里就把 `pub mod foo;` 加进 `src/lib.rs`**，后面「声明模块」那一步只用于核对最终形态（含该 Task 新增的模块，顺序按字母表）。判定标准：RED 运行必须出现与被测符号对应的编译错误（`cannot find`、`E0599`、`unresolved`）或真实断言失败；`test result: ok. 0 passed` 一律不算通过。
- 提交信息用 Conventional Commits（`feat:` / `test:` / `chore:`）。

## 接口契约（各 Task 共同遵守）

后续 Task 只能看到自己的正文，这一节是它们了解邻居签名的唯一来源。**改这里等于改所有 Task。**

```rust
// src/home.rs
pub struct Home { /* root: PathBuf */ }
impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self;
    pub fn resolve() -> anyhow::Result<Self>;          // $HENV_HOME 优先，其次 $HOME
    pub fn root(&self) -> &Path;
    pub fn agents_skills(&self) -> PathBuf;            // <root>/.agents/skills
    pub fn claude_dir(&self) -> PathBuf;               // <root>/.claude
    pub fn claude_json(&self) -> PathBuf;              // <root>/.claude.json
    pub fn claude_settings(&self) -> PathBuf;          // <root>/.claude/settings.json
    pub fn claude_installed_plugins(&self) -> PathBuf; // <root>/.claude/plugins/installed_plugins.json
    pub fn claude_skills(&self) -> PathBuf;            // <root>/.claude/skills
    pub fn codex_dir(&self) -> PathBuf;                // <root>/.codex
    pub fn codex_config(&self) -> PathBuf;             // <root>/.codex/config.toml
    pub fn codex_skills(&self) -> PathBuf;             // <root>/.codex/skills
    pub fn kimi_dir(&self) -> PathBuf;                 // <root>/.kimi-code
    pub fn kimi_mcp(&self) -> PathBuf;                 // <root>/.kimi-code/mcp.json
    pub fn kimi_installed_plugins(&self) -> PathBuf;   // <root>/.kimi-code/plugins/installed.json
    pub fn harness_dirs(&self) -> Vec<PathBuf>;        // [claude_dir, codex_dir, kimi_dir]
}

// src/capability.rs
pub enum Kind { Skill, Mcp, Command, Agent, Hook, Plugin }
impl Kind { pub fn all() -> [Kind; 6]; pub fn as_str(&self) -> &'static str; pub fn parse(s: &str) -> Option<Kind>; }

pub struct CapId { pub kind: Kind, pub name: String }
impl CapId { pub fn new(kind: Kind, name: impl Into<String>) -> Self; pub fn label(&self) -> String; } // "skill/foo"

pub enum Origin { Manifest, Plugin { name: String }, Adopted { harness: String } }

pub enum SkillSource {
    AgentsDir,
    Git { repo: String, path: Option<String>, git_ref: String },
    Local { path: PathBuf },
    PluginBundle { plugin: String },
    Unknown,
}

pub enum Spec {
    Skill { source: SkillSource, path: Option<PathBuf> },
    Mcp { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    Plugin { marketplace: Option<String>, entry: String, version: Option<String>,
             sha: Option<String>, install_path: Option<PathBuf>, enabled: bool },
    Hook { event: String, command: String },
    Command { path: PathBuf },
    Agent { path: PathBuf },
}

pub enum Targets { All, Only(Vec<String>) }   // Default = All
impl Targets { pub fn includes(&self, harness: &str) -> bool; }

pub enum Support { Native, Degraded { as_kind: Kind }, Unsupported { reason: String } }

pub struct Capability { pub id: CapId, pub origin: Origin, pub spec: Spec, pub targets: Targets }
impl Capability { pub fn new(id: CapId, spec: Spec) -> Self; }    // origin = Manifest, targets = All

pub enum VersionMatch { Same, Different { from: String, to: String }, Unknown }
pub fn compare_versions(desired: &Spec, actual: &Spec) -> VersionMatch;  // from = 现状, to = 期望

// src/secrets.rs
pub fn parse_ref(value: &str) -> Option<&str>;                     // "${FOO}" -> Some("FOO")
pub fn validate_env(key: &str, value: &str) -> Result<(), String>;
pub fn missing(vars: &[String], lookup: &dyn Fn(&str) -> Option<String>) -> Vec<String>;

// src/manifest.rs
pub const SUPPORTED_VERSION: u32;                                  // = 1
pub const KNOWN_HARNESSES: [&str; 3];                              // claude-code, codex, kimi-code
pub struct Manifest { pub version: u32, pub harnesses: BTreeMap<String, HarnessEntry>,
                      pub skills: BTreeMap<String, SkillEntry>, pub mcps: BTreeMap<String, McpEntry>,
                      pub plugins: BTreeMap<String, PluginEntry> }
impl Manifest { pub fn parse(text: &str) -> anyhow::Result<Manifest>; }
pub struct HarnessEntry { pub enabled: bool }                      // 默认 true
pub enum SkillSourceKind { AgentsDir, Git, Local }
pub struct SkillEntry { pub source: SkillSourceKind, pub repo: Option<String>, pub path: Option<String>,
                        pub git_ref: Option<String>, pub targets: Targets }   // git_ref 的 YAML 键是 `ref`
pub struct McpEntry { pub command: String, pub args: Vec<String>,
                      pub env: BTreeMap<String, String>, pub targets: Targets }
pub enum PluginSourceKind { ClaudeMarketplace, Github, ZipUrl }
pub struct PluginEntry { pub source: PluginSourceKind, pub marketplace: Option<String>,
                         pub entry: Option<String>, pub repo: Option<String>,
                         pub git_ref: Option<String>, pub degrade: DegradePolicy, pub targets: Targets }
pub struct DegradePolicy { pub hooks: DegradeAction, pub commands: DegradeAction,
                           pub skills: DegradeAction, pub mcps: DegradeAction, pub agents: DegradeAction }
impl DegradePolicy { pub fn action_for(&self, kind: Kind) -> DegradeAction; }
pub enum DegradeAction { Report, Skip }                            // Default = Report

// src/lower.rs
pub fn lower(m: &Manifest) -> Vec<Capability>;                     // 按 id.label() 排序
pub struct DegradeTable { /* BTreeMap<String, DegradePolicy> */ }
impl DegradeTable { pub fn new(m: &Manifest) -> Self; pub fn action(&self, cap: &Capability) -> DegradeAction; }

// src/skills_dir.rs
pub enum SkillStore { Shared, HarnessLocal, PluginCache, Other }
pub struct SkillEntry { pub name: String, pub path: PathBuf, pub link_target: Option<PathBuf>,
                        pub resolved: Option<PathBuf>, pub store: SkillStore }
impl SkillEntry { pub fn is_broken(&self) -> bool; }
pub fn read_skill_entries(dir: &Path, home: &Home) -> Vec<SkillEntry>;   // 按 name 排序
pub fn classify(resolved: &Path, home: &Home) -> SkillStore;
pub fn plugin_from_path(path: &Path) -> Option<String>;

// src/adapters/mod.rs
pub const CLAUDE_CODE: &str;  pub const CODEX: &str;  pub const KIMI_CODE: &str;
pub trait HarnessAdapter {
    fn name(&self) -> &str;
    fn detect(&self) -> bool;
    fn read_state(&self) -> anyhow::Result<Vec<Capability>>;
    fn supports(&self, cap: &Capability) -> Support;
}
pub fn registry(home: &Home) -> Vec<Box<dyn HarnessAdapter>>;   // 顺序: claude-code, codex, kimi-code
pub fn lookup<'a>(reg: &'a [Box<dyn HarnessAdapter>], name: &str) -> Option<&'a Box<dyn HarnessAdapter>>;

// src/diff.rs
pub struct HarnessState { pub name: String, pub detected: bool, pub actual: Vec<Capability> }
pub enum Action { Install, Update { from: String, to: String }, InSync,
                  Undeliverable { reason: String }, Drift }
impl Action { pub fn rank(&self) -> u8; }
pub struct PlanEntry { pub id: CapId, pub action: Action }
pub struct HarnessPlan { pub harness: String, pub detected: bool, pub entries: Vec<PlanEntry>,
                         pub actual_by_kind: BTreeMap<Kind, usize> }
pub struct Plan { pub harnesses: Vec<HarnessPlan> }
pub struct Counts { pub install: usize, pub update: usize, pub remove: usize,
                    pub undeliverable: usize, pub drift: usize }
impl Plan { pub fn counts(&self) -> Counts; pub fn is_clean(&self) -> bool; }
pub struct HarnessInput<'a> { pub state: &'a HarnessState, pub adapter: &'a dyn HarnessAdapter }
pub fn compute(desired: &[Capability], inputs: &[HarnessInput], degrade: &DegradeTable) -> Plan;

// src/render.rs
pub fn render(plan: &Plan) -> String;

// src/lib.rs
pub fn run_diff(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<()>;
```

## File Structure

| 文件 | 职责 |
|---|---|
| `Cargo.toml` | 依赖与 crate 元数据 |
| `src/lib.rs` | 模块声明 + `run_diff` 编排 |
| `src/main.rs` | clap 入口，仅解析参数后调 `henv::run_diff` |
| `src/home.rs` | HOME 根抽象（唯一路径来源，测试注入点） |
| `src/capability.rs` | 归一化能力模型 + SHA 优先版本比较 |
| `src/secrets.rs` | `${VAR}` 引用校验，永不展开 |
| `src/manifest.rs` | `env.yaml` 类型化解析 + 聚合校验（含密钥门禁） |
| `src/lower.rs` | `Manifest` → `Vec<Capability>` + `DegradeTable` |
| `src/skills_dir.rs` | skills 目录扫描：多跳 symlink 解析、断链、来源分类 |
| `src/adapters/mod.rs` | `HarnessAdapter` trait + 编译期注册表 |
| `src/adapters/claude_code.rs` | `~/.claude/skills`、`~/.claude.json:mcpServers`、`installed_plugins.json` |
| `src/adapters/codex.rs` | `~/.codex/skills`、`config.toml:[mcp_servers.*]`、`[plugins.*]` |
| `src/adapters/kimi_code.rs` | `mcp.json`、`plugins/installed.json`、bundle 内 `skills/` |
| `src/diff.rs` | `f(desired, actual) → Plan` 纯函数 |
| `src/render.rs` | `Plan` → 终端表格（insta golden 的唯一目标） |
| `src/testutil.rs` | `#[cfg(test)]` 假 HOME 构建器 + `FakeAdapter` |
| `examples/env.yaml` | 无密钥示例清单，冒烟测试与文档共用 |
| `tests/read_only_guard.rs` | 静态守卫：`src/` 里不得出现写 API（Plan A 只读承诺的唯一证据） |
| `tests/smoke_real_home.rs` | `#[ignore]` 真实 HOME 只读冒烟 |

**不在本计划内**（Plan B/C）：`src/store/`、`apply`、`.bak`/temp+rename 写入器、`claude mcp add-json` shell out、bundle manifest 探测（`bundle.rs`）、`scan`、`doctor`、`toml_edit`、`similar`、git 操作。

---

### Task 1: Crate 脚手架 + Home 抽象 + 假 HOME 测试工具

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `src/lib.rs`, `src/main.rs`, `src/home.rs`, `src/testutil.rs`

**Interfaces:**
- Consumes: 无（第一个 Task）
- Produces: `Home`（见接口契约）、`testutil::TempHome`

- [ ] **Step 1: 初始化 crate**

```bash
cd /Users/nava/Code/tools/henv
cargo init --lib --name henv --vcs none --edition 2024
```

`--vcs none`：仓库已存在，不要让 cargo 再 init 一次 git。

- [ ] **Step 2: 写 `Cargo.toml`**

```toml
[package]
name = "henv"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
description = "跨 harness 能力环境管理器"

[dependencies]
anyhow = "1"
clap = { version = "4.6", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml_ng = "0.10"
toml = "1.1"

[dev-dependencies]
insta = "1.48"
tempfile = "3"
```

`src/lib.rs` 与 `src/main.rs` 同时存在时 Cargo 自动识别 lib + bin，无需显式 `[[bin]]`。

- [ ] **Step 3: 写 `.gitignore`**

```
/target
```

- [ ] **Step 4: 写失败的测试 —— `src/home.rs`**

整文件（实现留到 Step 6，此时编译失败就是预期的「红」）：

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_harness_paths_under_root() {
        let h = Home::new("/fake/root");
        assert_eq!(h.root(), Path::new("/fake/root"));
        assert_eq!(h.agents_skills(), PathBuf::from("/fake/root/.agents/skills"));
        assert_eq!(h.claude_dir(), PathBuf::from("/fake/root/.claude"));
        assert_eq!(h.claude_json(), PathBuf::from("/fake/root/.claude.json"));
        assert_eq!(h.claude_settings(), PathBuf::from("/fake/root/.claude/settings.json"));
        assert_eq!(
            h.claude_installed_plugins(),
            PathBuf::from("/fake/root/.claude/plugins/installed_plugins.json")
        );
        assert_eq!(h.claude_skills(), PathBuf::from("/fake/root/.claude/skills"));
        assert_eq!(h.codex_dir(), PathBuf::from("/fake/root/.codex"));
        assert_eq!(h.codex_config(), PathBuf::from("/fake/root/.codex/config.toml"));
        assert_eq!(h.codex_skills(), PathBuf::from("/fake/root/.codex/skills"));
        assert_eq!(h.kimi_dir(), PathBuf::from("/fake/root/.kimi-code"));
        assert_eq!(h.kimi_mcp(), PathBuf::from("/fake/root/.kimi-code/mcp.json"));
        assert_eq!(
            h.kimi_installed_plugins(),
            PathBuf::from("/fake/root/.kimi-code/plugins/installed.json")
        );
        assert_eq!(h.harness_dirs().len(), 3);
    }

    #[test]
    fn resolve_prefers_henv_home_over_home() {
        // 不修改进程环境（测试并行会互相踩），直接验证优先级函数
        assert_eq!(
            Home::from_env(Some("/a".into()), Some("/b".into())).unwrap().root(),
            Path::new("/a")
        );
        assert_eq!(
            Home::from_env(None, Some("/b".into())).unwrap().root(),
            Path::new("/b")
        );
        assert!(Home::from_env(None, None).is_err());
        assert!(Home::from_env(Some("".into()), Some("/b".into())).is_err());
    }

    #[test]
    fn root_is_canonicalized_when_it_exists() {
        // macOS 上 /var 是 /private/var 的 symlink，tempfile 给的是 /var/... 而
        // fs::canonicalize 给的是 /private/var/...。两边必须落到同一形态，否则
        // skills_dir::classify 的前缀比较永远失配（真实踩过的坑）。
        let dir = tempfile::TempDir::new().unwrap();
        let h = Home::new(dir.path());
        assert_eq!(h.root(), std::fs::canonicalize(dir.path()).unwrap().as_path());
        assert!(h.agents_skills().starts_with(h.root()));
    }

    #[test]
    fn root_is_kept_verbatim_when_it_does_not_exist() {
        let h = Home::new("/definitely/not/here");
        assert_eq!(h.root(), Path::new("/definitely/not/here"));
    }
}
```

- [ ] **Step 5: 运行测试确认失败**

Run: `cargo test --lib home::`
Expected: FAIL —`Home::new` / `from_env` / 各路径方法未定义，编译错误。

- [ ] **Step 6: 实现 `Home`**

在 `src/home.rs` 的 `use` 之后、`#[cfg(test)]` 之前插入：

```rust
impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let raw = root.into();
        // 目录存在时归一化：macOS 上 /var 是 /private/var 的 symlink，而
        // skills_dir 用 fs::canonicalize 解析 skill 路径。两边形态不一致会让
        // classify 的前缀比较永远失配。目录不存在时保留原值（错误由上层报）。
        let root = std::fs::canonicalize(&raw).unwrap_or(raw);
        Self { root }
    }

    /// $HENV_HOME 优先，其次 $HOME。两者都缺或为空 → 错误。
    pub fn resolve() -> anyhow::Result<Self> {
        Self::from_env(std::env::var_os("HENV_HOME"), std::env::var_os("HOME"))
    }

    fn from_env(
        henv_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
    ) -> anyhow::Result<Self> {
        let raw = henv_home
            .or(home)
            .ok_or_else(|| anyhow::anyhow!("neither HENV_HOME nor HOME is set"))?;
        if raw.is_empty() {
            anyhow::bail!("HENV_HOME/HOME is set but empty");
        }
        Ok(Self::new(PathBuf::from(raw)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn agents_skills(&self) -> PathBuf {
        self.root.join(".agents").join("skills")
    }
    pub fn claude_dir(&self) -> PathBuf {
        self.root.join(".claude")
    }
    pub fn claude_json(&self) -> PathBuf {
        self.root.join(".claude.json")
    }
    pub fn claude_settings(&self) -> PathBuf {
        self.root.join(".claude").join("settings.json")
    }
    pub fn claude_installed_plugins(&self) -> PathBuf {
        self.root.join(".claude").join("plugins").join("installed_plugins.json")
    }
    pub fn claude_skills(&self) -> PathBuf {
        self.root.join(".claude").join("skills")
    }
    pub fn codex_dir(&self) -> PathBuf {
        self.root.join(".codex")
    }
    pub fn codex_config(&self) -> PathBuf {
        self.root.join(".codex").join("config.toml")
    }
    pub fn codex_skills(&self) -> PathBuf {
        self.root.join(".codex").join("skills")
    }
    pub fn kimi_dir(&self) -> PathBuf {
        self.root.join(".kimi-code")
    }
    pub fn kimi_mcp(&self) -> PathBuf {
        self.root.join(".kimi-code").join("mcp.json")
    }
    pub fn kimi_installed_plugins(&self) -> PathBuf {
        self.root.join(".kimi-code").join("plugins").join("installed.json")
    }
    pub fn harness_dirs(&self) -> Vec<PathBuf> {
        vec![self.claude_dir(), self.codex_dir(), self.kimi_dir()]
    }
}
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test --lib home::`
Expected: PASS（4 tests）

- [ ] **Step 8: 写 `src/testutil.rs`（假 HOME 构建器）**

这是 spec §6「fixture 假 HOME」策略的落地件，后续每个 adapter Task 都依赖它。整文件：

```rust
//! 测试专用：在 TempDir 里重建假 HOME。绝不触碰真实 ~。
#![allow(dead_code)]

use crate::home::Home;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct TempHome {
    // 必须保活：drop 即删除整棵目录树
    _dir: TempDir,
    pub home: Home,
    root: PathBuf,
}

impl TempHome {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("create tempdir");
        let home = Home::new(dir.path());
        // 用 canonicalize 后的 root 构造路径，才能与 adapter 输出（同样 canonicalize 过）直接比较
        let root = home.root().to_path_buf();
        Self { _dir: dir, home, root }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// 写文件，自动创建父目录。
    pub fn write(&self, rel: &str, body: &str) -> &Self {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::fs::write(&p, body).expect("write file");
        self
    }

    pub fn mkdir(&self, rel: &str) -> &Self {
        std::fs::create_dir_all(self.path(rel)).expect("mkdir");
        self
    }

    /// 创建符号链接（Unix only；本项目只跑 macOS/Linux）。
    /// `target` 原样写入，相对路径由调用方负责正确性。
    pub fn symlink(&self, link: &str, target: &str) -> &Self {
        let l = self.path(link);
        if let Some(parent) = l.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::os::unix::fs::symlink(Path::new(target), &l).expect("symlink");
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_files_and_symlinks_under_temp_root() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/real-one")
            .write(".agents/skills/real-one/SKILL.md", "# hi")
            .symlink(".claude/skills/real-one", "../../.agents/skills/real-one");

        assert!(t.path(".agents/skills/real-one/SKILL.md").exists());
        let link = t.path(".claude/skills/real-one");
        assert!(link.exists(), "symlink resolves through to the real dir");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../../.agents/skills/real-one")
        );
        assert!(t.home.root().is_absolute());
    }
}
```

- [ ] **Step 9: 接线 `src/lib.rs` 与 `src/main.rs`**

`src/lib.rs`：

```rust
pub mod home;

#[cfg(test)]
mod testutil;
```

`src/main.rs`：

```rust
fn main() {
    println!("henv: not yet wired");
}
```

（`main.rs` 的 clap 实现在 Task 15；此处只要 crate 能编译。）

- [ ] **Step 10: 运行全量测试确认通过**

Run: `cargo test`
Expected: PASS（5 tests：home 4 个 + testutil 1 个）

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/lib.rs src/main.rs src/home.rs src/testutil.rs
git commit -m "feat: henv crate 脚手架与可注入 Home 抽象"
```

---

### Task 2: 归一化能力模型（capability 核心类型）

**Files:**
- Create: `src/capability.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: 无
- Produces: `Kind`、`CapId`、`Origin`、`SkillSource`、`Spec`、`Targets`、`Support`、`Capability`

- [ ] **Step 1: 写失败的测试**

创建 `src/capability.rs`，末尾写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrips_through_str() {
        for k in Kind::all() {
            assert_eq!(Kind::parse(k.as_str()), Some(k), "{k:?} roundtrip");
        }
        assert_eq!(Kind::parse("nope"), None);
    }

    #[test]
    fn cap_id_label_is_kind_slash_name() {
        assert_eq!(CapId::new(Kind::Skill, "dingtalk-mail").label(), "skill/dingtalk-mail");
        assert_eq!(CapId::new(Kind::Mcp, "context7").label(), "mcp/context7");
        assert_eq!(CapId::new(Kind::Plugin, "superpowers").label(), "plugin/superpowers");
    }

    #[test]
    fn targets_all_includes_any_harness() {
        assert!(Targets::All.includes("claude-code"));
        assert!(Targets::All.includes("anything"));
    }

    #[test]
    fn targets_only_includes_listed_harnesses() {
        let t = Targets::Only(vec!["claude-code".into(), "codex".into()]);
        assert!(t.includes("claude-code"));
        assert!(t.includes("codex"));
        assert!(!t.includes("kimi-code"));
    }

    #[test]
    fn targets_default_is_all() {
        assert_eq!(Targets::default(), Targets::All);
    }

    #[test]
    fn capability_new_defaults_to_manifest_origin_and_all_targets() {
        let c = Capability::new(
            CapId::new(Kind::Mcp, "context7"),
            Spec::Mcp {
                command: "npx".into(),
                args: vec!["-y".into(), "@upstash/context7-mcp".into()],
                env: BTreeMap::new(),
            },
        );
        assert_eq!(c.origin, Origin::Manifest);
        assert_eq!(c.targets, Targets::All);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib capability::`
Expected: FAIL —`Kind` 等类型未定义，编译错误。

- [ ] **Step 3: 实现类型**

在 `src/capability.rs` 顶部（测试模块之前）写：

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 能力种类。`Plugin` 是一等公民——spec §2.1 第 2 条：三家 harness 都有 plugin 机制，
/// v1 以 bundle manifest 对账为主，只在目标 harness 缺 manifest 时才降级拆解。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Skill,
    Mcp,
    Command,
    Agent,
    Hook,
    Plugin,
}

impl Kind {
    pub fn all() -> [Kind; 6] {
        [Kind::Skill, Kind::Mcp, Kind::Command, Kind::Agent, Kind::Hook, Kind::Plugin]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Mcp => "mcp",
            Kind::Command => "command",
            Kind::Agent => "agent",
            Kind::Hook => "hook",
            Kind::Plugin => "plugin",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::all().into_iter().find(|k| k.as_str() == s)
    }
}

/// 能力身份。跨 harness 匹配只认 (kind, name)；plugin 的 name 是 entry 名
/// （spec §2.1 第 3 条），marketplace 不参与身份。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId {
    pub kind: Kind,
    pub name: String,
}

impl CapId {
    pub fn new(kind: Kind, name: impl Into<String>) -> Self {
        Self { kind, name: name.into() }
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.kind.as_str(), self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Manifest,
    Plugin { name: String },
    Adopted { harness: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// 内容已在 ~/.agents/skills 共享库
    AgentsDir,
    /// git 仓库，ref 就是锁（spec §4）
    Git { repo: String, path: Option<String>, git_ref: String },
    /// 本地开发中的目录
    Local { path: PathBuf },
    /// 由 plugin bundle 携带（不是散装 skill）
    PluginBundle { plugin: String },
    /// 来源无法判定（含断链）
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Skill { source: SkillSource, path: Option<PathBuf> },
    Mcp { command: String, args: Vec<String>, env: BTreeMap<String, String> },
    Plugin {
        marketplace: Option<String>,
        entry: String,
        version: Option<String>,
        sha: Option<String>,
        install_path: Option<PathBuf>,
        enabled: bool,
    },
    Hook { event: String, command: String },
    Command { path: PathBuf },
    Agent { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Targets {
    #[default]
    All,
    Only(Vec<String>),
}

impl Targets {
    pub fn includes(&self, harness: &str) -> bool {
        match self {
            Targets::All => true,
            Targets::Only(list) => list.iter().any(|h| h == harness),
        }
    }
}

/// 每家 harness 对一个能力的接纳度——「拆解降级」的判定点
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Native,
    Degraded { as_kind: Kind },
    Unsupported { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub id: CapId,
    pub origin: Origin,
    pub spec: Spec,
    pub targets: Targets,
}

impl Capability {
    pub fn new(id: CapId, spec: Spec) -> Self {
        Self { id, origin: Origin::Manifest, spec, targets: Targets::All }
    }
}
```

- [ ] **Step 4: 在 `src/lib.rs` 声明模块**

```rust
pub mod capability;
pub mod home;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib capability::`
Expected: PASS（7 tests）

第 7 条 `kind_all_order_is_pinned` 是 Task 2 评审后追加的：本文件的 `Kind::all()` 顺序是
Task 14 汇总表与 insta golden 的依赖，而原有的 6 条测试对任意排列都通过（`parse` 是从
`all()` 派生的，round-trip 检不出顺序）。

- [ ] **Step 6: Commit**

```bash
git add src/capability.rs src/lib.rs
git commit -m "feat: 归一化能力模型 Capability/Spec/Support"
```

---

### Task 3: SHA 优先的版本比较

**Files:**
- Modify: `src/capability.rs`

**Interfaces:**
- Consumes: Task 2 的 `Spec`、`SkillSource`
- Produces: `VersionMatch`、`compare_versions(desired: &Spec, actual: &Spec) -> VersionMatch`

背景（spec §2.1 第 3 条，本机实测）：claude 记 superpowers `version = "6.4.1"`、
`gitCommitSha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797"`；kimi 记同一 bundle
`version = "v6.3.0"`、`installedSha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797"`。
**version 字符串不同但 SHA 相同 ⇒ 必须判 Same。** 只有一侧有 SHA 时无法判定 ⇒ `Unknown`。

- [ ] **Step 1: 写失败的测试**

追加到 `src/capability.rs` 的 `mod tests` 内：

```rust
    fn plugin(version: Option<&str>, sha: Option<&str>) -> Spec {
        Spec::Plugin {
            marketplace: Some("m".into()),
            entry: "superpowers".into(),
            version: version.map(String::from),
            sha: sha.map(String::from),
            install_path: None,
            enabled: true,
        }
    }

    #[test]
    fn same_sha_wins_over_differing_version_strings() {
        // 本机实测场景：claude 6.4.1 vs kimi v6.3.0，SHA 相同
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        assert_eq!(
            compare_versions(&plugin(Some("6.4.1"), Some(sha)), &plugin(Some("v6.3.0"), Some(sha))),
            VersionMatch::Same
        );
    }

    #[test]
    fn differing_sha_is_different_and_reports_actual_then_desired() {
        let desired = plugin(Some("6.4.1"), Some("aaa"));
        let actual = plugin(Some("6.3.0"), Some("bbb"));
        assert_eq!(
            compare_versions(&desired, &actual),
            VersionMatch::Different { from: "bbb".into(), to: "aaa".into() }
        );
    }

    #[test]
    fn version_fallback_when_neither_side_has_sha() {
        assert_eq!(
            compare_versions(&plugin(Some("1.2.0"), None), &plugin(Some("1.2.0"), None)),
            VersionMatch::Same
        );
        assert_eq!(
            compare_versions(&plugin(Some("1.3.0"), None), &plugin(Some("1.2.0"), None)),
            VersionMatch::Different { from: "1.2.0".into(), to: "1.3.0".into() }
        );
    }

    #[test]
    fn mixed_sha_and_version_only_is_unknown() {
        assert_eq!(
            compare_versions(&plugin(None, Some("aaa")), &plugin(Some("6.3.0"), None)),
            VersionMatch::Unknown
        );
        assert_eq!(compare_versions(&plugin(None, None), &plugin(None, None)), VersionMatch::Unknown);
    }

    #[test]
    fn git_skill_compares_by_ref() {
        let git = |r: &str| Spec::Skill {
            source: SkillSource::Git {
                repo: "obra/superpowers".into(),
                path: Some("skills".into()),
                git_ref: r.into(),
            },
            path: None,
        };
        assert_eq!(compare_versions(&git("v6.3.0"), &git("v6.3.0")), VersionMatch::Same);
        assert_eq!(
            compare_versions(&git("v2.2"), &git("v2.1")),
            VersionMatch::Different { from: "v2.1".into(), to: "v2.2".into() }
        );
    }

    #[test]
    fn mcp_compares_command_args_and_env() {
        let mcp = |cmd: &str, args: Vec<&str>, env: Vec<(&str, &str)>| Spec::Mcp {
            command: cmd.into(),
            args: args.into_iter().map(String::from).collect(),
            env: env.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        assert_eq!(
            compare_versions(&mcp("npx", vec!["-y", "pkg"], vec![]), &mcp("npx", vec!["-y", "pkg"], vec![])),
            VersionMatch::Same
        );
        assert_ne!(
            compare_versions(&mcp("npx", vec!["-y", "pkg"], vec![]), &mcp("npx", vec!["-y", "other"], vec![])),
            VersionMatch::Same
        );
        // ${VAR} 引用参与比较，但比的是引用串本身，不展开
        assert_eq!(
            compare_versions(&mcp("npx", vec![], vec![("K", "${K}")]), &mcp("npx", vec![], vec![("K", "${K}")])),
            VersionMatch::Same
        );
    }

    /// 两条 `Different` 断言只检查「不等于 Same」，所以把 from/to 写反也照样绿；
    /// 这条钉死方向：`from` 是现状（actual），`to` 是期望（desired）。
    /// 顺带覆盖 `mcp_summary` 的空 args 分支。
    #[test]
    fn mcp_different_reports_actual_as_from_and_desired_as_to() {
        let mcp = |cmd: &str, args: Vec<&str>| Spec::Mcp {
            command: cmd.into(),
            args: args.into_iter().map(String::from).collect(),
            env: Default::default(),
        };
        assert_eq!(
            compare_versions(&mcp("npx", vec!["-y", "pkg-v2"]), &mcp("npx", vec!["-y", "pkg-v1"])),
            VersionMatch::Different {
                from: "npx -y pkg-v1".into(),
                to: "npx -y pkg-v2".into(),
            }
        );
        // 无 args 时 summary 退化成 command 本身
        assert_eq!(
            compare_versions(&mcp("newbin", vec![]), &mcp("oldbin", vec![])),
            VersionMatch::Different { from: "oldbin".into(), to: "newbin".into() }
        );
    }

    /// 这条是本 Task 真正的护栏。version-fallback 分支上那两个 `sha: None` 守卫一旦被删，
    /// 上面 `mixed_sha_and_version_only_is_unknown` 仍全绿——因为它两边各缺一个字段，
    /// 无论有没有守卫都进不了第二分支。必须用「一边 sha+version 都有、另一边只有 version」
    /// 这个形状才检得出来——而这恰好就是本机真实数据形状：claude 记 `6.4.1` + `gitCommitSha`，
    /// codex/kimi 只记 version。少了守卫，第二分支会先撞上，把本该 Unknown（按 in-sync 处理）
    /// 的一对报成 `Different { from: "6.3.0", to: "6.4.1" }`，于是每次 diff 都误报 drift。
    #[test]
    fn sha_plus_version_against_version_only_is_unknown() {
        assert_eq!(
            compare_versions(
                &plugin(Some("6.4.1"), Some("abc123")),
                &plugin(Some("6.3.0"), None)
            ),
            VersionMatch::Unknown
        );
        // 反方向同理：谁的哪一边有 sha 都不该改变结论
        assert_eq!(
            compare_versions(
                &plugin(Some("6.3.0"), None),
                &plugin(Some("6.4.1"), Some("abc123"))
            ),
            VersionMatch::Unknown
        );
    }

    #[test]
    fn cross_kind_specs_are_unknown() {
        let skill = Spec::Skill { source: SkillSource::AgentsDir, path: None };
        assert_eq!(compare_versions(&skill, &plugin(Some("1.0"), None)), VersionMatch::Unknown);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib capability::`
Expected: FAIL —`VersionMatch` / `compare_versions` 未定义。

- [ ] **Step 3: 实现**

在 `src/capability.rs` 中 `impl Capability` 之后追加：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionMatch {
    Same,
    /// from = 现状（actual），to = 期望（desired）
    Different { from: String, to: String },
    /// 无法判定（一侧有 SHA 一侧只有 version、跨 kind、或信息不足）。
    /// 只读工具不该误报，diff 把 Unknown 当 in-sync。
    Unknown,
}

fn mcp_summary(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        command.to_string()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

/// SHA 优先、version 兜底（spec §2.1 第 3 条）。
pub fn compare_versions(desired: &Spec, actual: &Spec) -> VersionMatch {
    match (desired, actual) {
        (Spec::Plugin { sha: Some(d), .. }, Spec::Plugin { sha: Some(a), .. }) => {
            if d == a {
                VersionMatch::Same
            } else {
                VersionMatch::Different { from: a.clone(), to: d.clone() }
            }
        }
        (
            Spec::Plugin { sha: None, version: Some(d), .. },
            Spec::Plugin { sha: None, version: Some(a), .. },
        ) => {
            if d == a {
                VersionMatch::Same
            } else {
                VersionMatch::Different { from: a.clone(), to: d.clone() }
            }
        }
        (Spec::Plugin { .. }, Spec::Plugin { .. }) => VersionMatch::Unknown,
        (
            Spec::Skill { source: SkillSource::Git { git_ref: d, .. }, .. },
            Spec::Skill { source: SkillSource::Git { git_ref: a, .. }, .. },
        ) => {
            if d == a {
                VersionMatch::Same
            } else {
                VersionMatch::Different { from: a.clone(), to: d.clone() }
            }
        }
        (
            Spec::Mcp { command: dc, args: da, env: de },
            Spec::Mcp { command: ac, args: aa, env: ae },
        ) => {
            if dc == ac && da == aa && de == ae {
                VersionMatch::Same
            } else {
                VersionMatch::Different { from: mcp_summary(ac, aa), to: mcp_summary(dc, da) }
            }
        }
        _ => VersionMatch::Unknown,
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib capability::`
Expected: PASS（16 tests：Task 2 的 7 个 + 本 Task 的 9 个）

- [ ] **Step 5: Commit**

```bash
git add src/capability.rs
git commit -m "feat: SHA 优先的版本比较，避免 version 字符串误报 drift"
```

---

#### Task 3 追加（评审后补，49 变异体检出三条）

评审对 `compare_versions` 扫了 49 个变异体。前一轮补的两条测试确实杀住了 M7/M8/M10，但又暴露三个洞。
以下三处都要做，全部在 `src/capability.rs` 内。

**A1 — MCP 的 env 相等性有代码、没测试。** 删掉 `&& de == ae` 后 16/16 仍绿，而测试名字
`mcp_compares_command_args_and_env` 明说 env 参与比较：该测试里每一对 fixture 都差在 command 或
args 上，从没只差在 env 上；比 `${K}` 的那对两边 env 完全相同，看不见这个字段。补一对断言：
command 与 args 全同、只有 env 值不同 → `Different`；env 也相同 → `Same`。缺了它，日后有人把比较
「简化」成 command+args，env 漂移会被静默判成 in-sync——漏报是一枚只读工具最坏的失效方向。

**A2 — `mcp_summary` 带上 env 的键名，但永远不带值。** 现状只吃 `(command, args)`，于是「只差 env」
的一对渲染成 `from == to` 的同一串，Task 13 生成的更新行显示成「A → A」，看不出变化。改成三参：

```rust
/// MCP 的人类可读摘要。**只渲染键名，绝不渲染 env 值**——desired 侧的值永远是 `${VAR}`
/// 引用，但 actual 侧是 adapter 从 `~/.claude.json` 这类活配置读出来的，可能就是明文真密钥。
/// spec §3 决策 3：密钥永不进任何产物。
fn mcp_summary(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> String {
    let mut s = String::from(command);
    if !args.is_empty() {
        s.push(' ');
        s.push_str(&args.join(" "));
    }
    if !env.is_empty() {
        s.push_str(" [env: ");
        s.push_str(&env.keys().cloned().collect::<Vec<_>>().join(","));
        s.push(']');
    }
    s
}
```

`from`/`to` 两个调用点都补第三个实参（`de` / `ae`）。**已知残余，不要试图消掉**：同一个键换了值时
摘要仍是 `[env: K] → [env: K]`。不显示值就无法区分值，这是安全约束换来的代价；`~ update` 前缀加上
键名已经足够让人定位到「是这个 key」。A1 的新断言按 `[env: K]` 形状写。

**A3 — `sha` / `version` 的哨兵值归一化。** spec §2.1 第 3 条实测本机版本串有五种形态、含字面量
`unknown`。而现在 `sha: Some("unknown")` 对 `sha: Some("unknown")` 会命中「两边都有 SHA」分支判成
`Same`——两个真不同的构建被报成同步；空串同理。补：

```rust
/// 把「没有值」和「有值但是哨兵」都收敛成 None，交给 compare_versions 的 `sha: None`
/// 守卫去降级到 version 分支或 Unknown。实测哨兵：`unknown`（大小写不一）、空串、纯空白。
pub fn normalize_locator(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("unknown") {
        return None;
    }
    Some(v.to_string())
}
```

两条测试：`normalize_locator` 对 `None`/`Some("")`/`Some("  ")`/`Some("unknown")`/`Some("UNKNOWN")`
返回 `None`，对 `Some("6.3.0")`/`Some("a1b2c3")` 原样返回；以及一条端到端的——两侧 `sha` 都是
`Some("unknown")` 时，**先归一化再比较**必须得 `Unknown` 而不是 `Same`。

**归一化的实际调用点在下游，不在本 Task**：Task 5 解析 manifest 的 `ref`、Task 9/10/11 的
`read_state` 读 version/sha 时都要过 `normalize_locator`。该要求已写进那几轮派发上下文。

补完后 `cargo test --lib capability::` 期望 **19 tests**（Task 2 的 7 个 + 本 Task 的 12 个）。

---

### Task 4: 密钥引用门禁（secrets）

**Files:**
- Create: `src/secrets.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: 无
- Produces: `parse_ref`、`validate_env`、`missing`

- [ ] **Step 1: 写失败的测试**

创建 `src/secrets.rs`，末尾写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wellformed_reference() {
        assert_eq!(parse_ref("${CONTEXT7_API_KEY}"), Some("CONTEXT7_API_KEY"));
        assert_eq!(parse_ref("${_X1}"), Some("_X1"));
    }

    #[test]
    fn rejects_literal_secret() {
        assert_eq!(parse_ref("sk-abc123"), None);
        assert_eq!(parse_ref(""), None);
    }

    #[test]
    fn rejects_malformed_reference_syntax() {
        assert_eq!(parse_ref("${}"), None);
        assert_eq!(parse_ref("$VAR}"), None);
        assert_eq!(parse_ref("${VAR"), None);
        assert_eq!(parse_ref("${VAR} "), None);
        assert_eq!(parse_ref(" ${VAR}"), None);
        assert_eq!(parse_ref("${A-B}"), None);
        assert_eq!(parse_ref("${1ABC}"), None);
        // 嵌套/拼接一律拒绝，避免半展开
        assert_eq!(parse_ref("${A}${B}"), None);
        assert_eq!(parse_ref("prefix-${A}"), None);
    }

    #[test]
    fn validate_env_names_the_offending_key() {
        assert!(validate_env("K", "${K}").is_ok());
        let err = validate_env("CONTEXT7_API_KEY", "sk-live-123").unwrap_err();
        assert!(err.contains("CONTEXT7_API_KEY"), "error names the key: {err}");
        assert!(!err.contains("sk-live-123"), "error must not echo the secret: {err}");
    }

    #[test]
    fn missing_reports_only_absent_vars_in_input_order() {
        let vars = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let got = missing(&vars, &|k| if k == "B" { Some("x".into()) } else { None });
        assert_eq!(got, vec!["A".to_string(), "C".to_string()]);
    }

    #[test]
    fn missing_treats_empty_value_as_absent() {
        let vars = vec!["A".to_string()];
        let got = missing(&vars, &|_| Some(String::new()));
        assert_eq!(got, vec!["A".to_string()]);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib secrets::`
Expected: FAIL —函数未定义。

- [ ] **Step 3: 实现 `src/secrets.rs`**

```rust
//! `${VAR}` 引用的校验与存在性检查。
//!
//! 密钥永远以引用形态流转：本模块只判断「语法是否为纯引用」与「变量是否存在」，
//! **绝不展开值进任何产物**（spec §3 决策 3）。

/// `"${FOO}"` → `Some("FOO")`；任何非纯引用形态 → `None`。
/// 手写校验而非 regex，以守住「不引入 regex 依赖」的约束。
pub fn parse_ref(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("${")?.strip_suffix('}')?;
    if inner.is_empty() {
        return None;
    }
    // 标识符规则本身就排除了 `$ { }`（它们都不在 [A-Za-z0-9_] 里），
    // 所以嵌套与拼接形态由下面这段一并拒掉，不需要单独的守卫。
    let mut chars = inner.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(inner)
}

/// 校验单个 env 值。错误信息包含 key 名但**绝不回显 value**（防止密钥进日志/终端）。
pub fn validate_env(key: &str, value: &str) -> Result<(), String> {
    match parse_ref(value) {
        Some(_) => Ok(()),
        None => Err(format!(
            "env `{key}` must be a bare ${{VAR}} reference; literal secrets are rejected"
        )),
    }
}

/// 返回在环境中缺失（或为空）的变量名，保持输入顺序。
pub fn missing(vars: &[String], lookup: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    vars.iter()
        .filter(|v| lookup(v).map(|s| s.is_empty()).unwrap_or(true))
        .cloned()
        .collect()
}
```

- [ ] **Step 4: 在 `src/lib.rs` 声明模块**

```rust
pub mod capability;
pub mod home;
pub mod secrets;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib secrets::`
Expected: PASS（6 tests）

- [ ] **Step 6: Commit**

```bash
git add src/secrets.rs src/lib.rs
git commit -m "feat: \${VAR} 密钥引用门禁，拒绝字面量且不回显值"
```

---

### Task 5: env.yaml 清单解析与校验

**Files:**
- Create: `src/manifest.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `secrets::validate_env`（Task 4）、`capability::{Kind, Targets}`（Task 2）
- Produces: `Manifest` 及各条目类型、`Manifest::parse`、`DegradePolicy::action_for`、`DegradeAction`、`SUPPORTED_VERSION`、`KNOWN_HARNESSES`

- [ ] **Step 1: 写失败的测试**

创建 `src/manifest.rs`，末尾写测试。测试 YAML 直接取 spec §4 的完整示例：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
version: 1

harnesses:
  claude-code: { enabled: true }
  codex:       { enabled: true }
  kimi-code:   { enabled: true }

skills:
  dingtalk-mail:
    source: agents-dir
  superpowers:
    source: git
    repo: obra/superpowers
    path: skills
    ref: v6.3.0
  my-draft:
    source: local
    path: ~/dev/skills/my-draft

mcps:
  context7:
    command: npx
    args: ["-y", "@upstash/context7-mcp"]
    env:
      CONTEXT7_API_KEY: ${CONTEXT7_API_KEY}
    targets: all

plugins:
  superpowers-bundle:
    source: claude-marketplace
    marketplace: obra/superpowers-marketplace
    entry: superpowers
    degrade:
      hooks: report
      commands: report
"#;

    #[test]
    fn parses_spec_example_in_full() {
        let m = Manifest::parse(FULL).expect("spec §4 example must parse");
        assert_eq!(m.version, 1);
        assert_eq!(m.harnesses.len(), 3);
        assert!(m.harnesses["codex"].enabled);

        assert_eq!(m.skills.len(), 3);
        assert!(matches!(m.skills["dingtalk-mail"].source, SkillSourceKind::AgentsDir));
        assert!(matches!(m.skills["superpowers"].source, SkillSourceKind::Git));
        assert_eq!(m.skills["superpowers"].repo.as_deref(), Some("obra/superpowers"));
        assert_eq!(m.skills["superpowers"].path.as_deref(), Some("skills"));
        assert_eq!(m.skills["superpowers"].git_ref.as_deref(), Some("v6.3.0"));
        assert_eq!(m.skills["my-draft"].path.as_deref(), Some("~/dev/skills/my-draft"));

        let c = &m.mcps["context7"];
        assert_eq!(c.command, "npx");
        assert_eq!(c.args, vec!["-y", "@upstash/context7-mcp"]);
        assert_eq!(c.env["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}");
        assert_eq!(c.targets, Targets::All);

        let p = &m.plugins["superpowers-bundle"];
        assert!(matches!(p.source, PluginSourceKind::ClaudeMarketplace));
        assert_eq!(p.entry.as_deref(), Some("superpowers"));
        assert_eq!(p.degrade.hooks, DegradeAction::Report);
    }

    #[test]
    fn defaults_are_applied_when_sections_omitted() {
        let m = Manifest::parse("version: 1\n").expect("minimal manifest");
        assert!(m.harnesses.is_empty());
        assert!(m.skills.is_empty());
        assert!(m.mcps.is_empty());
        assert!(m.plugins.is_empty());
    }

    #[test]
    fn harness_entry_defaults_to_enabled() {
        let m = Manifest::parse("version: 1\nharnesses:\n  codex: {}\n").unwrap();
        assert!(m.harnesses["codex"].enabled);
    }

    #[test]
    fn degrade_defaults_to_report_for_unlisted_kinds() {
        let m = Manifest::parse(FULL).unwrap();
        let d = &m.plugins["superpowers-bundle"].degrade;
        assert_eq!(d.hooks, DegradeAction::Report);
        assert_eq!(d.skills, DegradeAction::Report, "unlisted kind defaults to report");
        assert_eq!(d.action_for(Kind::Hook), DegradeAction::Report);
        assert_eq!(d.action_for(Kind::Mcp), DegradeAction::Report);
    }

    #[test]
    fn degrade_skip_is_honoured() {
        let yaml = "version: 1\nplugins:\n  p:\n    source: github\n    repo: a/b\n    degrade:\n      hooks: skip\n";
        let m = Manifest::parse(yaml).unwrap();
        assert_eq!(m.plugins["p"].degrade.hooks, DegradeAction::Skip);
        assert_eq!(m.plugins["p"].degrade.action_for(Kind::Hook), DegradeAction::Skip);
    }

    #[test]
    fn targets_accepts_all_or_explicit_list() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: all\n  b:\n    command: npx\n    targets: [claude-code, codex]\n";
        let m = Manifest::parse(yaml).unwrap();
        assert_eq!(m.mcps["a"].targets, Targets::All);
        assert_eq!(m.mcps["b"].targets, Targets::Only(vec!["claude-code".into(), "codex".into()]));
        assert!(m.mcps["b"].targets.includes("codex"));
        assert!(!m.mcps["b"].targets.includes("kimi-code"));
    }

    #[test]
    fn targets_rejects_unknown_harness_in_list() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: [cursor]\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("cursor"), "{err}");
    }

    #[test]
    fn targets_rejects_bare_keyword_that_is_not_all() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: none\n";
        assert!(Manifest::parse(yaml).is_err());
    }

    #[test]
    fn rejects_literal_secret_in_env_and_does_not_echo_it() {
        let yaml = "version: 1\nmcps:\n  context7:\n    command: npx\n    env:\n      CONTEXT7_API_KEY: sk-live-abc123\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("CONTEXT7_API_KEY"), "{err}");
        assert!(!err.contains("sk-live-abc123"), "must not echo secret: {err}");
    }

    #[test]
    fn rejects_unknown_version() {
        let err = Manifest::parse("version: 2\n").unwrap_err().to_string();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn rejects_missing_version() {
        assert!(Manifest::parse("skills: {}\n").is_err());
    }

    #[test]
    fn rejects_unknown_top_level_and_entry_fields() {
        assert!(Manifest::parse("version: 1\nskillz: {}\n").is_err());
        assert!(Manifest::parse("version: 1\nskills:\n  a:\n    source: local\n    pth: /x\n").is_err());
    }

    #[test]
    fn rejects_git_skill_without_repo_or_ref() {
        let err = Manifest::parse("version: 1\nskills:\n  a:\n    source: git\n").unwrap_err().to_string();
        assert!(err.contains("repo"), "{err}");
        assert!(err.contains("ref"), "{err}");
    }

    #[test]
    fn rejects_local_skill_without_path() {
        let err = Manifest::parse("version: 1\nskills:\n  a:\n    source: local\n").unwrap_err().to_string();
        assert!(err.contains("path"), "{err}");
    }

    #[test]
    fn rejects_agents_dir_skill_with_repo() {
        let err = Manifest::parse("version: 1\nskills:\n  a:\n    source: agents-dir\n    repo: x/y\n").unwrap_err().to_string();
        assert!(err.contains("agents-dir"), "{err}");
    }

    #[test]
    fn rejects_claude_marketplace_plugin_without_marketplace_or_entry() {
        let yaml = "version: 1\nplugins:\n  a:\n    source: claude-marketplace\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("marketplace"), "{err}");
        assert!(err.contains("entry"), "{err}");
    }

    #[test]
    fn rejects_github_plugin_without_repo() {
        let err = Manifest::parse("version: 1\nplugins:\n  a:\n    source: github\n").unwrap_err().to_string();
        assert!(err.contains("repo"), "{err}");
    }

    #[test]
    fn rejects_unknown_harness_name() {
        let err = Manifest::parse("version: 1\nharnesses:\n  cursor: { enabled: true }\n").unwrap_err().to_string();
        assert!(err.contains("cursor"), "{err}");
    }

    #[test]
    fn aggregates_all_errors_in_one_message() {
        let yaml = "version: 1\nskills:\n  a:\n    source: git\n  b:\n    source: local\nmcps:\n  c:\n    command: npx\n    env:\n      K: literal\n";
        let err = Manifest::parse(yaml).unwrap_err().to_string();
        assert!(err.contains("skills.a"), "{err}");
        assert!(err.contains("skills.b"), "{err}");
        assert!(err.contains("mcps.c"), "{err}");
    }

    #[test]
    fn yaml_syntax_error_surfaces_with_context() {
        let err = Manifest::parse("version: 1\nskills: [\n").unwrap_err().to_string();
        assert!(err.contains("env.yaml"), "{err}");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib manifest::`
Expected: FAIL —类型与 `parse` 未定义。

- [ ] **Step 3: 实现 `src/manifest.rs`**

```rust
//! env.yaml 的类型化解析与校验。
//!
//! 三类条目（skills / mcps / plugins）是用户书写视角，解析后统一带 `targets` 与
//! `degrade` 语义（spec §4 决策 1）。校验错误**聚合**后一次性报出，避免用户改一条跑一次。

use crate::capability::{Kind, Targets};
use crate::secrets;
use serde::de::{self, Deserializer, Unexpected};
use serde::Deserialize;
use std::collections::BTreeMap;

/// 清单支持的 schema 版本。
pub const SUPPORTED_VERSION: u32 = 1;

/// 已知 harness 名。新增 harness = 在此加一行 + adapters/ 新模块（spec §7）。
pub const KNOWN_HARNESSES: [&str; 3] = ["claude-code", "codex", "kimi-code"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub harnesses: BTreeMap<String, HarnessEntry>,
    #[serde(default)]
    pub skills: BTreeMap<String, SkillEntry>,
    #[serde(default)]
    pub mcps: BTreeMap<String, McpEntry>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessEntry {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillSourceKind {
    AgentsDir,
    Git,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    pub source: SkillSourceKind,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub targets: Targets,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub targets: Targets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginSourceKind {
    ClaudeMarketplace,
    Github,
    ZipUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    pub source: PluginSourceKind,
    #[serde(default)]
    pub marketplace: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub degrade: DegradePolicy,
    #[serde(default)]
    pub targets: Targets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeAction {
    /// 进 doctor / diff 的 `!` 行
    Report,
    /// 静默丢弃
    Skip,
}

impl Default for DegradeAction {
    fn default() -> Self {
        DegradeAction::Report
    }
}

impl<'de> Deserialize<'de> for DegradeAction {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "report" => Ok(DegradeAction::Report),
            "skip" => Ok(DegradeAction::Skip),
            other => Err(de::Error::invalid_value(
                Unexpected::Str(other),
                &"\"report\" or \"skip\"",
            )),
        }
    }
}

/// degrade 策略按 plugin 条目声明，不做全局开关（spec §4 决策 3）。
/// 未列出的 kind 默认 `report`——只读工具宁可多报也不静默丢能力。
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DegradePolicy {
    #[serde(default)]
    pub hooks: DegradeAction,
    #[serde(default)]
    pub commands: DegradeAction,
    #[serde(default)]
    pub skills: DegradeAction,
    #[serde(default)]
    pub mcps: DegradeAction,
    #[serde(default)]
    pub agents: DegradeAction,
}

impl DegradePolicy {
    pub fn action_for(&self, kind: Kind) -> DegradeAction {
        match kind {
            Kind::Hook => self.hooks,
            Kind::Command => self.commands,
            Kind::Skill => self.skills,
            Kind::Mcp => self.mcps,
            Kind::Agent => self.agents,
            // plugin 自身装不下不是 degrade 问题，交给 supports() 判 Unsupported
            Kind::Plugin => DegradeAction::Report,
        }
    }
}

/// `targets` 接受 `all` 或显式列表（spec §4 决策 4：`all` 是默认值）。
impl<'de> Deserialize<'de> for Targets {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Keyword(String),
            List(Vec<String>),
        }
        match Repr::deserialize(d)? {
            Repr::Keyword(k) if k == "all" => Ok(Targets::All),
            Repr::Keyword(other) => Err(de::Error::invalid_value(
                Unexpected::Str(&other),
                &"\"all\" or a list of harness names",
            )),
            Repr::List(list) => {
                for h in &list {
                    if !KNOWN_HARNESSES.contains(&h.as_str()) {
                        return Err(de::Error::invalid_value(
                            Unexpected::Str(h),
                            &"one of claude-code, codex, kimi-code",
                        ));
                    }
                }
                Ok(Targets::Only(list))
            }
        }
    }
}

impl Manifest {
    /// 解析 + 校验。校验错误聚合成单个 anyhow 错误，每条带 `skills.a` 式路径。
    pub fn parse(text: &str) -> anyhow::Result<Manifest> {
        let m: Manifest = serde_yaml_ng::from_str(text)
            .map_err(|e| anyhow::anyhow!("failed to parse env.yaml: {e}"))?;
        let errors = m.validate();
        if errors.is_empty() {
            Ok(m)
        } else {
            anyhow::bail!("env.yaml invalid:\n  - {}", errors.join("\n  - "));
        }
    }

    fn validate(&self) -> Vec<String> {
        let mut e = Vec::new();

        if self.version != SUPPORTED_VERSION {
            e.push(format!(
                "unsupported version {} (henv understands version {SUPPORTED_VERSION})",
                self.version
            ));
        }

        for name in self.harnesses.keys() {
            if !KNOWN_HARNESSES.contains(&name.as_str()) {
                e.push(format!(
                    "harnesses.{name}: unknown harness (known: {})",
                    KNOWN_HARNESSES.join(", ")
                ));
            }
        }

        for (name, s) in &self.skills {
            let at = format!("skills.{name}");
            match s.source {
                SkillSourceKind::AgentsDir => {
                    if s.repo.is_some() || s.git_ref.is_some() {
                        e.push(format!("{at}: source agents-dir takes no repo/ref"));
                    }
                }
                SkillSourceKind::Git => {
                    if s.repo.is_none() {
                        e.push(format!("{at}: source git requires `repo`"));
                    }
                    if s.git_ref.is_none() {
                        e.push(format!("{at}: source git requires `ref` (ref is the lock)"));
                    }
                }
                SkillSourceKind::Local => {
                    if s.path.is_none() {
                        e.push(format!("{at}: source local requires `path`"));
                    }
                }
            }
        }

        for (name, m) in &self.mcps {
            let at = format!("mcps.{name}");
            if m.command.trim().is_empty() {
                e.push(format!("{at}: `command` must not be empty"));
            }
            for (k, v) in &m.env {
                if let Err(msg) = secrets::validate_env(k, v) {
                    e.push(format!("{at}: {msg}"));
                }
            }
        }

        for (name, p) in &self.plugins {
            let at = format!("plugins.{name}");
            match p.source {
                PluginSourceKind::ClaudeMarketplace => {
                    if p.marketplace.is_none() {
                        e.push(format!("{at}: source claude-marketplace requires `marketplace`"));
                    }
                    if p.entry.is_none() {
                        e.push(format!("{at}: source claude-marketplace requires `entry`"));
                    }
                }
                PluginSourceKind::Github => {
                    if p.repo.is_none() {
                        e.push(format!("{at}: source github requires `repo`"));
                    }
                }
                PluginSourceKind::ZipUrl => {}
            }
        }

        e
    }
}
```

- [ ] **Step 4: 在 `src/lib.rs` 声明模块**

```rust
pub mod capability;
pub mod home;
pub mod manifest;
pub mod secrets;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib manifest::`
Expected: PASS（20 tests）

若 `rejects_unknown_top_level_and_entry_fields` 失败，确认 `Manifest`、`SkillEntry` 上都有
`deny_unknown_fields`。若 `aggregates_all_errors_in_one_message` 失败，确认 `validate()` 是收集
而非 early-return。若 `rejects_missing_version` 失败，确认 `version` 字段**没有** `#[serde(default)]`。

- [ ] **Step 6: Commit**

```bash
git add src/manifest.rs src/lib.rs
git commit -m "feat: env.yaml 类型化解析与聚合校验"
```

---

### Task 6: 清单降级为能力流（lower）

**Files:**
- Create: `src/lower.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `manifest::*`（Task 5）、`capability::*`（Task 2）
- Produces: `lower(&Manifest) -> Vec<Capability>`、`DegradeTable::{new, action}`

关键身份规则（spec §2.1 第 3 条）：plugin 的 `CapId.name` = **entry 名**，不是清单里的 key。
`superpowers-bundle: { entry: superpowers }` 降出 `CapId{Plugin, "superpowers"}`，才能与
claude 的 `superpowers@claude-plugins-official`、kimi 的 `id: superpowers` 对上。
`entry` 缺省时回退用 key。

- [ ] **Step 1: 写失败的测试**

创建 `src/lower.rs`，末尾写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Targets};
    use crate::manifest::{DegradeAction, Manifest};

    fn m(yaml: &str) -> Manifest {
        Manifest::parse(yaml).expect("test yaml must parse")
    }

    #[test]
    fn empty_manifest_lowers_to_nothing() {
        assert!(lower(&m("version: 1\n")).is_empty());
    }

    #[test]
    fn lowers_all_three_entry_kinds() {
        let yaml = r#"
version: 1
skills:
  dingtalk-mail: { source: agents-dir }
mcps:
  context7: { command: npx, args: ["-y", "@upstash/context7-mcp"] }
plugins:
  superpowers-bundle: { source: claude-marketplace, marketplace: obra/spm, entry: superpowers }
"#;
        let caps = lower(&m(yaml));
        assert_eq!(caps.len(), 3);

        let skill = caps.iter().find(|c| c.id.kind == Kind::Skill).unwrap();
        assert_eq!(skill.id.name, "dingtalk-mail");
        assert_eq!(skill.spec, Spec::Skill { source: SkillSource::AgentsDir, path: None });
        assert_eq!(skill.origin, Origin::Manifest);

        let mcp = caps.iter().find(|c| c.id.kind == Kind::Mcp).unwrap();
        assert_eq!(mcp.id.name, "context7");
        match &mcp.spec {
            Spec::Mcp { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &vec!["-y".to_string(), "@upstash/context7-mcp".to_string()]);
                assert!(env.is_empty());
            }
            other => panic!("expected Mcp, got {other:?}"),
        }

        let plugin = caps.iter().find(|c| c.id.kind == Kind::Plugin).unwrap();
        assert_eq!(plugin.id.name, "superpowers", "identity is the entry name, not the manifest key");
    }

    #[test]
    fn plugin_entry_defaults_to_manifest_key() {
        let yaml = "version: 1\nplugins:\n  superpowers:\n    source: github\n    repo: obra/superpowers\n";
        let caps = lower(&m(yaml));
        assert_eq!(caps[0].id.name, "superpowers");
        match &caps[0].spec {
            Spec::Plugin { entry, marketplace, sha, version, enabled, .. } => {
                assert_eq!(entry, "superpowers");
                // github 源的 repo 落在 marketplace 位，作为来源标识
                assert_eq!(marketplace.as_deref(), Some("obra/superpowers"));
                assert_eq!(*sha, None);
                assert_eq!(*version, None);
                assert!(*enabled, "desired plugins are enabled by definition");
            }
            other => panic!("expected Plugin, got {other:?}"),
        }
    }

    #[test]
    fn git_skill_ref_becomes_the_lock() {
        let yaml = "version: 1\nskills:\n  superpowers:\n    source: git\n    repo: obra/superpowers\n    path: skills\n    ref: v6.3.0\n";
        let caps = lower(&m(yaml));
        assert_eq!(
            caps[0].spec,
            Spec::Skill {
                source: SkillSource::Git {
                    repo: "obra/superpowers".into(),
                    path: Some("skills".into()),
                    git_ref: "v6.3.0".into(),
                },
                path: None,
            }
        );
    }

    #[test]
    fn local_skill_path_is_expanded() {
        let yaml = "version: 1\nskills:\n  d:\n    source: local\n    path: ~/dev/skills/d\n";
        let caps = lower(&m(yaml));
        match &caps[0].spec {
            Spec::Skill { source: SkillSource::Local { path }, .. } => {
                assert!(!path.to_string_lossy().contains('~'), "~ must be expanded: {path:?}");
                assert!(path.is_absolute(), "{path:?}");
                assert!(path.ends_with("dev/skills/d"), "{path:?}");
            }
            other => panic!("expected Local skill, got {other:?}"),
        }
    }

    #[test]
    fn targets_propagate_onto_every_capability() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    targets: [codex]\n  b:\n    command: npx\n";
        let caps = lower(&m(yaml));
        let a = caps.iter().find(|c| c.id.name == "a").unwrap();
        let b = caps.iter().find(|c| c.id.name == "b").unwrap();
        assert_eq!(a.targets, Targets::Only(vec!["codex".into()]));
        assert_eq!(b.targets, Targets::All);
    }

    #[test]
    fn mcp_env_stays_as_unexpanded_reference() {
        let yaml = "version: 1\nmcps:\n  a:\n    command: npx\n    env:\n      K: ${K}\n";
        let caps = lower(&m(yaml));
        match &caps[0].spec {
            Spec::Mcp { env, .. } => assert_eq!(env["K"], "${K}", "reference must not be expanded"),
            other => panic!("expected Mcp, got {other:?}"),
        }
    }

    #[test]
    fn output_is_deterministically_ordered() {
        let yaml = "version: 1\nmcps:\n  z: { command: a }\n  a: { command: a }\nskills:\n  y: { source: agents-dir }\n";
        let caps = lower(&m(yaml));
        let names: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "lowering must be deterministic: {names:?}");
    }

    #[test]
    fn degrade_table_routes_by_plugin_and_kind() {
        let yaml = "version: 1\nplugins:\n  a:\n    source: github\n    repo: x/y\n    entry: plug\n    degrade:\n      hooks: skip\n      commands: report\nmcps:\n  m:\n    command: npx\n";
        let man = m(yaml);
        let caps = lower(&man);
        let table = DegradeTable::new(&man);

        let hook = Capability {
            id: CapId::new(Kind::Hook, "plug:pre-commit"),
            origin: Origin::Plugin { name: "plug".into() },
            spec: Spec::Hook { event: "SessionStart".into(), command: "x".into() },
            targets: Targets::All,
        };
        assert_eq!(table.action(&hook), DegradeAction::Skip, "policy comes from the owning plugin entry");

        let plugin = caps.iter().find(|c| c.id.kind == Kind::Plugin).unwrap();
        assert_eq!(table.action(plugin), DegradeAction::Report);

        let mcp = caps.iter().find(|c| c.id.kind == Kind::Mcp).unwrap();
        assert_eq!(table.action(mcp), DegradeAction::Report, "non-plugin capabilities default to report");
    }

    #[test]
    fn degrade_table_tolerates_unknown_plugin_origin() {
        let man = m("version: 1\n");
        let table = DegradeTable::new(&man);
        let orphan = Capability {
            id: CapId::new(Kind::Hook, "x"),
            origin: Origin::Plugin { name: "not-in-manifest".into() },
            spec: Spec::Hook { event: "E".into(), command: "c".into() },
            targets: Targets::All,
        };
        assert_eq!(table.action(&orphan), DegradeAction::Report);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib lower::`
Expected: FAIL —`lower` / `DegradeTable` 未定义。

- [ ] **Step 3: 实现 `src/lower.rs`**

```rust
//! 三类清单条目 → 单一 Capability 流（spec §4 决策 1「一个出口」）。
//!
//! plugin 在此**不**被拆解：spec §2.1 第 2 条确认三家 harness 都有 plugin 机制且 bundle 自带
//! per-harness manifest，v1 以对账为主。拆解是 Plan B 在目标 harness 缺 manifest 时的降级路径。

use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec};
use crate::manifest::{DegradeAction, DegradePolicy, Manifest, PluginSourceKind, SkillSourceKind};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 把 `~/x` 展开为绝对路径。只处理前导 `~/` 与裸 `~`，不做 shell 语义。
fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_fallback();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_fallback().join(rest);
    }
    PathBuf::from(raw)
}

fn home_fallback() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn lower(m: &Manifest) -> Vec<Capability> {
    let mut out: Vec<Capability> = Vec::new();

    for (name, s) in &m.skills {
        let source = match s.source {
            SkillSourceKind::AgentsDir => SkillSource::AgentsDir,
            SkillSourceKind::Git => SkillSource::Git {
                repo: s.repo.clone().unwrap_or_default(),
                path: s.path.clone(),
                git_ref: s.git_ref.clone().unwrap_or_default(),
            },
            SkillSourceKind::Local => SkillSource::Local {
                path: expand_tilde(s.path.as_deref().unwrap_or_default()),
            },
        };
        out.push(Capability {
            id: CapId::new(Kind::Skill, name.clone()),
            origin: Origin::Manifest,
            spec: Spec::Skill { source, path: None },
            targets: s.targets.clone(),
        });
    }

    for (name, e) in &m.mcps {
        out.push(Capability {
            id: CapId::new(Kind::Mcp, name.clone()),
            origin: Origin::Manifest,
            spec: Spec::Mcp {
                command: e.command.clone(),
                args: e.args.clone(),
                env: e.env.clone(),
            },
            targets: e.targets.clone(),
        });
    }

    for (key, p) in &m.plugins {
        // 身份用 entry 名（spec §2.1 第 3 条），缺省回退到清单 key
        let entry = p.entry.clone().unwrap_or_else(|| key.clone());
        let marketplace = match p.source {
            PluginSourceKind::ClaudeMarketplace => p.marketplace.clone(),
            PluginSourceKind::Github => p.repo.clone(),
            PluginSourceKind::ZipUrl => None,
        };
        out.push(Capability {
            id: CapId::new(Kind::Plugin, entry.clone()),
            origin: Origin::Manifest,
            spec: Spec::Plugin {
                marketplace,
                entry,
                // 期望侧无 SHA：manifest 只有 ref（git 源）。ref 落在 version 位，
                // compare_versions 走 version 兜底；实际侧有 SHA 时判 Unknown → in-sync。
                version: p.git_ref.clone(),
                sha: None,
                install_path: None,
                enabled: true,
            },
            targets: p.targets.clone(),
        });
    }

    out.sort_by(|a, b| a.id.label().cmp(&b.id.label()));
    out
}

/// plugin 名 → degrade 策略。非 plugin 来源的能力一律 Report。
#[derive(Debug, Clone, Default)]
pub struct DegradeTable {
    by_plugin: BTreeMap<String, DegradePolicy>,
}

impl DegradeTable {
    pub fn new(m: &Manifest) -> Self {
        let mut by_plugin = BTreeMap::new();
        for (key, p) in &m.plugins {
            let entry = p.entry.clone().unwrap_or_else(|| key.clone());
            by_plugin.insert(entry, p.degrade.clone());
        }
        Self { by_plugin }
    }

    pub fn action(&self, cap: &Capability) -> DegradeAction {
        let policy = match &cap.origin {
            Origin::Plugin { name } => self.by_plugin.get(name),
            _ => None,
        };
        match policy {
            Some(p) => p.action_for(cap.id.kind),
            None => DegradeAction::Report,
        }
    }
}
```

- [ ] **Step 4: 在 `src/lib.rs` 声明模块**

```rust
pub mod capability;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod secrets;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib lower::`
Expected: PASS（10 tests）

- [ ] **Step 6: Commit**

```bash
git add src/lower.rs src/lib.rs
git commit -m "feat: 清单降级为归一化 Capability 流，plugin 身份用 entry 名"
```

---

### Task 7: skills 目录扫描与来源分类

**Files:**
- Create: `src/skills_dir.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `home::Home`（Task 1）
- Produces: `SkillStore`、`SkillEntry`、`read_skill_entries`、`classify`、`plugin_from_path`

这是 spec §2.1 第 4 条的落地件。实测本机存在五种形态：真实目录、相对 symlink 指向共享库、
symlink **指回 harness**（反向）、symlink 直接进 plugin 缓存（绕过 `.agents`）、断链。
`read_state` 必须全部认出来，否则 drift 判定会误报。

- [ ] **Step 1: 写失败的测试**

创建 `src/skills_dir.rs`，末尾写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    #[test]
    fn empty_or_missing_dir_yields_nothing() {
        let t = TempHome::new();
        assert!(read_skill_entries(&t.path(".claude/skills"), &t.home).is_empty());
        t.mkdir(".claude/skills");
        assert!(read_skill_entries(&t.path(".claude/skills"), &t.home).is_empty());
    }

    #[test]
    fn skips_dotfiles_like_ds_store() {
        let t = TempHome::new();
        t.write(".claude/skills/.DS_Store", "junk").mkdir(".claude/skills/real");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "real");
    }

    #[test]
    fn real_dir_is_harness_local() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/trim-cot-leakage");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got[0].link_target, None);
        assert!(got[0].resolved.is_some());
        assert_eq!(got[0].store, SkillStore::HarnessLocal);
        assert!(!got[0].is_broken());
    }

    #[test]
    fn relative_symlink_into_shared_store_is_shared() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/dingtalk-mail")
            .symlink(".claude/skills/dingtalk-mail", "../../.agents/skills/dingtalk-mail");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got[0].link_target, Some(PathBuf::from("../../.agents/skills/dingtalk-mail")));
        assert_eq!(got[0].store, SkillStore::Shared);
        assert!(!got[0].is_broken());
    }

    #[test]
    fn absolute_symlink_back_into_harness_is_harness_local() {
        // 实测 ~/.agents/skills/mcp-builder -> ~/.claude/skills/mcp-builder（反向）
        let t = TempHome::new();
        t.mkdir(".claude/skills/mcp-builder");
        let target = t.path(".claude/skills/mcp-builder");
        t.symlink(".agents/skills/mcp-builder", &target.to_string_lossy());
        let got = read_skill_entries(&t.path(".agents/skills"), &t.home);
        assert_eq!(got[0].name, "mcp-builder");
        assert_eq!(got[0].store, SkillStore::HarnessLocal);
    }

    #[test]
    fn symlink_into_plugin_cache_is_plugin_cache_and_yields_plugin_name() {
        // 实测 ~/.codex/skills/brainstorming ->
        //   ~/.claude/plugins/cache/<mp>/superpowers/<sha>/skills/brainstorming
        let t = TempHome::new();
        t.mkdir(".claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming");
        let target = t.path(
            ".claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming",
        );
        t.symlink(".codex/skills/brainstorming", &target.to_string_lossy());

        let got = read_skill_entries(&t.path(".codex/skills"), &t.home);
        assert_eq!(got[0].store, SkillStore::PluginCache);
        let resolved = got[0].resolved.clone().expect("resolves");
        assert_eq!(plugin_from_path(&resolved).as_deref(), Some("superpowers"));
    }

    #[test]
    fn plugin_from_path_handles_managed_layout() {
        // kimi: plugins/managed/<id>/skills/<name>
        let p = PathBuf::from("/h/.kimi-code/plugins/managed/superpowers/skills/tdd");
        assert_eq!(plugin_from_path(&p).as_deref(), Some("superpowers"));
    }

    #[test]
    fn plugin_from_path_returns_none_outside_caches() {
        assert_eq!(plugin_from_path(&PathBuf::from("/h/.agents/skills/foo")), None);
        assert_eq!(plugin_from_path(&PathBuf::from("/h/cache/foo/bar")), None, "cache without plugins/ is not a plugin cache");
    }

    #[test]
    fn broken_symlink_is_reported_not_skipped() {
        let t = TempHome::new();
        t.mkdir(".claude/skills")
            .symlink(".claude/skills/gone", "../../.agents/skills/does-not-exist");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got.len(), 1, "broken links must surface for doctor, not vanish");
        assert!(got[0].is_broken());
        assert_eq!(got[0].resolved, None);
        assert_eq!(got[0].store, SkillStore::Other);
    }

    #[test]
    fn symlink_cycle_is_reported_as_broken_not_infinite_loop() {
        let t = TempHome::new();
        t.mkdir(".agents/skills")
            .symlink(".agents/skills/a", "b")
            .symlink(".agents/skills/b", "a");
        let got = read_skill_entries(&t.path(".agents/skills"), &t.home);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.is_broken()), "cycle must not hang or resolve");
    }

    #[test]
    fn results_are_sorted_by_name() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/zeta")
            .mkdir(".claude/skills/alpha")
            .mkdir(".claude/skills/mid");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn plain_files_are_ignored_only_dirs_and_symlinks_count() {
        let t = TempHome::new();
        t.write(".claude/skills/notes.md", "x").mkdir(".claude/skills/real");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "real");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib skills_dir::`
Expected: FAIL —类型与函数未定义。

- [ ] **Step 3: 实现 `src/skills_dir.rs`**

```rust
//! skills 目录扫描。
//!
//! spec §2.1 第 4 条：`~/.agents/skills` 共享约定只部分成立——实测存在反向链接、
//! 直接指向 plugin 缓存的链接、以及不属于任何共享库的真实目录。本模块把这些形态统一成
//! 带来源分类的 `SkillEntry`，供 adapter 归一化与 doctor 断链报告使用。

use crate::home::Home;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// 一个 skill 条目的内容实际归属哪里。drift 判定依赖它：
/// `PluginCache` 内的 skill 是 plugin 的分发产物，不算 drift。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillStore {
    /// 在 ~/.agents/skills 共享库内
    Shared,
    /// 在某家 harness 自己的目录内
    HarnessLocal,
    /// 在某个 plugin 缓存 / managed 目录内
    PluginCache,
    /// 无法归类，或链接断裂
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    /// 扫描到的路径本身（目录或 symlink）
    pub path: PathBuf,
    /// `read_link` 的原始值；非 symlink 为 None
    pub link_target: Option<PathBuf>,
    /// 多跳解析后的真实路径；断链或成环为 None
    pub resolved: Option<PathBuf>,
    pub store: SkillStore,
}

impl SkillEntry {
    pub fn is_broken(&self) -> bool {
        self.link_target.is_some() && self.resolved.is_none()
    }
}

/// 扫描一个 skills 目录。目录不存在返回空。结果按 name 排序（确定性）。
pub fn read_skill_entries(dir: &Path, home: &Home) -> Vec<SkillEntry> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out: Vec<SkillEntry> = Vec::new();
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue; // .DS_Store 之类
        }

        let path = item.path();
        let link_target = fs::read_link(&path).ok();
        let file_type = match item.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        // 只收目录与 symlink（symlink 可能指向目录）；普通文件忽略。
        // 注意 file_type 不跟随 symlink，故断链也走到这里，由 canonicalize 判死。
        if !file_type.is_dir() && link_target.is_none() {
            continue;
        }

        // canonicalize 一次性解多跳；断链与成环都返回 Err（OS 报 ELOOP）
        let resolved = fs::canonicalize(&path).ok();
        let store = match &resolved {
            Some(r) if r.is_dir() => classify(r, home),
            _ => SkillStore::Other,
        };

        out.push(SkillEntry { name, path, link_target, resolved, store });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 判定一个已解析的真实路径归属哪个库。
pub fn classify(resolved: &Path, home: &Home) -> SkillStore {
    if resolved.starts_with(home.agents_skills()) {
        return SkillStore::Shared;
    }
    if is_in_plugin_cache(resolved) {
        return SkillStore::PluginCache;
    }
    if home.harness_dirs().iter().any(|d| resolved.starts_with(d)) {
        return SkillStore::HarnessLocal;
    }
    SkillStore::Other
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn is_in_plugin_cache(path: &Path) -> bool {
    let comps = normal_components(path);
    comps.iter().any(|c| c == "plugins")
        && comps.iter().any(|c| c == "cache" || c == "managed")
}

/// 从一个 skill 路径反推它所属的 plugin 名。覆盖两种实测布局：
///   `.../plugins/cache/<marketplace>/<name>/<version>/skills/<skill>` → `<name>`
///   `.../plugins/managed/<id>/skills/<skill>`                        → `<id>`
pub fn plugin_from_path(path: &Path) -> Option<String> {
    let comps = normal_components(path);
    let i = comps.iter().position(|c| c == "cache" || c == "managed")?;
    if !comps[..i].iter().any(|c| c == "plugins") {
        return None;
    }
    if comps[i] == "managed" {
        comps.get(i + 1).cloned()
    } else {
        comps.get(i + 2).cloned()
    }
}
```

- [ ] **Step 4: 在 `src/lib.rs` 声明模块**

```rust
pub mod capability;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod secrets;
pub mod skills_dir;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib skills_dir::`
Expected: PASS（12 tests）

若 `symlink_cycle_is_reported_as_broken_not_infinite_loop` 挂住不返回，说明用了手写循环解析而非
`fs::canonicalize`——改回 `canonicalize`，由 OS 检测 `ELOOP`。

若 `absolute_symlink_back_into_harness_is_harness_local` 失败，检查 `classify` 的判断顺序：
`Shared` 必须在 `HarnessLocal` 之前，`PluginCache` 必须在这两者之间（claude 插件缓存物理上位于
`~/.claude` 内，若先判 HarnessLocal 会误分类）。

- [ ] **Step 6: Commit**

```bash
git add src/skills_dir.rs src/lib.rs
git commit -m "feat: skills 目录扫描，识别多跳链接/断链/来源分类"
```

---

### Task 8: Adapter trait 与注册表

**Files:**
- Create: `src/adapters/mod.rs`, `src/adapters/claude_code.rs`, `src/adapters/codex.rs`, `src/adapters/kimi_code.rs`
- Modify: `src/lib.rs`, `src/testutil.rs`

**Interfaces:**
- Consumes: `Home`、`Capability`、`Support`
- Produces: `HarnessAdapter` trait、`registry`、`lookup`、`CLAUDE_CODE`/`CODEX`/`KIMI_CODE` 常量、`testutil::FakeAdapter`

本 Task 只建骨架：三个 adapter 的 `read_state` 先返回空、`supports` 先返回 `Native`，
真正实现分别在 Task 9 / 10 / 11 / 12。`apply` **不在** Plan A 的 trait 里（Plan B 再加，
避免此处出现无法测试的桩方法）。

- [ ] **Step 1: 在 `src/testutil.rs` 追加 `FakeAdapter`**

Task 13/14 的 diff 与 golden 测试需要可控 adapter，不该依赖真实配置树。追加到
`src/testutil.rs`（`impl TempHome` 之后、`#[cfg(test)] mod tests` 之前）：

```rust
use crate::adapters::HarnessAdapter;
use crate::capability::{CapId, Capability, Kind, Origin, Spec, Support, Targets};
use std::collections::BTreeMap;

/// 测试替身：read_state 返回预置能力，supports 按 kind 查表。
pub struct FakeAdapter {
    pub adapter_name: String,
    pub detected: bool,
    pub caps: Vec<Capability>,
    /// kind -> 拒绝理由。未列出的 kind 判 Native。
    pub unsupported: BTreeMap<Kind, String>,
}

impl FakeAdapter {
    pub fn native(name: &str) -> Self {
        Self {
            adapter_name: name.into(),
            detected: true,
            caps: Vec::new(),
            unsupported: BTreeMap::new(),
        }
    }

    pub fn with_caps(mut self, caps: Vec<Capability>) -> Self {
        self.caps = caps;
        self
    }

    pub fn with_detected(mut self, detected: bool) -> Self {
        self.detected = detected;
        self
    }

    pub fn refusing(mut self, kind: Kind, reason: &str) -> Self {
        self.unsupported.insert(kind, reason.into());
        self
    }

    /// 造一条「已安装」能力。
    pub fn cap(kind: Kind, name: &str, spec: Spec) -> Capability {
        Capability {
            id: CapId::new(kind, name),
            origin: Origin::Adopted { harness: String::new() },
            spec,
            targets: Targets::All,
        }
    }

    /// 造一条由 plugin bundle 携带的 skill。
    pub fn bundled_skill(name: &str, plugin: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Skill, name),
            origin: Origin::Plugin { name: plugin.into() },
            spec: Spec::Skill {
                source: crate::capability::SkillSource::PluginBundle { plugin: plugin.into() },
                path: None,
            },
            targets: Targets::All,
        }
    }

    /// 造一条 MCP 能力。
    pub fn mcp(name: &str, command: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Mcp, name),
            origin: Origin::Manifest,
            spec: Spec::Mcp { command: command.into(), args: Vec::new(), env: BTreeMap::new() },
            targets: Targets::All,
        }
    }

    /// 造一条 plugin 能力。
    pub fn plugin(entry: &str, version: Option<&str>, sha: Option<&str>) -> Capability {
        Capability {
            id: CapId::new(Kind::Plugin, entry),
            origin: Origin::Manifest,
            spec: Spec::Plugin {
                marketplace: None,
                entry: entry.into(),
                version: version.map(String::from),
                sha: sha.map(String::from),
                install_path: None,
                enabled: true,
            },
            targets: Targets::All,
        }
    }
}

impl HarnessAdapter for FakeAdapter {
    fn name(&self) -> &str {
        &self.adapter_name
    }
    fn detect(&self) -> bool {
        self.detected
    }
    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(self.caps.clone())
    }
    fn supports(&self, cap: &Capability) -> Support {
        match self.unsupported.get(&cap.id.kind) {
            Some(reason) => Support::Unsupported { reason: reason.clone() },
            None => Support::Native,
        }
    }
}
```

同时把文件顶部的 `use` 补上（`Home`、`Path`、`PathBuf`、`TempDir` 已在 Task 1 引入，无需重复）。

- [ ] **Step 2: 写失败的测试**

创建 `src/adapters/mod.rs`，末尾写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    #[test]
    fn registry_returns_three_adapters_in_fixed_order() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        let names: Vec<&str> = reg.iter().map(|a| a.name()).collect();
        assert_eq!(names, vec![CLAUDE_CODE, CODEX, KIMI_CODE]);
    }

    #[test]
    fn lookup_finds_registered_and_rejects_unknown() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        assert!(lookup(&reg, "codex").is_some());
        assert!(lookup(&reg, "cursor").is_none());
    }

    #[test]
    fn detect_is_false_on_empty_home() {
        let t = TempHome::new();
        for a in registry(&t.home) {
            assert!(!a.detect(), "{} should not detect on empty home", a.name());
        }
    }

    #[test]
    fn detect_is_true_when_harness_dir_exists() {
        let t = TempHome::new();
        t.mkdir(".claude").mkdir(".codex").mkdir(".kimi-code");
        let detected: Vec<&str> = registry(&t.home)
            .iter()
            .filter(|a| a.detect())
            .map(|a| a.name())
            .collect();
        assert_eq!(detected, vec![CLAUDE_CODE, CODEX, KIMI_CODE]);
    }

    #[test]
    fn detect_is_per_harness_not_all_or_nothing() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let reg = registry(&t.home);
        assert!(!lookup(&reg, CLAUDE_CODE).unwrap().detect());
        assert!(lookup(&reg, CODEX).unwrap().detect());
        assert!(!lookup(&reg, KIMI_CODE).unwrap().detect());
    }

    #[test]
    fn read_state_on_empty_home_is_empty_not_error() {
        let t = TempHome::new();
        for a in registry(&t.home) {
            let caps = a.read_state().expect("missing config must not be an error");
            assert!(caps.is_empty(), "{} returned {:?}", a.name(), caps);
        }
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib adapters::`
Expected: FAIL —模块不存在。

- [ ] **Step 4: 实现 `src/adapters/mod.rs`**

```rust
//! harness adapter 层。新增 harness = 新模块实现 trait + 在 `registry` 加一行（spec §7）。

pub mod claude_code;
pub mod codex;
pub mod kimi_code;

use crate::capability::{Capability, Support};
use crate::home::Home;

pub const CLAUDE_CODE: &str = "claude-code";
pub const CODEX: &str = "codex";
pub const KIMI_CODE: &str = "kimi-code";

pub trait HarnessAdapter {
    fn name(&self) -> &str;

    /// 本机是否装了这家。detect 失败只警告，不报错（spec §4 harnesses 注释）。
    fn detect(&self) -> bool;

    /// 扫描真实配置树 → 归一化能力。配置文件缺失返回空 Vec，**不是**错误。
    fn read_state(&self) -> anyhow::Result<Vec<Capability>>;

    /// 这家能否接纳该能力——「拆解降级」的判定点。
    fn supports(&self, cap: &Capability) -> Support;
}

pub fn registry(home: &Home) -> Vec<Box<dyn HarnessAdapter>> {
    vec![
        Box::new(claude_code::ClaudeCode::new(home.clone())),
        Box::new(codex::Codex::new(home.clone())),
        Box::new(kimi_code::KimiCode::new(home.clone())),
    ]
}

pub fn lookup<'a>(
    reg: &'a [Box<dyn HarnessAdapter>],
    name: &str,
) -> Option<&'a Box<dyn HarnessAdapter>> {
    reg.iter().find(|a| a.name() == name)
}
```

- [ ] **Step 5: 实现三个 adapter 骨架**

`src/adapters/claude_code.rs`：

```rust
use crate::adapters::{HarnessAdapter, CLAUDE_CODE};
use crate::capability::{Capability, Support};
use crate::home::Home;

pub struct ClaudeCode {
    home: Home,
}

impl ClaudeCode {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }
}

impl HarnessAdapter for ClaudeCode {
    fn name(&self) -> &str {
        CLAUDE_CODE
    }

    fn detect(&self) -> bool {
        self.home.claude_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(Vec::new()) // Task 9 实现
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}
```

`src/adapters/codex.rs`：

```rust
use crate::adapters::{HarnessAdapter, CODEX};
use crate::capability::{Capability, Support};
use crate::home::Home;

pub struct Codex {
    home: Home,
}

impl Codex {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }
}

impl HarnessAdapter for Codex {
    fn name(&self) -> &str {
        CODEX
    }

    fn detect(&self) -> bool {
        self.home.codex_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(Vec::new()) // Task 10 实现
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}
```

`src/adapters/kimi_code.rs`：

```rust
use crate::adapters::{HarnessAdapter, KIMI_CODE};
use crate::capability::{Capability, Support};
use crate::home::Home;

pub struct KimiCode {
    home: Home,
}

impl KimiCode {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }
}

impl HarnessAdapter for KimiCode {
    fn name(&self) -> &str {
        KIMI_CODE
    }

    fn detect(&self) -> bool {
        self.home.kimi_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(Vec::new()) // Task 11 实现
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}
```

- [ ] **Step 6: 在 `src/lib.rs` 声明模块**

```rust
pub mod adapters;
pub mod capability;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod secrets;
pub mod skills_dir;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test --lib adapters::`
Expected: PASS（6 tests）

`cargo build` 此时会有 dead-code 警告（`home()` 尚无调用方），Task 9–11 会消掉；不要为消除警告
而加 `#[allow(dead_code)]`。

- [ ] **Step 8: Commit**

```bash
git add src/adapters src/lib.rs src/testutil.rs
git commit -m "feat: HarnessAdapter trait、注册表与三家骨架"
```

---

### Task 9: claude-code read_state

**Files:**
- Modify: `src/adapters/claude_code.rs`

**Interfaces:**
- Consumes: `Home::{claude_skills, claude_json, claude_installed_plugins, claude_settings}`、`skills_dir::{read_skill_entries, plugin_from_path, SkillStore}`、`capability::*`
- Produces: `ClaudeCode::read_state()`；以及三个 `pub(crate)` 工具 `read_json_if_present`、
  `split_plugin_key`、`mcp_capability`。Task 11 的 kimi adapter 复用 `read_json_if_present`
  与 `mcp_capability`；`split_plugin_key` 只 claude 用（kimi 的 `installed.json` 是数组、
  身份在 `id` 字段里，不需要拆 `name@marketplace`）。

实测数据形状（本机 2026-09-23 盘点，即测试 fixture 的依据）：

`~/.claude.json` 顶层 `mcpServers`——**不是** `settings.json`，见 spec §2.1 第 1 条：
```json
{ "numStartups": 52,
  "mcpServers": { "kimi-cu": { "type": "stdio", "command": "/abs/bin",
                               "args": ["mcp","-s","user"], "env": {} } },
  "projects": { "/some/path": { "trust_level": "trusted" } } }
```

`~/.claude/plugins/installed_plugins.json`：
```json
{ "version": 2,
  "plugins": { "superpowers@claude-plugins-official": [
      { "scope": "user", "installPath": "/abs/path", "version": "6.4.1",
        "installedAt": "2026-08-26T15:19:21.589Z", "lastUpdated": "2026-09-23T00:37:36.131Z",
        "gitCommitSha": "b36e0829c6d0140e93cfef2ca599b1b07d4a7797" } ] } }
```

`~/.claude/settings.json` 的 `enabledPlugins`：`{ "superpowers@claude-plugins-official": true }`

范围限定：只读**用户级** MCP（`~/.claude.json` 顶层）。`projects.*.mcpServers` 是项目级，
不在 v1 范围（spec §1 非目标）。

- [ ] **Step 1: 写失败的测试**

追加到 `src/adapters/claude_code.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec, Targets};
    use crate::testutil::TempHome;
    use std::path::PathBuf;

    fn find<'a>(caps: &'a [Capability], kind: Kind, name: &str) -> Option<&'a Capability> {
        caps.iter().find(|c| c.id.kind == kind && c.id.name == name)
    }

    #[test]
    fn reads_skill_symlink_from_shared_store() {
        let t = TempHome::new();
        t.mkdir(".claude")
            .mkdir(".agents/skills/dingtalk-mail")
            .symlink(".claude/skills/dingtalk-mail", "../../.agents/skills/dingtalk-mail");

        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "dingtalk-mail").expect("skill present");
        assert_eq!(
            s.spec,
            Spec::Skill { source: SkillSource::AgentsDir, path: Some(t.path(".agents/skills/dingtalk-mail")) }
        );
        assert_eq!(s.origin, Origin::Adopted { harness: "claude-code".into() });
        assert_eq!(s.targets, Targets::All);
    }

    #[test]
    fn reads_real_dir_skill_as_harness_local() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/trim-cot-leakage");
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "trim-cot-leakage").unwrap();
        match &s.spec {
            Spec::Skill { source: SkillSource::Local { path }, .. } => {
                assert!(path.ends_with(".claude/skills/trim-cot-leakage"), "{path:?}");
            }
            other => panic!("expected Local, got {other:?}"),
        }
    }

    #[test]
    fn broken_skill_link_still_surfaces() {
        let t = TempHome::new();
        t.mkdir(".claude/skills")
            .symlink(".claude/skills/gone", "../../.agents/skills/nope");
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "gone").expect("broken link must be reported");
        assert_eq!(s.spec, Spec::Skill { source: SkillSource::Unknown, path: None });
    }

    #[test]
    fn reads_mcp_from_claude_json_not_settings_json() {
        let t = TempHome::new();
        t.mkdir(".claude")
            // settings.json 里放一个同名 MCP，若实现读错文件就会拿到 "wrong"
            .write(".claude/settings.json", r#"{ "mcpServers": { "ctx": { "command": "wrong" } } }"#)
            .write(
                ".claude.json",
                r#"{
                  "numStartups": 52,
                  "mcpServers": {
                    "kimi-cu": {
                      "type": "stdio",
                      "command": "/Applications/KimiCU.app/Contents/MacOS/kimi-cu",
                      "args": ["mcp", "-s", "user"],
                      "env": {}
                    },
                    "ctx": { "command": "npx", "args": ["-y", "@upstash/context7-mcp"],
                             "env": { "CONTEXT7_API_KEY": "${CONTEXT7_API_KEY}" } }
                  },
                  "projects": { "/some/path": { "trust_level": "trusted" } }
                }"#,
            );

        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let m = find(&caps, Kind::Mcp, "ctx").expect("mcp read from ~/.claude.json");
        assert_eq!(
            m.spec,
            Spec::Mcp {
                command: "npx".into(),
                args: vec!["-y".into(), "@upstash/context7-mcp".into()],
                env: [("CONTEXT7_API_KEY".to_string(), "${CONTEXT7_API_KEY}".to_string())].into(),
            }
        );
        assert!(find(&caps, Kind::Mcp, "kimi-cu").is_some());
        assert_eq!(caps.iter().filter(|c| c.id.kind == Kind::Mcp).count(), 2);
    }

    #[test]
    fn project_level_mcp_servers_are_out_of_scope() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": {}, "projects": { "/p": { "mcpServers": { "proj-only": { "command": "c" } } } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Mcp, "proj-only").is_none(), "v1 只读用户级 MCP");
    }

    #[test]
    fn env_reference_is_preserved_unexpanded() {
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "mcpServers": { "m": { "command": "c", "env": { "K": "${HENV_TEST_SECRET_XYZ}" } } } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Mcp, "m").unwrap().spec {
            Spec::Mcp { env, .. } => {
                assert_eq!(env["K"], "${HENV_TEST_SECRET_XYZ}", "must stay a reference");
                assert!(!env.values().any(|v| v == "expanded-value"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_plugin_with_sha_and_marketplace() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{
              "version": 2,
              "plugins": {
                "superpowers@claude-plugins-official": [
                  { "scope": "user",
                    "installPath": "/Users/x/.claude/plugins/cache/claude-plugins-official/superpowers/6.4.1",
                    "version": "6.4.1",
                    "installedAt": "2026-08-26T15:19:21.589Z",
                    "lastUpdated": "2026-09-23T00:37:36.131Z",
                    "gitCommitSha": "b36e0829c6d0140e93cfef2ca599b1b07d4a7797" }
                ],
                "context7@claude-plugins-official": [
                  { "scope": "user", "installPath": "/p/context7", "version": "db467cc56673" }
                ]
              }
            }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "enabledPlugins": { "context7@claude-plugins-official": false } }"#,
        );

        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let sp = find(&caps, Kind::Plugin, "superpowers").expect("identity is the entry name");
        assert_eq!(
            sp.spec,
            Spec::Plugin {
                marketplace: Some("claude-plugins-official".into()),
                entry: "superpowers".into(),
                version: Some("6.4.1".into()),
                sha: Some("b36e0829c6d0140e93cfef2ca599b1b07d4a7797".into()),
                install_path: Some(PathBuf::from(
                    "/Users/x/.claude/plugins/cache/claude-plugins-official/superpowers/6.4.1"
                )),
                enabled: true,
            }
        );

        let c7 = find(&caps, Kind::Plugin, "context7").unwrap();
        match &c7.spec {
            Spec::Plugin { enabled, sha, version, .. } => {
                assert!(!enabled, "settings.json enabledPlugins=false must win");
                assert_eq!(*sha, None);
                assert_eq!(
                    version.as_deref(),
                    Some("db467cc56673"),
                    "sha 形状的 version 目录名仍留在 version 字段，不猜测"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugin_without_sha_field_is_none_not_empty() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "x@mp": [ { "scope": "user", "version": "1.0.0" } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "x").unwrap().spec {
            Spec::Plugin { sha, version, .. } => {
                assert_eq!(*sha, None);
                assert_eq!(version.as_deref(), Some("1.0.0"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugin_key_without_marketplace_suffix_still_parses() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "solo": [ { "version": "1.0" } ] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "solo").unwrap().spec {
            Spec::Plugin { marketplace, entry, .. } => {
                assert_eq!(*marketplace, None);
                assert_eq!(entry, "solo");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_install_array_is_skipped() {
        let t = TempHome::new();
        t.write(
            ".claude/plugins/installed_plugins.json",
            r#"{ "version": 2, "plugins": { "ghost@mp": [] } }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Plugin, "ghost").is_none());
    }

    #[test]
    fn missing_files_are_not_errors() {
        let t = TempHome::new();
        t.mkdir(".claude");
        let caps = ClaudeCode::new(t.home.clone()).read_state().expect("no config != error");
        assert!(caps.is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_naming_the_file() {
        let t = TempHome::new();
        t.write(".claude.json", "{ not json");
        let err = ClaudeCode::new(t.home.clone()).read_state().unwrap_err().to_string();
        assert!(err.contains(".claude.json"), "{err}");
    }

    #[test]
    fn unrelated_user_fields_do_not_break_parsing() {
        // merge 安全的前置条件：读侧必须对未知字段免疫
        let t = TempHome::new();
        t.write(
            ".claude.json",
            r#"{ "userID": "abc", "tipsHistory": {"x": 1}, "mcpServers": { "m": { "command": "c" } },
                 "someFutureField": [1,2,3] }"#,
        );
        t.write(
            ".claude/settings.json",
            r#"{ "theme": "auto", "tui": "fullscreen", "permissions": { "allow": ["x"] },
                 "enabledPlugins": {} }"#,
        );
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Mcp, "m").is_some());
    }

    #[test]
    fn output_is_sorted_and_deduplicated_by_id() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/zzz")
            .mkdir(".claude/skills/aaa")
            .write(".claude.json", r#"{ "mcpServers": { "z": { "command": "c" }, "a": { "command": "c" } } }"#);
        let caps = ClaudeCode::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "{labels:?}");
        let mut dedup = labels.clone();
        dedup.dedup();
        assert_eq!(labels.len(), dedup.len(), "duplicate ids: {labels:?}");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib adapters::claude_code::`
Expected: FAIL —`read_state` 仍返回空 Vec。

- [ ] **Step 3: 实现**

替换 `src/adapters/claude_code.rs` 的 import 段：

```rust
use crate::adapters::{HarnessAdapter, CLAUDE_CODE};
use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets};
use crate::home::Home;
use crate::skills_dir::{plugin_from_path, read_skill_entries, SkillStore};
use anyhow::Context;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
```

在 `impl ClaudeCode` 中追加私有读取方法：

```rust
impl ClaudeCode {
    fn read_skills(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        for e in read_skill_entries(&self.home.claude_skills(), &self.home) {
            let source = match e.store {
                SkillStore::Shared => SkillSource::AgentsDir,
                SkillStore::PluginCache => {
                    let resolved = e.resolved.clone().unwrap_or_default();
                    match plugin_from_path(&resolved) {
                        Some(plugin) => SkillSource::PluginBundle { plugin },
                        None => SkillSource::Unknown,
                    }
                }
                SkillStore::HarnessLocal => SkillSource::Local {
                    path: e.resolved.clone().unwrap_or_else(|| e.path.clone()),
                },
                // 断链或指向库外：来源不可判定，交给 doctor 报告
                SkillStore::Other => SkillSource::Unknown,
            };
            out.push(Capability {
                id: CapId::new(Kind::Skill, e.name),
                origin: Origin::Adopted { harness: CLAUDE_CODE.into() },
                spec: Spec::Skill { source, path: e.resolved },
                targets: Targets::All,
            });
        }
        out
    }

    /// 用户级 MCP 在 ~/.claude.json 顶层 mcpServers（spec §2.1 第 1 条）。
    fn read_mcps(&self) -> anyhow::Result<Vec<Capability>> {
        let Some(root) = read_json_if_present(&self.home.claude_json())? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if let Some(servers) = root.get("mcpServers").and_then(Value::as_object) {
            for (name, cfg) in servers {
                out.push(mcp_capability(name, cfg, CLAUDE_CODE));
            }
        }
        Ok(out)
    }

    fn read_plugins(&self) -> anyhow::Result<Vec<Capability>> {
        let Some(root) = read_json_if_present(&self.home.claude_installed_plugins())? else {
            return Ok(Vec::new());
        };
        // settings.json 读失败不该拖垮 plugin 扫描，退化成「全部视为 enabled」
        let enabled_map = self.read_enabled_plugins().unwrap_or_default();

        let mut out = Vec::new();
        let plugins = root.get("plugins").and_then(Value::as_object);
        for (key, installs) in plugins.into_iter().flatten() {
            let (entry, marketplace) = split_plugin_key(key);
            let Some(install) = installs.as_array().and_then(|a| a.first()) else {
                continue;
            };
            let enabled = enabled_map.get(key).copied().unwrap_or(true);
            out.push(Capability {
                id: CapId::new(Kind::Plugin, entry.clone()),
                origin: Origin::Adopted { harness: CLAUDE_CODE.into() },
                spec: Spec::Plugin {
                    marketplace,
                    entry,
                    version: install.get("version").and_then(Value::as_str).map(String::from),
                    sha: install.get("gitCommitSha").and_then(Value::as_str).map(String::from),
                    install_path: install.get("installPath").and_then(Value::as_str).map(PathBuf::from),
                    enabled,
                },
                targets: Targets::All,
            });
        }
        Ok(out)
    }

    /// settings.json 的 enabledPlugins 是门禁；installed_plugins.json 才是安装记录。
    fn read_enabled_plugins(&self) -> anyhow::Result<BTreeMap<String, bool>> {
        let Some(root) = read_json_if_present(&self.home.claude_settings())? else {
            return Ok(BTreeMap::new());
        };
        let mut out = BTreeMap::new();
        if let Some(obj) = root.get("enabledPlugins").and_then(Value::as_object) {
            for (k, v) in obj {
                out.insert(k.clone(), v.as_bool().unwrap_or(true));
            }
        }
        Ok(out)
    }
}
```

在同文件末尾追加三个 `pub(crate)` 工具（Task 11 复用前两个）：

```rust
/// 读 JSON；文件不存在返回 Ok(None)，语法错误返回带路径的 Err。
pub(crate) fn read_json_if_present(path: &Path) -> anyhow::Result<Option<Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let v = serde_json::from_str(&text)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            Ok(Some(v))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("failed to read {}", path.display()))),
    }
}

/// `"superpowers@claude-plugins-official"` → `("superpowers", Some("claude-plugins-official"))`
pub(crate) fn split_plugin_key(key: &str) -> (String, Option<String>) {
    match key.split_once('@') {
        Some((entry, mp)) => (entry.to_string(), Some(mp.to_string())),
        None => (key.to_string(), None),
    }
}

/// 把一个 MCP 配置对象归一化。env 值保持 `${VAR}` 引用形态，绝不展开。
pub(crate) fn mcp_capability(name: &str, cfg: &Value, harness: &str) -> Capability {
    let mut env = BTreeMap::new();
    if let Some(obj) = cfg.get("env").and_then(Value::as_object) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }
    Capability {
        id: CapId::new(Kind::Mcp, name.to_string()),
        origin: Origin::Adopted { harness: harness.into() },
        spec: Spec::Mcp {
            command: cfg.get("command").and_then(Value::as_str).unwrap_or("").to_string(),
            args: cfg
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default(),
            env,
        },
        targets: Targets::All,
    }
}
```

最后把 `impl HarnessAdapter for ClaudeCode` 的 `read_state` 改为：

```rust
    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        let mut caps = self.read_skills();
        caps.extend(self.read_mcps()?);
        caps.extend(self.read_plugins()?);
        caps.sort_by(|a, b| a.id.label().cmp(&b.id.label()));
        // dedup_by 只去重相邻项，所以必须先 sort
        caps.dedup_by(|a, b| a.id == b.id);
        Ok(caps)
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib adapters::claude_code::`
Expected: PASS（14 tests）

若 `reads_skill_symlink_from_shared_store` 报路径不等且只差 `/private` 前缀，说明 Task 1 的
`Home::new` canonicalize 修正没生效——回去检查，不要在测试里 canonicalize 掩盖它。

- [ ] **Step 5: Commit**

```bash
git add src/adapters/claude_code.rs
git commit -m "feat: claude-code read_state（MCP 读自 ~/.claude.json 而非 settings.json）"
```

---

### Task 10: codex read_state

**Files:**
- Modify: `src/adapters/codex.rs`

**Interfaces:**
- Consumes: `Home::{codex_skills, codex_config}`、`skills_dir::*`、`capability::*`
- Produces: `Codex::read_state()`

实测 `~/.codex/config.toml` 形状：

```toml
model = "deepseek-v4-flash"
model_reasoning_effort = "high"

[projects."/Users/x/Code"]
trust_level = "trusted"

[mcp_servers]

[mcp_servers.aiflowdesign]
type = "stdio"
command = "/abs/Electron"
args = ["/abs/app", "--mcp-stdio", "--app", "desktop"]

[plugins."superpowers@openai-api-curated"]
enabled = true
```

实测 `~/.codex/skills`：26 条**绝对** symlink，直接指向
`~/.claude/plugins/cache/<mp>/<name>/<sha>/skills/<skill>`——完全绕过 `~/.agents`
（spec §2.1 第 4 条）。

**已知局限**（Plan C 的 doctor 要报出来）：codex 的 `config.toml` 不记录 plugin 版本或 SHA，
缓存目录名可能是 semver、git sha 或字面量 `unknown`。故 codex 侧 plugin 的 `version`/`sha`
均为 `None`，`compare_versions` 判 `Unknown` → diff 按 in-sync 处理，不误报 update。

用 `toml::Value` 动态导航而非 typed struct：codex 的 config.toml 里有大量用户手写无关字段
（`model_providers`、`projects`、`tui`、`hooks.state`），读侧必须对未知字段免疫。

- [ ] **Step 1: 写失败的测试**

追加到 `src/adapters/codex.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec};
    use crate::testutil::TempHome;

    fn find<'a>(caps: &'a [Capability], kind: Kind, name: &str) -> Option<&'a Capability> {
        caps.iter().find(|c| c.id.kind == kind && c.id.name == name)
    }

    #[test]
    fn reads_mcp_servers_from_config_toml() {
        let t = TempHome::new();
        t.write(
            ".codex/config.toml",
            r#"model = "deepseek-v4-flash"
model_reasoning_effort = "high"

[model_providers.custom]
name = "deepseek"
base_url = "https://api.deepseek.com"

[projects."/Users/x/Code"]
trust_level = "trusted"

[mcp_servers]

[mcp_servers.aiflowdesign]
type = "stdio"
command = "/abs/Electron"
args = ["/abs/app", "--mcp-stdio", "--app", "desktop"]

[mcp_servers.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp"]

[mcp_servers.context7.env]
CONTEXT7_API_KEY = "${CONTEXT7_API_KEY}"
"#,
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();

        let a = find(&caps, Kind::Mcp, "aiflowdesign").unwrap();
        assert_eq!(
            a.spec,
            Spec::Mcp {
                command: "/abs/Electron".into(),
                args: vec!["/abs/app".into(), "--mcp-stdio".into(), "--app".into(), "desktop".into()],
                env: Default::default(),
            }
        );

        let c = find(&caps, Kind::Mcp, "context7").unwrap();
        match &c.spec {
            Spec::Mcp { env, .. } => assert_eq!(env["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}"),
            other => panic!("{other:?}"),
        }
        // 无关字段不得被当成 MCP
        assert!(find(&caps, Kind::Mcp, "custom").is_none());
    }

    #[test]
    fn reads_plugins_from_config_toml() {
        let t = TempHome::new();
        t.write(
            ".codex/config.toml",
            r#"[plugins."superpowers@openai-api-curated"]
enabled = true

[plugins."coderabbit@openai-api-curated"]
enabled = false
"#,
        );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let sp = find(&caps, Kind::Plugin, "superpowers").unwrap();
        assert_eq!(
            sp.spec,
            Spec::Plugin {
                marketplace: Some("openai-api-curated".into()),
                entry: "superpowers".into(),
                version: None,
                sha: None,
                install_path: None,
                enabled: true,
            }
        );
        match &find(&caps, Kind::Plugin, "coderabbit").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(!enabled),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugin_key_without_marketplace_still_parses() {
        let t = TempHome::new();
        t.write(".codex/config.toml", "[plugins.local-thing]\nenabled = true\n");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "local-thing").unwrap().spec {
            Spec::Plugin { marketplace, entry, .. } => {
                assert_eq!(*marketplace, None);
                assert_eq!(entry, "local-thing");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugin_enabled_defaults_true_when_field_absent() {
        let t = TempHome::new();
        t.write(".codex/config.toml", "[plugins.bare@mp]\n");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "bare").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(*enabled),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_skills_that_symlink_into_claude_plugin_cache() {
        // 实测 codex 直接链进 claude 插件缓存，完全绕过 ~/.agents
        let t = TempHome::new();
        let rel = ".claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming";
        t.mkdir(rel);
        let cached = t.path(rel);
        t.symlink(".codex/skills/brainstorming", &cached.to_string_lossy());

        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "brainstorming").unwrap();
        assert_eq!(
            s.spec,
            Spec::Skill { source: SkillSource::PluginBundle { plugin: "superpowers".into() }, path: Some(cached) }
        );
        assert_eq!(s.origin, Origin::Adopted { harness: "codex".into() });
    }

    #[test]
    fn reads_skill_symlinked_into_shared_agents_store() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/dingtalk-mail");
        let target = t.path(".agents/skills/dingtalk-mail");
        t.symlink(".codex/skills/dingtalk-mail", &target.to_string_lossy());

        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "dingtalk-mail").unwrap();
        assert_eq!(
            s.spec,
            Spec::Skill { source: SkillSource::AgentsDir, path: Some(target) }
        );
    }

    #[test]
    fn reads_real_dir_skill_as_local() {
        let t = TempHome::new();
        t.mkdir(".codex/skills/my-own");
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Skill, "my-own").unwrap().spec {
            Spec::Skill { source: SkillSource::Local { path }, .. } => {
                assert!(path.ends_with(".codex/skills/my-own"), "{path:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_config_is_not_an_error() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let caps = Codex::new(t.home.clone()).read_state().expect("no config != error");
        assert!(caps.is_empty());
    }

    #[test]
    fn malformed_toml_is_an_error_naming_the_file() {
        let t = TempHome::new();
        t.write(".codex/config.toml", "model = \n[[[");
        let err = Codex::new(t.home.clone()).read_state().unwrap_err().to_string();
        assert!(err.contains("config.toml"), "{err}");
    }

    #[test]
    fn output_is_sorted_and_deduplicated() {
        let t = TempHome::new();
        t.mkdir(".codex/skills/zzz")
            .mkdir(".codex/skills/aaa")
            .write(
                ".codex/config.toml",
                "[mcp_servers.z]\ncommand = \"c\"\n[mcp_servers.a]\ncommand = \"c\"\n",
            );
        let caps = Codex::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "{labels:?}");
        let mut dedup = labels.clone();
        dedup.dedup();
        assert_eq!(labels.len(), dedup.len(), "duplicate ids: {labels:?}");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib adapters::codex::`
Expected: FAIL —`read_state` 仍返回空 Vec。

- [ ] **Step 3: 实现**

替换 `src/adapters/codex.rs` 的 import 段：

```rust
use crate::adapters::{HarnessAdapter, CODEX};
use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets};
use crate::home::Home;
use crate::skills_dir::{plugin_from_path, read_skill_entries, SkillStore};
use anyhow::Context;
use std::collections::BTreeMap;
```

在 `impl Codex` 中追加：

```rust
impl Codex {
    fn read_skills(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        for e in read_skill_entries(&self.home.codex_skills(), &self.home) {
            let source = match e.store {
                SkillStore::Shared => SkillSource::AgentsDir,
                SkillStore::PluginCache => {
                    let resolved = e.resolved.clone().unwrap_or_default();
                    match plugin_from_path(&resolved) {
                        Some(plugin) => SkillSource::PluginBundle { plugin },
                        None => SkillSource::Unknown,
                    }
                }
                SkillStore::HarnessLocal => SkillSource::Local {
                    path: e.resolved.clone().unwrap_or_else(|| e.path.clone()),
                },
                SkillStore::Other => SkillSource::Unknown,
            };
            out.push(Capability {
                id: CapId::new(Kind::Skill, e.name),
                origin: Origin::Adopted { harness: CODEX.into() },
                spec: Spec::Skill { source, path: e.resolved },
                targets: Targets::All,
            });
        }
        out
    }

    fn read_config(&self) -> anyhow::Result<Option<toml::Value>> {
        let path = self.home.codex_config();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let v: toml::Value = toml::from_str(&text)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                Ok(Some(v))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::Error::new(e).context(format!("failed to read {}", path.display()))),
        }
    }

    fn read_mcps(&self, root: &toml::Value) -> Vec<Capability> {
        let mut out = Vec::new();
        let Some(servers) = root.get("mcp_servers").and_then(toml::Value::as_table) else {
            return out;
        };
        for (name, cfg) in servers {
            let mut env = BTreeMap::new();
            if let Some(t) = cfg.get("env").and_then(toml::Value::as_table) {
                for (k, v) in t {
                    // 保持 ${VAR} 引用形态，绝不展开
                    if let Some(s) = v.as_str() {
                        env.insert(k.clone(), s.to_string());
                    }
                }
            }
            out.push(Capability {
                id: CapId::new(Kind::Mcp, name.clone()),
                origin: Origin::Adopted { harness: CODEX.into() },
                spec: Spec::Mcp {
                    command: cfg.get("command").and_then(toml::Value::as_str).unwrap_or("").to_string(),
                    args: cfg
                        .get("args")
                        .and_then(toml::Value::as_array)
                        .map(|a| a.iter().filter_map(toml::Value::as_str).map(String::from).collect())
                        .unwrap_or_default(),
                    env,
                },
                targets: Targets::All,
            });
        }
        out
    }

    fn read_plugins(&self, root: &toml::Value) -> Vec<Capability> {
        let mut out = Vec::new();
        let Some(plugins) = root.get("plugins").and_then(toml::Value::as_table) else {
            return out;
        };
        for (key, cfg) in plugins {
            let (entry, marketplace) = split_key(key);
            // codex 不记录版本/SHA，见 Task 10 的「已知局限」
            out.push(Capability {
                id: CapId::new(Kind::Plugin, entry.clone()),
                origin: Origin::Adopted { harness: CODEX.into() },
                spec: Spec::Plugin {
                    marketplace,
                    entry,
                    version: None,
                    sha: None,
                    install_path: None,
                    enabled: cfg
                        .get("enabled")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(true),
                },
                targets: Targets::All,
            });
        }
        out
    }
}

/// `"superpowers@openai-api-curated"` → `("superpowers", Some("openai-api-curated"))`
fn split_key(key: &str) -> (String, Option<String>) {
    match key.split_once('@') {
        Some((entry, mp)) => (entry.to_string(), Some(mp.to_string())),
        None => (key.to_string(), None),
    }
}
```

把 `impl HarnessAdapter for Codex` 的 `read_state` 改为：

```rust
    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        let mut caps = self.read_skills();
        if let Some(root) = self.read_config()? {
            caps.extend(self.read_mcps(&root));
            caps.extend(self.read_plugins(&root));
        }
        caps.sort_by(|a, b| a.id.label().cmp(&b.id.label()));
        caps.dedup_by(|a, b| a.id == b.id);
        Ok(caps)
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib adapters::codex::`
Expected: PASS（10 tests）

- [ ] **Step 5: 全量回归**

Run: `cargo test`
Expected: PASS（前面所有 Task 的测试仍绿）

- [ ] **Step 6: Commit**

```bash
git add src/adapters/codex.rs
git commit -m "feat: codex read_state（config.toml 动态导航，对未知字段免疫）"
```

---

### Task 11: kimi-code read_state

**Files:**
- Modify: `src/adapters/kimi_code.rs`

**Interfaces:**
- Consumes: `Home::{kimi_mcp, kimi_installed_plugins}`、`adapters::claude_code::{read_json_if_present, mcp_capability}`、`skills_dir::read_skill_entries`、`capability::*`
- Produces: `KimiCode::read_state()`

实测 `~/.kimi-code/mcp.json`：
```json
{ "mcpServers": { "kimi-cu": { "command": "/abs/bin", "args": ["mcp","-s","user"] } } }
```
注意与 claude 的差别：**没有** `type` 字段，且键序不同——但归一化后形状一致。

实测 `~/.kimi-code/plugins/installed.json`（`plugins` 是**数组**，不是 claude 那样的对象）：
```json
{ "version": 1,
  "plugins": [
    { "id": "superpowers", "root": "/abs/.kimi-code/plugins/managed/superpowers",
      "source": "github", "enabled": true,
      "installedAt": "...", "updatedAt": "...",
      "originalSource": "https://github.com/obra/superpowers",
      "github": { "owner": "obra", "repo": "superpowers",
                  "ref": { "kind": "tag", "value": "v6.3.0" },
                  "installedSha": "b36e0829c6d0140e93cfef2ca599b1b07d4a7797" } },
    { "id": "kimi-cu", "root": "/abs/.../managed/kimi-cu", "source": "zip-url",
      "enabled": true, "originalSource": "https://cdn.kimi.com/.../kimi-cu-plugin.zip" }
  ] }
```

**关键事实（spec §2.1 第 4 条）**：`~/.kimi-code` **没有** skills 目录。kimi 的 skill 只随
plugin bundle 分发，位于 `plugins/managed/<id>/skills/`。所以 kimi 的 skill 能力一律带
`Origin::Plugin{name}` + `SkillSource::PluginBundle`，且散装 skill 对 kimi 判 `Unsupported`
（Task 12 落地）——这正是 diff 输出里 `!` 行的真实来源。

`installedSha` 就是 spec §2.1 第 3 条里与 claude `gitCommitSha` 相同的那个 SHA，
必须读进 `Spec::Plugin::sha`，SHA 优先比较才能生效。

- [ ] **Step 1: 写失败的测试**

追加到 `src/adapters/kimi_code.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, Origin, SkillSource, Spec};
    use crate::testutil::TempHome;

    fn find<'a>(caps: &'a [Capability], kind: Kind, name: &str) -> Option<&'a Capability> {
        caps.iter().find(|c| c.id.kind == kind && c.id.name == name)
    }

    #[test]
    fn reads_mcp_from_mcp_json() {
        let t = TempHome::new();
        t.write(
            ".kimi-code/mcp.json",
            r#"{ "mcpServers": {
                  "kimi-cu": { "args": ["mcp","-s","user"], "command": "/Applications/KimiCU.app/Contents/MacOS/kimi-cu" },
                  "ctx": { "command": "npx", "args": ["-y","@upstash/context7-mcp"],
                           "env": { "CONTEXT7_API_KEY": "${CONTEXT7_API_KEY}" } } } }"#,
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        assert_eq!(
            find(&caps, Kind::Mcp, "kimi-cu").unwrap().spec,
            Spec::Mcp {
                command: "/Applications/KimiCU.app/Contents/MacOS/kimi-cu".into(),
                args: vec!["mcp".into(), "-s".into(), "user".into()],
                env: Default::default(),
            }
        );
        match &find(&caps, Kind::Mcp, "ctx").unwrap().spec {
            Spec::Mcp { env, .. } => assert_eq!(env["CONTEXT7_API_KEY"], "${CONTEXT7_API_KEY}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_plugin_with_installed_sha_and_ref() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/superpowers");
        t.mkdir(".kimi-code/plugins/managed/superpowers");
        t.write(
            ".kimi-code/plugins/installed.json",
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "superpowers", "root": "{}", "source": "github", "enabled": true,
                        "originalSource": "https://github.com/obra/superpowers",
                        "github": {{ "owner": "obra", "repo": "superpowers",
                                     "ref": {{ "kind": "tag", "value": "v6.3.0" }},
                                     "installedSha": "b36e0829c6d0140e93cfef2ca599b1b07d4a7797" }} }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let p = find(&caps, Kind::Plugin, "superpowers").unwrap();
        assert_eq!(
            p.spec,
            Spec::Plugin {
                marketplace: Some("obra/superpowers".into()),
                entry: "superpowers".into(),
                version: Some("v6.3.0".into()),
                sha: Some("b36e0829c6d0140e93cfef2ca599b1b07d4a7797".into()),
                install_path: Some(root),
                enabled: true,
            }
        );
    }

    #[test]
    fn zip_url_plugin_without_github_block_has_no_sha() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/kimi-cu");
        t.mkdir(".kimi-code/plugins/managed/kimi-cu");
        t.write(
            ".kimi-code/plugins/installed.json",
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "kimi-cu", "root": "{}", "source": "zip-url", "enabled": true,
                        "originalSource": "https://cdn.kimi.com/kimi-computer-use/latest/kimi-cu-plugin.zip" }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "kimi-cu").unwrap().spec {
            Spec::Plugin { sha, version, marketplace, .. } => {
                assert_eq!(*sha, None);
                assert_eq!(*version, None);
                assert_eq!(*marketplace, None, "zip-url 没有 repo 来源标识");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn disabled_plugin_is_reported_as_disabled() {
        let t = TempHome::new();
        t.write(
            ".kimi-code/plugins/installed.json",
            r#"{ "version": 1, "plugins": [ { "id": "off", "root": "/nope", "source": "github", "enabled": false } ] }"#,
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        match &find(&caps, Kind::Plugin, "off").unwrap().spec {
            Spec::Plugin { enabled, .. } => assert!(!enabled),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_bundled_skills_of_installed_plugins() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/superpowers");
        t.mkdir(".kimi-code/plugins/managed/superpowers/skills/brainstorming")
            .mkdir(".kimi-code/plugins/managed/superpowers/skills/test-driven-development");
        t.write(
            ".kimi-code/plugins/installed.json",
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "superpowers", "root": "{}", "source": "github", "enabled": true }}
                   ] }}"#,
                root.display()
            ),
        );

        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let s = find(&caps, Kind::Skill, "brainstorming").expect("bundled skill surfaced");
        assert_eq!(
            s.spec,
            Spec::Skill {
                source: SkillSource::PluginBundle { plugin: "superpowers".into() },
                path: Some(t.path(".kimi-code/plugins/managed/superpowers/skills/brainstorming")),
            }
        );
        assert_eq!(s.origin, Origin::Plugin { name: "superpowers".into() });
        assert!(find(&caps, Kind::Skill, "test-driven-development").is_some());
    }

    #[test]
    fn disabled_plugin_bundled_skills_are_not_reported() {
        let t = TempHome::new();
        let root = t.path(".kimi-code/plugins/managed/off");
        t.mkdir(".kimi-code/plugins/managed/off/skills/ghost");
        t.write(
            ".kimi-code/plugins/installed.json",
            &format!(
                r#"{{ "version": 1, "plugins": [
                     {{ "id": "off", "root": "{}", "source": "github", "enabled": false }}
                   ] }}"#,
                root.display()
            ),
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Skill, "ghost").is_none(), "disabled bundle contributes nothing");
    }

    #[test]
    fn plugin_root_that_does_not_exist_yields_no_skills_but_keeps_the_plugin() {
        let t = TempHome::new();
        t.write(
            ".kimi-code/plugins/installed.json",
            r#"{ "version": 1, "plugins": [ { "id": "ghost", "root": "/definitely/not/here", "source": "github", "enabled": true } ] }"#,
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        assert!(find(&caps, Kind::Plugin, "ghost").is_some());
        assert!(caps.iter().all(|c| c.id.kind != Kind::Skill));
    }

    #[test]
    fn missing_files_are_not_errors() {
        let t = TempHome::new();
        t.mkdir(".kimi-code");
        let caps = KimiCode::new(t.home.clone()).read_state().expect("no config != error");
        assert!(caps.is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_naming_the_file() {
        let t = TempHome::new();
        t.write(".kimi-code/mcp.json", "{ broken");
        let err = KimiCode::new(t.home.clone()).read_state().unwrap_err().to_string();
        assert!(err.contains("mcp.json"), "{err}");
    }

    #[test]
    fn output_is_sorted_and_deduplicated() {
        let t = TempHome::new();
        t.write(
            ".kimi-code/mcp.json",
            r#"{ "mcpServers": { "z": { "command": "c" }, "a": { "command": "c" } } }"#,
        );
        let caps = KimiCode::new(t.home.clone()).read_state().unwrap();
        let labels: Vec<String> = caps.iter().map(|c| c.id.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "{labels:?}");
        let mut dedup = labels.clone();
        dedup.dedup();
        assert_eq!(labels.len(), dedup.len(), "duplicate ids: {labels:?}");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib adapters::kimi_code::`
Expected: FAIL —`read_state` 仍返回空 Vec。

- [ ] **Step 3: 实现**

替换 `src/adapters/kimi_code.rs` 的 import 段：

```rust
use crate::adapters::claude_code::{mcp_capability, read_json_if_present};
use crate::adapters::{HarnessAdapter, KIMI_CODE};
use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Support, Targets};
use crate::home::Home;
use crate::skills_dir::read_skill_entries;
use serde_json::Value;
use std::path::PathBuf;
```

在 `impl KimiCode` 中追加：

```rust
impl KimiCode {
    fn read_mcps(&self) -> anyhow::Result<Vec<Capability>> {
        let Some(root) = read_json_if_present(&self.home.kimi_mcp())? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if let Some(servers) = root.get("mcpServers").and_then(Value::as_object) {
            for (name, cfg) in servers {
                out.push(mcp_capability(name, cfg, KIMI_CODE));
            }
        }
        Ok(out)
    }

    fn read_plugins(&self) -> anyhow::Result<Vec<InstalledPlugin>> {
        let Some(root) = read_json_if_present(&self.home.kimi_installed_plugins())? else {
            return Ok(Vec::new());
        };
        // kimi 的 plugins 是数组，claude 的是对象——形状差异压平在这里
        let Some(list) = root.get("plugins").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for p in list {
            let Some(id) = p.get("id").and_then(Value::as_str) else {
                continue;
            };
            let github = p.get("github");
            out.push(InstalledPlugin {
                id: id.to_string(),
                root: p.get("root").and_then(Value::as_str).map(PathBuf::from),
                enabled: p.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                marketplace: github
                    .and_then(|g| g.get("owner"))
                    .and_then(Value::as_str)
                    .zip(github.and_then(|g| g.get("repo")).and_then(Value::as_str))
                    .map(|(owner, repo)| format!("{owner}/{repo}")),
                version: github
                    .and_then(|g| g.get("ref"))
                    .and_then(|r| r.get("value"))
                    .and_then(Value::as_str)
                    .map(String::from),
                // installedSha 与 claude 的 gitCommitSha 是同一个 SHA，
                // 必须读进 sha 位，SHA 优先比较才成立（spec §2.1 第 3 条）
                sha: github
                    .and_then(|g| g.get("installedSha"))
                    .and_then(Value::as_str)
                    .map(String::from),
            });
        }
        Ok(out)
    }

    /// kimi 没有用户级 skills 目录：skill 只随 bundle 分发（spec §2.1 第 4 条）。
    fn read_bundled_skills(&self, plugins: &[InstalledPlugin]) -> Vec<Capability> {
        let mut out = Vec::new();
        for p in plugins {
            if !p.enabled {
                continue;
            }
            let Some(root) = &p.root else { continue };
            for e in read_skill_entries(&root.join("skills"), &self.home) {
                out.push(Capability {
                    id: CapId::new(Kind::Skill, e.name),
                    origin: Origin::Plugin { name: p.id.clone() },
                    spec: Spec::Skill {
                        source: SkillSource::PluginBundle { plugin: p.id.clone() },
                        path: e.resolved,
                    },
                    targets: Targets::All,
                });
            }
        }
        out
    }
}

/// kimi `installed.json` 条目压平后的形状。
struct InstalledPlugin {
    id: String,
    root: Option<PathBuf>,
    enabled: bool,
    marketplace: Option<String>,
    version: Option<String>,
    sha: Option<String>,
}
```

把 `impl HarnessAdapter for KimiCode` 的 `read_state` 改为：

```rust
    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        let plugins = self.read_plugins()?;
        let mut caps = self.read_mcps()?;
        caps.extend(self.read_bundled_skills(&plugins));
        for p in &plugins {
            caps.push(Capability {
                id: CapId::new(Kind::Plugin, p.id.clone()),
                origin: Origin::Adopted { harness: KIMI_CODE.into() },
                spec: Spec::Plugin {
                    marketplace: p.marketplace.clone(),
                    entry: p.id.clone(),
                    version: p.version.clone(),
                    sha: p.sha.clone(),
                    install_path: p.root.clone(),
                    enabled: p.enabled,
                },
                targets: Targets::All,
            });
        }
        caps.sort_by(|a, b| a.id.label().cmp(&b.id.label()));
        caps.dedup_by(|a, b| a.id == b.id);
        Ok(caps)
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib adapters::kimi_code::`
Expected: PASS（10 tests）

- [ ] **Step 5: 全量回归**

Run: `cargo test`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/adapters/kimi_code.rs
git commit -m "feat: kimi-code read_state（installedSha 进 sha 位，skill 仅来自 bundle）"
```

---

### Task 12: supports() —— 三家接纳度矩阵

**Files:**
- Modify: `src/adapters/claude_code.rs`, `src/adapters/codex.rs`, `src/adapters/kimi_code.rs`

**Interfaces:**
- Consumes: `Capability`、`Kind`、`Origin`、`Support`
- Produces: 三家 adapter 的 `supports(&Capability) -> Support`

矩阵（依据 spec §2.1 实测）：

| Kind | claude-code | codex | kimi-code |
|---|---|---|---|
| Skill | Native（`~/.claude/skills`） | Native（`~/.codex/skills`） | **Unsupported** —无用户级 skills 目录 |
| Mcp | Native | Native | Native |
| Plugin | Native | Native | Native |
| Hook | Native（`settings.json:hooks`） | Native（`hooks.json`） | **Unsupported** —无用户级 hooks 配置 |
| Command | Native | Native | **Unsupported** |
| Agent | Native | Native | **Unsupported** |

**kimi 的例外**：若能力的 `origin` 是 `Origin::Plugin{..}`，说明它由 bundle 携带、随 kimi 原生
plugin 安装一起落地（实测 kimi 已原生装 superpowers 并读 `.kimi-plugin/plugin.json` 的
`skills` / `sessionStart`），此时判 **Native**。这条规则同时是 Plan B 拆解降级路径的接缝。

**关于 `Support::Degraded`**：Plan A 没有任何 adapter 会构造它——降级拆解需要探测 bundle 的
per-harness manifest（`bundle.rs`），那是 Plan B 的工作。`diff`（Task 13）仍必须正确处理
`Degraded`，与 `Native` 同路径。此处不写死代码去构造它，避免无法验证的分支。

- [ ] **Step 1: 写失败的测试**

追加到 `src/adapters/claude_code.rs` 的 `mod tests`：

```rust
    #[test]
    fn supports_every_kind_natively() {
        let t = TempHome::new();
        let a = ClaudeCode::new(t.home.clone());
        for kind in Kind::all() {
            let cap = Capability::new(CapId::new(kind, "x"), Spec::Command { path: PathBuf::from("/x") });
            assert_eq!(a.supports(&cap), Support::Native, "{kind:?}");
        }
    }
```

（`CapId`、`Capability`、`Support`、`Kind` 需加进该测试模块的 `use super::*;` 可见范围——
`super::*` 已包含它们，因为 Task 9 的 import 段引入了 `CapId`/`Capability`/`Kind`/`Support`。
`Spec::Command` 只是占位载荷，`supports` 只看 `kind` 与 `origin`。）

追加到 `src/adapters/codex.rs` 的 `mod tests`：

```rust
    #[test]
    fn supports_every_kind_natively() {
        let t = TempHome::new();
        let a = Codex::new(t.home.clone());
        for kind in Kind::all() {
            let cap = Capability::new(CapId::new(kind, "x"), Spec::Command { path: std::path::PathBuf::from("/x") });
            assert_eq!(a.supports(&cap), Support::Native, "{kind:?}");
        }
    }
```

追加到 `src/adapters/kimi_code.rs` 的 `mod tests`：

```rust
    #[test]
    fn supports_mcp_and_plugin_natively() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());
        for kind in [Kind::Mcp, Kind::Plugin] {
            let cap = Capability::new(
                CapId::new(kind, "x"),
                Spec::Mcp { command: "c".into(), args: vec![], env: Default::default() },
            );
            assert_eq!(a.supports(&cap), Support::Native, "{kind:?}");
        }
    }

    #[test]
    fn refuses_loose_skill_hook_command_agent_with_reason() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());
        for kind in [Kind::Skill, Kind::Hook, Kind::Command, Kind::Agent] {
            let cap = Capability::new(
                CapId::new(kind, "x"),
                Spec::Hook { event: "E".into(), command: "c".into() },
            );
            match a.supports(&cap) {
                Support::Unsupported { reason } => {
                    assert!(!reason.is_empty(), "{kind:?} needs a reason for the `!` line");
                    assert!(reason.contains("kimi-code"), "{kind:?} reason names the harness: {reason}");
                }
                other => panic!("{kind:?} expected Unsupported, got {other:?}"),
            }
        }
    }

    #[test]
    fn loose_skill_is_refused_but_bundled_skill_is_native() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());

        let loose = Capability::new(
            CapId::new(Kind::Skill, "my-draft"),
            Spec::Skill { source: SkillSource::Local { path: PathBuf::from("/x") }, path: None },
        );
        assert!(matches!(a.supports(&loose), Support::Unsupported { .. }));

        // 同一个 kind，但由 bundle 携带 → 随原生 plugin 安装一起落地
        let bundled = Capability {
            id: CapId::new(Kind::Skill, "brainstorming"),
            origin: Origin::Plugin { name: "superpowers".into() },
            spec: Spec::Skill {
                source: SkillSource::PluginBundle { plugin: "superpowers".into() },
                path: None,
            },
            targets: Targets::All,
        };
        assert_eq!(a.supports(&bundled), Support::Native);
    }

    #[test]
    fn bundled_hook_is_native_too() {
        let t = TempHome::new();
        let a = KimiCode::new(t.home.clone());
        let bundled_hook = Capability {
            id: CapId::new(Kind::Hook, "superpowers:session-start"),
            origin: Origin::Plugin { name: "superpowers".into() },
            spec: Spec::Hook { event: "SessionStart".into(), command: "run-hook".into() },
            targets: Targets::All,
        };
        assert_eq!(a.supports(&bundled_hook), Support::Native);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib adapters:: -- supports`
Expected: FAIL —kimi 的 `supports` 仍无条件返回 `Native`。

- [ ] **Step 3: 实现 claude-code 与 codex**

两家的 `supports` 行为与 Task 8 的骨架相同（全 `Native`），只需把骨架里那句
`// Task 12 细化` 换成说明性注释——参数保持 `_cap`，因为两家都不看能力内容：

`src/adapters/claude_code.rs`：
```rust
    fn supports(&self, _cap: &Capability) -> Support {
        // claude-code 原生支持全部六种能力
        Support::Native
    }
```

`src/adapters/codex.rs`：
```rust
    fn supports(&self, _cap: &Capability) -> Support {
        // codex 原生支持全部六种能力
        Support::Native
    }
```

- [ ] **Step 4: 实现 kimi-code**

替换 `src/adapters/kimi_code.rs` 中 `impl HarnessAdapter for KimiCode` 的 `supports`：

```rust
    fn supports(&self, cap: &Capability) -> Support {
        // bundle 携带的能力随 kimi 原生 plugin 安装一起落地
        // （实测 kimi 读 .kimi-plugin/plugin.json 的 skills / sessionStart）
        if matches!(cap.origin, Origin::Plugin { .. }) {
            return Support::Native;
        }
        match cap.id.kind {
            Kind::Mcp | Kind::Plugin => Support::Native,
            Kind::Skill => Support::Unsupported {
                reason: "kimi-code 无用户级 skills 目录，散装 skill 只能随 plugin bundle 分发"
                    .to_string(),
            },
            Kind::Hook => Support::Unsupported {
                reason: "kimi-code 无用户级 hooks 配置".to_string(),
            },
            Kind::Command => Support::Unsupported {
                reason: "kimi-code 无用户级 commands 目录".to_string(),
            },
            Kind::Agent => Support::Unsupported {
                reason: "kimi-code 无用户级 agents 目录".to_string(),
            },
        }
    }
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib adapters::`
Expected: PASS（Task 8–12 全部 adapter 测试）

- [ ] **Step 6: 全量回归**

Run: `cargo test`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/adapters
git commit -m "feat: 三家 harness 接纳度矩阵，kimi 散装 skill/hook 判 Unsupported"
```

---

### Task 13: diff —— 纯函数 f(desired, actual) → Plan

**Files:**
- Create: `src/diff.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `capability::{compare_versions, CapId, Capability, Kind, Spec, SkillSource, Support, VersionMatch}`、`adapters::HarnessAdapter`、`lower::DegradeTable`、`manifest::DegradeAction`
- Produces: `HarnessState`、`Action`、`PlanEntry`、`HarnessPlan`、`Plan`、`Counts`、`HarnessInput`、`compute`

规则（spec §5，逐条对应测试）：

1. `targets` 不含该 harness → 完全忽略（该能力既不 install，也不让它把同名 actual 变成 drift 豁免）。
2. `supports() == Unsupported` → `Undeliverable{reason}`；但若该能力所属 plugin 的 degrade 策略是
   `Skip`，则**不产生任何条目**（静默丢弃）。
3. `supports() == Native | Degraded` → 按 `CapId` 在 actual 里找：
   - 找不到 → `Install`
   - 找到 → `compare_versions`：`Same`/`Unknown` → `InSync`；`Different{from,to}` → `Update`
4. actual 有、desired 无 → `Drift`。**例外**：`SkillSource::PluginBundle{plugin}` 且 `plugin`
   在 desired 的 Plugin 集合里 → 跳过（它是 plugin 的分发产物，不是漂移）。
5. `detected == false` 的 harness → 不产生任何条目（不是「全部 install」），由 render 显示
   「未安装，已跳过」。
6. **remove 恒为 0**：删除需显式 `apply --prune`，属 Plan B。`Counts.remove` 字段保留以匹配
   spec §5 的输出格式。

- [ ] **Step 1: 写失败的测试**

创建 `src/diff.rs`，末尾写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Capability, Kind, Origin, SkillSource, Spec, Targets};
    use crate::lower::DegradeTable;
    use crate::manifest::Manifest;
    use crate::testutil::FakeAdapter;

    fn no_degrade() -> DegradeTable {
        DegradeTable::new(&Manifest::parse("version: 1\n").unwrap())
    }

    fn desired(kind: Kind, name: &str, spec: Spec) -> Capability {
        Capability::new(CapId::new(kind, name), spec)
    }

    fn mcp_spec(cmd: &str) -> Spec {
        Spec::Mcp { command: cmd.into(), args: vec![], env: Default::default() }
    }

    fn run(desired: &[Capability], adapter: FakeAdapter) -> HarnessPlan {
        let state = HarnessState {
            name: adapter.name().to_string(),
            detected: adapter.detect(),
            actual: adapter.read_state().unwrap(),
        };
        let inputs = vec![HarnessInput { state: &state, adapter: &adapter }];
        let plan = compute(desired, &inputs, &no_degrade());
        plan.harnesses.into_iter().next().unwrap()
    }

    fn labels(hp: &HarnessPlan) -> Vec<(String, String)> {
        hp.entries
            .iter()
            .map(|e| (e.id.label(), match &e.action {
                Action::Install => "install".to_string(),
                Action::Update { .. } => "update".to_string(),
                Action::InSync => "in-sync".to_string(),
                Action::Undeliverable { .. } => "undeliverable".to_string(),
                Action::Drift => "drift".to_string(),
            }))
            .collect()
    }

    #[test]
    fn missing_capability_is_install() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex"),
        );
        assert_eq!(labels(&hp), vec![("mcp/context7".to_string(), "install".to_string())]);
    }

    #[test]
    fn identical_capability_is_in_sync() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "npx")]),
        );
        assert_eq!(labels(&hp), vec![("mcp/context7".to_string(), "in-sync".to_string())]);
    }

    #[test]
    fn changed_command_is_update_with_actual_then_desired() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::mcp("context7", "old")]),
        );
        assert_eq!(hp.entries.len(), 1);
        match &hp.entries[0].action {
            Action::Update { from, to } => {
                assert_eq!(from, "old", "from = 现状");
                assert_eq!(to, "npx", "to = 期望");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_version_match_counts_as_in_sync() {
        // codex 不记录 plugin 版本 → Unknown → 不误报 update
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None, entry: "superpowers".into(),
                version: Some("v6.3.0".into()), sha: None, install_path: None, enabled: true,
            },
        );
        let actual = FakeAdapter::plugin("superpowers", None, None);
        let hp = run(&[d], FakeAdapter::native("codex").with_caps(vec![actual]));
        assert_eq!(labels(&hp), vec![("plugin/superpowers".to_string(), "in-sync".to_string())]);
    }

    #[test]
    fn same_sha_across_differing_version_strings_is_in_sync() {
        // spec §2.1 第 3 条的实测场景
        let sha = "b36e0829c6d0140e93cfef2ca599b1b07d4a7797";
        let d = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None, entry: "superpowers".into(),
                version: Some("v6.3.0".into()), sha: Some(sha.into()), install_path: None, enabled: true,
            },
        );
        let actual = FakeAdapter::plugin("superpowers", Some("6.4.1"), Some(sha));
        let hp = run(&[d], FakeAdapter::native("claude-code").with_caps(vec![actual]));
        assert_eq!(labels(&hp), vec![("plugin/superpowers".to_string(), "in-sync".to_string())]);
    }

    #[test]
    fn unsupported_capability_is_undeliverable_with_reason() {
        let d = Capability {
            id: CapId::new(Kind::Skill, "my-draft"),
            origin: Origin::Manifest,
            spec: Spec::Skill { source: SkillSource::Local { path: "/x".into() }, path: None },
            targets: Targets::All,
        };
        let hp = run(
            &[d],
            FakeAdapter::native("kimi-code").refusing(Kind::Skill, "kimi-code 无用户级 skills 目录"),
        );
        assert_eq!(hp.entries.len(), 1);
        assert_eq!(hp.entries[0].id.label(), "skill/my-draft");
        match &hp.entries[0].action {
            Action::Undeliverable { reason } => assert!(reason.contains("kimi-code"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn degrade_skip_suppresses_the_undeliverable_entry() {
        let man = Manifest::parse(
            "version: 1\nplugins:\n  sp:\n    source: github\n    repo: obra/superpowers\n    entry: superpowers\n    degrade:\n      hooks: skip\n",
        )
        .unwrap();
        let table = DegradeTable::new(&man);

        let hook = Capability {
            id: CapId::new(Kind::Hook, "superpowers:session-start"),
            origin: Origin::Plugin { name: "superpowers".into() },
            spec: Spec::Hook { event: "SessionStart".into(), command: "run".into() },
            targets: Targets::All,
        };
        let state = HarnessState { name: "kimi-code".into(), detected: true, actual: vec![] };
        let adapter = FakeAdapter::native("kimi-code").refusing(Kind::Hook, "no hook support");
        let inputs = vec![HarnessInput { state: &state, adapter: &adapter }];

        let plan = compute(&[hook], &inputs, &table);
        assert!(plan.harnesses[0].entries.is_empty(), "skip means silent");
    }

    #[test]
    fn degrade_report_keeps_the_undeliverable_entry() {
        let man = Manifest::parse(
            "version: 1\nplugins:\n  sp:\n    source: github\n    repo: obra/superpowers\n    entry: superpowers\n    degrade:\n      hooks: report\n",
        )
        .unwrap();
        let table = DegradeTable::new(&man);
        let hook = Capability {
            id: CapId::new(Kind::Hook, "superpowers:session-start"),
            origin: Origin::Plugin { name: "superpowers".into() },
            spec: Spec::Hook { event: "SessionStart".into(), command: "run".into() },
            targets: Targets::All,
        };
        let state = HarnessState { name: "kimi-code".into(), detected: true, actual: vec![] };
        let adapter = FakeAdapter::native("kimi-code").refusing(Kind::Hook, "no hook support");
        let inputs = vec![HarnessInput { state: &state, adapter: &adapter }];

        let plan = compute(&[hook], &inputs, &table);
        assert_eq!(plan.harnesses[0].entries.len(), 1);
        assert_eq!(plan.counts().undeliverable, 1);
    }

    #[test]
    fn extra_actual_capability_is_drift() {
        let hp = run(
            &[],
            FakeAdapter::native("kimi-code").with_caps(vec![FakeAdapter::mcp("experimental-thing", "c")]),
        );
        assert_eq!(
            labels(&hp),
            vec![("mcp/experimental-thing".to_string(), "drift".to_string())]
        );
    }

    #[test]
    fn bundled_skill_of_a_desired_plugin_is_not_drift() {
        let plugin = desired(
            Kind::Plugin,
            "superpowers",
            Spec::Plugin {
                marketplace: None, entry: "superpowers".into(),
                version: None, sha: None, install_path: None, enabled: true,
            },
        );
        let actual = vec![
            FakeAdapter::plugin("superpowers", None, None),
            FakeAdapter::bundled_skill("brainstorming", "superpowers"),
            FakeAdapter::bundled_skill("test-driven-development", "superpowers"),
        ];
        let hp = run(&[plugin], FakeAdapter::native("kimi-code").with_caps(actual));
        let got = labels(&hp);
        assert_eq!(got, vec![("plugin/superpowers".to_string(), "in-sync".to_string())],
            "bundle 分发出来的 skill 不是漂移: {got:?}");
    }

    #[test]
    fn bundled_skill_of_an_undesired_plugin_is_drift() {
        let hp = run(
            &[],
            FakeAdapter::native("codex").with_caps(vec![FakeAdapter::bundled_skill("brainstorming", "superpowers")]),
        );
        assert_eq!(
            labels(&hp),
            vec![("skill/brainstorming".to_string(), "drift".to_string())]
        );
    }

    #[test]
    fn targets_exclude_harness_entirely() {
        let mut d = desired(Kind::Mcp, "context7", mcp_spec("npx"));
        d.targets = Targets::Only(vec!["codex".into()]);
        let hp = run(&[d], FakeAdapter::native("kimi-code"));
        assert!(hp.entries.is_empty(), "not targeted here → no entry at all");
    }

    #[test]
    fn targeted_elsewhere_but_installed_here_is_drift() {
        // spec §5「targets 不含它 → 忽略」+「actual 有、desired 无 → drift」的合流结果
        let mut d = desired(Kind::Mcp, "context7", mcp_spec("npx"));
        d.targets = Targets::Only(vec!["codex".into()]);
        let hp = run(&[d], FakeAdapter::native("kimi-code").with_caps(vec![FakeAdapter::mcp("context7", "npx")]));
        assert_eq!(labels(&hp), vec![("mcp/context7".to_string(), "drift".to_string())]);
    }

    #[test]
    fn undetected_harness_produces_no_entries() {
        let hp = run(
            &[desired(Kind::Mcp, "context7", mcp_spec("npx"))],
            FakeAdapter::native("codex").with_detected(false),
        );
        assert!(!hp.detected);
        assert!(hp.entries.is_empty(), "未安装 ≠ 全部待装");
    }

    #[test]
    fn entries_are_grouped_by_action_then_label() {
        let actual = vec![
            FakeAdapter::mcp("synced", "npx"),
            FakeAdapter::mcp("zzz-drift", "c"),
            FakeAdapter::mcp("aaa-drift", "c"),
        ];
        let adapter = FakeAdapter::native("codex")
            .with_caps(actual)
            .refusing(Kind::Hook, "no hooks");
        let d = vec![
            desired(Kind::Mcp, "synced", mcp_spec("npx")),
            desired(Kind::Mcp, "new-one", mcp_spec("npx")),
            Capability {
                id: CapId::new(Kind::Hook, "h"),
                origin: Origin::Manifest,
                spec: Spec::Hook { event: "E".into(), command: "c".into() },
                targets: Targets::All,
            },
        ];
        let hp = run(&d, adapter);
        let order: Vec<String> = hp.entries.iter().map(|e| e.id.label()).collect();
        assert_eq!(
            order,
            vec!["mcp/new-one", "hook/h", "mcp/aaa-drift", "mcp/zzz-drift"],
            "install → undeliverable → drift，组内按 label: {order:?}"
        );
    }

    #[test]
    fn actual_by_kind_counts_all_actual_capabilities() {
        let actual = vec![
            FakeAdapter::mcp("a", "c"),
            FakeAdapter::mcp("b", "c"),
            FakeAdapter::plugin("superpowers", None, None),
        ];
        let hp = run(&[], FakeAdapter::native("codex").with_caps(actual));
        assert_eq!(hp.actual_by_kind.get(&Kind::Mcp), Some(&2));
        assert_eq!(hp.actual_by_kind.get(&Kind::Plugin), Some(&1));
        assert_eq!(hp.actual_by_kind.get(&Kind::Skill), None);
    }

    #[test]
    fn counts_aggregate_across_harnesses() {
        let d = vec![desired(Kind::Mcp, "context7", mcp_spec("npx"))];
        let s1 = HarnessState { name: "codex".into(), detected: true, actual: vec![] };
        let s2 = HarnessState {
            name: "kimi-code".into(),
            detected: true,
            actual: vec![FakeAdapter::mcp("context7", "npx")],
        };
        let a1 = FakeAdapter::native("codex");
        let a2 = FakeAdapter::native("kimi-code");
        let inputs = vec![
            HarnessInput { state: &s1, adapter: &a1 },
            HarnessInput { state: &s2, adapter: &a2 },
        ];
        let plan = compute(&d, &inputs, &no_degrade());
        let c = plan.counts();
        assert_eq!(c.install, 1);
        assert_eq!(c.update, 0);
        assert_eq!(c.remove, 0, "remove 恒为 0，prune 属 Plan B");
        assert_eq!(c.undeliverable, 0);
        assert_eq!(c.drift, 0);
        assert!(!plan.is_clean());
    }

    #[test]
    fn is_clean_when_everything_in_sync() {
        let d = vec![desired(Kind::Mcp, "context7", mcp_spec("npx"))];
        let s = HarnessState {
            name: "codex".into(),
            detected: true,
            actual: vec![FakeAdapter::mcp("context7", "npx")],
        };
        let a = FakeAdapter::native("codex");
        let plan = compute(&d, &[HarnessInput { state: &s, adapter: &a }], &no_degrade());
        assert!(plan.is_clean());
    }

    #[test]
    fn is_clean_when_nothing_desired_and_nothing_installed() {
        let s = HarnessState { name: "codex".into(), detected: true, actual: vec![] };
        let a = FakeAdapter::native("codex");
        let plan = compute(&[], &[HarnessInput { state: &s, adapter: &a }], &no_degrade());
        assert!(plan.is_clean());
    }

    #[test]
    fn degraded_support_follows_the_same_path_as_native() {
        // Plan A 无 adapter 构造 Degraded，但 diff 必须已就绪（Plan B 的拆解降级走这条）
        struct DegradingAdapter;
        impl crate::adapters::HarnessAdapter for DegradingAdapter {
            fn name(&self) -> &str { "codex" }
            fn detect(&self) -> bool { true }
            fn read_state(&self) -> anyhow::Result<Vec<Capability>> { Ok(vec![]) }
            fn supports(&self, _cap: &Capability) -> Support {
                Support::Degraded { as_kind: Kind::Mcp }
            }
        }
        let s = HarnessState { name: "codex".into(), detected: true, actual: vec![] };
        let a = DegradingAdapter;
        let plan = compute(
            &[desired(Kind::Plugin, "bundle", Spec::Plugin {
                marketplace: None, entry: "bundle".into(), version: None,
                sha: None, install_path: None, enabled: true,
            })],
            &[HarnessInput { state: &s, adapter: &a }],
            &no_degrade(),
        );
        assert_eq!(plan.counts().install, 1, "Degraded 与 Native 同路径");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib diff::`
Expected: FAIL —类型与 `compute` 未定义。

- [ ] **Step 3: 实现 `src/diff.rs`**

```rust
//! diff：`f(desired, actual) → Plan`，纯函数、幂等、只读（spec §3 决策 2）。
//!
//! 清单定义「必须有的」，不定义「不许有的」：actual 多出来的报 drift 但**不**删，
//! 删除需显式 `apply --prune`（Plan B）。故 `Counts::remove` 在本计划中恒为 0。

use crate::adapters::HarnessAdapter;
use crate::capability::{
    compare_versions, CapId, Capability, Kind, SkillSource, Spec, Support, VersionMatch,
};
use crate::lower::DegradeTable;
use crate::manifest::DegradeAction;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessState {
    pub name: String,
    pub detected: bool,
    pub actual: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Install,
    /// from = 现状，to = 期望
    Update { from: String, to: String },
    InSync,
    Undeliverable { reason: String },
    Drift,
}

impl Action {
    /// 输出分组顺序：install → update → undeliverable → drift → in-sync
    pub fn rank(&self) -> u8 {
        match self {
            Action::Install => 0,
            Action::Update { .. } => 1,
            Action::Undeliverable { .. } => 2,
            Action::Drift => 3,
            Action::InSync => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub id: CapId,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessPlan {
    pub harness: String,
    pub detected: bool,
    pub entries: Vec<PlanEntry>,
    /// in-sync 汇总行要显示的各类实有数量
    pub actual_by_kind: BTreeMap<Kind, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub harnesses: Vec<HarnessPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub install: usize,
    pub update: usize,
    pub remove: usize,
    pub undeliverable: usize,
    pub drift: usize,
}

impl Plan {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for h in &self.harnesses {
            for e in &h.entries {
                match &e.action {
                    Action::Install => c.install += 1,
                    Action::Update { .. } => c.update += 1,
                    Action::Undeliverable { .. } => c.undeliverable += 1,
                    Action::Drift => c.drift += 1,
                    Action::InSync => {}
                }
            }
        }
        c
    }

    pub fn is_clean(&self) -> bool {
        let c = self.counts();
        c.install == 0 && c.update == 0 && c.remove == 0 && c.undeliverable == 0 && c.drift == 0
    }
}

pub struct HarnessInput<'a> {
    pub state: &'a HarnessState,
    pub adapter: &'a dyn HarnessAdapter,
}

pub fn compute(
    desired: &[Capability],
    inputs: &[HarnessInput],
    degrade: &DegradeTable,
) -> Plan {
    let mut harnesses = Vec::with_capacity(inputs.len());

    for input in inputs {
        let state = input.state;
        let harness = state.name.clone();

        let mut actual_by_kind: BTreeMap<Kind, usize> = BTreeMap::new();
        for a in &state.actual {
            *actual_by_kind.entry(a.id.kind).or_insert(0) += 1;
        }

        // 未安装的 harness 不产出「全部待装」的噪音，由 render 显示跳过原因
        if !state.detected {
            harnesses.push(HarnessPlan { harness, detected: false, entries: Vec::new(), actual_by_kind });
            continue;
        }

        let mut entries: Vec<PlanEntry> = Vec::new();
        let mut matched: HashSet<CapId> = HashSet::new();

        let desired_plugins: HashSet<String> = desired
            .iter()
            .filter(|c| c.id.kind == Kind::Plugin && c.targets.includes(&harness))
            .map(|c| c.id.name.clone())
            .collect();

        for cap in desired {
            if !cap.targets.includes(&harness) {
                continue;
            }
            match input.adapter.supports(cap) {
                Support::Unsupported { reason } => {
                    if degrade.action(cap) == DegradeAction::Report {
                        entries.push(PlanEntry { id: cap.id.clone(), action: Action::Undeliverable { reason } });
                    }
                }
                Support::Native | Support::Degraded { .. } => {
                    matched.insert(cap.id.clone());
                    let action = match state.actual.iter().find(|a| a.id == cap.id) {
                        None => Action::Install,
                        Some(a) => match compare_versions(&cap.spec, &a.spec) {
                            // Unknown 按 in-sync：只读工具不该误报（spec §2.1 第 3 条）
                            VersionMatch::Same | VersionMatch::Unknown => Action::InSync,
                            VersionMatch::Different { from, to } => Action::Update { from, to },
                        },
                    };
                    entries.push(PlanEntry { id: cap.id.clone(), action });
                }
            }
        }

        for a in &state.actual {
            if matched.contains(&a.id) {
                continue;
            }
            // plugin bundle 分发出来的 skill 不是漂移
            if let Spec::Skill { source: SkillSource::PluginBundle { plugin }, .. } = &a.spec {
                if desired_plugins.contains(plugin) {
                    continue;
                }
            }
            entries.push(PlanEntry { id: a.id.clone(), action: Action::Drift });
        }

        entries.sort_by(|x, y| {
            x.action
                .rank()
                .cmp(&y.action.rank())
                .then_with(|| x.id.label().cmp(&y.id.label()))
        });

        harnesses.push(HarnessPlan { harness, detected: true, entries, actual_by_kind });
    }

    Plan { harnesses }
}
```

- [ ] **Step 4: 在 `src/lib.rs` 声明模块**

```rust
pub mod adapters;
pub mod capability;
pub mod diff;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod secrets;
pub mod skills_dir;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib diff::`
Expected: PASS（20 tests）

若 `entries_are_grouped_by_action_then_label` 失败，检查 `Action::rank` 的数值顺序与
`entries.sort_by` 是否先比 rank 再比 label。

若 `bundled_skill_of_a_desired_plugin_is_not_drift` 失败，检查 `desired_plugins` 是否用
`targets.includes(&harness)` 过滤过——未定向到该 harness 的 plugin 不该豁免它的 skill。

- [ ] **Step 6: Commit**

```bash
git add src/diff.rs src/lib.rs
git commit -m "feat: diff 纯函数，drift 豁免 plugin bundle 分发物"
```

---

### Task 14: render — `Plan` → 终端表格 + insta golden

**Files:**
- Create: `src/render.rs`
- Create: `src/snapshots/henv-render__spec_example_matches_design_doc.snap`（由 insta 生成，需入库）
- Modify: `src/lib.rs`（加 `pub mod render;`）

**Interfaces:**
- Consumes: `diff::{Plan, HarnessPlan, PlanEntry, Action}`（Task 13）、`capability::Kind::{all, as_str}`（Task 2）、`CapId::label()`（Task 2）
- Produces: `pub fn render(plan: &Plan) -> String`（Task 15 的 `run_diff` 唯一消费者）

**格式规则（照 spec §5 示例逐列量出，不是凭感觉对齐）**

| 规则 | 值 | 依据 |
|---|---|---|
| harness 名列宽 | `{:<13}` + 1 空格 = 14 列 | 示例中 `claude-code` 后 3 空格、`codex` 后 9 空格，均落在第 15 列 |
| 动作前缀列宽 | 11 列 | `+ install`(9)+2、`~ update`(8)+3、`- drift`(7)+4、`✓ in-sync`(9)+2 |
| 前缀超长时 | 退化为 2 空格分隔 | `! undeliverable`(15) 在示例里就是 `! undeliverable  hook/...` |
| 续行缩进 | 14 空格 | 与首行内容起点对齐 |
| install 合并 | 同 harness 的 install 条目合并成一行、`, ` 连接 | 示例 `+ install  mcp/context7, mcp/browser-use` |
| 其余动作 | 一条一行，尾随 `(说明)` | update 带 `from → to`，undeliverable 带 reason，drift 带固定提示 |
| in-sync 汇总 | 仅当该 harness **无任何待办条目**时输出单行 `skills:N mcps:N plugins:N` | 示例中 claude-code 全同步才有汇总行，codex/kimi 有待办就没有 |
| 汇总计数来源 | `HarnessPlan::actual_by_kind`（实有数量，非 in-sync 条目数） | 字段注释即「in-sync 汇总行要显示的各类实有数量」 |
| 汇总为 0 | `(nothing installed)` | 三家全空时不能输出空串 |
| 未检测到 | `{name:<13} ⚠ not installed (skipped)` | Task 13 规则 5：未安装的 harness 不产条目，由 render 说明原因 |
| 末尾 | 空行 + `Plan: ...` 计数行 + 换行 | 示例末行 |

**两处与 spec 示例文字的有意偏离**（示例是手写示意，henv 必须确定性输出）：

1. install 列表按 `diff` 的排序（rank 再 label）输出，故为 `mcp/browser-use, mcp/context7`，
   而非示例的 `mcp/context7, mcp/browser-use`。Global Constraints 要求全链路确定性排序。
2. update 行输出 `(v2.1 → v2.2)`，丢掉示例里的 `ref drift:` 字样——`Action::Update` 只携带
   `from`/`to` 两个值，不携带「是哪个字段漂移了」。要恢复该措辞需给 `Update` 加字段标签，
   属 Plan B 的 apply 报告打磨，本计划不做。

- [ ] **Step 1: 写失败的测试 —— `src/render.rs` 只含测试**

整文件（`render` 与私有辅助函数留到 Step 3，此刻编译失败就是预期的「红」）：

```rust
//! render：`Plan` → 终端表格。格式变化会被 insta golden 捕捉（spec §6）。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Kind};
    use crate::diff::{Action, HarnessPlan, Plan, PlanEntry};
    use std::collections::BTreeMap;

    fn hp(harness: &str, detected: bool, entries: Vec<PlanEntry>, actual: Vec<(Kind, usize)>) -> HarnessPlan {
        HarnessPlan {
            harness: harness.to_string(),
            detected,
            entries,
            actual_by_kind: actual.into_iter().collect::<BTreeMap<_, _>>(),
        }
    }

    fn pe(kind: Kind, name: &str, action: Action) -> PlanEntry {
        PlanEntry { id: CapId::new(kind, name), action }
    }

    /// spec §5 的示例场景：claude-code 全同步，codex 有 install/update/undeliverable，
    /// kimi-code 有 install/drift。
    fn spec_plan() -> Plan {
        Plan {
            harnesses: vec![
                hp("claude-code", true, vec![], vec![(Kind::Skill, 42), (Kind::Mcp, 6), (Kind::Plugin, 3)]),
                hp(
                    "codex",
                    true,
                    vec![
                        pe(Kind::Mcp, "browser-use", Action::Install),
                        pe(Kind::Mcp, "context7", Action::Install),
                        pe(
                            Kind::Skill,
                            "dingtalk-mail",
                            Action::Update { from: "v2.1".into(), to: "v2.2".into() },
                        ),
                        pe(
                            Kind::Hook,
                            "superpowers:pre-commit",
                            Action::Undeliverable { reason: "no hook support".into() },
                        ),
                    ],
                    vec![(Kind::Skill, 30)],
                ),
                hp(
                    "kimi-code",
                    true,
                    vec![
                        pe(Kind::Mcp, "context7", Action::Install),
                        pe(Kind::Mcp, "experimental-thing", Action::Drift),
                    ],
                    vec![(Kind::Mcp, 1)],
                ),
            ],
        }
    }

    #[test]
    fn spec_example_matches_design_doc() {
        insta::assert_snapshot!("spec_example_matches_design_doc", render(&spec_plan()));
    }

    #[test]
    fn harness_name_column_is_14_wide() {
        // `claude-code`(11) + 3 空格；`codex`(5) + 9 空格 —— 内容都从第 15 列起。
        let out = render(&spec_plan());
        let mut lines = out.lines();
        assert!(lines.next().unwrap().starts_with("claude-code   ✓"), "{out}");
        let codex = lines.next().unwrap();
        assert!(codex.starts_with("codex         + install  "), "{codex:?}");
        // 续行缩进 14 空格，与首行内容起点对齐
        let cont = lines.next().unwrap();
        assert!(cont.starts_with("              ~ update   "), "{cont:?}");
    }

    #[test]
    fn installs_are_coalesced_onto_one_line_in_sorted_order() {
        let out = render(&spec_plan());
        assert!(out.contains("+ install  mcp/browser-use, mcp/context7\n"), "{out}");
        assert!(!out.contains("mcp/context7, mcp/browser-use"), "排序必须确定性，非示例顺序");
    }

    #[test]
    fn in_sync_summary_only_when_harness_has_no_pending_work() {
        let out = render(&spec_plan());
        assert!(out.contains("claude-code   ✓ in-sync  skills:42 mcps:6 plugins:3\n"), "{out}");
        // codex 有 30 个实有 skill，但有待办 → 不出汇总行
        assert!(!out.contains("skills:30"), "{out}");
    }

    #[test]
    fn in_sync_summary_kinds_follow_kind_all_order() {
        // Kind::all() = [Skill, Mcp, Command, Agent, Hook, Plugin]。六个种类**全部给非零计数**：
        // 只要有一个是 0，交换它与相邻种类的排列就检不出来（0 计数会被跳过）。
        // 这条是 `Kind::all()` 顺序在 Task 2 之外的第二道闸，两道都要在。
        let plan = Plan {
            harnesses: vec![hp(
                "codex",
                true,
                vec![],
                vec![
                    (Kind::Skill, 1),
                    (Kind::Mcp, 2),
                    (Kind::Command, 3),
                    (Kind::Agent, 4),
                    (Kind::Hook, 5),
                    (Kind::Plugin, 6),
                ],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("✓ in-sync  skills:1 mcps:2 commands:3 agents:4 hooks:5 plugins:6\n"),
            "{out}"
        );
    }

    #[test]
    fn empty_harness_says_nothing_installed() {
        let plan = Plan { harnesses: vec![hp("codex", true, vec![], vec![])] };
        assert!(render(&plan).contains("codex         ✓ in-sync  (nothing installed)\n"));
    }

    #[test]
    fn undetected_harness_is_reported_not_listed_as_install() {
        let plan = Plan {
            harnesses: vec![hp(
                "codex",
                false,
                // 即便 diff 误产了条目，未检测到的 harness 也只显示跳过原因
                vec![pe(Kind::Skill, "x", Action::Install)],
                vec![(Kind::Skill, 9)],
            )],
        };
        let out = render(&plan);
        assert!(out.contains("codex         ⚠ not installed (skipped)\n"), "{out}");
        // 页脚计数字里本来就有 "install" 这个词，故断言动作前缀而非词
        assert!(!out.contains("+ install"), "未安装的 harness 不该产出一堆待装噪音：{out}");
        assert!(!out.contains("skills:9"), "{out}");
    }

    #[test]
    fn undeliverable_prefix_is_not_padded_to_eleven() {
        // `! undeliverable` 是 15 字符，超过 11 列宽 → 退化为两空格分隔（spec 示例形态）。
        let plan = Plan {
            harnesses: vec![hp(
                "kimi-code",
                true,
                vec![pe(
                    Kind::Skill,
                    "my-draft",
                    Action::Undeliverable { reason: "kimi-code 无用户级 skills 目录".into() },
                )],
                vec![],
            )],
        };
        let out = render(&plan);
        assert!(
            out.contains("! undeliverable  skill/my-draft (kimi-code 无用户级 skills 目录)\n"),
            "{out}"
        );
    }

    #[test]
    fn drift_line_carries_the_prune_hint() {
        let out = render(&spec_plan());
        assert!(
            out.contains("- drift    mcp/experimental-thing (未在清单中，--prune 可删)\n"),
            "{out}"
        );
    }

    #[test]
    fn update_line_shows_from_then_to() {
        let out = render(&spec_plan());
        assert!(out.contains("~ update   skill/dingtalk-mail (v2.1 → v2.2)\n"), "{out}");
    }

    #[test]
    fn footer_counts_come_from_plan_not_from_lines() {
        let out = render(&spec_plan());
        assert!(
            out.ends_with("\nPlan: 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift\n"),
            "{out:?}"
        );
    }

    #[test]
    fn empty_plan_has_no_leading_blank_line() {
        let out = render(&Plan::default());
        assert_eq!(out, "Plan: 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift\n");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib render::`
Expected: FAIL —`render` 未定义，编译错误。

（`src/lib.rs` 尚未声明 `pub mod render;`，所以先加这一行再跑：）

```rust
pub mod adapters;
pub mod capability;
pub mod diff;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod render;
pub mod secrets;
pub mod skills_dir;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 3: 实现 `src/render.rs`**

在文件顶部 `//!` 之后、`#[cfg(test)]` 之前插入：

```rust
use crate::capability::Kind;
use crate::diff::{Action, HarnessPlan, Plan, PlanEntry};
use std::fmt::Write as _;

/// harness 名列宽。spec §5 示例里内容一律从第 15 列起 → `{:<13}` 再补一个空格。
const NAME_WIDTH: usize = 13;
/// 动作前缀列宽。`+ install`/`~ update`/`- drift`/`✓ in-sync` 都对齐到 11 列。
const ACTION_WIDTH: usize = 11;

pub fn render(plan: &Plan) -> String {
    let mut out = String::new();
    for h in &plan.harnesses {
        render_harness(h, &mut out);
    }
    if !plan.harnesses.is_empty() {
        out.push('\n');
    }
    let c = plan.counts();
    let _ = writeln!(
        out,
        "Plan: {} install, {} update, {} remove, {} undeliverable, {} drift",
        c.install, c.update, c.remove, c.undeliverable, c.drift
    );
    out
}

fn render_harness(h: &HarnessPlan, out: &mut String) {
    let head = format!("{:<NAME_WIDTH$} ", h.harness);

    if !h.detected {
        let _ = writeln!(out, "{head}⚠ not installed (skipped)");
        return;
    }

    let pending: Vec<&PlanEntry> = h
        .entries
        .iter()
        .filter(|e| !matches!(e.action, Action::InSync))
        .collect();

    // 全同步 → 单行汇总实有数量；有待办 → 逐条列出（汇总行此时是噪音）。
    if pending.is_empty() {
        let _ = writeln!(out, "{head}{}{}", action_cell("✓ in-sync"), summary(h));
        return;
    }

    let mut i = 0;
    while i < pending.len() {
        let e = pending[i];
        i += 1;
        // lead：该 harness 的首行带名字，后续行用等宽缩进对齐。
        let lead = if i == 1 { head.clone() } else { indent() };
        match &e.action {
            // install 无附加说明，合并成一行；diff 已按 label 排好序，故连续。
            Action::Install => {
                let mut labels = vec![e.id.label()];
                while i < pending.len() && matches!(pending[i].action, Action::Install) {
                    labels.push(pending[i].id.label());
                    i += 1;
                }
                let _ = writeln!(out, "{lead}{}{}", action_cell("+ install"), labels.join(", "));
            }
            Action::Update { from, to } => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} ({from} → {to})",
                    action_cell("~ update"),
                    e.id.label()
                );
            }
            Action::Undeliverable { reason } => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} ({reason})",
                    action_cell("! undeliverable"),
                    e.id.label()
                );
            }
            Action::Drift => {
                let _ = writeln!(
                    out,
                    "{lead}{}{} (未在清单中，--prune 可删)",
                    action_cell("- drift"),
                    e.id.label()
                );
            }
            // pending 已过滤掉 InSync；match 需穷尽故留空臂，不 panic。
            Action::InSync => {}
        }
    }
}

fn indent() -> String {
    " ".repeat(NAME_WIDTH + 1)
}

/// 前缀对齐到 `ACTION_WIDTH` 列；超长的 `! undeliverable`(15) 退化为两空格分隔。
fn action_cell(prefix: &str) -> String {
    let n = prefix.chars().count();
    if n >= ACTION_WIDTH {
        format!("{prefix}  ")
    } else {
        format!("{prefix}{}", " ".repeat(ACTION_WIDTH - n))
    }
}

/// `skills:42 mcps:6 plugins:3`——按 `Kind::all()` 顺序，计数 0 的种类跳过。
fn summary(h: &HarnessPlan) -> String {
    let parts: Vec<String> = Kind::all()
        .into_iter()
        .filter_map(|k| {
            let n = h.actual_by_kind.get(&k).copied().unwrap_or(0);
            (n > 0).then(|| format!("{}s:{n}", k.as_str()))
        })
        .collect();
    if parts.is_empty() {
        "(nothing installed)".to_string()
    } else {
        parts.join(" ")
    }
}
```

注：`{}s:` 直接加 `s` 即得 `skills`/`mcps`/`plugins`/`commands`/`agents`/`hooks`，
与 spec §5 示例一致，不需要不规则复数表。

- [ ] **Step 4: 运行测试**

Run: `cargo test --lib render::`
Expected: 11 个断言测试 PASS；`spec_example_matches_design_doc` **FAIL**，报
`snapshot assertion for 'spec_example_matches_design_doc' failed` 并写出
`src/snapshots/henv-render__spec_example_matches_design_doc.snap.new`。

这是 insta 的正常「新快照」流程，不是 bug。先看一眼 `.snap.new` 的内容是否与
spec §5 示例逐行对得上（除上文记录的两处有意偏离），对得上再进 Step 5。

若失败的是别的测试而非快照，按失败信息回去改实现，**不要**改测试里的期望字符串来将就。

- [ ] **Step 5: 接受快照**

Run: `INSTA_UPDATE=always cargo test --lib render::spec_example`
Expected: PASS，`.snap.new` 被改写为 `src/snapshots/henv-render__spec_example_matches_design_doc.snap`。

用单条命令的 `INSTA_UPDATE` 环境变量，**不要**全局装 `cargo-insta`
（用户约束：不做侵入式全局安装，环境变量只作用于一条命令）。

- [ ] **Step 6: 校验快照内容**

Run: `cat src/snapshots/henv-render__spec_example_matches_design_doc.snap`
Expected: 头部是 insta 的 `--- / source / expression` 元信息，正文精确为：

```text
claude-code   ✓ in-sync  skills:42 mcps:6 plugins:3
codex         + install  mcp/browser-use, mcp/context7
              ~ update   skill/dingtalk-mail (v2.1 → v2.2)
              ! undeliverable  hook/superpowers:pre-commit (no hook support)
kimi-code     + install  mcp/context7
              - drift    mcp/experimental-thing (未在清单中，--prune 可删)

Plan: 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift
```

逐列比对：`claude-code` 后 3 空格、`codex` 后 9 空格、`kimi-code` 后 5 空格；
续行 14 空格缩进；`! undeliverable` 后 2 空格（不是 6 空格——它本来就超宽）。

- [ ] **Step 7: 运行全量测试确认通过**

Run: `cargo test --lib render::`
Expected: PASS（12 tests，含快照）

- [ ] **Step 8: Commit**

```bash
git add src/render.rs src/lib.rs src/snapshots
git commit -m "feat: diff 报告渲染，格式由 insta golden 锁定"
```

---

### Task 15: CLI 接线 —— `henv diff` 端到端 + 只读守卫

**Files:**
- Modify: `src/lib.rs`（加 `run_diff` 编排 + `is_enabled` + 单元测试）
- Modify: `src/main.rs`（clap 入口，替换 Task 1 的占位 `println!`）
- Create: `examples/env.yaml`（无密钥示例清单）
- Create: `tests/read_only_guard.rs`（静态守卫，常驻 CI）
- Create: `tests/smoke_real_home.rs`（`#[ignore]`，真实 HOME 只读冒烟）

**Interfaces:**
- Consumes: `manifest::Manifest::parse`（Task 5）、`lower::{lower, DegradeTable}`（Task 6）、
  `adapters::registry`（Task 8）、`diff::{compute, HarnessInput, HarnessState}`（Task 13）、
  `render::render`（Task 14）、`home::Home`（Task 1）
- Produces: `pub fn run_diff(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<()>`
  ——本计划的最终交付物；`main.rs` 与两个集成测试都是它的消费者。

**两条本 Task 才定的语义（写进代码注释，Plan B/C 继承）**

1. **`harnesses` 是 opt-out 列表**：清单里没列出的 harness 视为启用。最小清单 `version: 1`
   也该对三家已安装的 harness 出报告；只有显式 `enabled: false` 才排除。
2. **`henv diff` 恒以 exit 0 结束**（只要没出错）。它是报告，不是断言。有 drift 就非零退出
   会让「切 harness 前看一眼」变成 CI 门禁，那是 Plan C `doctor` 的职责，不是 `diff` 的。

**不在本 Task 内**：`apply`/`scan`/`doctor` 子命令一个都不加——clap 的 `Command` 枚举只有
`Diff` 一个变体。加了会编译通过但没有实现，是半成品。

- [ ] **Step 1: 写失败的测试 —— `src/lib.rs` 单元测试**

在 `src/lib.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    /// 跑一次 run_diff，返回渲染文本。
    /// 用 `TempHome::write` 而非 `std::fs::write`：read_only_guard 扫的是整个
    /// `src/lib.rs`（含 `#[cfg(test)]`），只豁免 `testutil.rs`。
    fn diff(t: &TempHome, manifest: &str) -> String {
        t.write("env.yaml", manifest);
        let mut out = Vec::new();
        run_diff(&t.path("env.yaml"), &t.home, &mut out).expect("run_diff");
        String::from_utf8(out).expect("utf8")
    }

    #[test]
    fn harness_absent_from_manifest_is_enabled_by_default() {
        let m = Manifest::parse("version: 1\nharnesses:\n  codex: { enabled: false }\n").unwrap();
        assert!(is_enabled(&m, CLAUDE_CODE), "未列出 = 启用");
        assert!(is_enabled(&m, KIMI_CODE), "未列出 = 启用");
        assert!(!is_enabled(&m, CODEX), "显式 false 才排除");
    }

    #[test]
    fn empty_home_reports_all_three_as_skipped() {
        let t = TempHome::new();
        let out = diff(&t, "version: 1\n");
        assert!(out.contains("claude-code   ⚠ not installed (skipped)\n"), "{out}");
        assert!(out.contains("codex         ⚠ not installed (skipped)\n"), "{out}");
        assert!(out.contains("kimi-code     ⚠ not installed (skipped)\n"), "{out}");
        assert!(
            out.ends_with("Plan: 0 install, 0 update, 0 remove, 0 undeliverable, 0 drift\n"),
            "{out}"
        );
    }

    #[test]
    fn disabled_harness_is_omitted_entirely() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let out = diff(&t, "version: 1\nharnesses:\n  codex: { enabled: false }\n");
        assert!(!out.contains("codex"), "{out}");
        // 另外两家未安装，仍照常报告
        assert!(out.contains("claude-code   ⚠ not installed (skipped)\n"), "{out}");
    }

    #[test]
    fn desired_mcp_present_in_claude_json_is_in_sync_and_missing_one_is_install() {
        let t = TempHome::new();
        t.mkdir(".claude").write(
            ".claude.json",
            r#"{ "numStartups": 7,
                 "mcpServers": { "context7": { "command": "npx", "args": ["-y", "ctx7"] } } }"#,
        );
        let manifest = "version: 1\nmcps:\n  context7:\n    command: npx\n    args: [\"-y\", \"ctx7\"]\n  browser-use:\n    command: npx\n    args: [\"-y\", \"bu\"]\n";
        let out = diff(&t, manifest);

        assert!(out.contains("claude-code   + install  mcp/browser-use\n"), "{out}");
        // 有一条 install 待办 → 不出 in-sync 汇总行，但 context7 确实是实有的那 1 个 mcp
        assert!(!out.contains("✓ in-sync"), "{out}");
        assert!(
            out.ends_with("Plan: 1 install, 0 update, 0 remove, 0 undeliverable, 0 drift\n"),
            "{out}"
        );
        // 无关字段（numStartups）被读但不被报告
        assert!(!out.contains("numStartups"), "{out}");
    }

    #[test]
    fn fully_satisfied_manifest_renders_the_summary_line() {
        let t = TempHome::new();
        t.mkdir(".claude").write(
            ".claude.json",
            r#"{ "mcpServers": { "context7": { "command": "npx", "args": ["-y", "ctx7"] } } }"#,
        );
        let manifest = "version: 1\nmcps:\n  context7:\n    command: npx\n    args: [\"-y\", \"ctx7\"]\n";
        let out = diff(&t, manifest);
        assert!(out.contains("claude-code   ✓ in-sync  mcps:1\n"), "{out}");
    }

    #[test]
    fn unsupported_loose_skill_produces_the_bang_line_on_kimi() {
        // spec 的核心价值主张：切到 kimi 时这条 skill 会静默失效，diff 必须先喊出来。
        let t = TempHome::new();
        t.mkdir(".kimi-code");
        let manifest = "version: 1\nskills:\n  my-draft:\n    source: local\n    path: /tmp/my-draft\n";
        let out = diff(&t, manifest);
        assert!(out.contains("! undeliverable  skill/my-draft ("), "{out}");
        assert!(out.contains("kimi-code 无用户级 skills 目录"), "{out}");
    }

    #[test]
    fn missing_manifest_file_error_carries_the_path() {
        let t = TempHome::new();
        let mut out = Vec::new();
        let err = run_diff(&t.path("nope.yaml"), &t.home, &mut out).unwrap_err().to_string();
        assert!(err.contains("nope.yaml"), "{err}");
    }

    #[test]
    fn invalid_manifest_error_carries_the_path_and_the_validation_detail() {
        let t = TempHome::new();
        t.write(
            "env.yaml",
            "version: 1\nmcps:\n  c:\n    command: npx\n    env:\n      K: literal\n",
        );
        let mut out = Vec::new();
        let err = run_diff(&t.path("env.yaml"), &t.home, &mut out).unwrap_err().to_string();
        assert!(err.contains("env.yaml"), "{err}");
        assert!(err.contains("mcps.c"), "{err}");
        // 密钥门禁：错误里不许回显值
        assert!(!err.contains("literal"), "错误信息不得回显密钥值：{err}");
        assert!(out.is_empty(), "出错时不输出半截报告");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib`
Expected: FAIL —`run_diff` / `is_enabled` 未定义，编译错误。

（过滤串用不了 `tests::`：其余模块的测试模块也叫 `tests`，`cargo test` 做的是子串匹配，
`render::tests::x` 一样会命中。直接跑全量 `--lib`。）

- [ ] **Step 3: 实现 `run_diff` 与 `is_enabled`**

`src/lib.rs` 整文件替换为下面内容，**把 Step 1 追加的 `#[cfg(test)] mod tests { ... }`
原样保留在末尾**：

```rust
//! henv：跨 harness 能力环境管理器。
//!
//! Plan A 只读：解析 `env.yaml` → 归一化能力 → 扫本机三家现状 → 纯函数 diff → 渲染。
//! 全链路不写 `Home` 下任何文件；该不变量由 `tests/read_only_guard.rs` 静态守卫。

use adapters::HarnessAdapter;
use anyhow::Context as _;
use home::Home;
use manifest::Manifest;
use std::io::Write;
use std::path::Path;

pub mod adapters;
pub mod capability;
pub mod diff;
pub mod home;
pub mod lower;
pub mod manifest;
pub mod render;
pub mod secrets;
pub mod skills_dir;

// 只有测试引用这三个常量；不加 cfg 会让 `cargo build` 报 unused import。
#[cfg(test)]
use adapters::{CLAUDE_CODE, CODEX, KIMI_CODE};

#[cfg(test)]
mod testutil;

/// 只读比对：清单为期望，本机为现状，输出「切 harness 时什么会失效」。
///
/// 恒以 `Ok(())` 结束（有 drift 也是 `Ok`）——`diff` 是报告不是断言，
/// 门禁语义留给 Plan C 的 `doctor`。
pub fn run_diff(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("cannot read manifest: {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&text)
        .with_context(|| format!("invalid manifest: {}", manifest_path.display()))?;

    let desired = lower::lower(&manifest);
    let degrade = lower::DegradeTable::new(&manifest);

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
            diff::HarnessState { name: a.name().to_string(), detected, actual },
            &**a,
        ));
    }

    let inputs: Vec<diff::HarnessInput> = pairs
        .iter()
        .map(|(state, adapter)| diff::HarnessInput { state, adapter: *adapter })
        .collect();

    let plan = diff::compute(&desired, &inputs, &degrade);
    write!(out, "{}", render::render(&plan))?;
    Ok(())
}

/// `harnesses` 是 opt-out 列表：未列出 = 启用，只有显式 `enabled: false` 排除。
fn is_enabled(m: &Manifest, name: &str) -> bool {
    m.harnesses.get(name).map(|h| h.enabled).unwrap_or(true)
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib`
Expected: PASS（全量；其中根 `tests` 模块新增 8 个）

若 `desired_mcp_present_in_claude_json_is_in_sync_and_missing_one_is_install` 报
「多了 1 个 update」，说明 `compare_versions` 的 `Spec::Mcp` 分支把 `args` 比错了——
回 Task 3 查，不要在这里放宽断言。

若 `unsupported_loose_skill_produces_the_bang_line_on_kimi` 报 install 而非 undeliverable，
说明 Task 12 的 kimi `supports()` 没把 `Origin::Manifest` 的 `Kind::Skill` 判成 Unsupported
（`Origin::Plugin` 例外只对 bundle 分发物生效）。

- [ ] **Step 5: 写 `src/main.rs`（替换 Task 1 的占位实现）**

```rust
//! henv CLI。Plan A 只有 `diff`；`apply`/`scan`/`doctor` 由 Plan B/C 各自加一个变体。

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "henv", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 只读比对 env.yaml 与本机现状，报告切 harness 时会失效的能力
    Diff {
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Diff { manifest } => {
            let home = lenv::home::Home::resolve()?;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            lenv::run_diff(&manifest, &home, &mut lock)?;
        }
    }
    Ok(())
}
```

`#[command(about)]` 取 `Cargo.toml` 的 `description`（Task 1 已写「跨 harness 能力环境管理器」）。

- [ ] **Step 6: 写 `examples/env.yaml`**

```yaml
# henv 示例清单 —— spec §4 的完整形态。
# 密钥永远以 ${VAR} 引用出现，manifest 解析阶段拒绝任何字面量。本文件不含真实密钥。
version: 1

harnesses:
  claude-code: { enabled: true }
  codex:       { enabled: true }
  kimi-code:   { enabled: true }

skills:
  dingtalk-mail:
    source: agents-dir          # 已在 ~/.agents/skills，只做登记 + 链接分发
  superpowers:
    source: git
    repo: obra/superpowers
    path: skills                # 仓库内子目录（monorepo 型）
    ref: v6.3.0                 # ref 就是锁
  my-draft:
    source: local
    path: ~/dev/skills/my-draft

mcps:
  context7:
    command: npx
    args: ["-y", "@upstash/context7-mcp"]
    env:
      CONTEXT7_API_KEY: ${CONTEXT7_API_KEY}
    targets: all

plugins:
  superpowers-bundle:
    source: claude-marketplace
    marketplace: obra/superpowers-marketplace
    entry: superpowers          # 跨 harness 身份只认 entry 名（spec §2.1 第 3 条）
    degrade:
      hooks: report
      commands: report
```

- [ ] **Step 7: 写 `tests/read_only_guard.rs`（常驻，非 ignore）**

Plan A 的只读不变量不能靠人记——用一个静态守卫把它变成会失败的测试。
比「跑一遍再比对 mtime」可靠：真实 HOME 上的 harness 进程会自己改 `.claude.json`，
mtime 断言必然抖动；而源码里有没有写 API 是确定的。

```rust
//! Plan A 只读守卫：`src/` 里除 `testutil.rs` 外不得出现任何文件写 API。
//!
//! 写路径（`.bak` + temp + rename、`claude mcp add-json` shell out）属 Plan B。
//! 在 Plan B 落地前，这个测试是「henv diff 绝不改我的配置」这句承诺的唯一证据。

use std::path::{Path, PathBuf};

/// 带 `(` 的模式避免误伤 `fs::symlink_metadata` 这类只读 API。
const FORBIDDEN: &[&str] = &[
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

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
    out.sort();
}

fn offenders(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).expect("read src");
    let mut hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        // 文档与说明性注释里提到这些词是正常的，只看代码行
        if line.trim_start().starts_with("//") {
            continue;
        }
        for pat in FORBIDDEN {
            if line.contains(pat) {
                hits.push(format!("{}:{}: {pat}", path.display(), i + 1));
            }
        }
    }
    hits
}

fn src_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut v);
    v
}

#[test]
fn plan_a_source_has_no_write_api_outside_testutil() {
    let mut all = Vec::new();
    for f in src_files() {
        if f.file_name().and_then(|n| n.to_str()) == Some("testutil.rs") {
            continue;
        }
        all.extend(offenders(&f));
    }
    assert!(
        all.is_empty(),
        "Plan A 必须只读，发现写 API：\n{}\n（若确需写入，说明你在做 Plan B 的活）",
        all.join("\n")
    );
}

#[test]
fn scanner_is_not_vacuous() {
    // 反证扫描器有效：testutil.rs 故意含写 API，必须被抓到。
    // 若这条失败，说明 walk() 没走到文件，上一条的 PASS 毫无意义。
    let files = src_files();
    let testutil = files
        .iter()
        .find(|f| f.file_name().and_then(|n| n.to_str()) == Some("testutil.rs"))
        .expect("src/testutil.rs 必须存在且被扫描到");
    let hits = offenders(testutil);
    assert!(hits.iter().any(|h| h.contains("fs::write(")), "{hits:?}");
    assert!(hits.iter().any(|h| h.contains("fs::create_dir")), "{hits:?}");
    assert!(files.len() >= 10, "只扫到 {} 个文件，walk 可能坏了", files.len());
}
```

- [ ] **Step 8: 写 `tests/smoke_real_home.rs`（`#[ignore]`，手动跑）**

```rust
//! 真实 HOME 只读冒烟（spec §6）。默认不跑：
//!   cargo test --test smoke_real_home -- --ignored --nocapture
//!
//! 只读性不靠 mtime 断言（真实 HOME 上运行中的 harness 会自己改 `.claude.json`，
//! 那种断言必然抖动），而靠 `tests/read_only_guard.rs` 的静态守卫 + 本测试只调
//! `run_diff`（Plan A 里它没有任何写路径可达）。

#[test]
#[ignore = "读真实 HOME；手动跑：cargo test --test smoke_real_home -- --ignored --nocapture"]
fn diff_again_real_home_succeeds_and_reports_every_detected_harness() {
    let home = lenv::home::Home::resolve().expect("resolve HOME");
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/env.yaml");

    let mut out = Vec::new();
    lenv::run_diff(&manifest, &home, &mut out).expect("run_diff on real HOME must not error");
    let text = String::from_utf8(out).expect("utf8");

    println!("{text}");

    assert!(text.ends_with("drift\n"), "缺 Plan 页脚：{text}");
    // 本机三家都装着（spec §2）：一行都不该是 "not installed"
    for name in ["claude-code", "codex", "kimi-code"] {
        let line = text
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("报告里没有 {name} 行：{text}"));
        assert!(!line.contains("not installed"), "{line}");
    }
}
```

- [ ] **Step 9: 运行全量测试**

Run: `cargo test`
Expected: PASS。汇总各 Task 的测试数（lib 单元测试 + 2 个集成测试），
`smoke_real_home` 显示为 `1 ignored`。

- [ ] **Step 10: 跑一次真实冒烟（人工确认输出可读）**

Run: `cargo test --test smoke_real_home -- --ignored --nocapture`
Expected: PASS，并打印出本机三家的真实 diff 报告。

人工核对三点：
1. `claude-code` 行的 skills/mcps/plugins 计数与 `ls ~/.claude/skills | wc -l` 同量级
2. kimi-code 行对散装 skill 出 `!` undeliverable（spec 的核心诉求）
3. superpowers 在 claude 与 kimi 之间**不**报 update（SHA 优先比较生效，spec §2.1 第 3 条）

第 3 点若报 update，回 Task 3 检查 `compare_versions` 的 Plugin 分支是否真的先比 `sha`。

- [ ] **Step 11: 手跑一次二进制**

Run: `cargo run --quiet -- diff --manifest examples/env.yaml`
Expected: 与 Step 10 打印的报告一致。

Run: `cargo run --quiet -- diff --manifest /nope.yaml`
Expected: 非零退出，stderr 形如
`Error: cannot read manifest: /nope.yaml` + `Caused by: No such file or directory (os error 2)`。

Run: `cargo run --quiet -- --help`
Expected: 只列出 `diff` 一个子命令，不出现 apply/scan/doctor。

- [ ] **Step 12: clippy 与 fmt 收口**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: 无输出（零 warning）。

clippy 常见拦路：`needless_collect`（Task 15 的 `inputs` 收集是必要的，因为
`pairs` 的借用要活到 `compute` 调用；若被误报，改成显式 `let inputs: Vec<_> = ...;`
并在同一表达式内消费）、`manual_let_else`、`redundant_clone`。

- [ ] **Step 13: Commit**

```bash
git add src/lib.rs src/main.rs examples/env.yaml tests/read_only_guard.rs tests/smoke_real_home.rs
git commit -m "feat: henv diff CLI 端到端接线，附只读静态守卫"
```

---

## Self-Review（写计划时已跑完，执行者无需重复）

**1. spec 覆盖**

| spec 章节 | 覆盖 Task |
|---|---|
| §3 架构：五层、lib 划分 | File Structure + Task 1 |
| §3 `Capability`/`Support`/`HarnessAdapter` | Task 2、8、12 |
| §3 决策 1「plugin 不是特殊公民」 | Task 6（lower 拆解）、Task 7（`plugin_from_path`） |
| §3 决策 2「apply = f(desired, actual)」 | Task 13（Plan A 只做 f 的只读半边） |
| §3 决策 3「merge 不覆盖 / `${VAR}` 不展开」 | Task 4；merge 写入属 Plan B |
| §4 清单 schema 全部字段 | Task 5（20 tests 含 spec §4 完整示例） |
| §4 决策 2「密钥语法即门禁」 | Task 4 + Task 5 校验 |
| §5 `diff` 管道六条规则 | Task 13 |
| §5 drift 方向（报告不删） | Task 13 `Counts::remove` 恒 0 + render 的 `--prune` 提示 |
| §5 diff 输出形态 | Task 14 golden |
| §6 fixture 假 HOME | Task 1 `TempHome` + 各 adapter Task |
| §6 幂等 / roundtrip / merge 安全 | **Plan B**（需写路径才有意义） |
| §6 golden diff | Task 14 |
| §6 secrets 单测 | Task 4 |
| §6 真实环境冒烟 `#[ignore]` | Task 15 Step 8 |
| §2.1 五条实测修正 | 1→Plan B 记录；2→Task 7/12；3→Task 3；4→Task 7/11/12；5→Task 1 Cargo.toml |
| §8 技术选型 | Task 1 Cargo.toml（`toml_edit`/`similar`/`git2` 属 Plan B/C，未引入） |

**2. 占位符扫描**：全文无 TBD / TODO / 「适当处理错误」/「类似 Task N」。
Task 1 Step 9 的 `main.rs` 里那句 `println!("henv: not yet wired")` 不是占位符——
它是 Task 1 的可编译交付物，Task 15 Step 5 整文件替换掉它。

**3. 类型一致性**：`render` 与 `run_diff` 的签名与「接口契约」一节逐字一致；
`Action::Update { from, to }` 的字段名在 Task 13 定义、Task 14 消费处一致；
`HarnessPlan::actual_by_kind` 在 Task 13 产出、Task 14 `summary()` 消费一致；
`Kind::all()` 顺序（Skill, Mcp, Command, Agent, Hook, Plugin）在 Task 2 定义，
Task 14 的 `in_sync_summary_kinds_follow_kind_all_order` 按此顺序断言。

---

## 交接给 Plan B / C 的既定决策

写 Plan B/C 时不要重新讨论这几条，它们是本轮已与用户确认的结论：

1. **claude-code 的 MCP 写入走 `claude mcp add-json` shell out**，不直接 merge
   `~/.claude.json`。该文件 71KB、权限 0600、被 harness 进程频繁重写，直接改会与
   运行中的 claude 抢写。codex 的 `config.toml` 与 kimi 的 `mcp.json` 仍走
   `toml_edit` / serde merge + `.bak` + temp-rename。
2. **plugin 以 bundle manifest 对账为主、SHA 优先**：三家 bundle 都自带 per-harness
   manifest（`.claude-plugin/plugin.json`、`.codex-plugin/plugin.json`、
   `.kimi-plugin/plugin.json` 或根 `kimi.plugin.json`），kimi 的 manifest 有两种形态。
   只在目标 harness 缺 manifest 时才降级拆成散装 skill/MCP。Plan A 没实现 `bundle.rs`
   是因为没有任何调用方——Plan B 一开工就要补。
3. **`Support::Degraded` 在 Plan A 无构造路径**（Task 12 已记录）。拆解需要 `bundle.rs`，
   属 Plan B。`diff` 对 `Degraded` 与 `Native` 同路径处理，已有测试锁定。
4. **codex 不记录 plugin 的 version/SHA**（`[plugins."name@marketplace"]` 只有
   `enabled`），故其 plugin 版本比较恒为 `Unknown` → in-sync。这是已知盲区，
   Plan C 的 `doctor` 该显式报告「codex 侧 plugin 版本无法核验」，不要假装能比。
5. **`apply` 的确认交互与 `--yes`/`--prune` 标志**（spec §5）属 Plan B；
   `scan` 的收养写回 `env.yaml` 与 `doctor` 的环境体检属 Plan C。
6. **`henv diff` 恒 exit 0**。要门禁语义就加 `doctor --check`，别改 `diff`。
