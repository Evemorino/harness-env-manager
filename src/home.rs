use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let raw = root.into();
        // 目录存在时归一化：macOS 上 /var 是 /private/var 的 symlink，而
        // skills_dir 用 fs::canonicalize 解析 skill 路径。两边形态不一致会让
        // classify 的前缀比较永远失配。目录不存在时保留原值（错误由上层报）。
        let root = std::fs::canonicalize(&raw).unwrap_or(raw);
        Self { root }
    }

    /// $HENV_HOME 优先，其次 $HOME。两者都缺或为空 → 错误。
    pub fn resolve() -> anyhow::Result<Self> {
        Self::from_env(std::env::var_os("HENV_HOME"), std::env::var_os("HOME"))
    }

    fn from_env(
        henv_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
    ) -> anyhow::Result<Self> {
        let raw = henv_home
            .or(home)
            .ok_or_else(|| anyhow::anyhow!("neither HENV_HOME nor HOME is set"))?;
        if raw.is_empty() {
            anyhow::bail!("HENV_HOME/HOME is set but empty");
        }
        Ok(Self::new(PathBuf::from(raw)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn agents_skills(&self) -> PathBuf {
        self.root.join(".agents").join("skills")
    }
    pub fn claude_dir(&self) -> PathBuf {
        self.root.join(".claude")
    }
    pub fn claude_json(&self) -> PathBuf {
        self.root.join(".claude.json")
    }
    pub fn claude_settings(&self) -> PathBuf {
        self.root.join(".claude").join("settings.json")
    }
    pub fn claude_installed_plugins(&self) -> PathBuf {
        self.root.join(".claude").join("plugins").join("installed_plugins.json")
    }
    pub fn claude_skills(&self) -> PathBuf {
        self.root.join(".claude").join("skills")
    }
    pub fn codex_dir(&self) -> PathBuf {
        self.root.join(".codex")
    }
    pub fn codex_config(&self) -> PathBuf {
        self.root.join(".codex").join("config.toml")
    }
    pub fn codex_skills(&self) -> PathBuf {
        self.root.join(".codex").join("skills")
    }
    pub fn kimi_dir(&self) -> PathBuf {
        self.root.join(".kimi-code")
    }
    pub fn kimi_mcp(&self) -> PathBuf {
        self.root.join(".kimi-code").join("mcp.json")
    }
    pub fn kimi_installed_plugins(&self) -> PathBuf {
        self.root.join(".kimi-code").join("plugins").join("installed.json")
    }
    pub fn harness_dirs(&self) -> Vec<PathBuf> {
        vec![self.claude_dir(), self.codex_dir(), self.kimi_dir()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_harness_paths_under_root() {
        let h = Home::new("/fake/root");
        assert_eq!(h.root(), Path::new("/fake/root"));
        assert_eq!(h.agents_skills(), PathBuf::from("/fake/root/.agents/skills"));
        assert_eq!(h.claude_dir(), PathBuf::from("/fake/root/.claude"));
        assert_eq!(h.claude_json(), PathBuf::from("/fake/root/.claude.json"));
        assert_eq!(h.claude_settings(), PathBuf::from("/fake/root/.claude/settings.json"));
        assert_eq!(
            h.claude_installed_plugins(),
            PathBuf::from("/fake/root/.claude/plugins/installed_plugins.json")
        );
        assert_eq!(h.claude_skills(), PathBuf::from("/fake/root/.claude/skills"));
        assert_eq!(h.codex_dir(), PathBuf::from("/fake/root/.codex"));
        assert_eq!(h.codex_config(), PathBuf::from("/fake/root/.codex/config.toml"));
        assert_eq!(h.codex_skills(), PathBuf::from("/fake/root/.codex/skills"));
        assert_eq!(h.kimi_dir(), PathBuf::from("/fake/root/.kimi-code"));
        assert_eq!(h.kimi_mcp(), PathBuf::from("/fake/root/.kimi-code/mcp.json"));
        assert_eq!(
            h.kimi_installed_plugins(),
            PathBuf::from("/fake/root/.kimi-code/plugins/installed.json")
        );
        assert_eq!(h.harness_dirs().len(), 3);
    }

    #[test]
    fn resolve_prefers_henv_home_over_home() {
        // 不改进程环境（测试并行会互相踩），直接验证优先级函数
        assert_eq!(
            Home::from_env(Some("/a".into()), Some("/b".into())).unwrap().root(),
            Path::new("/a")
        );
        assert_eq!(
            Home::from_env(None, Some("/b".into())).unwrap().root(),
            Path::new("/b")
        );
        assert!(Home::from_env(None, None).is_err());
        assert!(Home::from_env(Some("".into()), Some("/b".into())).is_err());
    }

    #[test]
    fn root_is_canonicalized_when_it_exists() {
        // macOS 上 /var 是 /private/var 的 symlink，tempfile 给的是 /var/... 而
        // fs::canonicalize 给的是 /private/var/...。两边必须落到同一形态，否则
        // skills_dir::classify 的前缀比较永远失配（真实踩过的坑）。
        let dir = tempfile::TempDir::new().unwrap();
        let h = Home::new(dir.path());
        assert_eq!(h.root(), std::fs::canonicalize(dir.path()).unwrap().as_path());
        assert!(h.agents_skills().starts_with(h.root()));
    }

    #[test]
    fn root_is_kept_verbatim_when_it_does_not_exist() {
        let h = Home::new("/definitely/not/here");
        assert_eq!(h.root(), Path::new("/definitely/not/here"));
    }
}
