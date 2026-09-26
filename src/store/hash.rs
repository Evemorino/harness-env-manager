//! 目录内容指纹。spec §9.3 把「需不需要交付」的判定单位定成**内容是否等价**、
//! 不是形态是否相同——所以这里要回答的是「这两坨字节一不一样」。

use anyhow::Context as _;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 目录（或单文件）的内容指纹，十六进制 SHA-256。
///
/// 三态契约（与 `Home::try_canonicalize`、adapter `read_state` 同一条失败隔离）：
/// - 路径不存在 → `Ok(None)`
/// - 路径存在但解不开（断链）→ `Ok(None)`
/// - 读得出内容 → `Ok(Some(hash))`
/// - 存在、能解，但读的过程中出错（权限等）→ `Err`
///
/// 顶层**跟随**软链：`~/.claude/skills/dingtalk → ~/.agents/skills/dingtalk`
/// 必须与真目录同哈希，否则 §9.3 承诺的「那 14 条软链原样留着」会全变成待交付项。
/// 树**内部**的软链按链接目标字符串参与哈希、不跟随——跟随会把哈希绑到树外，
/// 也让一个指回祖先的链接变成死循环。
pub fn hash_dir(path: &Path) -> anyhow::Result<Option<String>> {
    let root = match std::fs::canonicalize(path) {
        Ok(p) => p,
        // canonicalize 把「不存在」与「断链」都报成 NotFound，对调用方而言它们等价：
        // 都没有内容可交付。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("cannot resolve {}", path.display())));
        }
    };
    let mut hasher = Sha256::new();
    hash_entry(&root, &root, &mut hasher)?;
    Ok(Some(hex(hasher.finalize().as_slice())))
}

fn hash_entry(root: &Path, path: &Path, hasher: &mut Sha256) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?;
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)
            .with_context(|| format!("cannot read link {}", path.display()))?;
        feed(hasher, b'l', &rel, target.as_os_str().as_encoded_bytes());
    } else if meta.is_dir() {
        feed(hasher, b'd', &rel, b"");
        let mut kids: Vec<PathBuf> = std::fs::read_dir(path)
            .with_context(|| format!("cannot list {}", path.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()
            .with_context(|| format!("cannot list {}", path.display()))?;
        // 排序是契约不是优化：readdir 顺序随文件系统而变，不排的话同一棵树
        // 两次哈希可能不同，「内容没变」会被报成漂移。
        kids.sort();
        for k in kids {
            hash_entry(root, &k, hasher)?;
        }
    } else {
        let bytes =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        feed(hasher, b'f', &rel, &bytes);
    }
    Ok(())
}

/// 逐条喂进 hasher：类型字符、相对路径、长度、内容，之间用 `\0` 分隔。
/// 长度前缀不可省——否则 `("ab","c")` 与 `("a","bc")` 喂出同一串字节。
fn feed(hasher: &mut Sha256, kind: u8, rel: &str, body: &[u8]) {
    hasher.update([kind, 0]);
    hasher.update(rel.as_bytes());
    hasher.update([0]);
    hasher.update((body.len() as u64).to_le_bytes());
    hasher.update(body);
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempHome;

    #[test]
    fn missing_path_has_no_content_hash() {
        let t = TempHome::new();
        assert_eq!(
            hash_dir(&t.path(".agents/skills/nope")).unwrap(),
            None,
            "不存在的路径没有内容可交付，不是错误"
        );
    }

    #[test]
    fn a_dangling_link_has_no_content_hash() {
        let t = TempHome::new();
        t.symlink(".claude/skills/ghost", "../../.agents/skills/ghost");
        assert_eq!(
            hash_dir(&t.path(".claude/skills/ghost")).unwrap(),
            None,
            "断链没有内容——这正是 §2.2 第 1 条那种形态"
        );
    }

    #[test]
    fn a_link_to_a_tree_hashes_as_that_tree() {
        // §9.3 的核心断言：本机那 14 条软链必须被判成「内容与源相同」，
        // 而不是「形态不对，待交付」。形态无关的判定单位就落在这一条上。
        let t = TempHome::new();
        t.write(".agents/skills/real-one/SKILL.md", "# hi")
            .symlink(".claude/skills/real-one", "../../.agents/skills/real-one");
        let src = hash_dir(&t.path(".agents/skills/real-one")).unwrap();
        let via_link = hash_dir(&t.path(".claude/skills/real-one")).unwrap();
        assert!(src.is_some(), "真目录必须有哈希");
        assert_eq!(src, via_link, "软链与其目标必须同哈希");
    }

    #[test]
    fn identical_trees_hash_identically_wherever_they_live() {
        let t = TempHome::new();
        t.write("a/x.md", "content").write("a/sub/y.md", "more");
        t.write("b/x.md", "content").write("b/sub/y.md", "more");
        assert_eq!(
            hash_dir(&t.path("a")).unwrap(),
            hash_dir(&t.path("b")).unwrap(),
            "哈希只看树内相对路径与内容，不看树在哪"
        );
    }

    #[test]
    fn content_or_layout_change_changes_the_hash() {
        let t = TempHome::new();
        t.write("a/x.md", "content").write("a/y.md", "a");
        let base = hash_dir(&t.path("a")).unwrap();

        let t2 = TempHome::new();
        t2.write("b/x.md", "content").write("b/y.md", "b");
        assert_ne!(base, hash_dir(&t2.path("b")).unwrap(), "内容变了");

        let t3 = TempHome::new();
        t3.write("c/x.md", "content");
        assert_ne!(base, hash_dir(&t3.path("c")).unwrap(), "少了一个文件");
    }

    #[test]
    fn entry_boundaries_are_not_ambiguous() {
        // 不做长度前缀分隔的话 ("ab","c") 与 ("a","bc") 会喂进同一串字节。
        let a = TempHome::new();
        a.write("t/ab", "c");
        let b = TempHome::new();
        b.write("t/a", "bc");
        assert_ne!(
            hash_dir(&a.path("t")).unwrap(),
            hash_dir(&b.path("t")).unwrap()
        );
    }

    #[test]
    fn a_nested_link_contributes_its_target_not_its_content() {
        // 树内部的软链按「链接目标字符串」参与哈希、不跟随：跟随会让一棵树的内容
        // 哈希绑到树外，也让一个指回祖先的链接变成死循环。
        let t = TempHome::new();
        t.write("src/real.md", "x").write("src/keep.md", "y");
        t.symlink("src/link.md", "real.md");
        let src = hash_dir(&t.path("src")).unwrap();

        let t2 = TempHome::new();
        t2.write("copy/real.md", "x")
            .write("copy/keep.md", "y")
            .symlink("copy/link.md", "real.md");
        assert_eq!(src, hash_dir(&t2.path("copy")).unwrap(), "同样的链接目标");

        let t3 = TempHome::new();
        t3.write("other/real.md", "x")
            .write("other/keep.md", "y")
            .symlink("other/link.md", "../elsewhere.md");
        assert_ne!(src, hash_dir(&t3.path("other")).unwrap(), "链接目标变了");
    }

    #[test]
    fn an_empty_dir_is_not_an_empty_file() {
        let t = TempHome::new();
        t.mkdir("d");
        t.write("f", "");
        assert_ne!(
            hash_dir(&t.path("d")).unwrap(),
            hash_dir(&t.path("f")).unwrap(),
            "空目录与空文件不是同一个东西"
        );
    }

    #[test]
    fn a_regular_file_also_hashes() {
        let t = TempHome::new();
        t.write("one.md", "hello");
        let h = hash_dir(&t.path("one.md")).unwrap().expect("文件有内容");
        assert_eq!(h.len(), 64, "SHA-256 的十六进制是 64 字符，实际 {h}");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
