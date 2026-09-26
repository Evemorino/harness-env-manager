//! skills 目录扫描。
//!
//! spec §2.1 第 4 条：`~/.agents/skills` 共享约定只部分成立——实测存在反向链接、
//! 直接指向 plugin 缓存的链接、以及不属于任何共享库的真实目录。本模块把这些形态统一成
//! 带来源分类的 `SkillEntry`，供 adapter 归一化与 doctor 断链报告使用。
//!
//! 错误处理契约：**只有「不存在」可以静默**。目录不存在 → 空表；目录读不了、前缀归一化
//! 不了 → 带着路径上抛（走 [`try_read_skill_entries`]）。把「读不了」报成「这个 harness
//! 一个 skill 都没装」是漏报，而漏报对只读对账工具是最坏的方向。断链与软链成环是**数据**，
//! 既不是 panic 也不是 `Err`。

use crate::home::Home;
use anyhow::Context;
use std::fs;
use std::io::ErrorKind;
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
    /// 扫描到的路径本身（目录或 symlink）。**刻意不归一化**：adapters/doctor 按它显示
    /// 磁盘位置，两侧比较请一律用 `resolved`——`path` 带着调用方传进来的形态（含软链段），
    /// 换成正则形态就不再是可 diff 的那个串。
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

    /// 交给内容层（§9.3）的路径：正常条目给解析后的真实路径；**断链给链接本身**——
    /// 断链有的是路径，缺的只是内容，而 `hash_dir` 会把它读成 `NoContent`，`apply`
    /// 于是能把它换成真拷贝。少了这一格，「断链自愈」在真实运行里就是死代码。
    ///
    /// 解不开却不只是断链的（权限、`read_link` 之外的失败）仍返回 `None`：那些是故障
    /// 不是数据，交给 doctor 报告，不拿它们去猜一个可交付的内容位置。
    pub fn content_path(&self) -> Option<PathBuf> {
        match &self.resolved {
            Some(r) => Some(r.clone()),
            None if self.is_broken() => Some(self.path.clone()),
            None => None,
        }
    }
}

/// 扫描一个 skills 目录（**不可失败**的便利版）。
///
/// 生产入口是 [`try_read_skill_entries`]：本包装把非 `NotFound` 的失败一并收敛成空表，
/// 也就是让「读不了」与「没有 skills」重新变得不可区分——那正是 D4 要消除的形状。留着它
/// 只为让只关心正常数据的调用方（和既有测试）不必处理 `Result`。
pub fn read_skill_entries(dir: &Path, home: &Home) -> Vec<SkillEntry> {
    try_read_skill_entries(dir, home).unwrap_or_default()
}

/// 扫描一个 skills 目录，结果按 name 排序（确定性）。
///
/// - 目录**不存在**（`NotFound`）→ 空表，不是错误（「配置文件缺失不是错误」只管这一种）。
/// - 其余失败（`PermissionDenied`、`ENOTDIR`、逐条 list/stat/`read_link` 失败、home 前缀
///   归一化失败）→ 带着路径上抛。**不能**降级成空表或少一条：少报对只读工具最坏。
/// - 断链、软链成环、指向普通文件的链接：都是数据，条目照常返回，由 `is_broken`/`store` 呈现。
pub fn try_read_skill_entries(dir: &Path, home: &Home) -> anyhow::Result<Vec<SkillEntry>> {
    // 前缀一次算好：classify 由此变成纯函数，且归一化失败在这里就冒出来（D3）。
    let prefixes = HomePrefixes::new(home)?;
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read skills dir {}", dir.display()));
        }
    };

    let mut out: Vec<SkillEntry> = Vec::new();
    for item in read {
        // 逐条 list 失败不能当成「这个条目不存在」：丢条目 = 漏报。
        let item = item.map_err(|e| {
            anyhow::Error::new(e).context(format!(
                "failed to list an entry of skills dir {}",
                dir.display()
            ))
        })?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue; // .DS_Store 之类：不是条目的元数据，丢弃不算漏报
        }

        let path = item.path();
        // file_type 不跟随 symlink，所以断链也能走到这里，之后由 canonicalize 判死。
        let file_type = item.file_type().map_err(|e| {
            anyhow::Error::new(e).context(format!("failed to stat skill entry {}", path.display()))
        })?;
        // 只收目录与 symlink（symlink 可能指向目录）；普通文件忽略。
        if !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        // 是 symlink 就必须读得出去向；读不出来记成「真实目录」会把断链伪装成正常条目。
        let link_target = if file_type.is_symlink() {
            Some(
                fs::read_link(&path)
                    .with_context(|| format!("failed to read link {}", path.display()))?,
            )
        } else {
            None
        };

        // canonicalize 一次性解多跳；断链（ENOENT）与成环（ELOOP）都由 OS 判死并落到
        // None——它们是数据，由 is_broken 报出，绝不上抛、绝不 panic。权限类失败同样落到
        // None：条目没有消失，只是被呈现成「解不开」，方向是过报而不是漏报。
        let resolved = fs::canonicalize(&path).ok();
        let store = match &resolved {
            Some(r) if r.is_dir() => classify(r, &prefixes),
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
    Ok(out)
}

/// `Home` 的派生前缀，一次扫描算一次。存在的三个理由：
///
/// 1. **形态一致**（D0）：`resolved` 来自 `fs::canonicalize`，root 之下**每一段**软链都已
///    经解掉；而 `Home` 只归一化了 root，`root.join(".agents")` 这类派生路径还留着未解析的
///    中间软链（把 `~/.agents` 整个链到别的盘是真实用法）。字面比较会永远失配。
/// 2. **不吞错**（D3）：归一化走 `Home::try_canonicalize`——全 crate 唯一一处
///    「什么时候允许保留字面路径」的规则。只有 `NotFound` 保留字面值，`EACCES`/`ENOTDIR`/
///    `ELOOP` 在这一步就带路径上抛，不会留下一个永远匹配不上的前缀让 `classify` 静默判 `Other`。
/// 3. **纯函数与确定性**：`classify` 不再带 IO，每条 entry 也不再重复 canonicalize 四个
///    前缀；整次扫描用的是同一份前缀，diff 结论不依赖「问的时机」。
pub struct HomePrefixes {
    agents_skills: PathBuf,
    harness_dirs: Vec<PathBuf>,
}

fn normalize_prefix(label: &str, path: &Path) -> anyhow::Result<PathBuf> {
    Home::try_canonicalize(path)
        .with_context(|| format!("failed to normalize home {label} {}", path.display()))
}

impl HomePrefixes {
    pub fn new(home: &Home) -> anyhow::Result<Self> {
        let mut harness_dirs = Vec::new();
        for dir in home.harness_dirs() {
            harness_dirs.push(normalize_prefix("harness dir", &dir)?);
        }
        Ok(Self {
            agents_skills: normalize_prefix("shared store", &home.agents_skills())?,
            harness_dirs,
        })
    }
}

/// 判定一个已解析的真实路径归属哪个库（纯函数：前缀已由 [`HomePrefixes`] 归一化好）。
///
/// **下面的裁决顺序本身是契约**（D2）：`Shared` → `PluginCache` → `HarnessLocal` → `Other`。
/// 每一对相邻 arm 都有一个「两边同时命中」的 fixture，顺序才是可观察的。实测的看守者：
///
/// - `Shared` 先于 `PluginCache` → `a_shared_store_skill_with_a_plugin_cache_tail_is_still_shared`
///   （缓存判定不锚定 harness 根，所以共享库里一个带缓存尾巴的目录两条 arm 全中）。把
///   `PluginCache` arm 挪到最前面，整套 120 个测试里**只有**这一条会红。
/// - `Shared` 先于 `HarnessLocal` → `a_harness_dir_that_is_the_shared_store_is_classified_shared`
///   （实测 `.agents` 与 `.codex` 指向同一真实目录，两侧前缀同时命中）。共享库是单一真源、
///   三家从它分发，所以归属应当是 `Shared`——内容改了是一处改动；判 `HarnessLocal` 会把同一份
///   字节报成三家的独立差异，drift 语义正好相反。
/// - `PluginCache` 先于 `HarnessLocal` → `symlink_into_plugin_cache_is_plugin_cache_and_yields_plugin_name`
///   与 `classify_requires_plugins_before_the_cache_segment`（claude 的插件缓存物理上就在
///   `~/.claude` 之内，把缓存 arm 挪到 `HarnessLocal` 之后，红的就是这两条）。
///
/// `Other` 是三条都不命中的兜底位，不参与排序裁决。
/// 判成 `PluginCache` 不是换个标签：它是 Task 13 的 drift **豁免位**，排错序的方向是漏报。
pub fn classify(resolved: &Path, prefixes: &HomePrefixes) -> SkillStore {
    if resolved.starts_with(&prefixes.agents_skills) {
        return SkillStore::Shared;
    }
    if is_in_plugin_cache(resolved) {
        return SkillStore::PluginCache;
    }
    if prefixes
        .harness_dirs
        .iter()
        .any(|d| resolved.starts_with(d))
    {
        return SkillStore::HarnessLocal;
    }
    SkillStore::Other
}

/// 「像 plugin 缓存」与「报得出 plugin 身份」是**同一个**判断（D1）。
///
/// 旧实现按「任意顺序、任意深度」命中，于是豁免位宽于身份位：
/// `~/.claude/skills/my-skill/plugins/cache/x` 这类手工目录被判 `PluginCache`——而
/// `PluginCache` 正是 Task 13 的 drift 豁免位，真 drift 被静默藏掉，还会向 diff 吐出
/// 「有 plugin 缓存形态却没有 plugin 身份」的能力。现在两处共用 `plugin_from_path` 的
/// 位置守卫（`plugins` 必须在缓存段**之前**），恒等式即
/// `is_in_plugin_cache(p) == plugin_from_path(p).is_some()`。
fn is_in_plugin_cache(path: &Path) -> bool {
    plugin_from_path(path).is_some()
}

fn normal_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
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
    use std::os::unix::fs::PermissionsExt;

    /// 把一条路径临时设成 000，Drop 时恢复原权限。断言失败 panic 也不会把 000 留在
    /// TempDir 里（那会让 TempDir 删不掉，现场不可复现）。
    struct Unreadable {
        path: PathBuf,
        saved: fs::Permissions,
    }

    impl Unreadable {
        fn new(path: &Path) -> Self {
            let saved = fs::metadata(path).expect("metadata").permissions();
            fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
            Self {
                path: path.to_path_buf(),
                saved,
            }
        }
    }

    impl Drop for Unreadable {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.path, self.saved.clone());
        }
    }

    /// 把 anyhow 错误链里的 `io::Error` kind 挖出来。断 kind 而不是断文案，
    /// 才分得清「不存在」和「读不了」。
    fn io_kind(err: &anyhow::Error) -> Option<ErrorKind> {
        err.downcast_ref::<std::io::Error>().map(|e| e.kind())
    }

    #[test]
    fn empty_or_missing_dir_yields_nothing() {
        let t = TempHome::new();
        assert!(read_skill_entries(&t.path(".claude/skills"), &t.home).is_empty());
        t.mkdir(".claude/skills");
        assert!(read_skill_entries(&t.path(".claude/skills"), &t.home).is_empty());
    }

    #[test]
    fn only_absence_is_silent_an_unreadable_dir_or_prefix_is_an_error() {
        // D4 + D3：这条规则是「只有 NotFound 可以静默」，三个 site 同一 fixture 家族，
        // 每个 arm 只变动**失败落在哪个 kind / 哪个 syscall** 这一个字段。
        // 旧实现把三处一律降级成空表/字面回退，把它们换成 panic! 都仍然 17/17 全绿。
        let t = TempHome::new();
        let dir = t.path(".claude/skills");

        // arm 1：目录不存在 → Ok(空)。这也顺手钉住 HomePrefixes 的 NotFound 分支
        // （三个 home 前缀这时全都不存在；把它换成上抛就变红）。
        let missing = try_read_skill_entries(&dir, &t.home).expect("目录不存在不是错误");
        assert_eq!(missing.len(), 0);

        // arm 2：目录存在、里面确实有一个条目，但读不了 → 必须是 Err，而不是「没装 skill」。
        t.mkdir(".claude/skills/real");
        assert_eq!(
            try_read_skill_entries(&dir, &t.home).expect("可读").len(),
            1,
            "先确认 fixture 本身非空：否则 Err→空表 与 空目录 仍然分不清"
        );
        {
            let _locked = Unreadable::new(&dir);
            let err = try_read_skill_entries(&dir, &t.home)
                .expect_err("读不了的目录不能降级成空表，那是漏报");
            assert_eq!(
                io_kind(&err),
                Some(ErrorKind::PermissionDenied),
                "setup 必须真的落在 PermissionDenied 上，否则本测试退化成「目录不存在」"
            );
            assert!(
                format!("{err:#}").contains(&dir.to_string_lossy().to_string()),
                "错误里必须带上是哪一个目录读不了，实际：{err:#}"
            );
        }
        assert_eq!(
            try_read_skill_entries(&dir, &t.home)
                .expect("权限恢复后可读")
                .len(),
            1,
            "同一个 fixture 只是读不了，不是空的"
        );
        // 便利版仍交出同样的结论（权限恢复后）；它吞错的那一面正是 D4 说的，所以文档
        // 写明生产入口必须走 try_ 版。
        assert_eq!(read_skill_entries(&dir, &t.home).len(), 1);

        // arm 3（D3）：被扫目录完全可读，但 home 派生前缀归一化不了。旧实现静默退回
        // 字面路径，于是本 fix 要消除的「静默 Other」换个触发条件原地复活。
        t.mkdir(".agents/skills");
        {
            let _locked = Unreadable::new(&t.path(".agents"));
            let err = try_read_skill_entries(&dir, &t.home)
                .expect_err("前缀解不开时不能退回字面值静默分类");
            assert_eq!(io_kind(&err), Some(ErrorKind::PermissionDenied));
            let prefix = t.home.agents_skills().to_string_lossy().into_owned();
            assert!(
                format!("{err:#}").contains(&prefix),
                "错误里必须带上是哪一个前缀归一化失败，实际：{err:#}"
            );
        }
    }

    #[test]
    fn skips_dotfiles_like_ds_store() {
        let t = TempHome::new();
        // .git 必须是**目录**：只放 .DS_Store 的话，普通文件过滤器就会先把它剔掉，
        // dotfile 规则被删也照样绿（变异体实测存活）。
        t.write(".claude/skills/.DS_Store", "junk")
            .mkdir(".claude/skills/real")
            .mkdir(".claude/skills/.git");
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
        // resolved / path 都断**具体值**：`is_some()` 与 `!is_broken()` 是 store 断言的
        // 推论（store 只有在解得开时才可能不是 Other），逐条删除都不红。
        assert_eq!(
            got[0].resolved,
            Some(t.path(".claude/skills/trim-cot-leakage")),
            "真实目录必须原样解析到自己"
        );
        assert_eq!(
            got[0].path,
            t.path(".claude/skills/trim-cot-leakage"),
            "path 是扫描到的位置，不能被换成 canonical 形态"
        );
        assert_eq!(got[0].store, SkillStore::HarnessLocal);
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
        // 钉具体值：只要求「还在共享库下面」的话，解到同名另一个候选目录也照样绿。
        assert_eq!(
            got[0].resolved,
            Some(t.path(".agents/skills/dingtalk-mail")),
            "相对基准必须是链接自己的父目录，结果就是这个确定的路径"
        );
    }

    #[test]
    fn path_stays_the_scanned_location_and_a_two_hop_chain_still_resolves() {
        // D5 两件事，共用一个 fixture（只有跳数在变）：
        // (a) `SkillEntry.path` 此前无人固定——把它换成 canonical 后的 `resolved` 仍
        //     17/17 全绿，而 adapters/doctor 要按它显示磁盘位置，也必须保持原样才可 diff；
        // (b) 多跳解析是本模块的标题能力，此前只测过一跳：改成「只解一跳」的
        //     「优化」不会被任何测试发现。
        let t = TempHome::new();
        t.mkdir(".agents/skills/real")
            .symlink(".claude/skills/mid", "../../.agents/skills/real")
            .symlink(".claude/skills/link", "../../.claude/skills/mid");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got.len(), 2, "link 与 mid 都在扫描目录里，两条都得报出来");

        let (link, mid) = (&got[0], &got[1]);
        assert_eq!(link.name, "link");
        assert_eq!(mid.name, "mid");
        assert_eq!(
            link.path,
            t.path(".claude/skills/link"),
            "path 是扫描到的位置，不是 canonical 形态"
        );
        assert_eq!(
            link.link_target,
            Some(PathBuf::from("../../.claude/skills/mid")),
            "read_link 只给出第一跳原值"
        );
        assert_eq!(
            link.resolved,
            Some(t.path(".agents/skills/real")),
            "第二跳也必须解掉，停在 mid 就不算解析"
        );
        assert_eq!(link.store, SkillStore::Shared);
        // 一跳的那条作为对照：同一目标、同一 store，差别只在跳数。
        assert_eq!(
            mid.resolved,
            Some(t.path(".agents/skills/real")),
            "两跳与一跳必须收敛到同一个真实路径"
        );
        assert_eq!(mid.store, SkillStore::Shared);
    }

    #[test]
    fn shared_store_behind_a_symlinked_agents_dir_is_still_shared() {
        // 把 ~/.agents 整个软链到别处（另一个盘 / dotfiles 仓库）是真实用法。
        // canonicalize 会把这一段解掉，所以 home 侧前缀也必须解到同一形态，
        // 否则 Shared 永远匹配不上，共享库里的 skill 静默消失。
        let t = TempHome::new();
        let real = t.path("store/agents-skills");
        t.mkdir("store/agents-skills/skills/dingtalk-mail");
        std::os::unix::fs::symlink(&real, t.path(".agents")).unwrap();
        t.symlink(
            ".claude/skills/dingtalk-mail",
            "../../.agents/skills/dingtalk-mail",
        );
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got[0].store, SkillStore::Shared);
        assert_eq!(
            got[0].resolved,
            Some(t.path("store/agents-skills/skills/dingtalk-mail")),
            "解析结果必须已经跨过 .agents 那一层软链"
        );
        assert_eq!(
            got[0].path,
            t.path(".claude/skills/dingtalk-mail"),
            "磁盘位置照旧是扫描到的那一条"
        );
    }

    #[test]
    fn harness_local_behind_a_symlinked_claude_dir_is_still_harness_local() {
        // 同一个前缀形态问题的 harness 侧：~/.claude 本体是软链时也不能掉到 Other。
        let t = TempHome::new();
        let real = t.path("store/claude-real");
        t.mkdir("store/claude-real/skills/trim-cot-leakage");
        std::os::unix::fs::symlink(&real, t.path(".claude")).unwrap();
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got[0].store, SkillStore::HarnessLocal);
        assert_eq!(
            got[0].resolved,
            Some(t.path("store/claude-real/skills/trim-cot-leakage")),
            "前缀与条目两侧必须都跨过 .claude 那一层软链，才谈得上前缀匹配"
        );
    }

    #[test]
    fn absolute_symlink_back_into_harness_is_harness_local() {
        // 实测 ~/.agents/skills/mcp-builder -> ~/.claude/skills/mcp-builder（反向）
        let t = TempHome::new();
        t.mkdir(".claude/skills/mcp-builder");
        let target = t.path(".claude/skills/mcp-builder");
        t.symlink(".agents/skills/mcp-builder", &target.to_string_lossy());
        let got = read_skill_entries(&t.path(".agents/skills"), &t.home);
        assert_eq!(got[0].store, SkillStore::HarnessLocal);
        // 断 name 没有信号（扫描目录里只有这一个条目，name 就是文件名）；断解析落点才行。
        assert_eq!(
            got[0].resolved,
            Some(t.path(".claude/skills/mcp-builder")),
            "反向链接必须解回 harness 本体，而不是停在 .agents 之下"
        );
    }

    #[test]
    fn a_harness_dir_that_is_the_shared_store_is_classified_shared() {
        // D2：`.agents` 与 `.codex` 指向**同一个**真实目录时（实测存在这种用法），
        // `Shared` 与 `HarnessLocal` 两侧前缀同时命中，裁决顺序从此才可观察。
        // 交换 classify 里这两个 arm 会得到 HarnessLocal，而两者 drift 语义相反：
        // 共享库是单一真源，内容改了是一处改动；判 HarnessLocal 会把同一份字节
        // 报成三家各自的差异。此前这个顺序 17/17 全绿，无人看守。
        let t = TempHome::new();
        let real = t.path("store/shared");
        t.mkdir("store/shared/skills/dingtalk-mail");
        std::os::unix::fs::symlink(&real, t.path(".agents")).unwrap();
        std::os::unix::fs::symlink(&real, t.path(".codex")).unwrap();
        let got = read_skill_entries(&t.path(".codex/skills"), &t.home);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].resolved,
            Some(t.path("store/shared/skills/dingtalk-mail")),
            "两侧前缀现在都等于这个真实目录，才有裁决可谈"
        );
        assert_eq!(got[0].store, SkillStore::Shared, "冲突时归共享库");
    }

    /// 裁决顺序契约的另一半：`Shared` 也必须排在 `PluginCache` 之前。
    ///
    /// 上面那条只钉住了 `Shared` vs `HarnessLocal`——实测把 `PluginCache` arm 挪到
    /// `classify` 最前面，整套（119 个）测试全绿。这里造出**两条 arm 同时命中**的形态：
    /// `is_in_plugin_cache` 只看段序列、不锚定 harness 根（`is_in_plugin_cache` 的文档里
    /// 记着这条残留），所以共享库里一个带 `plugins/cache/<mp>/<name>/<sha>/skills/<s>`
    /// 尾巴的目录**既**在共享库之下**又**长得像缓存。
    ///
    /// 结论必须是 `Shared`：判成 `PluginCache` 不是换个标签，而是把它交给 Task 13 的
    /// drift **豁免位**——共享库里真被改动的 skill 就此静默免检，方向正好是漏报。
    #[test]
    fn a_shared_store_skill_with_a_plugin_cache_tail_is_still_shared() {
        let t = TempHome::new();
        let entry = ".agents/skills/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming";
        let scanned =
            ".agents/skills/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills";
        t.mkdir(entry);
        let got = read_skill_entries(&t.path(scanned), &t.home);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].path,
            t.path(entry),
            "path 是扫描到的位置，必须仍在共享库内"
        );
        let resolved = got[0].resolved.clone().expect("resolves");

        // 两条 setup 断言是**前提**不是结论：它们不成立时这里就没有「两个 arm 同时命中」
        // 可谈，裁决顺序也无从观察——那种 fixture 会一路绿到底，所以前提必须能报红。
        let prefixes = HomePrefixes::new(&t.home).expect("prefixes");
        assert!(
            resolved.starts_with(&prefixes.agents_skills),
            "前提 1：解析结果必须落在共享库内（`Shared` arm 命中），实际 {resolved:?}"
        );
        assert_eq!(
            plugin_from_path(&resolved).as_deref(),
            Some("superpowers"),
            "前提 2：同一串必须同时报得出 plugin 身份（`PluginCache` arm 也命中）"
        );

        assert_eq!(
            got[0].store,
            SkillStore::Shared,
            "两条 arm 同时命中时归共享库：Shared 必须排在 PluginCache 之前"
        );
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
    fn classify_requires_a_plugins_segment_for_plugin_cache() {
        // is_in_plugin_cache 要同时看到 plugins/ 和 cache|managed/，且 plugins 在缓存段
        // 之前（顺序那一半见 classify_requires_plugins_before_the_cache_segment）。
        // 负例此前只在 plugin_from_path 上钉过，classify 这条规则无人看守。
        let t = TempHome::new();
        t.mkdir("mirror/cache/mp/name/skills/s")
            .mkdir("mirror/plugins/cache/mp/name/skills/s");
        let prefixes = HomePrefixes::new(&t.home).expect("prefixes");
        let bare = fs::canonicalize(t.path("mirror/cache/mp/name/skills/s")).expect("bare");
        assert_eq!(
            classify(&bare, &prefixes),
            SkillStore::Other,
            "cache/ 不在 plugins/ 之下就不是 plugin 缓存"
        );
        let real = fs::canonicalize(t.path("mirror/plugins/cache/mp/name/skills/s")).expect("real");
        assert_eq!(classify(&real, &prefixes), SkillStore::PluginCache);
    }

    #[test]
    fn classify_requires_plugins_before_the_cache_segment() {
        // D1：与上面那条的分工——上面只变动 `plugins` 的**有无**，这条只变动**顺序**。
        // 两个 fixture 段数相同、段集合完全相同（.claude + plugins + cache + mp + name +
        // skills + s），唯一差别是 `plugins` 在缓存段之下还是之上。
        // 旧实现（any-order 命中）会把第二个也判 PluginCache，而 PluginCache 正是 Task 13
        // 的 drift 豁免位：一次真实 drift 就此被静默豁免。
        let t = TempHome::new();
        t.mkdir(".claude/plugins/cache/mp/name/skills/s")
            .mkdir(".claude/cache/plugins/mp/name/skills/s");
        let prefixes = HomePrefixes::new(&t.home).expect("prefixes");

        let ok = fs::canonicalize(t.path(".claude/plugins/cache/mp/name/skills/s")).expect("ok");
        assert_eq!(
            classify(&ok, &prefixes),
            SkillStore::PluginCache,
            "plugins/cache/<mp>/<name>/<sha>/skills/<s> 才是真缓存布局"
        );
        assert_eq!(plugin_from_path(&ok).as_deref(), Some("name"));

        let inverted =
            fs::canonicalize(t.path(".claude/cache/plugins/mp/name/skills/s")).expect("inverted");
        assert_eq!(
            classify(&inverted, &prefixes),
            SkillStore::HarnessLocal,
            "cache 在 plugins 之上就不是 plugin 缓存：豁免位不能宽于身份位"
        );
        assert_eq!(
            plugin_from_path(&inverted),
            None,
            "两个判断必须同起同落（is_in_plugin_cache(p) == plugin_from_path(p).is_some()）"
        );

        // 评审实测的那条真实探针：埋在 skill 目录里的 `plugins/cache/x`。它被拒的机理与
        // 上面不同（尾部太浅，报不出 plugin 身份），但结论同样由「身份守卫」给出。
        t.mkdir(".claude/skills/my-skill/plugins/cache/x");
        let buried =
            fs::canonicalize(t.path(".claude/skills/my-skill/plugins/cache/x")).expect("buried");
        assert_eq!(classify(&buried, &prefixes), SkillStore::HarnessLocal);
        assert_eq!(plugin_from_path(&buried), None);
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
    fn is_broken_requires_both_a_link_and_a_failed_resolution() {
        // is_broken 的真值表必须纯逻辑钉住：只断言「有 link 且没解开」。
        // 真目录若因权限解不开，不是断链——它没有 link 可断。
        let entry = |link: Option<&str>, resolved: Option<&str>| SkillEntry {
            name: "x".into(),
            path: PathBuf::from("/h/.claude/skills/x"),
            link_target: link.map(PathBuf::from),
            resolved: resolved.map(PathBuf::from),
            store: SkillStore::Other,
        };
        assert!(
            !entry(None, None).is_broken(),
            "非链接条目解析失败不能报成断链"
        );
        assert!(!entry(None, Some("/h/.claude/skills/x")).is_broken());
        assert!(
            entry(Some("../../.agents/skills/x"), None).is_broken(),
            "有链接却解不开 = 断链"
        );
        assert!(!entry(Some("../../.agents/skills/x"), Some("/h/.agents/skills/x")).is_broken());
    }

    #[test]
    fn symlink_to_a_plain_file_inside_the_store_is_other_not_shared() {
        // 只有**目录**才算 skill；`resolved.is_dir()` 这道门必须有人钉住。
        // 目标故意落在共享库内部：门被删时 classify 会把它报成 Shared。
        let t = TempHome::new();
        t.write(".agents/skills/README.md", "不是 skill，只是个说明文件");
        let target = t.path(".agents/skills/README.md");
        t.symlink(".claude/skills/readme", &target.to_string_lossy());
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].resolved, Some(target), "链接本身是解得开的");
        assert!(!got[0].is_broken(), "解得开就不是断链，哪怕目标不是目录");
        assert_eq!(got[0].store, SkillStore::Other);
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
        // `store == Other` 是 resolved==None 的推论（删掉不红）；真正还没人钉的是「断链
        // 条目带着它指向哪里」——doctor 只能报路径，report 侧全靠这个原值。
        assert_eq!(
            got[0].link_target,
            Some(PathBuf::from("../../.agents/skills/does-not-exist")),
            "断链必须原样报告它指向哪里"
        );
    }

    #[test]
    fn content_path_offers_a_broken_link_its_own_path() {
        // §9.3 的断链自愈要能成立，内容层就必须看得见这个条目：`resolved` 为 None 不等于
        // 「没有路径」——断链有的是路径，缺的只是内容。少了这一条，apply 端到端永远收不到
        // `TargetContent::NoContent`，那句「断链会被换成真拷贝」在真实运行里就是死的。
        let t = TempHome::new();
        t.mkdir(".claude/skills")
            .write(".claude/skills/real/SKILL.md", "# hi")
            .symlink(".claude/skills/linked", "real")
            .symlink(".claude/skills/gone", "../../.agents/skills/does-not-exist");
        let got = read_skill_entries(&t.path(".claude/skills"), &t.home);

        let broken = got.iter().find(|e| e.name == "gone").expect("断链条目");
        assert_eq!(
            broken.content_path(),
            Some(t.path(".claude/skills/gone")),
            "断链交给内容层的是链接本身"
        );

        // 解得开的软链才是能区分两种形态的样本：是普通目录时 `path` 与 `resolved` 恰好相同，
        // 拿它做对比等于什么都没钉。
        let linked = got.iter().find(|e| e.name == "linked").expect("可解软链");
        assert_eq!(
            linked.content_path(),
            Some(t.path(".claude/skills/real")),
            "正常条目照旧给解析后的真实路径"
        );
    }

    /// 把一条 symlink 的**读取**弄坏，和把一条链接**弄断**，是两件不同的事。
    ///
    /// 断链是数据（上面那条已钉）；`read_link` 失败是**故障**，必须上抛。把这条 arm 换成
    /// `.ok()` 会把一根读不出向外的链接呈现成 `link_target: None`——也就是伪装成一条正常
    /// 真实目录，而它是 Task 13 判断 `SkillStore` 的输入。`item.file_type()` 在同一路径下
    /// 仍返回 `Ok`（std 用 dirent 缓存的 `d_type`，不发 syscall），所以 list/stat 两处维持
    /// 「形状正确、无测试看守」。
    /// 变异实测：`?`→`.ok()` 时本测试红在 `expect_err`（函数改而返回 Ok）。
    #[test]
    fn an_unreadable_link_propagates_instead_of_looking_like_a_real_dir() {
        // 把被扫描目录的路径字符串用 "/." 垫到接近 read_dir 的上限，
        // 此时 DirEntry::path() 拼出的条目路径超过上限，fs::read_link 确定地返回 InvalidFilename。
        // 2026-09-24 实测：dirlen=999 时 read_dir 仍成功（上限附近是 1015），
        // 而条目名 `skill-with-a-deliberately-long-name-40`（38 个字符，末尾的 40 是字面量不是长度）
        // 拼出的 entrylen=1038 > PATH_MAX(1024)，已经让 read_link 失败；file_type() 仍是 Ok(true)。
        let long = "skill-with-a-deliberately-long-name-40";
        let t = TempHome::new();
        t.mkdir(".claude/skills/realdir")
            .symlink(&format!(".claude/skills/{long}"), "realdir");
        let mut padded = t.path(".claude/skills").to_string_lossy().into_owned();
        while padded.len() + 2 <= 999 {
            padded.push_str("/.");
        }
        let dir = PathBuf::from(&padded);
        // 前提自证：垫完仍列得出目录，否则这条测试什么都没测（宁可红，不要静默通过）。
        assert!(
            fs::read_dir(&dir).is_ok(),
            "前提失效：dirlen={} 已让 read_dir 失败",
            padded.len()
        );
        let err = try_read_skill_entries(&dir, &t.home)
            .expect_err("read_link 失败必须上抛，不能伪装成一条正常条目");
        let msg = err.to_string();
        assert!(
            msg.starts_with("failed to read link "),
            "错误应由 read_link 那条 arm 产生，实际：{msg}"
        );
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
