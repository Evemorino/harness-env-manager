use anyhow::Context;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// 测试路径专用的不可失败构造器。**会吞错**：对 canonicalize 的任何失败都静默
    /// 回退原路径，不区分 kind。`TempHome` 只会传 `TempDir`，它们总能 canonicalize
    /// 成功，所以测试里用它没问题；**生产入口不要用这里**，走 `try_new`／`resolve`。
    ///
    /// 归一化本身仍走 [`Home::try_canonicalize`]（本文件里 `fs::canonicalize` 的唯一调用
    /// 点），只是把它的错误也一律吃掉——「吞错」是这里的契约，不是那条归一化规则的。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let raw = root.into();
        // 归一化的原因：macOS 上 /var 是 /private/var 的 symlink，而 skills_dir 用
        // fs::canonicalize 解析 skill 路径。两边形态不一致会让 classify 的前缀比较
        // 永远失配。
        let root = Self::try_canonicalize(&raw).unwrap_or(raw);
        Self { root }
    }

    /// 生产入口：归一化 root，并且**只**在「确实不存在」（`NotFound`）时保留原值，
    /// 把缺失本身留给上层去报。其余错误 kind（`PermissionDenied`、`ELOOP`、
    /// `ENOTDIR`、`ENAMETOOLONG` ……）都带着路径冒出去。
    ///
    /// 之所以不能一律静默回退：那会让 `Home` 揣着一个非归一化的 root 活下去，
    /// `skills_dir::classify` 的 `starts_with` 前缀比较永远失配，henv 于是报
    /// 「没有安装任何 skill」而不是启动即失败。
    ///
    /// 归一化规则本身在 [`Home::try_canonicalize`]，与 root 派生前缀（`skills_dir` 的
    /// `HomePrefixes`）共用同一份实现——同一语义不允许存在两份、两套契约。
    pub fn try_new(root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let raw = root.into();
        let canonical = Self::try_canonicalize(&raw)
            .with_context(|| format!("failed to canonicalize home root {}", raw.display()))?;
        Ok(Self { root: canonical })
    }

    /// root 与 root 派生路径**共用**的归一化规则，是整个 crate 里「什么时候允许保留
    /// 字面路径」的唯一出处：
    ///
    /// - `Ok(canonical)`：能解就解到与 `fs::canonicalize` 同一形态。
    /// - 只有 `NotFound`（那一层还没建）才原样返回字面路径——「还没建的库」继续按
    ///   字面前缀匹配，行为不变，且这条路径**不**是错误。
    /// - 其余 kind（`PermissionDenied`/`ENOTDIR`/`ELOOP`/…）原样上抛，由调用方补自己
    ///   的上下文：静默回退会让比较两侧退回不同形态，归类于是悄悄错成 `Other`。
    pub(crate) fn try_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
        match std::fs::canonicalize(path) {
            Ok(canonical) => Ok(canonical),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
            Err(e) => Err(e),
        }
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
        Self::try_new(PathBuf::from(raw))
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
        self.root
            .join(".claude")
            .join("plugins")
            .join("installed_plugins.json")
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
        self.root
            .join(".kimi-code")
            .join("plugins")
            .join("installed.json")
    }
    pub fn harness_dirs(&self) -> Vec<PathBuf> {
        vec![self.claude_dir(), self.codex_dir(), self.kimi_dir()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    #[test]
    fn joins_harness_paths_under_root() {
        let h = Home::new("/fake/root");
        assert_eq!(h.root(), Path::new("/fake/root"));
        assert_eq!(
            h.agents_skills(),
            PathBuf::from("/fake/root/.agents/skills")
        );
        assert_eq!(h.claude_dir(), PathBuf::from("/fake/root/.claude"));
        assert_eq!(h.claude_json(), PathBuf::from("/fake/root/.claude.json"));
        assert_eq!(
            h.claude_settings(),
            PathBuf::from("/fake/root/.claude/settings.json")
        );
        assert_eq!(
            h.claude_installed_plugins(),
            PathBuf::from("/fake/root/.claude/plugins/installed_plugins.json")
        );
        assert_eq!(
            h.claude_skills(),
            PathBuf::from("/fake/root/.claude/skills")
        );
        assert_eq!(h.codex_dir(), PathBuf::from("/fake/root/.codex"));
        assert_eq!(
            h.codex_config(),
            PathBuf::from("/fake/root/.codex/config.toml")
        );
        assert_eq!(h.codex_skills(), PathBuf::from("/fake/root/.codex/skills"));
        assert_eq!(h.kimi_dir(), PathBuf::from("/fake/root/.kimi-code"));
        assert_eq!(
            h.kimi_mcp(),
            PathBuf::from("/fake/root/.kimi-code/mcp.json")
        );
        assert_eq!(
            h.kimi_installed_plugins(),
            PathBuf::from("/fake/root/.kimi-code/plugins/installed.json")
        );
        assert_eq!(h.harness_dirs().len(), 3);
        // 契约是**有序**三元组：只断言长度的话，换掉任意一个目录（比如把 kimi_dir
        // 换成 agents_skills）或打乱顺序都能让套件继续绿。
        assert_eq!(
            h.harness_dirs(),
            vec![h.claude_dir(), h.codex_dir(), h.kimi_dir()]
        );
    }

    #[test]
    fn resolve_prefers_henv_home_over_home() {
        // 不改进程环境（测试并行会互相踩），直接验证优先级函数
        assert_eq!(
            Home::from_env(Some("/a".into()), Some("/b".into()))
                .unwrap()
                .root(),
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
        assert_eq!(
            h.root(),
            std::fs::canonicalize(dir.path()).unwrap().as_path()
        );
        assert!(h.agents_skills().starts_with(h.root()));
    }

    #[test]
    fn root_is_kept_verbatim_when_it_does_not_exist() {
        let h = Home::new("/definitely/not/here");
        assert_eq!(h.root(), Path::new("/definitely/not/here"));
    }

    #[test]
    fn try_new_reports_canonicalize_errors_that_are_not_a_missing_dir() {
        // ENOTDIR：路径中段是一个普通文件。这类错误不能像 NotFound 那样静默回退，
        // 否则 Home 揣着一个非归一化的 root 活下去，skills_dir::classify 的
        // starts_with 前缀比较永远失配，henv 会报「没有安装任何 skill」而不是启动即失败。
        let t = TempHome::new();
        t.write("not-a-directory", "这是一个文件，不是目录");
        let bad = t.path("not-a-directory").join("subdir");
        // 先确认这个 setup 真的落在**非** NotFound 分支上：否则本测试会退化成
        // 「一个不存在的路径」，那样即使修复写错了它也照样通过。
        let setup_kind = std::fs::canonicalize(&bad).err().map(|e| e.kind());
        assert_ne!(
            setup_kind,
            Some(std::io::ErrorKind::NotFound),
            "setup 必须命中非 NotFound 的 canonicalize 错误，实际是 {setup_kind:?}"
        );
        let err = Home::try_new(bad).expect_err("ENOTDIR 必须作为错误返回，不能被回退吞掉");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not-a-directory"),
            "错误信息里必须带上出问题的路径，实际是：{msg}"
        );
    }

    #[test]
    fn from_env_does_not_swallow_an_unreadable_root() {
        // from_env（即 resolve 的实体）必须走 try_new：一个真实但归一化不了的 $HOME
        // 要在启动时带着路径报错，而不是悄悄留下一个非归一化的 root。
        let t = TempHome::new();
        t.write("not-a-directory", "这是一个文件，不是目录");
        let bad = t.path("not-a-directory").join("subdir");
        let err = Home::from_env(Some(bad.into_os_string()), None)
            .expect_err("$HOME 归一化失败时不能回退成 Ok");
        assert!(
            format!("{err:#}").contains("not-a-directory"),
            "错误信息里必须带上出问题的路径"
        );
    }
}
