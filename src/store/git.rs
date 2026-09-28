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
    // `.git` 尾巴不参与判断，但它必须紧跟在名字后面（`obra/.git` 不是仓库）。
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

/// `git` 本身**跑不起来**：二进制不在 PATH、cwd 不存在、没有执行权限。
///
/// 与「git 跑了但报错」是两个量级：前者是「这台机器上 henv 不可用」，后者是
/// 「这一个源取不到」。区别的后果是硬错误还是单个条目的失败（§9.3 的失败隔离），
/// 所以它得是个类型，不能靠匹配错误文案。
#[derive(Debug)]
struct GitNotRunnable(String);

impl std::fmt::Display for GitNotRunnable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GitNotRunnable {}

/// 克隆一条命令的 argv。`--` 让 `url` 之后的一切都只能是位置参数：
/// 以 `-` 开头的值不会被 git 当成选项解释（`--upload-pack=<cmd>` 会执行命令）。
///
/// 今天这一层是**加固**而不是补漏：`resolve` 只递一个位置参数（那个还没建出来的
/// 临时目录），git 在源检查上就失败了，`--upload-pack` 根本轮不到执行——实测如此。
/// 但 argv 的形状是会变的，而这行 `--` 让「URL 是数据、不是选项」这件事不再依赖
/// 我们对 git 内部顺序的了解。清单侧的 `manifest` 另有拒绝 `-` 开头的闸（值从哪来）。
fn clone_args<'a>(url: &'a str, dest: &'a str) -> Vec<&'a str> {
    vec!["clone", "--quiet", "--", url, dest]
}

/// 这个错误是不是「git 跑不起来」——也就是**该不该终止整次运行**的判据。
///
/// 调用方（`skills::resolve_sources`）靠它做失败隔离（§9.3）：跑不起来 git 是
/// 这台机器上 henv 不可用，只有这一种错误值得向上冒泡；其余都是某一个源自己的
/// 问题，隔离成那个条目的失败。
pub fn is_not_runnable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<GitNotRunnable>().is_some()
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
        .map_err(|e| {
            anyhow::Error::new(GitNotRunnable(format!("无法执行 git（{args:?}）：{e}")))
        })?;
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

/// 缓存目录是否是一个**可用**的仓库。只看 `.git` 存在是不够的：
/// clone 被打断会留下一个空的 `.git`，之后的 fetch/checkout 会以各种
/// 难懂的方式失败。`a_corrupt_cache_directory_is_reported_not_used` 钉住这条。
fn cache_is_usable(dir: &Path) -> bool {
    dir.join(".git").is_dir() && git_output(dir, &["rev-parse", "--git-dir"]).is_ok()
}

/// 判定模式（§9.2 与 §9.4 的接口）：决定这次解析会不会联网。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceMode {
    /// 只读缓存，不联网（`diff`）。拿不到内容就是「这次判不了」，不是失败。
    Cached,
    /// fetch 并把内容同步到缓存（`apply`）——**唯一**会改盘的调用方。
    Fetch,
}

#[derive(Debug)]
pub struct GitSource {
    /// 仓库里那个子目录在缓存中的路径。
    pub path: PathBuf,
    /// 这次解析到的提交。
    pub sha: String,
    /// `Some(原因)` 表示内容来自没能刷新的缓存（§9.4 的 stale）。
    pub stale: Option<String>,
}

/// 解出这次要用的提交（§9.4）；解不出返回 `None` —— ref 不存在（拼错、或缓存里
/// 还没取到这个 ref），调用方据此判成 deferred / failed。
///
/// **`origin/<ref>` 必须排在裸 `<ref>` 前面**，因为 `git fetch` 只更新远程跟踪
/// 引用、**不移动本地分支**：clone 那天的本地 `main` 会一直停在原处，对着它解出来
/// 的是旧提交——上游前进了也照样报「新鲜」，而 §9.4 存在的全部理由就是防这种静默
/// 过期。非默认分支更极端：本地根本没有这个分支，它只活在 `origin/<ref>` 里。
///
/// 回退到裸 `<ref>` 覆盖 `origin/` 管不到的形状：tag（fetch `--tags` 更新的是
/// `refs/tags/`）、裸提交 sha、以及缓存里恰好只有这个引用的情形。
///
/// 两个模式都优先 `origin/`（不只是 `Fetch`）：`diff`（Cached）读的是同一份缓存，
/// 与 `apply` 解得同一个提交才不会出现「一个说 update、一个说 in-sync」的
/// 自相矛盾报告。
///
/// 这里的 `.ok()` 不会咽掉「git 跑不起来」：能走到这行，说明 `cache_is_usable`
/// 的 `rev-parse` 刚刚成功过、或者我们刚 clone 成功——git 一定是能跑的。
fn resolve_commit(dir: &Path, git_ref: &str) -> Option<String> {
    ["origin/", ""].iter().find_map(|prefix| {
        git_output(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{prefix}{git_ref}^{{commit}}"),
            ],
        )
        .ok()
    })
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
        if let Err(e) = git_output(cache, &clone_args(&url, tmp.to_str().unwrap())) {
            let _ = std::fs::remove_dir_all(&tmp);
            // git 跑不起来（二进制不在、cwd 没了）是这台机器的问题，报出去；
            // git 跑了但克隆失败（源不存在、网络断、凭据不对）只是**这一个源**
            // 取不到——其余源照常交付，这一条由调用方分类成 failed（§9.3）。
            if is_not_runnable(&e) {
                return Err(e);
            }
            return Ok(None);
        }
        std::fs::rename(&tmp, &dir)?;
    } else if mode == SourceMode::Fetch {
        // fetch 失败不是致命的：§9.4 要求「离线也要能用」。
        if let Err(e) = git_output(&dir, &["fetch", "--quiet", "--tags", "--force", "origin"]) {
            stale_reason = Some(format!("未能刷新上游（{}）", summarize(&format!("{e:#}"))));
        }
    }

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
    // 子目录必须真的停在这次 checkout 里面。清单层的 `git_sub_path_is_safe` 只做
    // 字面检查，挡不住仓库里**本来就有**的软链（`skills/etc -> /etc` 字面上合法），
    // 而 `hash_dir` 顶层跟随软链是 §9.3 要的行为——没有这道闸，一个陌生仓库就能
    // 把 `~/.ssh` 当成一个 skill 交付出去。canonicalize 解掉路径上的所有软链，
    // 只放行仍然落在 checkout 内的（仓库内部互链是正当用法，见反向测试）。
    //
    // 闸在这里而不是在 `hash_dir`：那边必须继续跟随顶层软链，否则「目标侧那条
    // 指向源的软链与源同哈希」就不成立，14 条软链会被反复判成待交付。
    let root = std::fs::canonicalize(&dir)?;
    if !std::fs::canonicalize(&content)?.starts_with(&root) {
        anyhow::bail!(
            "仓库 {url} 的 `{}` 指向 checkout 之外，拒绝读取",
            sub.unwrap_or("")
        );
    }
    Ok(Some(GitSource {
        path: content,
        sha,
        stale: stale_reason,
    }))
}

/// 仓库里最新的「version 形」tag（`v1.2.3` / `1.2.3`），按数字段比较大小。
///
/// 给 `henv upgrade` 用。**只认 version 形**：一个都没有时返回 `None`，由调用方提示用户
/// 用 `--to` 指定——绝不回退到默认分支。「ref 就是锁」的前提是在两个明确的点之间移动，
/// 回退到分支 HEAD 会让 ref 随上游漂移，那正是本项目的清单要防的事。
///
/// 走 `git ls-remote`，不 clone：查一次上游只要几百毫秒，而 clone 一个陌生的仓库不是。
pub fn latest_version_tag(repo: &str, cache: &Path) -> anyhow::Result<Option<String>> {
    let url = repo_url(repo);
    // `ls-remote` 不读本地仓库，但需要一个存在的 cwd 才能跑起来。
    std::fs::create_dir_all(cache)?;
    let out = git_output(cache, &["ls-remote", "--tags", "--refs", "--", &url])?;
    let mut best: Option<(Vec<u64>, String)> = None;
    for line in out.lines() {
        let Some(tag) = line.split_whitespace().nth(1) else {
            continue;
        };
        let Some(tag) = tag.strip_prefix("refs/tags/") else {
            continue;
        };
        let Some(parts) = version_parts(tag) else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, _)| parts > *b) {
            best = Some((parts, tag.to_string()));
        }
    }
    Ok(best.map(|(_, tag)| tag))
}

/// tag → 数字段。判据与 [`crate::capability::normalize_version`] 同源（去前导 `v` 后含
/// 小数点），额外要求**每一段都是数字**——要真能比出大小，`v2.0.0-rc.1` 这种不算。
fn version_parts(tag: &str) -> Option<Vec<u64>> {
    let body = crate::capability::normalize_version(tag)?;
    body.split('.').map(|part| part.parse().ok()).collect()
}

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
        // 复用 `clone_args`（而不是手写 argv）：这里的 `repo` 直接来自命令行，
        // 是比清单更靠外的入口，`--` 那层保护同样要有。
        git_output(&scratch, &clone_args(&url, "repo"))?;
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
        let mut stack = vec![root];
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

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

    #[test]
    fn cached_mode_never_touches_the_network_and_reports_none_when_has_no_cache() {
        let t = crate::testutil::TempHome::new();
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
        let t = crate::testutil::TempHome::new();
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
        let t = crate::testutil::TempHome::new();
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

    /// `upgrade` 的选版本规则：只认 version 形，按**数字段**比大小。
    #[test]
    fn latest_version_tag_picks_the_highest_version_shaped_tag() {
        let t = crate::testutil::TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("SKILL.md", "# x");
        repo.commit("init");
        for name in [
            "v1.2.3",
            "v1.9.0",
            "v1.10.0",
            "nightly",
            "b36e082",
            "v2.0.0-rc.1",
        ] {
            repo.tag(name);
        }

        let got = latest_version_tag(&repo.url, &t.home.henv_git_cache()).unwrap();
        assert_eq!(
            got.as_deref(),
            Some("v1.10.0"),
            "1.10.0 要压过 1.9.0（字典序会反）；nightly / sha / 预发布都不是 version 形"
        );
    }

    #[test]
    fn latest_version_tag_is_none_when_nothing_looks_like_a_version() {
        let t = crate::testutil::TempHome::new();
        let cache = t.home.henv_git_cache();

        let repo = t.git_repo("upstream");
        repo.write("SKILL.md", "# x");
        repo.commit("init");
        repo.tag("nightly");
        assert_eq!(latest_version_tag(&repo.url, &cache).unwrap(), None);

        // 一个 tag 都没有的仓库同样返回 None，不是错误——由调用方提示用 `--to`。
        let empty = t.git_repo("empty");
        assert_eq!(latest_version_tag(&empty.url, &cache).unwrap(), None);
    }

    #[test]
    fn a_second_resolve_picks_up_an_upstream_commit() {
        // §9.4 的配方：「重跑时 fetch 后比对」。而 fetch **只更新 `origin/*`，
        // 不移动本地分支**——对着本地 `main` 解出来的是 clone 那天的提交，
        // 上游前进了也照样报「新鲜」，恰恰是 §9.4 存在的全部理由要防的那种静默过期。
        let t = crate::testutil::TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/foo/SKILL.md", "# foo v1");
        let first = repo.commit("init");

        let cache = t.home.henv_git_cache();
        let got = resolve(&repo.url, Some("skills/foo"), "main", &cache, SourceMode::Fetch)
            .unwrap()
            .unwrap();
        assert_eq!(got.sha, first);

        repo.write("skills/foo/SKILL.md", "# foo v2");
        let second = repo.commit("v2");
        assert_ne!(first, second);

        let got = resolve(&repo.url, Some("skills/foo"), "main", &cache, SourceMode::Fetch)
            .unwrap()
            .unwrap();
        assert_eq!(got.sha, second, "上游前进后必须解析到新提交");
        assert_eq!(got.stale, None, "这次 fetch 是成功的，不该标 stale");
        assert_eq!(
            std::fs::read_to_string(got.path.join("SKILL.md")).unwrap(),
            "# foo v2",
            "内容要跟着 checkout 到新提交"
        );

        // `diff`（Cached）读的是同一份缓存，必须与 `apply` 解得同一个提交——
        // 否则一个说 update、一个说 in-sync，报告就自相矛盾了。
        let got = resolve(&repo.url, Some("skills/foo"), "main", &cache, SourceMode::Cached)
            .unwrap()
            .expect("缓存里有这个提交的内容");
        assert_eq!(got.sha, second, "Cached 与 Fetch 要看同一个提交");
    }

    #[test]
    fn a_non_default_branch_resolves_from_what_the_clone_brought_in() {
        // clone 只为默认分支建本地分支；`develop` 只活在 `origin/develop` 里。
        // 对着裸 `develop` 解会**永远**失败，于是清单里的 `ref` / §9.8 的 `--ref`
        // 在非默认分支上形同虚设。
        let t = crate::testutil::TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/foo/SKILL.md", "# foo on main");
        repo.commit("init");
        repo.branch("develop");
        repo.write("skills/foo/SKILL.md", "# foo on develop");
        let dev = repo.commit("develop work");
        // 回到默认分支：clone 跟随源仓库的 HEAD，留着 develop 的话克隆出来的
        // 本地分支就是 develop，这条测试要验的场景就不存在了。
        repo.checkout("main");

        let got = resolve(
            &repo.url,
            Some("skills/foo"),
            "develop",
            &t.home.henv_git_cache(),
            SourceMode::Fetch,
        )
        .expect("这不是错误")
        .expect("非默认分支必须能解析出来");
        assert_eq!(got.sha, dev);
        assert_eq!(
            std::fs::read_to_string(got.path.join("SKILL.md")).unwrap(),
            "# foo on develop"
        );
    }

    #[test]
    fn an_unknown_ref_is_a_failure_not_a_fallback_to_head() {
        // 拼错的 ref 绝不能悄悄退回默认分支：那会把另一个提交的内容当成用户要的。
        let t = crate::testutil::TempHome::new();
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
        let t = crate::testutil::TempHome::new();
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

    #[test]
    fn a_failed_clone_is_a_source_level_failure_not_a_hard_error() {
        // 「git 跑了、但这个源取不到」（路径不存在 / 远端拒绝 / 网络断）是一个**源**的
        // 问题，不是这次运行的问题：其余源照常交付，这一条进 unresolved（§9.3 的失败隔离）。
        // 仓库用相对路径，不联网。
        let t = crate::testutil::TempHome::new();
        let got = resolve(
            "../nowhere/henv-nonexistent",
            None,
            "main",
            &t.home.henv_git_cache(),
            SourceMode::Fetch,
        )
        .expect("单个源取不到不该让整次解析报错");
        assert!(got.is_none(), "取不到就是 None，由调用方分类成 Failed");
    }

    #[test]
    fn spawn_failures_are_distinguishable_from_command_failures() {
        // 上面那条的分界线：**git 本身跑不起来**（二进制不在、cwd 不存在）是另一个
        // 量级的问题——这台机器上 henv 不可用，不该被当成「某个源取不到」咽下去。
        // `a_git_that_cannot_be_started_is_an_error_not_a_silent_skip` 钉住它对用户
        // 可见；这条钉住判定它的那个谓词，否则 `resolve` 的分支只能靠改代码来验。
        let spawn = git_output(Path::new("/nonexistent/henv-cwd"), &["status"]).unwrap_err();
        assert!(is_not_runnable(&spawn), "跑不起来 git 要能被认出来");
        let cmd = git_output(Path::new("."), &["rev-parse", "no-such-ref-henv"]).unwrap_err();
        assert!(!is_not_runnable(&cmd), "git 自己报的错不算跑不起来");
    }

    #[test]
    fn a_sub_directory_that_escapes_the_checkout_is_refused() {
        // 清单层的 `git_sub_path_is_safe` 只做字面检查（拒 `..` 与绝对路径），
        // 挡不住仓库里**本来就有**的软链——`skills/etc -> /etc` 字面上完全合法。
        // 而 `hash_dir` 顶层跟随软链（§9.3 要的），于是仓库外的内容会被哈希、
        // 再被当成一个 skill 拷进 harness 目录：一个陌生仓库就能把 `~/.ssh`
        // 交付出去。所以子目录必须真的落在这次 checkout 里面。
        let t = crate::testutil::TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/foo/SKILL.md", "# foo");
        std::os::unix::fs::symlink("/etc", repo.dir.join("skills/etc")).unwrap();
        std::os::unix::fs::symlink("../..", repo.dir.join("skills/above")).unwrap();
        repo.commit("init");

        for sub in ["skills/etc", "skills/above"] {
            let err = resolve(
                &repo.url,
                Some(sub),
                "main",
                &t.home.henv_git_cache(),
                SourceMode::Fetch,
            )
            .expect_err("指向 checkout 之外的子目录必须被拒");
            assert!(
                format!("{err:#}").contains("checkout 之外"),
                "{sub} 的报错要说清是逃逸，实际：{err:#}"
            );
        }
    }

    #[test]
    fn a_symlink_that_stays_inside_the_checkout_is_still_readable() {
        // 上面那条的反向钉子：仓库内部的软链是正当用法（`skills/foo -> ../shared/foo`），
        // 判据是「有没有出 checkout」，不是「是不是软链」——一刀切会把好仓库也拒了。
        let t = crate::testutil::TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("shared/foo/SKILL.md", "# foo");
        std::fs::create_dir_all(repo.dir.join("skills")).unwrap();
        std::os::unix::fs::symlink("../shared/foo", repo.dir.join("skills/link")).unwrap();
        repo.commit("init");

        let got = resolve(
            &repo.url,
            Some("skills/link"),
            "main",
            &t.home.henv_git_cache(),
            SourceMode::Fetch,
        )
        .expect("仓库内部的软链不该被拒")
        .expect("应当解析出内容");
        assert_eq!(
            std::fs::read_to_string(got.path.join("SKILL.md")).unwrap(),
            "# foo"
        );
    }

    #[test]
    fn the_clone_url_is_a_positional_not_an_option() {
        // 以 `-` 开头的 `repo` 走到这里，唯一的保证是它排在 `--` 之后：git 于是
        // 只会把它当成 URL（连不上 → 这一个源取不到），而不是当成选项。
        //
        // 为什么钉 argv 而不钉效果：效果看不出来。`resolve` 只递一个位置参数，
        // 源检查先失败，`--upload-pack=<脚本>` 那种「选项执行脚本」的路径根本走不到
        // （实测：脚本事后没被跑过）。也就是说今天这行 `--` 是加固、不是补漏，
        // 而加固唯一能被钉住的地方就是它自己。清单层另有一道闸拒绝 `-` 开头的 `repo`。
        let args = clone_args("--upload-pack=/tmp/x.sh", "/tmp/dst");
        let sep = args.iter().position(|a| *a == "--").expect("必须有 `--`");
        let url = args
            .iter()
            .position(|a| *a == "--upload-pack=/tmp/x.sh")
            .expect("URL 要在命令里");
        assert!(
            sep < url,
            "URL 必须排在 `--` 之后，否则 git 会把它当选项，实际 {args:?}"
        );
    }

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
