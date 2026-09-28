//! henv CLI。`diff`/`doctor` 只读（`doctor` 还带门禁语义）、`apply` 与 `scan` 落盘。

use clap::{Parser, Subcommand};
use henv::{AssumeYes, Confirm, StdinConfirm};
use std::path::PathBuf;

/// `--version` 的完整串：Cargo 的版本号 + 构建时的 git 描述（由 `build.rs` 经
/// `cargo:rustc-env` 注入）。光有 `0.1.0` 回答不了唯一要紧的问题——「我现在跑的是不是
/// 刚编译的那份」。
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("HENV_BUILD"), ")");

#[derive(Parser)]
#[command(
    name = "henv",
    version = VERSION,
    about,
    // `--help` 是人读得到的第二处披露面（第一处是 `examples/env.yaml` 的注释与 lib.rs 的
    // rustdoc）。两件事必须在这里说清：`degrade:` 从 B3b 起真的生效（但只对写了
    // `source_harness:` 的条目——没有拆解就没有「装不下」），以及哪个子命令会写盘
    // ——只写进文档的话，读终端的人和读代码的人拿到的信息不对称，而「我配了 skip 怎么没静音」
    // 与「只想看看怎么把我配置改了」正是那种误会的产物。
    // 只披露，不改 `render()` 的产物——那是被 insta golden 逐字节锁住的格式。
    long_about = "比对 env.yaml 与本机现状，报告切换 harness 时会失效的能力；`apply` 还能把清单要的能力交付到各家。\n\n\
写盘边界：`diff` 全程只输出报告，不写任何配置文件，有 drift 也算成功退出（门禁语义留给 Plan C 的 doctor）；\n\
`apply` 会写——它向各家的 skills 目录真拷贝内容，并把「放了什么」记进 ~/.local/state/henv/provenance.json。\n\
`apply` 落盘前会先渲染计划并要一次 y/N 确认（`--yes` 跳过，供脚本与 CI 用）；\n\
`apply --prune` 只撤销账本里记着的条目（henv 自己交付过的 skill 拷贝与 MCP 键），用户手放的 drift 一律只报不删；\n\
`apply --harness <name>` 把一次运行限定在一家（值域：claude-code / codex / kimi-code）。\n\
`skills list` 同样只读：它只把仓库取到 henv 自己的缓存目录下看完即删。\n\
`doctor` 同样只读：它把「必须处理」的问题（同名冲突、交付不了、配置读不出来、${VAR} 缺值）报出来，\n\
并按门禁语义退出——0 = 没有问题、1 = 有问题、2 = henv 自己没跑成（清单读不了/解不开）。\n\
`scan` 会写，但是只追加：它把本机已有、清单还没描述的能力接在对应段（skills/mcps/plugins）的末尾，\n\
已有字节一个不动；落盘前先自己解一遍（解不开就拒绝写），并落一份 `env.yaml.henv.bak`。\n\
`scan --dry-run` 只打印。它不做门禁——退出码只有 0（跑成了）与 2（工具没跑成）。\n\
`add`/`remove`/`upgrade` 会写清单：文本级编辑，只切自己那几行，注释与排版原样保留，落盘前自检并留 .bak；\n\
默认接着交付或撤销（落盘前一次 y/N 确认），`--no-apply`（`remove` 是 `--keep`）只改清单，`--dry-run` 什么都不写。\n\
`upgrade` 只升 `source: git` 条目的 ref，且只认 version 形 tag——没有就提示用 `--to`，不猜默认分支。\n\n\
degrade: 政策决定「目标 harness 吃不下、但内容已经拆出来了」的能力是进报告还是静默丢弃（§9.5）。"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 列出一个 git 仓库里所有包含 SKILL.md 的目录（只读）
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },

    /// 只读比对 env.yaml 与本机现状，报告切 harness 时会失效的能力
    Diff {
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
    },
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
    /// 添加一条 skill：写进清单，随后交付（会写文件）
    Add {
        /// skill 名，也就是清单里那个键
        name: String,
        /// 来源类型
        #[arg(long, value_enum, default_value_t = SourceArg::Git)]
        source: SourceArg,
        /// 仓库地址或 GitHub 的 owner/repo 简写（source=git 时必需）
        #[arg(long)]
        repo: Option<String>,
        /// 仓库内路径，指向 skill 目录本身（source=git/local 时用）
        #[arg(long)]
        path: Option<String>,
        /// 分支 / tag / 提交（source=git 时必需，ref 就是锁）
        #[arg(long = "ref", value_name = "REF")]
        git_ref: Option<String>,
        /// 交付目标，逗号分隔（claude-code,codex,kimi-code）；不写就是 all
        #[arg(long, value_delimiter = ',', value_parser = parse_harness)]
        targets: Option<Vec<String>>,
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
        /// 只打印将要做什么，不写清单也不交付
        #[arg(long)]
        dry_run: bool,
        /// 只改清单，交付留给 `henv apply`
        #[arg(long)]
        no_apply: bool,
        /// 不问确认直接执行（用于脚本与 CI）
        #[arg(long)]
        yes: bool,
        /// 交付只对这一家生效
        #[arg(long, value_name = "NAME", value_parser = parse_harness)]
        harness: Option<String>,
    },
    /// 从清单删条目，并撤销本机由 henv 交付的副本（会写文件）
    Remove {
        /// 要删的 skill 名，可以给多个
        #[arg(required = true)]
        names: Vec<String>,
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
        /// 只打印将要做什么，不写清单也不撤销
        #[arg(long)]
        dry_run: bool,
        /// 只改清单，本机副本原样留着
        #[arg(long)]
        keep: bool,
        /// 不问确认直接执行（用于脚本与 CI）
        #[arg(long)]
        yes: bool,
        /// 撤销只对这一家生效
        #[arg(long, value_name = "NAME", value_parser = parse_harness)]
        harness: Option<String>,
    },
    /// 把清单里 git 源的 ref 移到上游最新的 version 形 tag（会写文件）
    Upgrade {
        /// 只升级这些 skill；不写就是清单里所有 git 源条目
        names: Vec<String>,
        /// 指定目标 ref，给了就不查上游
        #[arg(long, value_name = "REF")]
        to: Option<String>,
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
        /// 只打印 old → new，不写清单也不交付
        #[arg(long)]
        dry_run: bool,
        /// 只改清单，交付留给 `henv apply`
        #[arg(long)]
        no_apply: bool,
        /// 不问确认直接执行（用于脚本与 CI）
        #[arg(long)]
        yes: bool,
        /// 交付只对这一家生效
        #[arg(long, value_name = "NAME", value_parser = parse_harness)]
        harness: Option<String>,
    },
    /// 只读体检：报出「必须处理」的问题，并按门禁语义退出（0/1/2）
    Doctor {
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
    },
    /// 收养：把本机已有、清单还没描述的能力追加进清单（会写 env.yaml）
    Scan {
        /// 清单路径
        #[arg(short, long, default_value = "env.yaml")]
        manifest: PathBuf,
        /// 只打印将要追加的条目，不写清单
        #[arg(long)]
        dry_run: bool,
    },
}

/// `add --source` 的值域（§4 的三种 skill 来源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SourceArg {
    Git,
    Local,
    AgentsDir,
}

impl From<SourceArg> for henv::manifest::SkillSourceKind {
    fn from(value: SourceArg) -> Self {
        match value {
            SourceArg::Git => Self::Git,
            SourceArg::Local => Self::Local,
            SourceArg::AgentsDir => Self::AgentsDir,
        }
    }
}

#[derive(Subcommand)]
enum SkillsCmd {
    /// 列出仓库里的 skill（名字 + 仓库内相对路径）
    ///
    /// 只读：不写 harness 目录、不写账本。唯一的写入是 henv 自己缓存下的
    /// 一个临时子目录，命令结束前删掉。
    List {
        /// 仓库地址，或 GitHub 的 owner/repo 简写
        #[arg(long)]
        repo: String,
        /// 分支 / tag / 提交；缺省用仓库的默认分支
        // 显式写成 `--ref`（值名也一并跟规格对齐）：字段名 `git_ref` 只是我们内部
        // 给 `resolve` 传参时的叫法，CLI 对外的是 §9.8 承诺的那个拼写。下面这段是
        // 普通注释而不是文档注释——clap 把文档注释整段渲染进 `--help`，
        // 用户不该在帮助里读到「字段名叫什么」。
        #[arg(long = "ref", value_name = "REF")]
        git_ref: Option<String>,
        /// 只看仓库里的这个子目录
        #[arg(long)]
        path: Option<String>,
    },
}

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

fn main() -> anyhow::Result<()> {
    let home = henv::home::Home::resolve()?;
    match Cli::parse().command {
        Command::Diff { manifest } => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::run_diff(&manifest, &home, &mut lock)?;
        }
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
        Command::Add {
            name,
            source,
            repo,
            path,
            git_ref,
            targets,
            manifest,
            dry_run,
            no_apply,
            yes,
            harness,
        } => {
            let mut confirm: Box<dyn Confirm> = if yes {
                Box::new(AssumeYes)
            } else {
                Box::new(StdinConfirm)
            };
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let req = henv::AddRequest {
                name: &name,
                source: source.into(),
                repo: repo.as_deref(),
                path: path.as_deref(),
                git_ref: git_ref.as_deref(),
                targets,
                manifest: &manifest,
                dry_run,
                no_apply,
            };
            henv::run_add(&req, &home, &mut lock, confirm.as_mut(), harness.as_deref())?;
        }
        Command::Remove {
            names,
            manifest,
            dry_run,
            keep,
            yes,
            harness,
        } => {
            let mut confirm: Box<dyn Confirm> = if yes {
                Box::new(AssumeYes)
            } else {
                Box::new(StdinConfirm)
            };
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let req = henv::RemoveRequest {
                names,
                manifest: &manifest,
                dry_run,
                keep,
            };
            henv::run_remove(&req, &home, &mut lock, confirm.as_mut(), harness.as_deref())?;
        }
        Command::Upgrade {
            names,
            to,
            manifest,
            dry_run,
            no_apply,
            yes,
            harness,
        } => {
            let mut confirm: Box<dyn Confirm> = if yes {
                Box::new(AssumeYes)
            } else {
                Box::new(StdinConfirm)
            };
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let req = henv::UpgradeRequest {
                names,
                manifest: &manifest,
                to: to.as_deref(),
                dry_run,
                no_apply,
            };
            henv::run_upgrade(&req, &home, &mut lock, confirm.as_mut(), harness.as_deref())?;
        }
        Command::Skills { cmd } => match cmd {
            SkillsCmd::List {
                repo,
                git_ref,
                path,
            } => {
                let out = henv::run_skills_list(
                    &repo,
                    git_ref.as_deref(),
                    path.as_deref(),
                    &home,
                )?;
                print!("{out}");
            }
        },
        Command::Doctor { manifest } => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let report = match henv::doctor::run_doctor(&manifest, &home, &mut lock) {
                Ok(report) => report,
                // `2` = henv 没跑成（清单读不了/解不开、归不了因的失败）。与 `1`
                // （体检发现问题）分开，脚本才能区分「环境有问题」与「工具坏了」。
                // 错误形状照抄 anyhow 自己的 `Termination`（`Error: {err:?}`），
                // 免得两个出口打印两种风格。
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
        Command::Scan { manifest, dry_run } => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            // `scan` 不是门禁，没有 `1`：只有「跑成了」（0）与「工具没跑成」（2）。
            if let Err(err) = henv::scan::run_scan(&manifest, &home, dry_run, &mut lock) {
                drop(lock);
                eprintln!("Error: {err:?}");
                std::process::exit(2);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §9.8 的 CLI 面写的是 `skills list --repo <url> [--ref <r>] [--path <p>]`。
    /// clap 缺省按字段名生成长选项，字段叫 `git_ref` 就得到 `--git-ref`——
    /// 照规格敲 `--ref` 的人会拿到「unexpected argument」。规格是绑定权威，
    /// 选项名得跟着它。
    #[test]
    fn skills_list_accepts_the_ref_flag_the_spec_promises() {
        let cli = Cli::try_parse_from([
            "henv", "skills", "list", "--repo", "obra/superpowers", "--ref", "main",
        ])
        .expect("§9.8 承诺的 --ref 必须能被解析");

        match cli.command {
            Command::Skills {
                cmd: SkillsCmd::List { git_ref, .. },
            } => assert_eq!(git_ref.as_deref(), Some("main")),
            _ => panic!("解析成了别的子命令"),
        }
    }

    /// §9.8 的 `--harness <name>`：值域在 CLI 层就判，敲错一个字母得到的是报错，
    /// 不是「无事可做」。
    #[test]
    fn apply_rejects_an_unknown_harness_name() {
        // `Cli` 没实现 `Debug`（clap 的派生不生成它），所以不能用 `unwrap_err()`。
        match Cli::try_parse_from(["henv", "apply", "--harness", "cursor"]) {
            Ok(_) => panic!("未知的 harness 名必须在 CLI 层就被拒"),
            Err(err) => assert!(err.to_string().contains("unknown harness"), "{err}"),
        }
        assert!(Cli::try_parse_from(["henv", "apply", "--harness", "codex"]).is_ok());
    }

    /// `--version` 要能回答「跑的是哪一次构建」：`build.rs` 注入的 git 描述必须真的进到
    /// 那个串里。构建脚本在没 git 的环境退化成 `unknown`，那时也必须是一段非空的描述——
    /// 空括号比没有这段更糟。
    #[test]
    fn version_carries_the_build_description() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")), "{VERSION}");
        assert!(VERSION.contains('(') && VERSION.ends_with(')'), "{VERSION}");
        assert!(!VERSION.contains("()"), "{VERSION}");
    }
}
