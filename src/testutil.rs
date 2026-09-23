//! 测试专用：在 TempDir 里重建假 HOME。绝不触碰真实 ~。
#![allow(dead_code)]

use crate::home::Home;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct TempHome {
    // 必须保活：drop 即删除整棵目录树
    _dir: TempDir,
    pub home: Home,
    root: PathBuf,
}

impl TempHome {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("create tempdir");
        let home = Home::new(dir.path());
        // 用 canonicalize 后的 root 构造路径，才能与 adapter 输出（同样 canonicalize 过）直接比较
        let root = home.root().to_path_buf();
        Self { _dir: dir, home, root }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// 写文件，自动创建父目录。
    pub fn write(&self, rel: &str, body: &str) -> &Self {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::fs::write(&p, body).expect("write file");
        self
    }

    pub fn mkdir(&self, rel: &str) -> &Self {
        std::fs::create_dir_all(self.path(rel)).expect("mkdir");
        self
    }

    /// 创建符号链接（Unix only；本项目只跑 macOS/Linux）。
    /// `target` 原样写入，相对路径由调用方负责正确性。
    pub fn symlink(&self, link: &str, target: &str) -> &Self {
        let l = self.path(link);
        if let Some(parent) = l.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::os::unix::fs::symlink(Path::new(target), &l).expect("symlink");
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_files_and_symlinks_under_temp_root() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/real-one")
            .write(".agents/skills/real-one/SKILL.md", "# hi")
            .symlink(".claude/skills/real-one", "../../.agents/skills/real-one");

        assert!(t.path(".agents/skills/real-one/SKILL.md").exists());
        let link = t.path(".claude/skills/real-one");
        assert!(link.exists(), "symlink resolves through to the real dir");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../../.agents/skills/real-one")
        );
        assert!(t.home.root().is_absolute());
    }
}
