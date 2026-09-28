//! 构建时把「这次构建出自哪个提交」写进二进制（`cargo:rustc-env`）。
//!
//! `--version` 只报 `0.1.0` 回答不了唯一要紧的那个问题：**我现在跑的是不是刚编译的那份**。
//! 所以版本串带上一段 git 描述：`0.1.0 (a3bc156-dirty 2026-09-28)`。
//!
//! 三件事各自失败时都不该拦住构建——没有 git、不在仓库里、或者 `date` 跑不了，都退化成
//! `unknown`：一个开发工具因为环境缺东西而编译不过，比它的版本串不够漂亮糟得多。

use std::path::Path;
use std::process::Command;

/// 跑一条命令，成功就返回 trim 过的 stdout；失败（跑不起来或非零退出）返回 `None`。
fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    // 只在**会影响这段描述**的输入变化时重跑：提交、分支指针、以及本文件。
    // `.git` 在发布 tarball 里不存在，所以逐个判断存在性——对不存在的路径声明
    // `rerun-if-changed` 只会让 cargo 反复重跑这个脚本。
    for watch in ["build.rs", ".git/HEAD", ".git/refs"] {
        if Path::new(watch).exists() {
            println!("cargo:rerun-if-changed={watch}");
        }
    }

    let sha = output("git", &["rev-parse", "--short", "HEAD"])
        .unwrap_or_else(|| String::from("unknown"));
    // 只看**已跟踪**文件的改动：`--porcelain` 默认把 untracked 也算脏，那会让任何一次
    // 「新建了一个还没 add 的文件」都标成 dirty，这个标记就没信息量了。
    // 空输出与「git 跑不起来」都落到干净的这一支——宁可不标脏，也不要假脏。
    let dirty = if output("git", &["status", "--porcelain", "--untracked-files=no"]).is_some() {
        "-dirty"
    } else {
        ""
    };
    let date = output("date", &["-u", "+%Y-%m-%d"]).unwrap_or_else(|| String::from("unknown"));
    println!("cargo:rustc-env=HENV_BUILD={sha}{dirty} {date}");
}
