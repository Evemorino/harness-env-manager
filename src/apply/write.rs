//! 落盘原语。`apply` 里所有真的动字节的操作都在这里，且只有这几个。

use anyhow::Context as _;
use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
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

/// 写一个配置文件（§9.7）：同目录临时文件 → fsync → rename。
///
/// **为什么不经 `fs::write` 直接落到目标**：写一半被打断（断电、进程被杀、
/// 磁盘满）会留下一个语法坏掉的 `config.toml`，而那会连坐整个 codex——
/// 用户为了装一条 MCP 赔上整个 harness。rename 在同一文件系统内是原子的：
/// 目标要么是旧字节，要么是完整的新字节，没有中间态。
///
/// **备份是单槽的**：只在 `<file>.henv.bak` 不存在时写一次。第二次交付不覆盖它——
/// 要留的是「用户最初的配置」，不是「我们上一次写的那份」。
/// 目标文件不存在时不备份：没有可失去的东西。
///
/// **目标自己是符号链接时不写**（§9.7「不改写既有的符号链接」）：`rename` 换掉的是
/// 链接本身，用户的 dotfiles 关系会被静默销毁，而 `fs::copy` 与 `rename` 对链接的
/// 处置相反（前者跟随、后者不跟随），备份与结果因此对不上。链条归用户，我们报错。
/// 只在**最后一段**是链接时拒绝——父目录是软链是另一回事，照常写。
pub fn write_config(target: &Path, body: &[u8]) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
        let dest = std::fs::read_link(target)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unreadable>".to_string());
        anyhow::bail!(
            "{} is a symlink (-> {dest}); henv does not rewrite existing symlinks, \
             please replace it with a real file or point the manifest elsewhere",
            target.display()
        );
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{}: no parent directory", target.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("{}: cannot create parent directory", parent.display()))?;

    let name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{}: no file name", target.display()))?;

    // 备份先做：它读的是**改动前**的目标。
    // `symlink_metadata` 而不是 `Path::exists()`：后者跟随符号链接，
    // 一个指向不存在目标的链接会被判成「不存在」，于是我们不备份就把它换掉了。
    if std::fs::symlink_metadata(target).is_ok() {
        let bak = parent.join(format!("{}.henv.bak", name.to_string_lossy()));
        if std::fs::symlink_metadata(&bak).is_err() {
            std::fs::copy(target, &bak)
                .with_context(|| format!("{}: cannot back up", target.display()))?;
        }
    }

    // 后缀用 `henv-tmp` 与 `deliver_skill` 同一家族：`.config.toml.henv-tmp-<pid>`。
    // 取别的名字（比如 `tmp`）会让测试里的 `.henv-` 过滤条件一个字节都匹配不到，
    // 「不留残骸」那几条断言随即变成空转。
    // 权限**只许保留、不许放宽**：这些配置里可能有 token（`config.toml` 的
    // `experimental_bearer_token` 就是明文），而 `~` 通常是 `drwxr-x---`——
    // 组内其他用户能穿过去，umask 默认的 0644 就等于把 token 递出去。
    // 目标已存在就照抄它的权限位（用户或 harness 设过的，我们无权改）；
    // 不存在就用 0600，**不跟 umask 走**。
    let mode = std::fs::metadata(target)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o600);

    let tmp = parent.join(sibling(name, "henv-tmp"));
    let write = || -> anyhow::Result<()> {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("{}: cannot create temp file", tmp.display()))?;
        // 在写入前设权限：临时文件存在的每一刻都不该比目标更开放。
        f.set_permissions(std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("{}: cannot set permissions", tmp.display()))?;
        f.write_all(body)
            .with_context(|| format!("{}: cannot write", tmp.display()))?;
        // fsync：没有它，rename 可能先于数据落盘被持久化，掉电后目标是个空文件。
        f.sync_all()
            .with_context(|| format!("{}: cannot sync", tmp.display()))?;
        Ok(())
    };
    if let Err(err) = write().and_then(|()| {
        std::fs::rename(&tmp, target)
            .with_context(|| format!("{}: cannot replace", target.display()))
    }) {
        // 失败也要收尾：半截临时文件留在用户的配置目录里，比不写更糟。
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
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
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn tmp_leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("read target parent")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".henv-"))
            .collect()
    }

    fn bak_of(p: &Path) -> PathBuf {
        let name = p.file_name().expect("file name").to_string_lossy();
        p.with_file_name(format!("{name}.henv.bak"))
    }

    #[test]
    fn writing_a_new_config_creates_no_backup() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let target = t.path(".codex/config.toml");
        write_config(&target, b"a = 1\n").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "a = 1\n");
        assert!(!bak_of(&target).exists(), "原本就没有文件，没什么可备份的");
        assert!(
            tmp_leftovers(&t.path(".codex")).is_empty(),
            "{:?}",
            tmp_leftovers(&t.path(".codex"))
        );
    }

    #[test]
    fn the_first_write_backs_up_the_original() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "user's own\n");
        write_config(&target, b"ours\n").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "ours\n");
        assert_eq!(
            fs::read_to_string(bak_of(&target)).unwrap(),
            "user's own\n",
            "备份要留住用户原来那份"
        );
        assert!(tmp_leftovers(&t.path(".codex")).is_empty());
    }

    /// 单槽：第一次之后就不该再动它。否则第二次交付会把「用户最初的配置」
    /// 换成「我们上一次写的」，备份就失去了意义。
    #[test]
    fn the_backup_is_single_slot_and_keeps_the_oldest() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "original\n");
        write_config(&target, b"first\n").unwrap();
        write_config(&target, b"second\n").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "second\n");
        assert_eq!(
            fs::read_to_string(bak_of(&target)).unwrap(),
            "original\n",
            "第二次写不许覆盖备份"
        );
    }

    /// 写盘要经临时文件：目标文件在 rename 之前不许出现新内容。
    /// 这条用一个**同名目录**占住目标路径来间接证伪——rename 一个普通文件到已存在
    /// 的目录上必定失败（EISDIR），而临时文件仍要清干净（半截文件留在用户配置
    /// 目录里是最糟的形状）。
    ///
    /// `.bak` 必须先占上：目标是个目录时，备份那步会先去 `fs::copy` 这个目录而
    /// **先**失败，那样根本走不到 rename，测试名字里的承诺就成了空转。
    #[test]
    fn a_failed_rename_still_cleans_up_its_temp_file() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.mkdir(".codex/config.toml"); // 目录占位
        t.write(".codex/config.toml.henv.bak", "already backed up\n");
        write_config(&target, b"nope\n").unwrap_err();
        assert!(
            tmp_leftovers(&t.path(".codex")).is_empty(),
            "失败的写入也不能留临时文件：{:?}",
            tmp_leftovers(&t.path(".codex"))
        );
    }

    fn mode_of(p: &Path) -> u32 {
        fs::metadata(p).expect("stat").permissions().mode() & 0o777
    }

    /// 权限不许被我们**放宽**：用户或 harness 把这些配置设成 0600（里面有 token），
    /// 重写一遍不该把它变成 umask 默认的 0644。
    ///
    /// 选 `0o640` 而不是 `0o600` 是刻意的：0640 在任何 umask 下都不是 `File::create`
    /// 的默认产物，这条断言因此不会因为开发机 umask 恰好是 077 而空转。
    #[test]
    fn rewriting_a_config_keeps_its_permissions() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "a = 1\n");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();

        write_config(&target, b"a = 2\n").unwrap();

        assert_eq!(mode_of(&target), 0o640, "重写不许动用户设的权限");
    }

    /// 新建的配置是 owner-only：`~/.codex/config.toml`、`~/.kimi-code/mcp.json`
    /// 都可能装 token（`experimental_bearer_token` 之类），而 `~` 通常是
    /// `drwxr-x---`——组内其他用户能穿过去，0644 就等于把 token 递出去。
    #[test]
    fn a_new_config_is_owner_only() {
        let t = TempHome::new();
        let target = t.path(".kimi-code/mcp.json");
        write_config(&target, b"{}\n").unwrap();
        assert_eq!(mode_of(&target) & 0o077, 0, "同机其他用户不许读得到");
    }

    /// 备份是「用户原来那份」的忠实副本，权限也要跟着（`fs::copy` 在 Unix 上会带过
    /// 权限位——这条把它钉住，将来换成手动读写就不会悄悄丢掉）。
    #[test]
    fn the_backup_keeps_the_original_permissions() {
        let t = TempHome::new();
        let target = t.path(".codex/config.toml");
        t.write(".codex/config.toml", "user's own\n");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        write_config(&target, b"ours\n").unwrap();

        assert_eq!(mode_of(&bak_of(&target)), 0o600, "备份不许比原文件更开放");
    }

    /// **配置文件本身是一条符号链接时必须拒绝写**。
    ///
    /// 我们落盘的办法是「同目录临时文件 → rename 到目标」，而 rename 换掉的是
    /// **链接本身**：用户的 chezmoi/stow 配置会静默变成脱离管理的普通文件，
    /// 链接关系永久丢失，内容只以普通文件形式留在 `.bak` 里
    /// （`fs::copy` 反而跟随链接，所以备份里是**链接那头**的内容——两边不一致，
    /// 用户看到的是一份没人再同步的文件）。
    ///
    /// §9.7 最后一条「henv 既不创建也不改写既有的符号链接」按字面执行：
    /// 链条归用户，我们只报错并指出它指向哪儿。
    #[test]
    fn a_symlinked_config_is_refused_rather_than_replaced() {
        let t = TempHome::new();
        t.write("dotfiles/codex.toml", "managed elsewhere\n")
            .symlink(".codex/config.toml", "../dotfiles/codex.toml");
        let target = t.path(".codex/config.toml");

        let err = write_config(&target, b"ours\n").expect_err("目标是符号链接时必须拒绝写入");
        let msg = format!("{err:#}");
        assert!(msg.contains("config.toml"), "要说清是哪条路径：{msg}");

        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink(),
            "链接本身必须原样保留——换成普通文件就等于把用户的 dotfiles 关系弄丢了"
        );
        assert_eq!(
            fs::read_to_string(t.path("dotfiles/codex.toml")).unwrap(),
            "managed elsewhere\n",
            "链接那头也不许动"
        );
        assert!(!bak_of(&target).exists(), "拒绝了就不该留下 .bak");
        assert!(tmp_leftovers(&t.path(".codex")).is_empty());
    }

    /// 父目录是软链（`~/.codex` → dotfiles）是另一回事，**不许连坐**：
    /// 目标自己不是链接，rename 落在解析后的目录里，一切照常。
    #[test]
    fn a_symlinked_parent_directory_is_written_through_normally() {
        let t = TempHome::new();
        t.mkdir("dotfiles/codex");
        t.symlink(".codex", "dotfiles/codex");
        let target = t.path(".codex/config.toml");

        write_config(&target, b"a = 1\n").unwrap();

        assert_eq!(
            fs::read_to_string(t.path("dotfiles/codex/config.toml")).unwrap(),
            "a = 1\n",
            "父目录是软链时应当照常写进去"
        );
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
