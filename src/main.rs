//! henv CLI。`diff` 只读、`apply` 落盘；`scan`/`doctor` 由 Plan C 各自加一个变体。

use clap::{Parser, Subcommand};
use henv::{AssumeYes, Confirm, StdinConfirm};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "henv",
    version,
    about,
    // `--help` 是人读得到的第二处披露面（第一处是 `examples/env.yaml` 的注释与 lib.rs 的
    // rustdoc）。两件事必须在这里说清：`degrade:` 在 Plan A 不生效，以及哪个子命令会写盘
    // ——只写进文档的话，读终端的人和读代码的人拿到的信息不对称，而「我配了 skip 怎么没静音」
    // 与「只想看看怎么把我配置改了」正是那种误会的产物。
    // 只披露，不改 `render()` 的产物——那是被 insta golden 逐字节锁住的格式。
    long_about = "比对 env.yaml 与本机现状，报告切换 harness 时会失效的能力；`apply` 还能把清单要的能力交付到各家。\n\n\
写盘边界：`diff` 全程只输出报告，不写任何配置文件，有 drift 也算成功退出（门禁语义留给 Plan C 的 doctor）；\n\
`apply` 会写——它向各家的 skills 目录真拷贝内容，并把「放了什么」记进 ~/.local/state/henv/provenance.json。\n\
`apply` 落盘前会先渲染计划并要一次 y/N 确认（`--yes` 跳过，供脚本与 CI 用）；\n\
`skills list` 同样只读：它只把仓库取到 henv 自己的缓存目录下看完即删。\n\n\
degrade: 政策在 Plan A 只是声明性占位——plugin 尚未拆解，skip/report 两侧都不生效，报告不会因为它而改变。"
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
    },
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

fn main() -> anyhow::Result<()> {
    let home = henv::home::Home::resolve()?;
    match Cli::parse().command {
        Command::Diff { manifest } => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::run_diff(&manifest, &home, &mut lock)?;
        }
        Command::Apply { manifest, yes } => {
            let mut confirm: Box<dyn Confirm> = if yes {
                Box::new(AssumeYes)
            } else {
                Box::new(StdinConfirm)
            };
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::run_apply(&manifest, &home, &mut lock, confirm.as_mut())?;
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
}
