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

/// 按 `name()` 精确匹配查一家；未注册的名字返回 `None`（调用方据此报
/// 「未知 harness」，见 Task 15）。
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
    use crate::manifest::KNOWN_HARNESSES;
    use crate::testutil::TempHome;

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
}
