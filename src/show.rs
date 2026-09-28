//! `henv show`：盘点**一个仓库**里有什么（§9.8）。
//!
//! 与另外两个「查看」命令的分工是正交的：
//!
//! - `status` 读**本机**（我装了什么）；
//! - `browse` 读**各家的市场清单**（还能装什么 plugin）；
//! - `show` 读**一个 git 仓库**（这个仓库提供什么）。
//!
//! 它存在的理由是：远程仓库不止有 skill。实测 `obra/superpowers` 一个仓库里同时有 14 个
//! skill、6 份 per-harness `plugin.json`、以及 `.agents/`、`.pi/` 这些 henv 不解析的目录。
//! 只看 skill（`skills list`）会漏掉"这个仓库其实是个 plugin bundle"这个事实。
//!
//! 只读：clone 到临时目录、扫完即删，不碰 harness 目录也不碰账本。

use crate::home::Home;
use crate::store::git::{ManifestFound, RepoInspection};
use std::io::Write;

pub fn run_show(
    home: &Home,
    out: &mut dyn Write,
    repo: &str,
    git_ref: Option<&str>,
    sub: Option<&str>,
) -> anyhow::Result<()> {
    let found = crate::store::git::inspect(repo, sub, git_ref, &home.henv_git_cache())?;
    render(out, repo, &found);
    Ok(())
}

fn render(out: &mut dyn Write, repo: &str, found: &RepoInspection) {
    let _ = writeln!(out, "{repo}");

    if found.skills.is_empty() {
        let _ = writeln!(out, "\nskills\n  （没有含 SKILL.md 的目录）");
    } else {
        let _ = writeln!(out, "\nskills ({})", found.skills.len());
        let width = found
            .skills
            .iter()
            .map(|(n, _)| n.chars().count())
            .max()
            .unwrap_or(0);
        for (name, path) in &found.skills {
            let _ = writeln!(out, "  {name:<width$}  {path}");
        }
    }

    if !found.manifests.is_empty() {
        let _ = writeln!(out, "\nplugin manifests ({})", found.manifests.len());
        let width = found
            .manifests
            .iter()
            .map(|m| m.path.chars().count())
            .max()
            .unwrap_or(0);
        for m in &found.manifests {
            let _ = writeln!(out, "  {:<width$}  {}{}", m.path, describe(m), declared(m));
        }
    }

    if !found.other_dirs.is_empty() {
        let _ = writeln!(out, "\n其它 harness 目录（henv 不解析）");
        let _ = writeln!(out, "  {}", found.other_dirs.join("  "));
    }

    let _ = writeln!(out, "\nMCP");
    if found.mcp_files.is_empty() {
        let _ = writeln!(out, "  （没有 .mcp.json）");
    } else {
        for f in &found.mcp_files {
            let _ = writeln!(out, "  {f}");
        }
    }
}

/// `claude-code  superpowers 6.4.1` —— 认不出哪家就如实说，不假装。
fn describe(m: &ManifestFound) -> String {
    let harness = m.harness.unwrap_or("（其它 harness）");
    match (m.name.as_deref(), m.version.as_deref()) {
        (Some(name), Some(v)) => format!("{harness}  {name} {v}"),
        (Some(name), None) => format!("{harness}  {name}"),
        (None, _) => harness.to_string(),
    }
}

/// manifest 里声明了哪些能力段。没有就什么都不加——空括号只是噪音。
fn declared(m: &ManifestFound) -> String {
    if m.abilities.is_empty() {
        String::new()
    } else {
        format!("  [声明: {}]", m.abilities.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::git::ManifestFound;
    use crate::testutil::TempHome;

    fn render_to_string(found: &RepoInspection) -> String {
        let mut out = Vec::new();
        render(&mut out, "owner/repo", found);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn renders_skills_manifests_and_mcp_sections() {
        let found = RepoInspection {
            skills: vec![("tdd".into(), "skills/tdd".into())],
            manifests: vec![ManifestFound {
                path: ".claude-plugin/plugin.json".into(),
                harness: Some("claude-code"),
                name: Some("superpowers".into()),
                version: Some("6.4.1".into()),
                abilities: vec!["hooks".into()],
            }],
            other_dirs: vec![".pi".into()],
            mcp_files: vec![".mcp.json".into()],
        };
        let s = render_to_string(&found);

        assert!(s.contains("skills (1)"), "{s}");
        assert!(s.contains("tdd  skills/tdd"), "{s}");
        assert!(s.contains("plugin manifests (1)"), "{s}");
        assert!(s.contains("claude-code  superpowers 6.4.1"), "{s}");
        assert!(s.contains("[声明: hooks]"), "{s}");
        assert!(s.contains(".pi"), "{s}");
        assert!(s.contains("  .mcp.json"), "{s}");
    }

    /// 认不出的 manifest 也要列出来（`show` 回答的是「仓库里有什么」，不是「henv 能拿它做什么」）。
    #[test]
    fn an_unknown_harness_manifest_is_still_listed() {
        let found = RepoInspection {
            manifests: vec![ManifestFound {
                path: ".cursor-plugin/plugin.json".into(),
                harness: None,
                name: Some("superpowers".into()),
                version: None,
                abilities: Vec::new(),
            }],
            ..Default::default()
        };
        let s = render_to_string(&found);
        assert!(s.contains(".cursor-plugin/plugin.json"), "{s}");
        assert!(s.contains("（其它 harness）"), "{s}");
    }

    #[test]
    fn an_empty_repo_says_so_instead_of_printing_nothing() {
        let s = render_to_string(&RepoInspection::default());
        assert!(s.contains("（没有含 SKILL.md 的目录）"), "{s}");
        assert!(s.contains("（没有 .mcp.json）"), "{s}");
        assert!(!s.contains("plugin manifests"), "没有就不该有这一段：{s}");
    }

    /// 端到端：一个真的本地仓库（用 `file://` 夹具），确认 inspect 扫得出来。
    #[test]
    fn a_real_repo_is_inspected_end_to_end() {
        let t = TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/tdd/SKILL.md", "# tdd");
        repo.write(
            ".claude-plugin/plugin.json",
            r#"{"name":"demo","version":"1.2.3","hooks":["x"]}"#,
        );
        repo.write(".mcp.json", r#"{"mcpServers":{}}"#);
        repo.commit("init");

        let mut out = Vec::new();
        run_show(&t.home, &mut out, &repo.url, None, None).unwrap();
        let s = String::from_utf8(out).unwrap();

        assert!(s.contains("skills (1)"), "{s}");
        assert!(s.contains("claude-code  demo 1.2.3"), "{s}");
        assert!(s.contains("[声明: hooks]"), "{s}");
        assert!(s.contains(".mcp.json"), "{s}");
    }
}
