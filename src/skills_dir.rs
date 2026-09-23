//! skills 目录扫描。
//!
//! spec §2.1 第 4 条：`~/.agents/skills` 共享约定只部分成立——实测存在反向链接、
//! 直接指向 plugin 缓存的链接、以及不属于任何共享库的真实目录。本模块把这些形态统一成
//! 带来源分类的 `SkillEntry`，供 adapter 归一化与 doctor 断链报告使用。

use crate::home::Home;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// 一个 skill 条目的内容实际归属哪里。drift 判定依赖它：
/// `PluginCache` 内的 skill 是 plugin 的分发产物，不算 drift。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillStore {
    /// 在 ~/.agents/skills 共享库内
    Shared,
    /// 在某家 harness 自己的目录内
    HarnessLocal,
    /// 在某个 plugin 缓存 / managed 目录内
    PluginCache,
    /// 无法归类，或链接断裂
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    /// 扫描到的路径本身（目录或 symlink）
    pub path: PathBuf,
    /// `read_link` 的原始值；非 symlink 为 None
    pub link_target: Option<PathBuf>,
    /// 多跳解析后的真实路径；断链或成环为 None
    pub resolved: Option<PathBuf>,
    pub store: SkillStore,
}

impl SkillEntry {
    pub fn is_broken(&self) -> bool {
        self.link_target.is_some() && self.resolved.is_none()
    }
}

/// 扫描一个 skills 目录。目录不存在返回空。结果按 name 排序（确定性）。
pub fn read_skill_entries(dir: &Path, home: &Home) -> Vec<SkillEntry> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out: Vec<SkillEntry> = Vec::new();
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue; // .DS_Store 之类
        }

        let path = item.path();
        let link_target = fs::read_link(&path).ok();
        let file_type = match item.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        // 只收目录与 symlink（symlink 可能指向目录）；普通文件忽略。
        // 注意 file_type 不跟随 symlink，故断链也走到这里，由 canonicalize 判死。
        if !file_type.is_dir() && link_target.is_none() {
            continue;
        }

        // canonicalize 一次性解多跳；断链与成环都返回 Err（OS 报 ELOOP）
        let resolved = fs::canonicalize(&path).ok();
        let store = match &resolved {
            Some(r) if r.is_dir() => classify(r, home),
            _ => SkillStore::Other,
        };

        out.push(SkillEntry {
            name,
            path,
            link_target,
            resolved,
            store,
        });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 判定一个已解析的真实路径归属哪个库。
pub fn classify(resolved: &Path, home: &Home) -> SkillStore {
    if resolved.starts_with(home.agents_skills()) {
        return SkillStore::Shared;
    }
    if is_in_plugin_cache(resolved) {
        return SkillStore::PluginCache;
    }
    if home.harness_dirs().iter().any(|d| resolved.starts_with(d)) {
        return SkillStore::HarnessLocal;
    }
    SkillStore::Other
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn is_in_plugin_cache(path: &Path) -> bool {
    let comps = normal_components(path);
    comps.iter().any(|c| c == "plugins") && comps.iter().any(|c| c == "cache" || c == "managed")
}

/// 从一个 skill 路径反推它所属的 plugin 名。覆盖两种实测布局：
///   `.../plugins/cache/<marketplace>/<name>/<version>/skills/<skill>` → `<name>`
///   `.../plugins/managed/<id>/skills/<skill>`                        → `<id>`
pub fn plugin_from_path(path: &Path) -> Option<String> {
    let comps = normal_components(path);
    let i = comps.iter().position(|c| c == "cache" || c == "managed")?;
    if !comps[..i].iter().any(|c| c == "plugins") {
        return None;
    }
    if comps[i] == "managed" {
        comps.get(i + 1).cloned()
    } else {
        comps.get(i + 2).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    #[test]
    fn empty_or_missing_dir_yields_nothing() {
        let t = TempHome::new();
        assert!(read_skill_entries(&t.path(".claude/skills"), &t.home).is_empty());
        t.mkdir(".claude/skills");
        assert!(read_skill_entries(&t.path(".claude/skills"), &t.home).is_empty());
    }

    #[test]
    fn skips_dotfiles_like_ds_store() {
        let t = TempHome::new();
        t.write(".claude/skills/.DS_Store", "junk")
            .mkdir(".claude/skills/real");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "real");
    }

    #[test]
    fn real_dir_is_harness_local() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/trim-cot-leakage");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got[0].link_target, None);
        assert!(got[0].resolved.is_some());
        assert_eq!(got[0].store, SkillStore::HarnessLocal);
        assert!(!got[0].is_broken());
    }

    #[test]
    fn relative_symlink_into_shared_store_is_shared() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/dingtalk-mail").symlink(
            ".claude/skills/dingtalk-mail",
            "../../.agents/skills/dingtalk-mail",
        );
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(
            got[0].link_target,
            Some(PathBuf::from("../../.agents/skills/dingtalk-mail"))
        );
        assert_eq!(got[0].store, SkillStore::Shared);
        assert!(!got[0].is_broken());
    }

    #[test]
    fn absolute_symlink_back_into_harness_is_harness_local() {
        // 实测 ~/.agents/skills/mcp-builder -> ~/.claude/skills/mcp-builder（反向）
        let t = TempHome::new();
        t.mkdir(".claude/skills/mcp-builder");
        let target = t.path(".claude/skills/mcp-builder");
        t.symlink(".agents/skills/mcp-builder", &target.to_string_lossy());
        let got = read_skill_entries(&t.path(".agents/skills"), &t.home);
        assert_eq!(got[0].name, "mcp-builder");
        assert_eq!(got[0].store, SkillStore::HarnessLocal);
    }

    #[test]
    fn symlink_into_plugin_cache_is_plugin_cache_and_yields_plugin_name() {
        // 实测 ~/.codex/skills/brainstorming ->
        //   ~/.claude/plugins/cache/<mp>/superpowers/<sha>/skills/brainstorming
        let t = TempHome::new();
        t.mkdir(
            ".claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming",
        );
        let target = t.path(
            ".claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming",
        );
        t.symlink(".codex/skills/brainstorming", &target.to_string_lossy());

        let got = read_skill_entries(&t.path(".codex/skills"), &t.home);
        assert_eq!(got[0].store, SkillStore::PluginCache);
        let resolved = got[0].resolved.clone().expect("resolves");
        assert_eq!(plugin_from_path(&resolved).as_deref(), Some("superpowers"));
    }

    #[test]
    fn plugin_from_path_handles_managed_layout() {
        // kimi: plugins/managed/<id>/skills/<name>
        let p = PathBuf::from("/h/.kimi-code/plugins/managed/superpowers/skills/tdd");
        assert_eq!(plugin_from_path(&p).as_deref(), Some("superpowers"));
    }

    #[test]
    fn plugin_from_path_returns_none_outside_caches() {
        assert_eq!(
            plugin_from_path(&PathBuf::from("/h/.agents/skills/foo")),
            None
        );
        assert_eq!(
            plugin_from_path(&PathBuf::from("/h/cache/foo/bar")),
            None,
            "cache without plugins/ is not a plugin cache"
        );
    }

    #[test]
    fn broken_symlink_is_reported_not_skipped() {
        let t = TempHome::new();
        t.mkdir(".claude/skills")
            .symlink(".claude/skills/gone", "../../.agents/skills/does-not-exist");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(
            got.len(),
            1,
            "broken links must surface for doctor, not vanish"
        );
        assert!(got[0].is_broken());
        assert_eq!(got[0].resolved, None);
        assert_eq!(got[0].store, SkillStore::Other);
    }

    #[test]
    fn symlink_cycle_is_reported_as_broken_not_infinite_loop() {
        let t = TempHome::new();
        t.mkdir(".agents/skills")
            .symlink(".agents/skills/a", "b")
            .symlink(".agents/skills/b", "a");
        let got = read_skill_entries(&t.path(".agents/skills"), &t.home);
        assert_eq!(got.len(), 2);
        assert!(
            got.iter().all(|e| e.is_broken()),
            "cycle must not hang or resolve"
        );
    }

    #[test]
    fn results_are_sorted_by_name() {
        let t = TempHome::new();
        t.mkdir(".claude/skills/zeta")
            .mkdir(".claude/skills/alpha")
            .mkdir(".claude/skills/mid");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn plain_files_are_ignored_only_dirs_and_symlinks_count() {
        let t = TempHome::new();
        t.write(".claude/skills/notes.md", "x")
            .mkdir(".claude/skills/real");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "real");
    }
}
