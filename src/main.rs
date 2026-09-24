//! henv CLI。Plan A 只有 `diff`；`apply`/`scan`/`doctor` 由 Plan B/C 各自加一个变体。

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "henv",
    version,
    about,
    // `--help` 是人读得到的第二处披露面（第一处是 `examples/env.yaml` 的注释与 lib.rs 的
    // rustdoc）。`degrade:` 在 Plan A 不生效这件事必须在这里也说一句：只写进文档的话，
    // 读终端的人和读代码的人拿到的信息不对称，而「我配了 skip 怎么没静音」正是那种误会的产物。
    // 只披露，不改 `render()` 的产物——那是被 insta golden 逐字节锁住的格式。
    long_about = "只读比对 env.yaml 与本机现状，报告切换 harness 时会失效的能力。\n\n\
只读承诺：全程只输出报告，不写任何配置文件；有 drift 也算成功退出（门禁语义留给 Plan C 的 doctor）。\n\n\
degrade: 政策在 Plan A 只是声明性占位——plugin 尚未拆解，skip/report 两侧都不生效，报告不会因为它而改变。"
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
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Diff { manifest } => {
            let home = henv::home::Home::resolve()?;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            henv::run_diff(&manifest, &home, &mut lock)?;
        }
    }
    Ok(())
}
