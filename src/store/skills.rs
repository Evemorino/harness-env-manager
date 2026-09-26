//! 内容层（spec §9.2 第 1 步 + §9.3）：把「清单要什么」与「本机现在是什么」
//! 都翻译成内容指纹，交给 `diff::compute` 做纯函数比较。
//!
//! 本模块是 `store` 里唯一的 I/O 入口，且只在 `compute` **之前**跑——
//! `compute` 之后就是纯函数，`apply` 之后才落盘。

use crate::capability::{CapId, Capability, Kind, SkillSource, Spec};
use crate::home::Home;
use crate::store::hash::hash_dir;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::PathBuf;

/// 清单里一条 skill 的本地内容来源，已解析成真实路径 + 指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedContent {
    /// 源所在目录（`Local` 是清单声明的路径，`AgentsDir` 是 `~/.agents/skills/<name>`）。
    pub path: PathBuf,
    /// 源内容的 [`hash_dir`] 值。
    pub hash: String,
    /// 写进账本的来源标识（§9.3）。
    pub source_id: String,
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
/// 两个空表的 `Default` 让 Plan A 的行为**逐字节**保持——那是既有 306 条测试的前提，
/// 也是本计划能小步落地的基础。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentIndex {
    /// 清单侧：能力 → 本地源内容。
    pub sources: BTreeMap<CapId, ResolvedContent>,
    /// 目标侧：harness 名 → skill 名 → 目标内容。
    pub targets: BTreeMap<String, BTreeMap<String, TargetContent>>,
}

/// §9.2 第 1 步：全局来源解析。
///
/// B1 只解析 `agents-dir` 与 `local`（§9.1 的范围）。解析不出来的条目**不进**表，
/// 于是 `compute` 对它们落回 Plan A 的 `compare_versions`（对 `AgentsDir`/`Local`
/// 是 `Unknown`，对 `Git` 是比 ref）——行为与今天一致。
pub fn resolve_sources(
    desired: &[Capability],
    home: &Home,
) -> anyhow::Result<BTreeMap<CapId, ResolvedContent>> {
    let mut out = BTreeMap::new();
    for cap in desired {
        if cap.id.kind != Kind::Skill {
            continue;
        }
        let Spec::Skill { source, .. } = &cap.spec else {
            continue;
        };
        let (path, source_id) = match source {
            SkillSource::AgentsDir => (
                home.agents_skills().join(&cap.id.name),
                "agents-dir".to_string(),
            ),
            SkillSource::Local { path } => (path.clone(), format!("local:{}", path.display())),
            // Git（B2）与 PluginBundle（B3）：不是今天的事，安静跳过。
            _ => continue,
        };
        // None = 源不存在或断链。没有源就没有内容可交付，也不该让整次运行失败。
        if let Some(hash) = hash_dir(&path)? {
            out.insert(
                cap.id.clone(),
                ResolvedContent {
                    path,
                    hash,
                    source_id,
                },
            );
        }
    }
    Ok(out)
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
        let idx = resolve_sources(&[desired("foo", SkillSource::AgentsDir)], &t.home).unwrap();
        let r = idx
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
        )
        .unwrap();
        let r = idx
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
    fn an_unresolvable_source_is_simply_absent() {
        // 三件事都落在这条上：源目录还不存在、git 源（B2 的范围）、bundle 源（B3）。
        // 「absent」的后果是 compute 落回 compare_versions —— 即 Plan A 的行为。
        let t = TempHome::new();
        let wanted = vec![
            desired("missing", SkillSource::AgentsDir),
            desired(
                "from-git",
                SkillSource::Git {
                    repo: "https://example.invalid/x".into(),
                    path: None,
                    git_ref: "main".into(),
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
        let idx = resolve_sources(&wanted, &t.home).unwrap();
        assert!(idx.is_empty(), "一个都不该解析出来，实际 {:?}", idx.keys());
    }

    #[test]
    fn non_skill_capabilities_are_ignored() {
        let t = TempHome::new();
        let mcp = FakeAdapter::mcp("context7", "npx");
        assert!(resolve_sources(&[mcp], &t.home).unwrap().is_empty());
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
        let src = resolve_sources(&[desired("foo", SkillSource::AgentsDir)], &t.home).unwrap();
        let tgt = target_content(&[actual("foo", t.path(".claude/skills/foo"))]).unwrap();
        let ResolvedContent { hash, .. } = src.values().next().unwrap();
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
