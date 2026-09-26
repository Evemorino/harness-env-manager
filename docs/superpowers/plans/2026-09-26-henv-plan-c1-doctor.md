# Plan C1：`doctor` 门禁实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `henv doctor` 成为这个项目一直承诺的**门禁**：只读跑一遍体检，把「必须处理」的问题（同名冲突、交付不了、配置读不出来、`${VAR}` 缺值）报出来并**以非零码退出**；把「只是信息」的事实（drift、版本判不了、副本过期、断链、清单里启用了但本机没装的家）报成**警告**，不影响退出码。

**Architecture:** doctor 复用 `diff` 的同一个规划函数 `plan_for(.., SourceMode::Cached, None)`——**同一份观察、同一条读盘路径**，不新增任何写路径、不新增 I/O 种类。它做的是三件事：把计划里已经算出来的事实**重新分类**（`conflict`/`undeliverable` 是问题，`drift`/`unknown`/`update`/注记是警告），加上两项计划算不出来的环境检查（`${VAR}` 存在性、逐家配置可解析性），再把结果渲染成自己的报告。分类与渲染是纯函数（`Plan` + `Manifest` + `HarnessCheck` → `DoctorReport` → 文本），I/O 只在一处。`render()` 的产物与既有 golden **一个字节不动**：doctor 有自己的渲染函数与自己的 golden。

**Tech Stack:** Rust 2024 / clap 4.6 / serde / serde_yaml_ng / anyhow。**不新增任何依赖。** dev-deps：insta、tempfile。`henv` 的 `testutil` 非默认 feature（自指 dev-dependency 打开）。

**Spec:** `docs/design.md` §5（`doctor（体检）= diff 的 undeliverable + 环境校验`：`${VAR}` 引用的变量当前是否存在、henv 交付的拷贝是否与源脱节（§9.4）、各 harness 配置文件是否可解析、`detect()` 失败的 enabled harness）、§2.1（条目的 `Shared | HarnessLocal | PluginCache | Other` 分类是 drift 判定与 **doctor 断链报告**的基础）、§9.3/§9.4/§9.5（内容判定、过期与注记、`degrade` 的出处）、§9.9（只读守卫的白名单）、§6/§9.10（测试策略：绝不碰真实 `~`）。本计划由 Task 1 在 `docs/design.md` 新增 §10.1 把下面的契约落成正式条款。

## Global Constraints

- 对话中文；代码标识符、注释、commit message 一律英文。
- **仓库没有远程**：不 push、不创建远程、不改写历史。每个 Task 的 commit 步骤只提交本 Task 涉及的文件，**是否真的提交由用户决定**（全局约定：不主动 commit）。
- **doctor 只读。** 除 stdout 外不写任何东西：不落 `.bak`、不碰账本、不碰 harness 目录。`SourceMode::Cached` 下 `resolve` 只读缓存（缺内容记 `Deferred`），不 clone。
- **`tests/read_only_guard.rs` 的两张表一个字符都不改。** 本计划不新增写路径；`src/doctor.rs` 里出现任何 `WRITE_API` 模式都会（正确地）让守卫变红。
- **测试绝不触碰真实 `~`**：一切夹具落在 `TempHome`。唯一读真实 HOME 的是既有的 `#[ignore]` 冒烟测试。
- **`render()` 的输出与 `src/snapshots/henv__render__tests__*.snap` 一个字节不动。** doctor 用 `render_doctor`，不进 `render()`。
- **`diff` / `apply` 行为不变**：`plan_for` 只从私有提到 `pub(crate)`，签名与语义不动；`Planned` 只多一个字段。
- **rust-version 1.85**：**没有 let-chains**（`let ... &&` 要 1.88）。需要「两个条件同时成立」时写嵌套 `if`。
- 本仓库**今天就有既有 lint 债**（`cargo fmt --check` 在 `src/main.rs`、`src/store/git.rs`、`src/store/skills.rs`、`tests/git_source_end_to_end.rs` 上不过；`src/store/skills.rs` 有两条 `clippy::unnecessary_get_then_check`）。**本计划不清理那笔债**，只在**新增/改动的行**上保持 `cargo fmt` 与 clippy 干净。

## Review Focus

按「最可能先咬人」排序。每一条都在它该在的那个 Task 里有测试钉着。

1. **三个退出码不许互相退化。** `0` = 没问题、`1` = 体检发现问题、`2` = 工具压根没跑成（清单读不了/解不开，或一个我们归不了因的失败）。「发现问题」与「跑不动」必须可区分——把 2 写成 1 会让脚本把「henv 自己坏了」当成「环境有问题」。→ Task 1、Task 2。
2. **doctor 不回答「同步了没有」。** `install` 行（清单里有、本机还没装）**不是** finding——那是 apply 的活，把它当问题会让 doctor 在每台新机器上恒红。`update` 行（**已经交付过**、但内容与源不同）是警告：环境里存在过期物。→ Task 1。
3. **计划类判据与 `diff` 同源。** `conflict`/`undeliverable` 直接取 `plan_for` 产出的 `Action`，不许另写一套判据、也不许重新读盘。→ Task 1。
4. **健康路径只观察一次。** 逐家读盘（`read_state`）只在**失败归因**时才发生：`plan_for` 成功时，harness 的「装没装」直接取 `plan.harnesses[].detected`，不再读第二遍（§9.5 那条「不许有第二条读路径」的同一理由）。→ Task 2。
5. **`${VAR}` 检查绝不回显值。** 报告里只出现变量名与引用它的 key（`secrets` 既有约定：错误消息带 key 名、绝不带 value）。→ Task 3。
6. **空报告/空段是空操作。** 没有问题的机器上，`(env)`/`(plan)` 段不出现，页脚仍是 `Doctor: 0 problems, N warnings`；`doctor` 永远是最后一行。→ Task 1、Task 4。

## File Structure

**新增**
- `src/doctor.rs` —— 报告的数据模型（`DoctorReport`/`HarnessCheck`/`Check`/`CheckKind`）、分类（`plan_checks`/`env_checks`/`harness_checks`）、渲染（`render_doctor`）、入口 `run_doctor`。一个文件，因为它就是一个纯函数管道 + 一层 I/O。
- `tests/doctor_end_to_end.rs` —— 走公开入口与真实二进制的端到端：健康/问题/警告三态、退出码 0/1/2、三段的出现与顺序。
- `src/snapshots/henv__doctor__tests__a_problem_report_renders_every_section.snap` —— doctor 报告的 golden（Task 1 建立，Task 2/3/4 各更新一次）。

**修改**
- `src/lib.rs` —— `pub mod doctor;`；`Planned` 与 `plan_for` 提到 `pub(crate)`；`Planned` 多一个 `manifest: Manifest` 字段（`${VAR}` 检查要用——已经在手上解析过一次，不再读第二次盘）。
- `src/render.rs` —— `NAME_WIDTH` 与 `action_cell` 提到 `pub(crate)`：doctor 复用同一套列宽与动作前缀，两份报告看起来才是一家。
- `src/main.rs` —— `Doctor` 子命令 + `0/1/2` 出口映射 + `long_about` 披露 doctor 的契约（`--help` 是第二处披露面）。
- `docs/design.md` —— 新增 `## 10. Plan C：体检与收养`，Task 1 先落 `### 10.1 doctor（门禁与退出码）`；`scan` 由 C2 的计划补 `### 10.2`。

---

### Task 1: `doctor` 的骨架、plan 段与出口契约（0/1/2）

**Files:**
- Create: `src/doctor.rs`、`tests/doctor_end_to_end.rs`、`src/snapshots/henv__doctor__tests__a_problem_report_renders_every_section.snap`
- Modify: `src/lib.rs`、`src/render.rs`、`src/main.rs`、`docs/design.md`

**Interfaces:**
- Produces: `henv::doctor::run_doctor(manifest_path: &Path, home: &Home, out: &mut dyn Write) -> anyhow::Result<DoctorReport>`；`DoctorReport::{problems(), warnings(), healthy()}`；`render_doctor(&DoctorReport) -> String`；`CheckKind::{Env, Plan, Warning}`（`Env`/`Plan` 是问题，`Warning` 不影响退出码）。
- Consumes: `crate::plan_for`（本 Task 提到 `pub(crate)`）、`crate::diff::{Action, Plan}`、`crate::render::{NAME_WIDTH, action_cell}`、`crate::home::Home`。

- [ ] **Step 1: 把契约落纸（`docs/design.md` §10.1）**

在 §9.12 之后新增：

```markdown
## 10. Plan C：体检与收养

### 10.1 doctor（门禁与退出码）

`henv doctor [-m env.yaml]`。**只读**：与 `diff` 同一份规划（`SourceMode::Cached`）、同一条读盘路径，
不写任何文件。它是 `diff` 从不承担的那半句承诺——「门禁语义留给 Plan C 的 doctor」。

**退出码**（doctor 是唯一一个退出码承载语义的子命令，因此它必须把「发现问题」与「跑不动」分开）：

| 码 | 含义 |
|---|---|
| `0` | 体检完成，没有必须处理的问题（可以有警告） |
| `1` | 体检完成，**有必须处理的问题**（下表左列） |
| `2` | henv 压根没跑成：清单读不了/解不开，或一个归不了因的失败 |

**问题（fail）**：`conflict`（同名冲突，apply 见到就停）、`undeliverable`（清单要求了却交付不了）、
某家配置读不出来（这一家的判定不可信）、`${VAR}` 引用的变量当前不存在（apply 会把未展开的引用写进配置）。

**警告（不 fail）**：`drift`（盘上有、清单没有）、`unknown`（版本判不了）、`stale`（**已经交付过**的
副本与源不同）、行上的注记（源未验证 / 副本被手改）、断链、清单里启用了但本机没装的家。

**doctor 不回答「同步了没有」**：`install` 行（清单里有、本机还没装）不是 finding——那是 apply 的活，
把它当问题会让 doctor 在每台新机器上恒红。这是本命令与 `diff` 的分工：`diff` 是普查，`doctor` 是门禁。
```

- [ ] **Step 2: 写失败的端到端测试（`tests/doctor_end_to_end.rs`）**

测试用具沿用既有集成测试的做法（`henv::testutil::TempHome` + `env!("CARGO_BIN_EXE_henv")`）。先只写三条：

```rust
use henv::testutil::TempHome;
use std::process::Command;

/// 只启 claude-code：否则「启用了但本机没装」会各带一条警告，把干净环境的页脚搅浑。
const ONLY_CLAUDE: &str =
    "harnesses:\n  claude-code: { enabled: true }\n  codex: { enabled: false }\n  kimi-code: { enabled: false }\n";

fn doctor(home: &TempHome, manifest: &str) -> (i32, String, String) {
    home.write("env.yaml", manifest);
    let out = Command::new(env!("CARGO_BIN_EXE_henv"))
        .arg("doctor")
        .arg("-m")
        .arg(home.path("env.yaml"))
        .env("HENV_HOME", home.home.root())
        .env("HOME", home.home.root())
        .output()
        .expect("run henv doctor");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 空 HOME + 只有版本号的清单：没问题，也没有警告 → 0。
#[test]
fn a_clean_environment_exits_zero() {
    let home = TempHome::new();
    let (code, out, err) = doctor(&home, &format!("version: 1\n{ONLY_CLAUDE}"));
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.ends_with("Doctor: 0 problems, 0 warnings\n"), "{out}");
}

/// 同名冲突是必须处理的问题 → 1。
#[test]
fn a_name_conflict_exits_one() {
    let home = TempHome::new();
    // 目标位置预埋一个**内容不同、又不是 henv 交付的**同名 skill（§9.3 不变式二）。
    home.write(".claude/skills/foo/SKILL.md", "hand written\n");
    home.write(".agents/skills/foo/SKILL.md", "from the shared library\n");
    let manifest = format!(
        "version: 1\n{ONLY_CLAUDE}skills:\n  foo:\n    source: agents-dir\n    targets: [claude-code]\n"
    );
    let (code, out, err) = doctor(&home, &manifest);
    assert_eq!(code, 1, "stdout={out} stderr={err}");
    assert!(out.contains("(plan)"), "{out}");
    assert!(out.contains("conflict"), "{out}");
    assert!(out.contains("skill/foo@claude-code"), "{out}");
}

/// 清单解不开 = 工具没跑成 → 2（不是 1）。
#[test]
fn a_broken_manifest_exits_two() {
    let home = TempHome::new();
    let (code, out, err) = doctor(&home, "version: nope\n");
    assert_eq!(code, 2, "stdout={out} stderr={err}");
    assert!(!out.contains("Doctor:"), "没跑成就不该有报告：{out}");
    assert!(err.contains("Error:"), "{err}");
}
```

（`TempHome::write` 的确切形状以 `src/testutil.rs` 为准：既有集成测试里已有等价写法，照抄即可。）

- [ ] **Step 3: 跑测试，确认失败**

Run: `cargo test --test doctor_end_to_end 2>&1 | tail -20`
Expected: 三条全失败——第一条因为 `henv doctor` 这个子命令还不存在（clap 报 unknown subcommand，退出码并非我们要断言的语义）。

- [ ] **Step 4: 最小实现（类型 + 分类 + 渲染 + 入口 + CLI）**

`src/doctor.rs` 的骨架（文档注释按仓库习惯写「为什么」，不是「做什么」）：

```rust
//! doctor：只读门禁（spec §5、§10.1）。

use crate::capability::CapId;
use crate::diff::{Action, Plan};
use crate::home::Home;
use crate::render::{NAME_WIDTH, action_cell};
use crate::store::skills::SourceMode;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// 一条检查结果的落点：决定它渲染在哪个段、以及是否影响退出码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// 环境问题（`${VAR}` 缺失）——必须处理。
    Env,
    /// 计划问题（conflict / undeliverable）——必须处理。
    Plan,
    /// 只报告，不影响退出码。
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub kind: CheckKind,
    /// 段名之后、行尾之前的整段文本（含动作/种类前缀，已对齐）。
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    pub fn problems(&self) -> usize {
        self.checks.iter().filter(|c| c.kind != CheckKind::Warning).count()
    }
    pub fn warnings(&self) -> usize {
        self.checks.iter().filter(|c| c.kind == CheckKind::Warning).count()
    }
    pub fn healthy(&self) -> bool {
        self.problems() == 0
    }
}

/// 计划里已经算出来的事实 → 检查项。判据与 `diff` 同源（同一个 `Action`），
/// 只是分类不同：`conflict`/`undeliverable` 挡路，其余是信息。
fn plan_checks(plan: &Plan) -> Vec<Check> {
    let mut checks = Vec::new();
    // 行的主语带 `@harness`：doctor 的报告是扁平的（不像 `diff` 那样按家分节），
    // 不带这一截，同一条 skill 在两家的两行会长得一模一样。
    let entries = plan
        .harnesses
        .iter()
        .flat_map(|h| h.entries.iter().map(move |e| (h.harness.as_str(), e)))
        .chain(plan.unattached.iter().map(|e| ("(global)", e)));
    for (harness, e) in entries {
        let subject = format!("{}@{harness}", e.id.label());
        match &e.action {
            Action::Conflict { reason } => checks.push(Check {
                kind: CheckKind::Plan,
                text: line("! conflict", &subject, reason),
            }),
            Action::Undeliverable { reason } => checks.push(Check {
                kind: CheckKind::Plan,
                text: line("! undeliverable", &subject, reason),
            }),
            Action::Drift => checks.push(Check {
                kind: CheckKind::Warning,
                text: line("- drift", &subject, e.note.as_deref().unwrap_or("未在清单中")),
            }),
            Action::Unknown => checks.push(Check {
                kind: CheckKind::Warning,
                text: line("- unknown", &subject, "版本无法判定"),
            }),
            // 已经交付过、但与源不同 → 环境里有过期物（警告，不是问题）。
            // `from == "断链"` 是 §9.3 的自愈判定：前缀换成 `- link`（§2.1 的
            // 「doctor 断链报告」就是这一行——断链与过期是两件事）。
            Action::Update { from, to } => {
                let (prefix, detail) = if from == "断链" {
                    ("- link", format!("盘上是断链，apply 会换成真拷贝（→ {to}）"))
                } else {
                    ("- stale", format!("副本与源不同（{from} → {to}）"))
                };
                checks.push(Check { kind: CheckKind::Warning, text: line(prefix, &subject, &detail) });
            }
            // install **不是** finding：清单里有、本机还没装 = apply 的活。
            Action::Install | Action::InSync => {}
        }
        // 行上的注记是「判定未被上游确认」或「副本被手改过」这类必须被看见的信息
        // ——它不改变动作，所以既不进 problems 也不许丢。
        if let Some(note) = &e.note {
            if !matches!(e.action, Action::Drift) {
                checks.push(Check {
                    kind: CheckKind::Warning,
                    text: line("- note", &label, note),
                });
            }
        }
    }
    checks
}

fn line(prefix: &str, subject: &str, detail: &str) -> String {
    format!("{}{subject} ({detail})", action_cell(prefix))
}

pub fn render_doctor(report: &DoctorReport) -> String {
    let mut out = String::new();
    // 段落顺序：问题在前（先 (env) 后 (plan)：环境不可信时计划判定也没意义），警告在后。
    for (kind, head) in [
        (CheckKind::Env, "(env)"),
        (CheckKind::Plan, "(plan)"),
        (CheckKind::Warning, "(warn)"),
    ] {
        let rows: Vec<&Check> = report.checks.iter().filter(|c| c.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        let lead = format!("{:<NAME_WIDTH$} ", head);
        for (i, c) in rows.iter().enumerate() {
            let head = if i == 0 { lead.as_str() } else { "" };
            // 续行按 NAME_WIDTH+1 缩进，与 render 的表格同一个视觉列。
            let indent = if i == 0 { "" } else { "             " };
            let _ = writeln!(out, "{head}{indent}{}", c.text);
        }
    }
    if !report.checks.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "Doctor: {} problems, {} warnings",
        report.problems(),
        report.warnings()
    );
    out
}

pub fn run_doctor(
    manifest_path: &Path,
    home: &Home,
    out: &mut dyn Write,
) -> anyhow::Result<DoctorReport> {
    let planned = crate::plan_for(manifest_path, home, SourceMode::Cached, None)?;
    let report = DoctorReport {
        checks: plan_checks(&planned.plan),
    };
    write!(out, "{}", render_doctor(&report))?;
    Ok(report)
}
```

`src/lib.rs`：`pub mod doctor;`；`struct Planned` 与 `fn plan_for` 改成 `pub(crate)`（字段保持 `pub`）。
`src/render.rs`：`const NAME_WIDTH` 与 `fn action_cell` 改成 `pub(crate)`（加一句注释说明 doctor 复用它们是为了两份报告同一套列宽）。
`src/main.rs`：

```rust
    /// 只读体检：把「必须处理」的问题报出来，有问题时以非零码退出（门禁）
    Doctor {
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
    },
```

```rust
        Command::Doctor { manifest } => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let report = match henv::doctor::run_doctor(&manifest, &home, &mut lock) {
                Ok(report) => report,
                // `2` = henv 没跑成（清单读不了/解不开、归不了因的失败）。与 `1`
                // （体检发现问题）分开，脚本才能区分「环境有问题」与「工具坏了」。
                Err(err) => {
                    eprintln!("Error: {err:?}");
                    std::process::exit(2);
                }
            };
            drop(lock);
            if !report.healthy() {
                std::process::exit(1);
            }
        }
```

同时把 doctor 的契约写进 `long_about`（只披露，不改 `render()`）：

```text
`doctor` 同样只读：它把「必须处理」的问题（同名冲突、交付不了、配置读不出来、${VAR} 缺值）报出来，
并按门禁语义退出——0 = 没有问题、1 = 有问题、2 = henv 自己没跑成。
```

- [ ] **Step 5: 跑测试，确认通过**

Run: `cargo test --test doctor_end_to_end 2>&1 | tail -20`
Expected: 三条全绿。

- [ ] **Step 6: 补 golden 与「空段是空操作」**

`src/doctor.rs` 测试模块里手造一份含各段的 `DoctorReport`，`insta::assert_snapshot!` 钉住 `render_doctor` 的完整输出（空 `checks` 时只有页脚、没有空行前缀）。golden 文件手写进 `src/snapshots/`（与 `render` 的既有做法一致）。

- [ ] **Step 7: 全量验证**

Run: `cargo test --all-features 2>&1 | grep -E "^test result"`
Expected: 全绿；`render` 的既有 golden **未被改动**（`git status` 里没有 `src/snapshots/henv__render__*`）。

- [ ] **Step 8: commit（等用户同意）**

```bash
git add src/doctor.rs src/lib.rs src/render.rs src/main.rs tests/doctor_end_to_end.rs src/snapshots docs/design.md
git commit -m "feat(doctor): add the read-only gate with a 0/1/2 exit contract"
```

---

### Task 2: 逐家体检（装没装、配置读不读得出来）与失败归因

**Files:**
- Modify: `src/doctor.rs`（`HarnessCheck`、harness 段渲染、归因路径）、`tests/doctor_end_to_end.rs`、`src/snapshots/henv__doctor__tests__*.snap`

**Interfaces:**
- Consumes: `crate::plan_for`、`crate::adapters::{registry, lookup}`、`crate::is_enabled`、`crate::manifest::KNOWN_HARNESSES`。
- Produces: `HarnessCheck { name, installed, parse_error: Option<String> }`；`DoctorReport.harnesses`。

- [ ] **Step 1: 写失败测试**

```rust
/// 清单里启用了、但本机没装的家：只警告（退出码仍是 0），行上写明「跳过」。
#[test]
fn an_enabled_but_absent_harness_only_warns() {
    let home = TempHome::new().unwrap();
    let manifest = "version: 1\nharnesses:\n  kimi-code:\n    enabled: true\n";
    let (code, out) = doctor(&home, manifest);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("kimi-code"), "{out}");
    assert!(out.contains("not installed"), "{out}");
    assert!(out.contains("1 warning"), "{out}");
}

/// 配置读不出来 = 判定不可信 → 问题（1），而且要说清是哪一家、为什么。
#[test]
fn an_unreadable_harness_config_exits_one() {
    let home = TempHome::new().unwrap();
    // 造一个解不开的配置文件（形状以该 adapter 的 reader 为准：claude 读 settings.json，
    // codex 读 config.toml，kimi 读 mcp.json / installed.json）。
    home.write(".codex/config.toml", "this is not toml = = =\n");
    let (code, out) = doctor(&home, "version: 1\n");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("codex"), "{out}");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test doctor_end_to_end an_enabled_but_absent_harness_only_warns an_unreadable_harness_config_exits_one 2>&1 | tail -20`
Expected: 两条失败（没有 harness 行；配置坏掉时 `plan_for` 直接把错误上抛成退出码 2）。

- [ ] **Step 3: 实现**

harness 行来自 `planned.plan.harnesses`（`detected` 字段，**不额外读盘**）；`enabled` 来自清单。
配置不可解析走**失败归因**：`plan_for` 返回 `Err` 时，逐家 `detect()` + `read_state()` 找出是哪一家坏；
找得到 → 报成 `(plan)` 段的问题 + 一句「计划这一轮没跑」，找得到但…没有 → `Err` 上抛（退出码 2）。

```rust
/// 逐家体检：只看「装没装」与「配置读不读得出来」。
///
/// **只在归因路径上调用**：`plan_for` 成功时，装没装直接取 `plan.harnesses[].detected`。
/// 健康路径上再读一遍盘就违反了 §9.5 那条「不许有第二条读路径」——两次观察之间盘面会变。
fn probe_harnesses(manifest: &Manifest, home: &Home) -> Vec<HarnessCheck> { /* ... */ }
```

- [ ] **Step 4: 跑测试确认通过，并更新 golden**

Run: `cargo test --test doctor_end_to_end 2>&1 | tail -20`
Expected: 全绿；golden 里多出 harness 行与 `(warn)` 的 `- harness` 行（按新版手写 `.snap`）。

- [ ] **Step 5: commit（等用户同意）**

```bash
git add src/doctor.rs tests/doctor_end_to_end.rs src/snapshots
git commit -m "feat(doctor): check each harness (installed, config parseable)"
```

---

### Task 3: `${VAR}` 存在性检查（`(env)` 段）

**Files:**
- Modify: `src/lib.rs`（`Planned` 多 `manifest` 字段）、`src/doctor.rs`（`env_checks`）、`tests/doctor_end_to_end.rs`、golden

**Interfaces:**
- Consumes: `crate::manifest::{Manifest, McpEntry}`、`crate::secrets::{parse_ref, missing}`。
- Produces: `env_checks(manifest: &Manifest, lookup: &dyn Fn(&str) -> Option<String>) -> Vec<Check>`（纯函数，测试注入假 lookup）。

- [ ] **Step 1: 写失败测试（纯函数层）**

```rust
/// 引用了当前环境里没有的变量 → 问题，且消息里只有变量名与 key 名（绝不回显值）。
#[test]
fn a_missing_reference_is_a_problem_and_never_echoes_a_value() {
    let m = Manifest::parse("version: 1\nmcps:\n  context7:\n    command: npx\n    env:\n      CONTEXT7_API_KEY: ${CONTEXT7_API_KEY}\n").unwrap();
    let checks = env_checks(&m, &|_| None);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].kind, CheckKind::Env);
    assert!(checks[0].text.contains("CONTEXT7_API_KEY"), "{}", checks[0].text);
    assert!(checks[0].text.contains("context7"), "{}", checks[0].text);

    let ok = env_checks(&m, &|k| (k == "CONTEXT7_API_KEY").then(|| "x".into()));
    assert!(ok.is_empty());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib doctor:: 2>&1 | tail -20`
Expected: 编译失败（`env_checks` 不存在）。

- [ ] **Step 3: 实现**

`Planned` 多一个 `manifest: Manifest`（`plan_for` 已经解析过一次，直接搬进去——再读一次盘既慢又可能与本次规划看到的那份不一致）。`env_checks` 遍历 `manifest.mcps`，对每个非空 `env` 值取 `parse_ref`，用 `secrets::missing` 判缺；一条缺失 = 一条 `Check { kind: CheckKind::Env, text: line("! env", "mcp `<key>`", &format!("${{{var}}} 在当前环境里没有值")) }`。`run_doctor` 传 `&|v| std::env::var_os(v).map(|s| s.to_string_lossy().into_owned())`。

- [ ] **Step 4: 端到端补一条**

```rust
/// 端到端：清单引用一个几乎不可能存在的变量 → 1，且 `(env)` 段在 `(plan)` 之前。
#[test]
fn a_missing_env_reference_exits_one() {
    let home = TempHome::new().unwrap();
    let manifest = "version: 1\nmcps:\n  context7:\n    command: npx\n    env:\n      K: ${HENV_TEST_MISSING_VAR_9F3A}\n";
    let (code, out) = doctor(&home, manifest);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("(env)"), "{out}");
    assert!(!out.contains("HENV_TEST_MISSING_VAR_9F3A="), "只出现名字，不出现值：{out}");
}
```

- [ ] **Step 5: 全量验证 + commit（等用户同意）**

Run: `cargo test --all-features 2>&1 | grep -E "^test result"`

```bash
git add src/lib.rs src/doctor.rs tests/doctor_end_to_end.rs src/snapshots
git commit -m "feat(doctor): check that \${VAR} references exist in the environment"
```

---

### Task 4: 断链行的端到端验收（§2.1）

> **与原计划的一处偏离（执行 Task 1 时在真机上发现，同日改掉）**：原计划让 Task 4 新增
> `link_checks`，从 `planned.content.targets` 读 `TargetContent::NoContent`。**没有那样做**——
> 断链在计划里已经长成 `Update { from: "断链" }`（§9.3 的自愈判定），`plan_checks` 只要挑
> `- link` 前缀就够了；再读一遍 `content.targets` 是**第二条读盘路径**，还会把同一件事报两遍。
> 分类与 golden 因此已经在 Task 1 落地，本 Task 只补它的端到端证据。

**Files:**
- Modify: `tests/doctor_end_to_end.rs`、`src/snapshots/henv__doctor__tests__*.snap`（只在需要时）

**Interfaces:**
- Consumes: `TempHome::symlink(link, target)`（`src/testutil.rs`）。

- [ ] **Step 1: 写端到端测试**

```rust
/// §2.1：条目分类是 doctor 断链报告的基础。断链是**警告**：§9.3 说 apply 会把它换成
/// 真拷贝（旧的脆弱形态在重跑中自愈），所以它不挡路——退出码仍是 0。
#[test]
fn a_broken_link_is_reported_as_a_warning_and_does_not_fail() {
    let home = TempHome::new();
    // 清单要求 skill/foo（内容在共享库里），claude 那边这条是个断链。
    home.write(".agents/skills/foo/SKILL.md", "from the shared library\n");
    home.symlink(".claude/skills/foo", "/nonexistent/henv-doctor-test");
    let manifest = format!(
        "version: 1\n{ONLY_CLAUDE}skills:\n  foo:\n    source: agents-dir\n    targets: [claude-code]\n"
    );

    let (code, out, err) = doctor(&home, &manifest);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("- link"), "{out}");
    assert!(out.contains("skill/foo@claude-code"), "{out}");
    assert!(out.contains("1 warnings"), "{out}");
}
```

- [ ] **Step 2: 跑它**

Run: `cargo test --test doctor_end_to_end a_broken_link_is_reported_as_a_warning_and_does_not_fail 2>&1 | tail -20`
Expected: PASS（分类在 Task 1 已落地）。**若失败**，说明 Task 1 的 `Update { from: "断链" }`
分支没走到——先修那里，不要在本 Task 里补第二条读盘路径。

- [ ] **Step 3: 全量验证 + 真机一条**

Run: `cargo test --all-features 2>&1 | grep -E "^test result"`
Run（只读，真机）：`cargo run --quiet -- doctor -m examples/env.yaml; echo "exit=$?"`
Expected: 真机上那 14 条 claude-code 的断链副本各出现一行 `- link`，退出码 1（因为清单里
另有 20 条 `undeliverable`——断链自己**不**贡献退出码）。

- [ ] **Step 4: commit（等用户同意）**

```bash
git add tests/doctor_end_to_end.rs src/snapshots
git commit -m "test(doctor): pin the broken-link warning end to end"
```

---

## 验收（C1 完成时）

- `henv doctor` 在清单健康、环境健康的机器上退出 `0`，输出只有页脚。
- 有问题时退出 `1`，且在 `(env)`/`(plan)` 段里逐条说清「哪一条、为什么、怎么办」。
- 清单读不了/解不开时退出 `2`，错误走 stderr，stdout 不留半份报告。
- `diff`、`apply`、`render` 的输出与行为**一个字节不变**；`read_only_guard` 两张表未改。
- 真机冒烟（`#[ignore]`）仍通过：`cargo test --test smoke_real_home -- --ignored --nocapture`。
