//! harness adapter 层。新增 harness = 新模块实现 trait + 在 `registry` 加一行（spec §7）。

pub mod claude_code;
pub mod codex;
pub mod kimi_code;

use crate::capability::{Capability, Support};
use crate::home::Home;

/// 本计划**只**承认这三家 harness（spec §1）。这三串同时是：`manifest` 里 `targets:`
/// 允许出现的名字、`registry` 的返回顺序（Task 14 golden 逐行锁定）、报告里的行标签。
///
/// 注意 `manifest::KNOWN_HARNESSES` 是**第二处**字面抄写（spec §7 就是让两处各加一行）。
/// 两份独立串一旦漂移，症状是「diff 里叫 codex 的那一家在清单里变成未知 harness」，
/// 所以 `tests::harness_name_constants_are_the_three_spellings_manifest_accepts`
/// 把它们连**顺序**一起钉成相等。合并成单一出处要改 `manifest.rs`，不在本 Task 的
/// Files 列表里，留给协调方裁定。
pub const CLAUDE_CODE: &str = "claude-code";
/// 见 [`CLAUDE_CODE`]。
pub const CODEX: &str = "codex";
/// 见 [`CLAUDE_CODE`]。
pub const KIMI_CODE: &str = "kimi-code";

/// 一家 harness 的两件事：本机装没装（`detect`）、装成什么样（`read_state`），
/// 以及「我们想给它这个能力，它接不接」——`supports`。
///
/// trait 里**没有** `apply`：Plan A 全程只读，放一个桩方法进去就只能靠「期望 panic」
/// 来测，那种测试杀不掉任何东西（spec §3 决策 1）。Plan B 再加。
/// 降级规则也不在这里：adapter 只回答自己这一家的事，plugin / bundle 怎么拆由
/// `supports` 的返回值自然承接——一个吃不下 plugin 的 harness 在这里返回
/// `Support::Unsupported` 就够了，它不需要理解 bundle 是什么。
pub trait HarnessAdapter {
    /// harness 名，取值必为 [`CLAUDE_CODE`]/[`CODEX`]/[`KIMI_CODE`] 之一。
    /// 既是 `targets:` 的匹配串，也是 `lookup` 的键与报告里的行标签。
    fn name(&self) -> &str;

    /// 本机是否装了这家。只查 harness 根目录在不在，**此外不做任何事**：
    /// 不读文件、不解析、不创建（Plan A 只读）。detect 失败只警告，不报错
    /// （spec §4 harnesses 注释）。
    fn detect(&self) -> bool;

    /// 扫描真实配置树 → 归一化能力。
    ///
    /// 契约（Task 9/10/11 逐条实现并各自带测试）：
    /// - **配置文件缺失返回空 `Vec`，不是错误**——「这台机器没装这家」是正常状态。
    ///   但**语法错误必须报错**：一个坏掉的 `config.toml` 静默成空表，会被
    ///   `read_state_on_empty_home_is_empty_not_error` 那类断言念成「这个 harness
    ///   什么能力都没有」，于是 diff 报出假同步。
    /// - **只读**：不得在注入的 root 下创建、修改、删除任何东西。
    /// - skills 目录一律走 [`crate::skills_dir::try_read_skill_entries`]，
    ///   **不要**用便利版 `read_skill_entries`——后者把读不了的目录吞成空表，
    ///   一个只是权限坏掉的 skills 目录就会被报成「这家没装 skill」，正是上面
    ///   那条契约要消灭的假阴性。
    /// - 从活配置里读出的每一个 version / sha / ref 串，装进 `Spec` 之前先过
    ///   [`crate::capability::normalize_locator`]：本机在 `gitCommitSha` 这类字段上
    ///   记的是字面量 `unknown`，不归一化的话两条 `Some("unknown")` 会命中
    ///   「两边都有 SHA」分支判成 `Same`，两个真不同的构建被报成同步。
    fn read_state(&self) -> anyhow::Result<Vec<Capability>>;

    /// 这家能否接纳该能力——「拆解降级」的判定点（Task 12 细化）。
    fn supports(&self, cap: &Capability) -> Support;
}

/// 全部已知 harness，**固定顺序** claude-code → codex → kimi-code。
///
/// 顺序是契约不是细节：Task 14 的 diff 报告按这个顺序每家打印一行，并由 insta
/// golden 逐字节锁定。返回 `Vec<Box<dyn _>>` 而非常量数组，是为了让 Task 13 的
/// 「按 `targets:` 过滤」能直接 `retain`。
pub fn registry(home: &Home) -> Vec<Box<dyn HarnessAdapter>> {
    vec![
        Box::new(claude_code::ClaudeCode::new(home.clone())),
        Box::new(codex::Codex::new(home.clone())),
        Box::new(kimi_code::KimiCode::new(home.clone())),
    ]
}

/// 按 `name()` 精确匹配查一家；未注册的名字返回 `None`，大小写敏感且不做前缀匹配
/// （与 `manifest` 侧的 `targets:` 校验同一套判据，测试逐条钉着）。
///
/// **消费者尚未落地**：这里的文档曾经承诺它撑着 Task 15 的「未知 harness」报错路径，
/// 那条路径最终没接上——`lib.rs::run_diff` 直接遍历 `registry()`，`lookup` 在生产代码里
/// **零调用点**，目前只有它自己的测试在用。留着它是给后续接线（Plan C / 新一轮 manifest
/// 校验）一个现成入口，不是「有人在用」的声明。
///
/// 返回 `&dyn HarnessAdapter` 而不是计划里写的 `&Box<dyn HarnessAdapter>`：
/// `&Box<T>` 是多余的一层间接，`clippy::borrowed_box` 在 `-D warnings` 下直接
/// 拒绝，而本计划不允许为消警告加 `#[allow]`。调用方要的就是那个 trait 对象
/// （`diff::HarnessInput { adapter: &dyn HarnessAdapter }`），少掉这层壳不影响任何
/// 消费点。**后续 Task 不要把签名改回 `&Box<...>`。**
pub fn lookup<'a>(
    reg: &'a [Box<dyn HarnessAdapter>],
    name: &str,
) -> Option<&'a dyn HarnessAdapter> {
    reg.iter().map(|a| a.as_ref()).find(|a| a.name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Kind, SkillSource, Spec};
    use crate::manifest::KNOWN_HARNESSES;
    use crate::testutil::{FakeAdapter, TempHome};
    use std::path::PathBuf;

    /// 三个常量的**字面拼写**必须在 registry 之外再钉一遍。少了这条，把
    /// `CLAUDE_CODE`/`CODEX` 的串值互换是一个能让上面所有 detect/lookup 测试
    /// 照常全绿的变异——因为那些断言两边都用的是同一批常量。而互换之后
    /// Task 14 的 golden 会把 claude 那一行标成 `codex`，manifest 的 `targets:`
    /// 也接不上（`KNOWN_HARNESSES` 是另一处独立字面串，Task 5 已钉死）。
    /// 顺带把「恰好这三家、没有第四家可构造」和 registry 顺序一起绑上。
    #[test]
    fn harness_name_constants_are_the_three_spellings_manifest_accepts() {
        assert_eq!([CLAUDE_CODE, CODEX, KIMI_CODE], KNOWN_HARNESSES);
    }

    #[test]
    fn registry_returns_three_adapters_in_fixed_order() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        let names: Vec<&str> = reg.iter().map(|a| a.name()).collect();
        assert_eq!(names, vec![CLAUDE_CODE, CODEX, KIMI_CODE]);
    }

    #[test]
    fn lookup_finds_registered_and_rejects_unknown() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        assert!(lookup(&reg, "codex").is_some());
        assert!(lookup(&reg, "cursor").is_none());
        // 大小写敏感的精确匹配：`manifest` 侧的 `KNOWN_HARNESSES.contains` 是精确串
        // 比较，这里若放宽成 `eq_ignore_ascii_case`，`Codex` 就能穿过 lookup 进到报告
        // 行标签里，两处的判定却还互不一致（变异实测：只留前两行的话该放宽能活下来）。
        assert!(
            lookup(&reg, "Codex").is_none(),
            "lookup 必须按精确串匹配，否则与 manifest 的 targets 校验不一致"
        );
        // 变异实测：`starts_with` 让 lookup("code") 命中 codex，而 manifest 侧是精确串比较。
        assert!(lookup(&reg, "code").is_none(), "lookup 不允许前缀匹配");
        // 反方向的同一条例子（变异实测）：`name.starts_with(a.name())` 逃得过上面那行——
        // "code" 不是任何注册名的前缀——却让 lookup("codex-plus") 命中 codex。两个方向都是
        // 前缀匹配，都得钉；精确匹配契约要求两侧一律 None。
        assert!(
            lookup(&reg, "codex-plus").is_none(),
            "lookup 不允许前缀匹配（反方向：注册名是入参的前缀）"
        );
    }

    #[test]
    fn detect_is_false_on_empty_home() {
        let t = TempHome::new();
        for a in registry(&t.home) {
            assert!(!a.detect(), "{} should not detect on empty home", a.name());
        }
    }

    #[test]
    fn detect_is_true_when_harness_dir_exists() {
        let t = TempHome::new();
        t.mkdir(".claude").mkdir(".codex").mkdir(".kimi-code");
        let reg = registry(&t.home);
        let detected: Vec<&str> = reg
            .iter()
            .filter(|a| a.detect())
            .map(|a| a.name())
            .collect();
        assert_eq!(detected, vec![CLAUDE_CODE, CODEX, KIMI_CODE]);
    }

    #[test]
    fn detect_is_per_harness_not_all_or_nothing() {
        let t = TempHome::new();
        t.mkdir(".codex");
        let reg = registry(&t.home);
        assert!(!lookup(&reg, CLAUDE_CODE).unwrap().detect());
        assert!(lookup(&reg, CODEX).unwrap().detect());
        assert!(!lookup(&reg, KIMI_CODE).unwrap().detect());
    }

    #[test]
    fn read_state_on_empty_home_is_empty_not_error() {
        let t = TempHome::new();
        for a in registry(&t.home) {
            let caps = a.read_state().expect("missing config must not be an error");
            assert!(caps.is_empty(), "{} returned {:?}", a.name(), caps);
        }
    }

    /// Plan A 全程只读：`detect` 就是「目录在不在」这一个检查，`read_state` 只往里看。
    /// 上面那组 detect 测试**拦不住**一个顺手建目录的实现——`create_dir_all(..).is_ok()`
    /// 调用完自己就把 `is_dir` 变成正，于是「空 home 上 detect 为假」那条断言是在给
    /// 上一次调用擦过的地板作证。这里直接查注入 root 的目录项，把「什么都没留下」钉死。
    /// Task 9-11 往 `read_state` 里填真实扫描时，这条是同一道闸门。
    #[test]
    fn detect_and_read_state_leave_the_injected_root_untouched() {
        let t = TempHome::new();
        for a in registry(&t.home) {
            let _ = a.detect();
            let _ = a.read_state().expect("missing config must not be an error");
        }
        let left: Vec<std::ffi::OsString> = std::fs::read_dir(t.home.root())
            .expect("injected root still exists")
            .map(|e| e.expect("read_dir entry").file_name())
            .collect();
        assert!(
            left.is_empty(),
            "adapter 在注入的 root 下留下了东西：{left:?}"
        );
    }

    /// 逐 kind 造一条**非 Plugin origin** 的能力（`FakeAdapter::cap` = `Origin::Adopted`）。
    /// `supports` 只看 `kind` 与 `origin`，不看 `spec` 内容，所以载荷按 kind 一一对应填即可。
    fn cap_of_kind(kind: Kind) -> Capability {
        let spec = match kind {
            Kind::Skill => Spec::Skill {
                source: SkillSource::AgentsDir,
                path: None,
            },
            Kind::Mcp => Spec::Mcp {
                command: String::new(),
                args: Vec::new(),
                env: Default::default(),
            },
            Kind::Plugin => Spec::Plugin {
                marketplace: None,
                entry: "x".into(),
                version: None,
                sha: None,
                install_path: None,
                enabled: true,
            },
            Kind::Hook => Spec::Hook {
                event: "E".into(),
                command: "c".into(),
            },
            Kind::Command => Spec::Command {
                path: PathBuf::from("/x"),
            },
            Kind::Agent => Spec::Agent {
                path: PathBuf::from("/x"),
            },
        };
        FakeAdapter::cap(kind, "x", spec)
    }

    /// Task 12 把 `supports()` 从占位换成真实接纳度矩阵，这条把「kind × harness」的
    /// **每一格**都钉死：唯一非 Native 的四个格是 kimi 对散装 Skill/Hook/Command/Agent
    /// （无用户级目录/配置），其余 14 格全部 Native。能力一律用 `FakeAdapter::cap`
    /// （`Origin::Adopted`）构造——若带 `Origin::Plugin`，kimi 的「bundle 随原生 plugin
    /// 落地」例外会把那四个 Unsupported 格也判成 Native，整张矩阵就少钉了四格。
    /// 逐格断言（而非一刀切 `matches!(.., Native | Unsupported { .. })`）正是为了守住
    /// 「kimi 吃不下散装 skill」这个 degrade-or-not 的判定点，谁把某一格改成别的值都能
    /// 被定位到。
    #[test]
    fn supports_matrix_is_native_except_kimi_loose_skill_hook_command_agent() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        for kind in Kind::all() {
            let cap = cap_of_kind(kind);
            for a in &reg {
                let is_kimi_loose = a.name() == KIMI_CODE
                    && matches!(
                        kind,
                        Kind::Skill | Kind::Hook | Kind::Command | Kind::Agent
                    );
                if is_kimi_loose {
                    assert!(
                        matches!(a.supports(&cap), Support::Unsupported { .. }),
                        "{kind:?} 在 kimi-code 上必须是 Unsupported"
                    );
                } else {
                    assert_eq!(
                        a.supports(&cap),
                        Support::Native,
                        "{} 对 {kind:?} 必须 Native",
                        a.name()
                    );
                }
            }
        }
    }

    /// `.claude` 是一个**普通文件**或**断链**时，`exists()` 也返回 true，于是 detect 声称
    /// 「装了 claude」，紧接着 `read_state` 交出空表——「没装」和「装了但读不出」重新变得
    /// 不可区分，这正是 D4 在 scanner 侧刚消掉的形状。detect 只认目录。
    #[test]
    fn detect_rejects_a_regular_file_at_the_harness_path() {
        let t = TempHome::new();
        t.write(".claude", "not a directory");
        let reg = registry(&t.home);
        assert!(
            !lookup(&reg, CLAUDE_CODE).unwrap().detect(),
            "detect 必须要求目录；换成 exists() 时普通文件也会被认成已安装"
        );
    }
}
