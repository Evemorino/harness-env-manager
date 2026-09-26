//! 落盘原语。`apply` 里所有真的动字节的操作都在这里，且只有这几个。

use anyhow::Context as _;
use std::ffi::OsString;
use std::path::Path;

/// 把一个 skill 目录**真拷贝**到目标位置（§9.3 不变式一：henv 绝不在 harness 的
/// skills 目录里创建软链，一律真拷贝）。
///
/// 落盘顺序（§9.7）：同层临时目录 → 装完 → rename 到位。同名旧条目先 rename 走、
/// 新的进来、最后删旧的——**不落 `.bak`**：skills 目录是构建产物，不是用户手写的
/// 配置（§9.7 只给配置文件留 .bak）。
///
/// 已经存在的条目**整体替换**，不做逐文件 merge：源里删掉的文件必须在目标里消失，
/// 否则「交付完的目标==源」这个等式不成立，`diff` 会永远报这条 skill 待交付。
///
/// 失败时目标保持原样：源读不了、临时目录装不进去、rename 失败，都在动到旧条目
/// 之前或之后回退。没有回滚命令——重跑就是修复路径。
pub fn deliver_skill(target: &Path, source: &Path) -> anyhow::Result<()> {
    // 顶层跟随软链：源本身是一条链时，交付物必须是它指向的真内容，
    // 否则不变式一从源那头被绕过去。
    let source = std::fs::canonicalize(source)
        .with_context(|| format!("cannot resolve source {}", source.display()))?;

    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target {} has no parent", target.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;

    let name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("target {} has no file name", target.display()))?;

    let tmp = parent.join(sibling(name, "henv-tmp"));
    // 上次跑到一半留下的残骸：先清掉，否则拷贝会往里叠。
    remove_any(&tmp)?;
    copy_tree(&source, &tmp)
        .with_context(|| format!("cannot copy {} to {}", source.display(), tmp.display()))?;

    let old = parent.join(sibling(name, "henv-old"));
    remove_any(&old)?;
    let had_old = match std::fs::rename(target, &old) {
        Ok(()) => true,
        // rename 不跟随软链：断链也能这样换走。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            remove_any(&tmp)?;
            return Err(
                anyhow::Error::new(e).context(format!("cannot move aside {}", target.display()))
            );
        }
    };

    if let Err(e) = std::fs::rename(&tmp, target) {
        // 放不进去就把旧的放回来，别把用户的东西弄丢。
        if had_old {
            let _ = std::fs::rename(&old, target);
        }
        remove_any(&tmp)?;
        return Err(
            anyhow::Error::new(e).context(format!("cannot put {} in place", target.display()))
        );
    }
    if had_old {
        remove_any(&old)?;
    }
    Ok(())
}

/// `.<name>.henv-<suffix>-<pid>`：与目标同层，且带上进程号——
/// 同一个目标并发跑两次时互相踩不到对方的临时目录。
fn sibling(name: &std::ffi::OsStr, suffix: &str) -> OsString {
    let mut s = OsString::from(".");
    s.push(name);
    s.push(format!(".{suffix}-{}", std::process::id()));
    s
}

/// 删掉一个条目，**不跟随软链**：断链要能被删掉，目录要整棵删。
/// 本来就不存在不是错误。
fn remove_any(path: &Path) -> anyhow::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("cannot remove dir {}", path.display()))?;
    } else {
        std::fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
    }
    Ok(())
}

/// 递归拷贝。**树内部的软链照搬为软链**（不跟随）：这样交付后的内容哈希与源相同
/// ——`hash_dir` 也是按链接目标字符串算的。跟随会让带内部软链的 skill 每次都被
/// 判成待交付，也让一条指回祖先的链接变成死循环。
fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let meta =
        std::fs::symlink_metadata(src).with_context(|| format!("cannot stat {}", src.display()))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(src)
            .with_context(|| format!("cannot read link {}", src.display()))?;
        // 本项目只跑 macOS/Linux（spec §1），unix 扩展足够。
        std::os::unix::fs::symlink(&target, dst)
            .with_context(|| format!("cannot link {}", dst.display()))?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(dst).with_context(|| format!("cannot create {}", dst.display()))?;
        let mut kids: Vec<_> = std::fs::read_dir(src)
            .with_context(|| format!("cannot list {}", src.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()
            .with_context(|| format!("cannot list {}", src.display()))?;
        kids.sort();
        for k in kids {
            let file_name = k.file_name().expect("read_dir entry has a name");
            copy_tree(&k, &dst.join(file_name))?;
        }
    } else {
        std::fs::copy(src, dst)
            .with_context(|| format!("cannot copy {} to {}", src.display(), dst.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::hash::hash_dir;
    use crate::testutil::TempHome;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("read target parent")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".henv-"))
            .collect()
    }

    #[test]
    fn delivery_is_a_real_copy_not_a_link() {
        // §9.3 不变式一，最硬的一条。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo")
            .write("src/foo/sub/x.md", "x");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(
            meta.file_type().is_dir() && !meta.file_type().is_symlink(),
            "交付物必须是真目录，实际 {:?}",
            meta.file_type()
        );
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "# foo"
        );
        assert_eq!(fs::read_to_string(target.join("sub/x.md")).unwrap(), "x");
        assert!(
            tmp_leftovers(target.parent().unwrap()).is_empty(),
            "不留残骸"
        );
    }

    #[test]
    fn delivery_creates_a_missing_target_directory() {
        // Review Focus 5：claude 装了但没建过 .claude/skills 是常态。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        let target = t.path(".claude/skills/foo");
        assert!(!t.path(".claude").exists());
        deliver_skill(&target, &t.path("src/foo")).unwrap();
        assert!(target.join("SKILL.md").exists());
    }

    #[test]
    fn re_delivering_replaces_wholesale_and_is_idempotent() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# v1")
            .write("src/foo/extra.md", "e");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        // 改源、删掉一个文件，再交付一次：目标必须整体等于新源，
        // 而不是旧内容上面叠新内容。
        t.write("src/foo/SKILL.md", "# v2");
        fs::remove_file(t.path("src/foo/extra.md")).unwrap();
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# v2");
        assert!(
            !target.join("extra.md").exists(),
            "整体替换：源里删掉的文件不能在目标里活下来"
        );
        assert!(tmp_leftovers(target.parent().unwrap()).is_empty());
    }

    #[test]
    fn a_dangling_link_is_replaced_by_a_real_copy() {
        // §9.3 的自愈：断链没有内容，覆盖它不破坏任何东西。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo")
            .symlink(".claude/skills/foo", "../../src/gone");
        let target = t.path(".claude/skills/foo");
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        deliver_skill(&target, &t.path("src/foo")).unwrap();
        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "# foo"
        );
    }

    #[test]
    fn a_working_link_is_replaced_by_a_real_copy_too() {
        // apply 只在内容层判成 update 时才走到这儿。走到这儿就意味着
        // 「这个位置的形态要变成真拷贝」——§9.3 不变式一没有例外。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# v1")
            .write("other/foo/SKILL.md", "# v2")
            .symlink(".claude/skills/foo", "../../other/foo");
        deliver_skill(&t.path(".claude/skills/foo"), &t.path("src/foo")).unwrap();
        let target = t.path(".claude/skills/foo");
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "# v1");
    }

    #[test]
    fn a_source_that_is_itself_a_link_delivers_real_content() {
        // 源侧软链（本机那 14 条 agents-dir skill 就长这样被声明）不能让交付物
        // 也变成一条链——否则不变式一从源那头绕过去了。
        let t = TempHome::new();
        t.write("real/foo/SKILL.md", "# real")
            .symlink("src/foo", "../real/foo");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();
        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "# real"
        );
    }

    #[test]
    fn a_nested_link_is_copied_as_a_link() {
        // 树内部的软链按链接目标字符串照搬：`hash_dir` 也是这么算的，两边
        // 一致才能保证「交付完再算一次哈希 == 源哈希」，否则带内部软链的 skill
        // 会被永久判成待交付。
        let t = TempHome::new();
        t.write("src/foo/real.md", "x")
            .symlink("src/foo/link.md", "real.md");
        let target = t.path(".claude/skills/foo");
        deliver_skill(&target, &t.path("src/foo")).unwrap();

        let link = target.join("link.md");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("real.md"));
        assert_eq!(
            hash_dir(&target).unwrap(),
            hash_dir(&t.path("src/foo")).unwrap(),
            "交付前后内容哈希必须相同，否则每次 diff 都会重报这条 skill"
        );
    }

    #[test]
    fn awkward_names_are_delivered_verbatim() {
        // Review Focus 3：临时目录名是从目标名派生的，带空格/非 ASCII 的名字
        // 一旦被拼错，交付出来的目录名就不是清单里那个。
        let t = TempHome::new();
        for name in ["with space", "中文名字", "dash-and_underscore.v2"] {
            let src = format!("src/{name}");
            t.write(&format!("{src}/SKILL.md"), name);
            let target = t.path(&format!(".claude/skills/{name}"));
            deliver_skill(&target, &t.path(&src)).unwrap();
            assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), name);
            assert!(
                fs::symlink_metadata(&target).unwrap().file_type().is_dir(),
                "{name} 必须是真目录"
            );
        }
        assert!(tmp_leftovers(&t.path(".claude/skills")).is_empty());
    }

    #[test]
    fn leftovers_from_a_half_finished_run_are_cleaned_and_replaced() {
        // Review Focus 1：上次跑到一半被杀，临时目录留在那儿。重跑必须自愈，
        // 而不是把新内容往里叠。
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        let target = t.path(".claude/skills/foo");
        let tmp = t.path(&format!(
            ".claude/skills/.foo.henv-tmp-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("stale.md"), "leftover").unwrap();

        deliver_skill(&target, &t.path("src/foo")).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("SKILL.md")).unwrap(),
            "# foo"
        );
        assert!(
            !target.join("stale.md").exists(),
            "残骸不能成为交付的一部分"
        );
        assert!(tmp_leftovers(target.parent().unwrap()).is_empty());
    }

    #[test]
    fn a_missing_source_is_an_error_and_leaves_the_target_alone() {
        let t = TempHome::new();
        t.write(".claude/skills/foo/SKILL.md", "# keep me");
        let err = deliver_skill(&t.path(".claude/skills/foo"), &t.path("src/gone"))
            .expect_err("源不存在必须报错");
        assert!(format!("{err:#}").contains("gone"), "{err:#}");
        assert_eq!(
            fs::read_to_string(t.path(".claude/skills/foo/SKILL.md")).unwrap(),
            "# keep me",
            "失败不许碰目标"
        );
    }
}
