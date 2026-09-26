//! 内容层（spec §9.2 第 1 步 + §9.3）：把「清单要什么」与「本机现在是什么」
//! 都翻译成内容指纹，交给 `diff::compute` 做纯函数比较。
//!
//! 本模块是 `store` 里唯一的 I/O 入口，且只在 `compute` **之前**跑——
//! `compute` 之后就是纯函数，`apply` 之后才落盘。

use crate::capability::{CapId, Capability, Kind, SkillSource, Spec};
use crate::home::Home;
use crate::store::git;
use crate::store::hash::hash_dir;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::PathBuf;

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

/// 清单里一条 skill 的本地内容来源，已解析成真实路径 + 指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedContent {
    /// 源所在目录（`Local` 是清单声明的路径，`AgentsDir` 是 `~/.agents/skills/<name>`）。
    pub path: PathBuf,
    /// 源内容的 [`hash_dir`] 值。
    pub hash: String,
    /// 写进账本的来源标识（§9.3）。
    pub source_id: String,
    /// `Some(原因)`：内容来自没能刷新的缓存（§9.4 的 stale）。
    pub stale: Option<String>,
}

/// 目标侧一个 skill 的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetContent {
    /// 目标存在且读得到内容（真目录，或能解析的软链——§9.3 不看形态）。
    Has(String),
    /// 盘上有这个条目，但它解不开（断链）。**它没有内容，所以没有东西可被破坏**——
    /// `apply` 会把它换成真拷贝（§9.3：旧的脆弱形态在重跑中自愈）。
    NoContent,
}

/// `compute` 的内容层入参。
///
/// 三个空表的 `Default` 让 Plan A 的行为**逐字节**保持——那是既有 306 条测试的前提，
/// 也是本计划能小步落地的基础。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentIndex {
    /// 清单侧：能力 → 本地源内容。
    pub sources: BTreeMap<CapId, ResolvedContent>,
    /// 目标侧：harness 名 → skill 名 → 目标内容。
    pub targets: BTreeMap<String, BTreeMap<String, TargetContent>>,
    /// 没解析出内容的源，以及为什么（§9.1：报告里必须写清楚）。
    ///
    /// 与 `sources` 是两个集合：一条源要么在 `sources`（有内容可比），要么在
    /// `unresolved`（没有内容，且这本身是一件要报告的事），不会两边都在。
    pub unresolved: BTreeMap<CapId, Unresolved>,
}

/// §9.2 第 1 步：全局来源解析。
///
/// 解析不出来的条目**不进** `sources`，于是 `compute` 对它们落回 Plan A 的
/// `compare_versions`（对 `AgentsDir`/`Local` 是 `Unknown`，对 `Git` 是比 ref）——
/// 行为与今天一致。但拿不到**这件事本身**要进 `unresolved`：一行报告要能回答
/// 「它为什么没被判」，而不是让源安静消失（§9.1）。
///
/// `mode` 决定这次会不会联网（§9.4）：`Cached`（`diff`）只读缓存，缺内容记
/// `Deferred`；`Fetch`（`apply`）会去取，取不到就是 `Failed`。
pub fn resolve_sources(
    desired: &[Capability],
    home: &Home,
    mode: SourceMode,
) -> anyhow::Result<ContentIndex> {
    let mut out = ContentIndex::default();
    for cap in desired {
        let Some(source) = skill_source_of(cap) else {
            continue;
        };
        match source {
            SkillSource::AgentsDir => resolve_one(
                &mut out,
                cap,
                home.agents_skills().join(&cap.id.name),
                "agents-dir".to_string(),
            )?,
            SkillSource::Local { path } => resolve_one(
                &mut out,
                cap,
                path.clone(),
                format!("local:{}", path.display()),
            )?,
            SkillSource::Git {
                repo,
                path,
                git_ref,
            } => resolve_git(&mut out, cap, repo, path.as_deref(), git_ref, home, mode)?,
            // PluginBundle（B3）：不是今天的事，安静跳过——它连「henv 该去哪取」
            // 都还没定义，报出来只会让人以为是自己清单写错了。
            SkillSource::PluginBundle { .. } => {}
            SkillSource::Unknown => {}
        }
    }
    Ok(out)
}

/// 只对 `Spec::Skill` 给出源。非 skill 能力不进内容层（Plan A 的 `continue` 语义）。
fn skill_source_of(cap: &Capability) -> Option<&SkillSource> {
    if cap.id.kind != Kind::Skill {
        return None;
    }
    match &cap.spec {
        Spec::Skill { source, .. } => Some(source),
        _ => None,
    }
}

/// git 源那一段。与 `resolve_one` 同样是「写进 `out`，不往外抛」的形态。
///
/// 唯一会向上冒泡的错误是「git 根本跑不起来」——那是这台机器的问题，
/// 不是某一个源的问题（见 `git::is_not_runnable`）。其余一切（ref 不存在、
/// 目录不存在、读出来逃出 checkout、缓存坏了）都归这一个条目，
/// 理由原样带上（§9.1），其余源照常交付（§9.3 的失败隔离）。
fn resolve_git(
    out: &mut ContentIndex,
    cap: &Capability,
    repo: &str,
    sub: Option<&str>,
    git_ref: &str,
    home: &Home,
    mode: SourceMode,
) -> anyhow::Result<()> {
    let got = match git::resolve(repo, sub, git_ref, &home.henv_git_cache(), mode) {
        Ok(got) => got,
        Err(e) if git::is_not_runnable(&e) => return Err(e),
        Err(e) => {
            out.unresolved
                .insert(cap.id.clone(), Unresolved::Failed(format!("{e:#}")));
            return Ok(());
        }
    };

    let Some(src) = got else {
        out.unresolved.insert(
            cap.id.clone(),
            if mode == SourceMode::Cached {
                Unresolved::Deferred(
                    "diff 不联网；缓存里没有这个源的内容（apply 会去取）".into(),
                )
            } else {
                Unresolved::Failed(format!("取不到 {repo} 的 {git_ref}（缓存里也没有）"))
            },
        );
        return Ok(());
    };

    match hash_dir(&src.path)? {
        Some(hash) => {
            out.sources.insert(
                cap.id.clone(),
                ResolvedContent {
                    path: src.path,
                    hash,
                    source_id: format!("{repo}@{git_ref}@{}", src.sha),
                    stale: src.stale,
                },
            );
        }
        None => {
            out.unresolved.insert(
                cap.id.clone(),
                Unresolved::Failed(format!("仓库里 {} 那份内容读不出来", sub.unwrap_or("."))),
            );
        }
    }
    Ok(())
}

/// `agents-dir` 与 `local` 共用的那一段：算指纹，算不出就记原因。
///
/// `None` = 源不存在或断链。没有源就没有内容可交付，也不该让整次运行失败——
/// 但「没有」有个理由，理由要带上（§9.1）。
fn resolve_one(
    out: &mut ContentIndex,
    cap: &Capability,
    path: PathBuf,
    source_id: String,
) -> anyhow::Result<()> {
    match hash_dir(&path)? {
        Some(hash) => {
            out.sources.insert(
                cap.id.clone(),
                ResolvedContent {
                    path,
                    hash,
                    source_id,
                    stale: None,
                },
            );
        }
        None => {
            out.unresolved.insert(
                cap.id.clone(),
                Unresolved::Failed(format!("源目录不存在或解不开：{}", path.display())),
            );
        }
    }
    Ok(())
}

/// 目标侧内容扫描：从 adapter 扫到的 `Spec::Skill.path` 上算内容。
///
/// §9.3 的「不看它是真目录、软链还是别的」在这里落地——`hash_dir` 顶层跟随软链，
/// 所以一条指向源目录的软链与那个源目录哈希相同，被判 in-sync 而不是待交付。
///
/// adapter 没填 `path` 的能力不进表：`compute` 于是落回 `compare_versions`，
/// 而不是对一个我们根本没读到的位置下内容判决。
pub fn target_content(actual: &[Capability]) -> anyhow::Result<BTreeMap<String, TargetContent>> {
    let mut out = BTreeMap::new();
    for cap in actual {
        if cap.id.kind != Kind::Skill {
            continue;
        }
        let Spec::Skill { path: Some(p), .. } = &cap.spec else {
            continue;
        };
        // 同名取先扫到的那条（同名不同路径是 adapter 侧的怪象，不是这里的判决对象）。
        if let Entry::Vacant(slot) = out.entry(cap.id.name.clone()) {
            let content = match hash_dir(p)? {
                Some(h) => TargetContent::Has(h),
                None => TargetContent::NoContent,
            };
            slot.insert(content);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapId, Origin, SkillSource, Spec, Targets};
    use crate::testutil::{FakeAdapter, TempHome};

    /// 清单侧的一条 skill（与 `lower::lower` 的产物同形：`path: None`）。
    fn desired(name: &str, source: SkillSource) -> Capability {
        Capability {
            id: CapId::new(Kind::Skill, name),
            origin: Origin::Manifest,
            spec: Spec::Skill { source, path: None },
            targets: Targets::All,
        }
    }

    /// 现状侧的一条 skill（带 adapter 填入的路径）。
    fn actual(name: &str, path: PathBuf) -> Capability {
        let mut c = FakeAdapter::cap(
            Kind::Skill,
            name,
            Spec::Skill {
                source: SkillSource::AgentsDir,
                path: Some(path),
            },
        );
        c.origin = Origin::Adopted {
            harness: "claude-code".into(),
        };
        c
    }

    #[test]
    fn agents_dir_source_resolves_to_root_agents_skills() {
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo");
        let idx = resolve_sources(
            &[desired("foo", SkillSource::AgentsDir)],
            &t.home,
            SourceMode::Fetch,
        )
        .unwrap();
        let r = idx
            .sources
            .get(&CapId::new(Kind::Skill, "foo"))
            .expect("must resolve");
        assert_eq!(r.path, t.path(".agents/skills/foo"));
        assert_eq!(r.source_id, "agents-dir");
        assert_eq!(r.hash.len(), 64);
    }

    #[test]
    fn local_source_resolves_to_the_declared_path_and_records_it() {
        let t = TempHome::new();
        let dir = t.path("elsewhere/bar");
        t.write("elsewhere/bar/SKILL.md", "# bar");
        let idx = resolve_sources(
            &[desired("bar", SkillSource::Local { path: dir.clone() })],
            &t.home,
            SourceMode::Fetch,
        )
        .unwrap();
        let r = idx
            .sources
            .get(&CapId::new(Kind::Skill, "bar"))
            .expect("must resolve");
        assert_eq!(r.path, dir);
        assert_eq!(
            r.source_id,
            format!("local:{}", dir.display()),
            "来源标识要能告诉人这份内容从哪个目录来的"
        );
    }

    #[test]
    fn an_unresolvable_source_has_no_content_and_a_named_reason() {
        // 没有源就没有内容可比——这一点没变（`sources` 里一个都不该有）。
        // B2 变的是另一半：拿不到内容的条目不再「安静地缺席」，而是进 `unresolved`。
        // 缺席的后果是 compute 落回 compare_versions（Plan A 的行为）——
        // 那些行不会因此消失，所以「为什么这次判不了」必须有个地方记着。
        let t = TempHome::new();
        // 本地夹具 + 一个不存在的 ref：取不到，但一个字节都不出网（§9.10）。
        let repo = t.git_repo("upstream");
        repo.write("skills/foo/SKILL.md", "# foo");
        repo.commit("init");
        let wanted = vec![
            desired("missing", SkillSource::AgentsDir),
            desired(
                "from-git",
                SkillSource::Git {
                    repo: repo.url.clone(),
                    path: None,
                    git_ref: "no-such-ref".into(),
                },
            ),
            desired(
                "from-bundle",
                SkillSource::PluginBundle {
                    plugin: "superpowers".into(),
                },
            ),
            desired(
                "bad-local",
                SkillSource::Local {
                    path: t.path("nope"),
                },
            ),
        ];
        let idx = resolve_sources(&wanted, &t.home, SourceMode::Fetch).unwrap();
        assert!(
            idx.sources.is_empty(),
            "一个都不该解析出来，实际 {:?}",
            idx.sources.keys()
        );
        for name in ["missing", "bad-local"] {
            assert!(
                matches!(
                    idx.unresolved.get(&CapId::new(Kind::Skill, name)),
                    Some(Unresolved::Failed(_))
                ),
                "{name} 拿不到内容要有名有姓地记着，实际 {:?}",
                idx.unresolved.get(&CapId::new(Kind::Skill, name))
            );
        }
        assert!(
            idx.unresolved
                .get(&CapId::new(Kind::Skill, "from-git"))
                .is_some(),
            "git 源取不到也要登记"
        );
        assert!(
            !idx.unresolved
                .contains_key(&CapId::new(Kind::Skill, "from-bundle")),
            "bundle 源是 B3 的范围，今天不该出现在报告里"
        );
    }

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

        let r = idx
            .sources
            .get(&CapId::new(Kind::Skill, "foo"))
            .expect("应当解析出来");
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
        let r = idx
            .sources
            .get(&CapId::new(Kind::Skill, "foo"))
            .expect("缓存还在，内容要给");
        assert!(r.stale.is_some());
        assert!(!idx.unresolved.contains_key(&CapId::new(Kind::Skill, "foo")));
    }

    #[test]
    fn a_source_level_git_error_does_not_take_the_whole_resolution_down() {
        // §9.3 的失败隔离，第二层：一个源被**拒绝**（这里是仓库里的软链逃出
        // checkout）不是这次运行的问题，是这一个源的问题。`git::resolve(..)?`
        // 会让它终止整次解析——其余条目连报告都进不了，用户看到的是「henv 挂了」
        // 而不是「这个仓库的这一条读不了」。理由必须原样留下（§9.1），
        // 否则「拒绝读取」会被误报成「取不到」。
        let t = TempHome::new();
        let repo = t.git_repo("upstream");
        repo.write("skills/ok/SKILL.md", "# ok");
        std::os::unix::fs::symlink("/etc", repo.dir.join("skills/etc")).unwrap();
        repo.commit("init");
        t.write("elsewhere/bar/SKILL.md", "# bar");

        let idx = resolve_sources(
            &[
                desired(
                    "good",
                    SkillSource::Local {
                        path: t.path("elsewhere/bar"),
                    },
                ),
                desired(
                    "evil",
                    SkillSource::Git {
                        repo: repo.url.clone(),
                        path: Some("skills/etc".into()),
                        git_ref: "main".into(),
                    },
                ),
            ],
            &t.home,
            SourceMode::Fetch,
        )
        .expect("一个源被拒不该让整次解析报错");

        assert!(
            idx.sources.contains_key(&CapId::new(Kind::Skill, "good")),
            "好源必须照常解析出来，实际 {:?}",
            idx.sources.keys()
        );
        match idx.unresolved.get(&CapId::new(Kind::Skill, "evil")) {
            Some(Unresolved::Failed(reason)) => assert!(
                reason.contains("checkout 之外"),
                "拒绝的理由要原样带到报告里，实际：{reason}"
            ),
            other => panic!("应当是 Failed，实际 {other:?}"),
        }
    }

    #[test]
    fn non_skill_capabilities_are_ignored() {
        let t = TempHome::new();
        let mcp = FakeAdapter::mcp("context7", "npx");
        assert!(
            resolve_sources(&[mcp], &t.home, SourceMode::Fetch)
                .unwrap()
                .sources
                .is_empty()
        );
    }

    #[test]
    fn target_content_reads_the_copy_or_reports_it_has_none() {
        let t = TempHome::new();
        t.write(".claude/skills/real/SKILL.md", "# real");
        t.symlink(".claude/skills/ghost", "../../.agents/skills/ghost");
        t.mkdir(".codex/skills");
        let caps = vec![
            actual("real", t.path(".claude/skills/real")),
            actual("ghost", t.path(".claude/skills/ghost")),
        ];
        let map = target_content(&caps).unwrap();
        assert!(matches!(map.get("real"), Some(TargetContent::Has(h)) if h.len() == 64));
        assert_eq!(
            map.get("ghost"),
            Some(&TargetContent::NoContent),
            "盘上有条目但内容解不开 → NoContent"
        );
        assert!(!map.contains_key("absent"), "没扫到就是没扫到");
    }

    #[test]
    fn a_skill_without_a_path_is_not_in_the_target_map() {
        // adapter 没填路径 = 不知道它在哪。凭空补一个猜测路径会让 compute 对一个
        // 根本没读到的位置下内容判决——这正是要防的假判定。
        let mut c = actual("no-path", PathBuf::from("/irrelevant"));
        c.spec = Spec::Skill {
            source: SkillSource::AgentsDir,
            path: None,
        };
        assert!(target_content(&[c]).unwrap().is_empty());
    }

    #[test]
    fn a_link_to_the_source_hashes_the_same_as_the_source() {
        // 这一条是 §9.3 那 14 条软链不被动的前提，跨 resolve/target 两侧验证。
        let t = TempHome::new();
        t.write(".agents/skills/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../.agents/skills/foo");
        let src = resolve_sources(
            &[desired("foo", SkillSource::AgentsDir)],
            &t.home,
            SourceMode::Fetch,
        )
        .unwrap();
        let tgt = target_content(&[actual("foo", t.path(".claude/skills/foo"))]).unwrap();
        let ResolvedContent { hash, .. } = src.sources.values().next().unwrap();
        assert_eq!(
            tgt.get("foo"),
            Some(&TargetContent::Has(hash.clone())),
            "源与指向源的软链必须同哈希，否则那 14 条软链会被反复判成待交付"
        );
    }

    #[test]
    fn the_index_default_is_two_empty_tables() {
        // compute 的 Plan A 行为靠这个 Default 复现，它是既有 306 条测试的前提。
        let idx = ContentIndex::default();
        assert!(idx.sources.is_empty());
        assert!(idx.targets.is_empty());
    }
}
