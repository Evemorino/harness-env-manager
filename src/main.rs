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
`show` 同样只读：它只把仓库取到 henv 自己的缓存目录下看完即删。\n\
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
    /// 添加一条 skill 或一个 MCP：写进清单，随后交付（会写文件）
    ///
    /// 不带 `--mcp` 时加的是 skill（`--source` 三选一）；带 `--mcp` 时加的是 MCP，用
    /// `--command/--arg/--env`（stdio）或 `--url/--header`（远端）描述它——两种传输也互斥，
    /// 不在 `--command` 与 `--url` 之间猜。两类载荷互斥——「这次加的是哪一类」由参数本身说清。
    Add {
        /// 条目名，也就是清单里那个键
        name: String,
        /// 这次加的是 MCP（与 skill 那组参数互斥）
        #[arg(long)]
        mcp: bool,
        /// skill 的来源类型，缺省 `git`
        #[arg(long, value_enum, conflicts_with = "mcp")]
        source: Option<SourceArg>,
        /// 仓库地址或 GitHub 的 owner/repo 简写（source=git 时必需）
        #[arg(long, conflicts_with = "mcp")]
        repo: Option<String>,
        /// 仓库内路径，指向 skill 目录本身（source=git/local 时用）
        #[arg(long, conflicts_with = "mcp")]
        path: Option<String>,
        /// 分支 / tag / 提交（source=git 时必需，ref 就是锁）
        #[arg(long = "ref", value_name = "REF", conflicts_with = "mcp")]
        git_ref: Option<String>,
        /// MCP 的可执行文件（`--mcp` 时必需）
        #[arg(long, requires = "mcp", conflicts_with = "url")]
        command: Option<String>,
        /// MCP 的远端地址（`--mcp` 且不带 `--command` 时用它）
        #[arg(long, requires = "mcp", conflicts_with = "command")]
        url: Option<String>,
        /// MCP 的参数，可重复，顺序保留
        ///
        /// `allow_hyphen_values`：参数里带前导 `-` 是常态（`-y`、`--stdio`），
        /// 没有它 clap 会把 `--arg -y` 里的 `-y` 当成另一个选项。
        #[arg(
            long = "arg",
            value_name = "ARG",
            requires = "mcp",
            allow_hyphen_values = true
        )]
        args: Vec<String>,
        /// MCP 的环境变量，写成 KEY='${VAR}'，可重复
        ///
        /// **值要用单引号包住**，否则 shell 会先把 `${VAR}` 展开成空串。
        #[arg(
            long = "env",
            value_name = "KEY=${VAR}",
            requires = "mcp",
            value_parser = parse_env
        )]
        env: Vec<(String, String)>,
        /// 远端 MCP 的请求头，写成 KEY='${VAR}'，可重复
        ///
        /// **值要用单引号包住**，否则 shell 会先把 `${VAR}` 展开成空串。
        #[arg(
            long = "header",
            value_name = "KEY=${VAR}",
            requires = "url",
            value_parser = parse_env
        )]
        headers: Vec<(String, String)>,
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
    /// 盘点一个 git 仓库里有什么：skill、各家的 plugin manifest、MCP 声明（只读）
    ///
    /// 远程仓库不止有 skill——`obra/superpowers` 一个仓库里同时是 12 家 harness 的 plugin。
    Show {
        /// 仓库地址，或 GitHub 的 owner/repo 简写
        repo: String,
        /// 分支 / tag / 提交；缺省用仓库默认分支
        #[arg(long = "ref", value_name = "REF")]
        git_ref: Option<String>,
        /// 只看仓库里的这个子目录
        #[arg(long)]
        path: Option<String>,
    },
    /// 浏览 plugin 市场：有哪些可装、同一名字在各家是什么状态（只读）
    ///
    /// 只覆盖 plugin——只有它有「市场」这个概念（skill 与 MCP 没有地方登记「有哪些可下载」）。
    Browse {
        /// 家名（claude-code / codex / kimi-code）或关键词
        ///
        /// 写家名 = 只看那家的市场；写别的 = 按名字搜索。
        query: Option<String>,
        /// 显式指定只看这一家的市场
        #[arg(long, value_name = "NAME", value_parser = parse_harness)]
        harness: Option<String>,
        /// 显式按关键词搜（与位置参数等价，用于避开家名撞车）
        #[arg(long, value_name = "PATTERN")]
        search: Option<String>,
    },
    /// 列出本机每一家当前装着的 skills / MCP / plugins（只读）
    ///
    /// 与 `diff` 的分工：`diff` 说「清单要的和本机差在哪」，`status` 说「本机现在有什么」。
    /// 它不读清单，所以没有 env.yaml 也能用。
    Status {
        /// 只看这一家（缺省 = 全部已安装的家）
        #[arg(long, value_name = "NAME", value_parser = parse_harness)]
        harness: Option<String>,
        /// 只看这一类（缺省 = 全部六类）
        #[arg(long, value_name = "KIND", value_parser = parse_kind)]
        kind: Option<henv::capability::Kind>,
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

/// `add --env KEY=${VAR}` / `add --mcp --url … --header KEY=${VAR}` → `(KEY, "${VAR}")`。
/// 两处共用：报错措辞不点名「环境变量」，否则 `--header` 那条会读出错误的上下文。
///
/// **值的 `${VAR}` 形式在这里就校验**：清单层也拒字面量（§4 决策 3：密钥永远以引用出现），
/// 但那时用户已经敲完一整串参数、还要回头找是哪一行；不如敲下回车时就拦住。
fn parse_env(s: &str) -> Result<(String, String), String> {
    let Some((key, value)) = s.split_once('=') else {
        return Err(format!("expected `KEY=${{VAR}}`, got `{s}`"));
    };
    if key.is_empty() {
        return Err("键不能为空".to_string());
    }
    // **与清单层同一条规则**（`secrets::parse_manifest_ref`，也就是 `validate_env` 用的
    // 那条）：这里放行的，清单层必须收。曾经这里是「以 `${` 开头、以 `}` 结尾」的粗判，
    // 于是 `${K:-x}` 在命令行放行、落盘时才被清单层拦下，用户看到的是一句
    // 「refusing to write a manifest that does not parse back」——把「哪一层在判什么」
    // 暴露给了用户，而那本来只是我们内部的次序。
    if henv::secrets::parse_manifest_ref(value).is_none() {
        // 不回显值：与清单层同一条纪律（那可能是密钥，而终端会进日志与回滚缓冲）。
        return Err("值必须是 ${VAR} 引用（或 ${VAR:-} 空默认值）——密钥不写进清单".to_string());
    }
    Ok((key.to_string(), value.to_string()))
}

/// `add` 的两条正交维度：**载荷**（skill / MCP）与 MCP 的**传输**（stdio / 远端）。
/// 每次调用只能落在其中一个格子里，所以三个格子之外的所有参数都必须被拒。
///
/// 为什么不全靠 clap 的 `requires` / `conflicts_with`：实测它们在「A 要求 B、而 B 与已给的
/// C 冲突」这类组合上**不报错**——`--command <c> --header K=${V}`、
/// `--url <u> --arg -y`、`--url <u> --env K=${V}`、以及 skill 模式带 `--url` 都会被解析器
/// 收下，然后那个参数在构造载荷时**静默消失**：清单里少一个字段，用户以为加上了。
/// 这类「静默成功」是本项目一贯要挡掉的（§9.8：静默成功会让脚本以为生效了），所以在这里
/// 显式再判一遍，报错点名那个被丢下的参数。
fn check_add_flags(
    mcp: bool,
    command: Option<&str>,
    url: Option<&str>,
    args: &[String],
    env: &[(String, String)],
    headers: &[(String, String)],
) -> Result<(), String> {
    if !mcp {
        let stray = [
            command.map(|_| "--command"),
            url.map(|_| "--url"),
            (!args.is_empty()).then_some("--arg"),
            (!env.is_empty()).then_some("--env"),
            (!headers.is_empty()).then_some("--header"),
        ];
        if let Some(flag) = stray.into_iter().flatten().next() {
            return Err(format!(
                "{flag} 只在 `--mcp` 时有效——这次加的是 skill，宁可不写也不静默丢掉它"
            ));
        }
        return Ok(());
    }
    match url {
        // 远端：`--url` 那边的格子，stdio 的两个参数没有落脚处。
        Some(_) => {
            if command.is_some() {
                return Err(
                    "`--command` 与 `--url` 是 MCP 的两种传输，二选一——不能同时给".to_string(),
                );
            }
            if !args.is_empty() {
                return Err(
                    "`--arg` 是 stdio 的参数，远端 MCP（`--url`）没有它——宁可不写也不静默丢掉"
                        .to_string(),
                );
            }
            if !env.is_empty() {
                return Err(
                    "`--env` 是 stdio 的参数，远端 MCP 的凭据走 `--header`——宁可不写也不静默丢掉"
                        .to_string(),
                );
            }
        }
        // stdio：`--header` 是远端才有的字段。
        None => {
            if !headers.is_empty() {
                return Err(
                    "`--header` 是远端 MCP（`--url`）的请求头，stdio 这边没有它——否则它会\
                     静默丢掉"
                        .to_string(),
                );
            }
            if command.is_none() {
                return Err("--mcp requires `--command` or `--url`".to_string());
            }
        }
    }
    Ok(())
}

/// `status --kind` 的值域：复用 [`henv::capability::Kind::parse`]，不在 CLI 层另写一张表。
fn parse_kind(s: &str) -> Result<henv::capability::Kind, String> {
    henv::capability::Kind::parse(s).ok_or_else(|| {
        let known: Vec<&str> = henv::capability::Kind::all()
            .iter()
            .map(|k| k.as_str())
            .collect();
        format!("unknown kind `{s}` (known: {})", known.join(", "))
    })
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
            mcp,
            source,
            repo,
            path,
            git_ref,
            command,
            url,
            args,
            env,
            headers,
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
            // 先判「有没有给了却用不上的参数」：clap 的 `requires`/`conflicts_with` 在几种
            // 组合下不报错，参数会**静默消失**（见 check_add_flags 的注释）。
            check_add_flags(
                mcp,
                command.as_deref(),
                url.as_deref(),
                &args,
                &env,
                &headers,
            )
            .map_err(anyhow::Error::msg)?;
            // 两类载荷共用一个请求：`--mcp` 时用 MCP 那组参数，否则是 skill（缺省 source=git）。
            //
            // transport 先落在一个**外层**的 `Option` 里：`AddKind` 借它，局部变量活不过
            // 那个借用。
            let transport = if mcp {
                // 两种传输二选一：给了 `--url` 就是 http，否则要 `--command`（stdio）。
                Some(match url.as_deref() {
                    Some(url) => henv::capability::McpTransport::Http {
                        url: url.to_string(),
                        headers: headers.iter().cloned().collect(),
                    },
                    None => henv::capability::McpTransport::Stdio {
                        command: command.clone().ok_or_else(|| {
                            anyhow::anyhow!("--mcp requires `--command` or `--url`")
                        })?,
                        args: args.clone(),
                        env: env.iter().cloned().collect(),
                    },
                })
            } else {
                None
            };
            let kind = match &transport {
                Some(transport) => henv::AddKind::Mcp { transport },
                None => henv::AddKind::Skill {
                    source: source.unwrap_or(SourceArg::Git).into(),
                    repo: repo.as_deref(),
                    path: path.as_deref(),
                    git_ref: git_ref.as_deref(),
                },
            };
            let req = henv::AddRequest {
                name: &name,
                kind,
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
        Command::Show {
            repo,
            git_ref,
            path,
        } => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::show::run_show(&home, &mut lock, &repo, git_ref.as_deref(), path.as_deref())?;
        }
        Command::Browse {
            query,
            harness,
            search,
        } => {
            // 位置参数先认家名（写 `claude-code` 是想看那一家），认不出就当关键词。
            let (query_harness, query_pattern) = match query {
                Some(q) if henv::manifest::KNOWN_HARNESSES.contains(&q.as_str()) => (Some(q), None),
                other => (None, other),
            };
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::browse::run_browse(
                &home,
                &mut lock,
                harness.or(query_harness).as_deref(),
                search.or(query_pattern).as_deref(),
            )?;
        }
        Command::Status { harness, kind } => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::run_status(&home, &mut lock, harness.as_deref(), kind)?;
        }
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

    /// 一条 `add` 的参数串（不带程序名）→ argv。写成空格切开的字符串而不是一列
    /// `&["henv", "add", …]`：读起来就是命令行的样子，也不会被 rustfmt 拆成竖排。
    fn add_argv(args: &str) -> Vec<&str> {
        std::iter::once("henv").chain(args.split(' ')).collect()
    }

    /// 把一条 `add` 命令的参数喂给 `check_add_flags`——`run` 里的调用点就是这些字段，
    /// 而 clap 拦不下的那些组合只能靠它挡。clap 自己就拒的也算「拒」。
    fn add_is_refused(args: &str) -> bool {
        match Cli::try_parse_from(add_argv(args)) {
            // clap 自己就拒了，也是拒。
            Err(_) => true,
            Ok(cli) => match cli.command {
                Command::Add {
                    mcp,
                    command,
                    url,
                    args,
                    env,
                    headers,
                    ..
                } => check_add_flags(
                    mcp,
                    command.as_deref(),
                    url.as_deref(),
                    &args,
                    &env,
                    &headers,
                )
                .is_err(),
                _ => panic!("不是 add"),
            },
        }
    }

    /// 这四种组合 clap **收下**——`requires` 在「A 要求 B、而 B 与已给的 C 冲突」时会连要求
    /// 一起丢掉，不报错（实测）。它们曾经真的静默丢掉那个参数：`--header` 不见了、
    /// `--arg` 不见了、`--env` 不见了，`--url` 出现在 skill 模式里也不吭声。所以挡它们的
    /// 责任完全在 `check_add_flags`：这个测试是那道判断的回归锁。
    #[test]
    fn clap_accepts_these_combinations_so_the_cli_must_refuse_them() {
        for args in [
            "add x --mcp --command npx --header K=${V}",
            "add x --mcp --url u --arg -y",
            "add x --mcp --url u --env K=${V}",
            // 单给 `--url` 时 clap 会报「缺 --mcp」；带上一个与 `--mcp` 冲突的 skill 参数，
            // 那条要求就被丢掉了——这才是要挡的形状。
            "add x --source git --ref v1 --url u",
        ] {
            assert!(
                Cli::try_parse_from(add_argv(args)).is_ok(),
                "clap 这一层现在会拦它了，这条前置条件失效——重新想这道判断放在哪：{args}"
            );
            assert!(add_is_refused(args), "必须被拒（否则参数静默消失）：{args}");
        }
    }

    /// 反过来：三个格子里各自合法的形状不能被误伤。
    #[test]
    fn add_accepts_each_of_the_three_payload_shapes() {
        for args in [
            // skill（缺省 git 源）
            "add x --repo o/r --ref v1",
            // stdio 的 MCP
            "add x --mcp --command npx --arg -y --env K=${V}",
            // 远端的 MCP
            "add x --mcp --url u --header K=${V}",
        ] {
            assert!(!add_is_refused(args), "不该被拒：{args}");
        }
    }

    /// **`--env` / `--header` 的门禁必须与清单层是同一条规则**：CLI 放行的形状，清单层
    /// 必须收；CLI 拒的形状，清单层也不该放。
    ///
    /// 这条是被一次真实的撞墙逼出来的（2026-09-28）：CLI 层原来只判「以 `${` 开头、以 `}`
    /// 结尾」，于是把 claude 那份 context7 plugin 的值 `${CONTEXT7_API_KEY:-}` 原样敲进来
    /// 会在**落盘那一刻**被清单层拦下，报的还是「refusing to write a manifest that does not
    /// parse back」——用户看到的是我们内部的次序，而不是「这个值不行」。
    #[test]
    fn the_env_gate_is_the_same_rule_the_manifest_uses() {
        for good in ["K=${K}", "K=${K:-}"] {
            assert!(parse_env(good).is_ok(), "CLI 该收：{good}");
            let (k, v) = parse_env(good).unwrap();
            assert!(
                henv::secrets::validate_env(&k, &v).is_ok(),
                "CLI 收下的形状，清单层必须也收：{good}"
            );
        }
        for bad in [
            "K=sk-live-123",
            "K=${K:-sk-live-123}",
            "K=Bearer ${K}",
            "K=",
        ] {
            assert!(parse_env(bad).is_err(), "CLI 该拒：{bad}");
            let value = bad.split_once('=').map(|(_, v)| v).unwrap_or(bad);
            assert!(
                henv::secrets::validate_env("K", value).is_err(),
                "CLI 拒的形状，清单层也不该放：{bad}"
            );
        }
    }
}
