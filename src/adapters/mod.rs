//! harness adapter 层。新增 harness = 新模块实现 trait + 在 `registry` 加一行（spec §7）。

pub mod claude_code;
pub mod codex;
pub mod kimi_code;

use crate::apply::HarnessReport;
use crate::apply::mcp::McpTarget;
use crate::capability::{Capability, McpTransport, McpValue, SkillSource, Support};
use crate::diff::HarnessPlan;
use crate::home::Home;
use crate::skills_dir::{SkillEntry, SkillStore, plugin_from_path};
use crate::store::provenance::Ledger;
use crate::store::skills::ContentIndex;
use std::path::PathBuf;

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

/// 一条 MCP 在某家能不能**忠实**写成它的配置形态（§9.6）。
///
/// 「忠实」是关键：语法上写得出一个键，但它发出去的东西与清单要的不是一回事，那不算
/// 表达得了。三家实测（2026-09-28）的取值通道完全不同：
///
/// - **claude-code 会展开 `${VAR}`**：它自己那份 context7 plugin 就写着
///   `Authorization: ${CONTEXT7_API_KEY:-}`，`claude mcp list` 也会为难解析的引用报
///   「Missing environment variables」。所以在它那儿把引用写进 `env` / `headers`
///   就是「取这个环境变量」，**永远是忠实的**。
/// - **codex 的 `env` / `http_headers` 是字面量**：`codex doctor` 只对
///   `env_vars` / `env_http_headers` / `bearer_token_env_var` 报「env var … is not set」，
///   把 `${CONTEXT7_API_KEY}` 写进 `http_headers` 时一声不响（= 按字面量发出去）。
///   从环境取值只有那两个专用字段，且 `env_vars` 是**同名转发**。
/// - **kimi-code 的 `env` / `headers` 也是字面量**：它的 `mergeStdioEnv` 直接
///   `Object.assign(merged, configEnv)`，`buildMcpRemoteHeaders` 直接 `{ ...config.headers }`。
///   唯一的环境取值通道是 `bearerTokenEnvVar`，而它**固定发 `Bearer <token>`**——
///   把裸值 `Authorization: ${VAR}` 映射过去会悄悄改掉语义，宁可说交付不了。
///
/// 返回 `Err(理由)` 由 [`crate::diff`] 转成 `undeliverable`：**要写盘的动作**改判，
/// 对账结果（in-sync / conflict / unknown）保留——它们回答的是「本机现在是不是清单要的」。
pub(crate) fn mcp_expressible(harness: &str, transport: &McpTransport) -> Result<(), String> {
    match (harness, transport) {
        // claude 展开引用，两种传输、处处都忠实。
        (CLAUDE_CODE, _) => Ok(()),
        (CODEX, McpTransport::Stdio { env, .. }) => {
            for (key, value) in env {
                match value {
                    // 同名转发（`K: ${K}`）就是 codex 的 `env_vars`。
                    McpValue::Ref {
                        var,
                        bearer: false,
                        optional: false,
                    } if var == key => {}
                    // `Bearer ${VAR}` 是 header 的写法：codex 的 bearer 字段专指
                    // `Authorization`，`env` 里没有它的位置。
                    McpValue::Ref { bearer: true, .. } => {
                        return Err(format!(
                            "`Bearer ${{VAR}}` 是 header 的写法（codex 的 \
                             `bearer_token_env_var` 专指 `Authorization`）；`{key}` 这个 env \
                             表达不了"
                        ));
                    }
                    // `${K:-}` 也是同名，但带着「变量缺失时取空值」那一层语义，而
                    // `env_vars` 只收变量名——那层语义没有落点。**不能把默认值扔掉当成
                    // 同名转发**：变量缺失时 codex 会报缺输入（可见），而清单说的是
                    // 「缺了也行」。
                    McpValue::Ref { optional: true, .. } => {
                        return Err(format!(
                            "codex 的 `env_vars` 只收变量名，表达不了 `{key}` 上 `${{VAR:-}}` 的\
                             「变量缺失时取空值」语义"
                        ));
                    }
                    // 剩下的 `Ref` 就是换名映射（`K: ${OTHER}`）：`env` 只能放字面量。
                    McpValue::Ref { .. } => {
                        return Err(format!(
                            "codex 的 `env` 只收字面量，从环境取值只能**同名**转发\
                             （`env_vars`）；`{key}` 映射到别的变量名表达不了"
                        ));
                    }
                    // 清单不收字面量，所以这条不可达；真到了这儿也不能顺手写下去。
                    McpValue::Literal(_) => {
                        return Err(format!("`{key}` 的值是字面量，而清单只收引用"));
                    }
                }
            }
            Ok(())
        }
        (CODEX, McpTransport::Http { headers, .. }) => {
            for (name, value) in headers {
                match value {
                    // 裸引用 → `env_http_headers`（头名 → 变量名）。
                    McpValue::Ref {
                        bearer: false,
                        optional: false,
                        ..
                    } => {}
                    // `Bearer ${VAR}` → `bearer_token_env_var`，但那个字段**只**管
                    // `Authorization`：别的头名它送不出去。
                    McpValue::Ref {
                        bearer: true,
                        optional: false,
                        ..
                    } if name.eq_ignore_ascii_case("authorization") => {}
                    McpValue::Ref { bearer: true, .. } => {
                        return Err(format!(
                            "codex 的 `bearer_token_env_var` 只作用于 `Authorization`；\
                             `{name}` 这个头写不出 `Bearer ${{VAR}}` 的语义"
                        ));
                    }
                    McpValue::Ref { .. } => {
                        return Err(format!(
                            "codex 的取值字段收的是**变量名**（`env_http_headers` 或 \
                             `bearer_token_env_var`），`{name}` 上 `${{VAR:-}}` 的默认值表达不了"
                        ));
                    }
                    McpValue::Literal(_) => {
                        return Err(format!("`{name}` 的值是字面量，而清单只收引用"));
                    }
                }
            }
            Ok(())
        }
        (KIMI_CODE, McpTransport::Stdio { env, .. }) => {
            if env.is_empty() {
                return Ok(());
            }
            Err(
                "kimi-code 的 MCP `env` 是字面量（不做 `${VAR}` 展开），henv 不会把值写死进配置"
                    .to_string(),
            )
        }
        (KIMI_CODE, McpTransport::Http { headers, .. }) => {
            if headers.is_empty() {
                return Ok(());
            }
            // kimi 只有一条环境取值通道，而它固定发 `Bearer <token>`——所以
            // `Authorization: Bearer ${VAR}` 正是它能表达的那一种（且只有这一种）。
            if let Some((
                name,
                McpValue::Ref {
                    bearer: true,
                    optional: false,
                    ..
                },
            )) = (headers.len() == 1)
                .then(|| headers.iter().next())
                .flatten()
            {
                if name.eq_ignore_ascii_case("authorization") {
                    return Ok(());
                }
            }
            Err(
                "kimi-code 的 MCP `headers` 是字面量，它唯一的环境取值通道 `bearerTokenEnvVar` \
                 固定发 `Bearer <token>`——所以只有 `Authorization: Bearer ${VAR}` 这一种\
                 header 写法表达得了（裸值、别的头名、多个头都不行）"
                    .to_string(),
            )
        }
        // 只有上面三家；多出来的名字是 registry 出错，不该在这儿假装判断过。
        (other, _) => Err(format!("unknown harness `{other}`")),
    }
}

/// 一条 skill 条目（`try_read_skill_entries` 的产物）→ 清单侧那个来源。
///
/// 判据只有一份：四个分支（共享库 / 插件缓存 / harness 本地 / 说不清）一旦各写一份，
/// 迟早在某个角落漂移——「共享库」与「插件缓存」的边界尤其如此，而它同时是 drift 豁免位
/// 与 `supports` 的判据。claude 与 codex 原来各有一份逐字相同的 match，本函数把它们收成一份。
///
/// `SkillStore::Shared` 只说明「这条内容的真身落在 `~/.agents/skills` 里」，不代表它由 henv
/// 放过——它可能是别的工具（`npx skills`、`dws skill setup`）铺的。`supports` 拿它判
/// 「这家读得到吗」，而不是「这家归 henv 管吗」，正是这个区别。
pub(crate) fn skill_source_of(entry: &SkillEntry) -> SkillSource {
    match entry.store {
        SkillStore::Shared => SkillSource::AgentsDir,
        // 实测（spec §2.1 第 4 条）：claude/codex 的 skills 里有一批绝对 symlink，直接指向
        // `~/.claude/plugins/cache/<mp>/<name>/<sha>/skills/<skill>`。
        SkillStore::PluginCache => {
            let resolved = entry.resolved.clone().unwrap_or_default();
            match plugin_from_path(&resolved) {
                Some(plugin) => SkillSource::PluginBundle { plugin },
                None => SkillSource::Unknown,
            }
        }
        SkillStore::HarnessLocal => SkillSource::Local {
            path: entry.resolved.clone().unwrap_or_else(|| entry.path.clone()),
        },
        // 断链或指向库外：来源不可判定，交给 doctor 报告
        SkillStore::Other => SkillSource::Unknown,
    }
}

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
    ///   记的是字面量 `unknown`，不归一化的话两条 `Some("unknown")` 会被当成真 SHA
    ///   参与比较，两个真不同的构建被误判。
    fn read_state(&self) -> anyhow::Result<Vec<Capability>>;

    /// 这家能否接纳该能力——「拆解降级」的判定点（Task 12 细化）。
    fn supports(&self, cap: &Capability) -> Support;

    /// 本家交付 skill 的目标目录。`None` = 这家**没有**用户级 skills 目录
    /// （kimi-code，spec §9.1：它的散装 skill 只能随 plugin bundle 分发）。
    ///
    /// 返回 `None` 不是错误，是这家的事实；「拒绝」的判定在 `supports` 里做，
    /// 这个方法只回答「往哪写」。
    ///
    /// **没有默认实现**：新加一家 harness 时必须显式回答这个问题，
    /// 否则一个默认的 `None` 会让它的 skill 交付静默变成空操作。
    fn skills_target_dir(&self) -> Option<PathBuf>;

    /// 这家把 MCP 写在哪、用什么机制写（§9.6）。`None` = 这家没有用户级 MCP 配置。
    ///
    /// 与 `skills_target_dir` 一样**没有默认实现**：默认实现会让「新加的 adapter 忘了
    /// 说自己写在哪」编译通过，然后静默地什么都不交付。
    fn mcp_target(&self) -> Option<McpTarget>;

    /// 执行层的第 2 步（spec §9.2）：把**已经解析好的**内容交付到本家。
    /// 不碰网络、不解析来源——那些在 `store` 的前置步骤里已经做完了。
    ///
    /// 默认实现覆盖三家在当前范围内的全部差异：它们交付 skill 的方式一模一样
    /// （拷进各自的 skills 目录），差异只体现在 [`skills_target_dir`] 上。
    /// B3 的 MCP 写入会给 claude 覆盖这个方法（它走 shell out `claude mcp add-json`）。
    ///
    /// 这**不是** Plan A 里那种「放个桩只能靠期望 panic 来测」的方法：三条真实路径
    /// （claude/codex 各交付一次、kimi 交付零次）都有测试钉着。
    ///
    /// [`skills_target_dir`]: HarnessAdapter::skills_target_dir
    fn apply(
        &self,
        slice: &HarnessPlan,
        desired: &[Capability],
        content: &ContentIndex,
        ledger: &mut Ledger,
    ) -> anyhow::Result<HarnessReport> {
        let mut report = crate::apply::deliver_skills(
            self.name(),
            self.skills_target_dir().as_deref(),
            slice,
            content,
            ledger,
        )?;
        let mcps = crate::apply::mcp::deliver_mcps(
            self.name(),
            self.mcp_target().as_ref(),
            slice,
            desired,
            ledger,
        )?;
        report.absorb(mcps);
        Ok(report)
    }
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
    use crate::apply::mcp::McpMechanism;
    use crate::capability::{Kind, McpTransport, SkillSource, Spec};
    use crate::diff::{Action, PlanEntry};
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

    /// `mcp_expressible` 的三条判据（实测依据见函数文档）：claude 什么都能写；
    /// codex 只认**同名裸引用**（stdio）与裸引用（http）；kimi 的凭据一个都写不出来。
    #[test]
    fn mcp_expressibility_matches_each_harnesss_value_channel() {
        let stdio = |env: &[(&str, &str)]| McpTransport::Stdio {
            command: "npx".into(),
            args: vec![],
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_string(), McpValue::from_text_or_literal(v)))
                .collect(),
        };
        let http = |headers: &[(&str, &str)]| McpTransport::Http {
            url: "https://x/mcp".into(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), McpValue::from_text_or_literal(v)))
                .collect(),
        };

        // claude 展开 `${VAR}`：连带默认值的写法都照写。
        assert!(mcp_expressible(CLAUDE_CODE, &http(&[("Authorization", "${K:-}")])).is_ok());
        assert!(mcp_expressible(CLAUDE_CODE, &stdio(&[("A", "${B}")])).is_ok());

        // codex 的取值字段收的是**变量名**：同名裸引用能表达。
        assert!(mcp_expressible(CODEX, &stdio(&[("K", "${K}")])).is_ok());
        assert!(mcp_expressible(CODEX, &http(&[("Authorization", "${K}")])).is_ok());
        assert!(mcp_expressible(CODEX, &http(&[])).is_ok());

        // 换名映射没有落点（`env_vars` 只按原名转发）。
        let renamed = mcp_expressible(CODEX, &stdio(&[("K", "${OTHER}")])).unwrap_err();
        assert!(renamed.contains("同名"), "{renamed}");

        // `${VAR:-}` 的「缺了也行」语义没有落点——不能把默认值扔掉当同名转发。
        let defaulted = mcp_expressible(CODEX, &stdio(&[("K", "${K:-}")])).unwrap_err();
        assert!(defaulted.contains("env_vars"), "{defaulted}");
        let defaulted = mcp_expressible(CODEX, &http(&[("Authorization", "${K:-}")])).unwrap_err();
        assert!(defaulted.contains("env_http_headers"), "{defaulted}");

        // `Bearer ${VAR}` 走两家的 bearer 字段：codex 的只作用于 `Authorization`，
        // 换个头名它送不出去；kimi 也只有这一条通道。
        assert!(mcp_expressible(CODEX, &http(&[("Authorization", "Bearer ${K}")])).is_ok());
        assert!(
            mcp_expressible(CODEX, &http(&[("X-Auth", "Bearer ${K}")])).is_err(),
            "codex 的 bearer 通道只认 Authorization"
        );
        assert!(mcp_expressible(KIMI_CODE, &http(&[("Authorization", "Bearer ${K}")])).is_ok());
        // 带默认值的 bearer 两家都表达不了（它们的字段收的是变量名）。
        assert!(mcp_expressible(CODEX, &http(&[("Authorization", "Bearer ${K:-}")])).is_err());
        assert!(mcp_expressible(KIMI_CODE, &http(&[("Authorization", "Bearer ${K:-}")])).is_err());

        // kimi：`Bearer` 之外的凭据写法一个都写不出来，「没有凭据」的条目照常交付。
        assert!(mcp_expressible(KIMI_CODE, &http(&[])).is_ok());
        assert!(mcp_expressible(KIMI_CODE, &stdio(&[])).is_ok());
        assert!(mcp_expressible(KIMI_CODE, &stdio(&[("K", "${K}")])).is_err());
        assert!(mcp_expressible(KIMI_CODE, &http(&[("Authorization", "${K}")])).is_err());
        assert!(
            mcp_expressible(
                KIMI_CODE,
                &http(&[("Authorization", "Bearer ${K}"), ("X-Extra", "${K}")])
            )
            .is_err(),
            "kimi 只有一条 header 通道，多个头表达不了"
        );

        // 第五家的名字不该在这里被当成「写得出来」。
        assert!(mcp_expressible("cursor", &http(&[])).is_err());
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
                transport: McpTransport::Stdio {
                    command: String::new(),
                    args: Vec::new(),
                    env: Default::default(),
                },
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
    /// **每一格**都钉死：唯一非 Native 的三个格是 kimi 对 Hook/Command/Agent
    /// （无用户级目录/配置），其余 15 格全部 Native。
    ///
    /// `Kind::Skill` 为什么是 Native：`cap_of_kind` 造的 skill 是**共享库来源**，而 kimi
    /// 读得到共享库——它按来源分岔，那条判据由下一条测试单独钉（2026-09-28 实测修正：
    /// 旧矩阵把 Skill 归进「kimi 吃不下」，那个假设已经被证伪）。
    ///
    /// 能力一律用 `FakeAdapter::cap`（`Origin::Adopted`）构造。逐格断言（而非一刀切
    /// `matches!(.., Native | Unsupported { .. })`）是为了谁把某一格改成别的值都能被定位到。
    #[test]
    fn supports_matrix_is_native_except_kimi_loose_hook_command_agent() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        for kind in Kind::all() {
            let cap = cap_of_kind(kind);
            for a in &reg {
                let is_kimi_loose = a.name() == KIMI_CODE
                    && matches!(kind, Kind::Hook | Kind::Command | Kind::Agent);
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

    /// `Kind::Skill` 在 kimi 上按**来源**分岔。
    ///
    /// 2026-09-28 实测：kimi 的 `USER_GENERIC_DIRS = [".agents/skills"]`，共享库里的 skill
    /// 它直接读得到——判 `Unsupported` 会让报告对一条已经能用的能力说谎。反过来，
    /// local / git / bundle 拆解出来的 skill 它确实没有落点（`skills_target_dir()` 是
    /// `None`），判 `Native` 就成了「报告写着 install、盘上什么都没写」的假成功。
    #[test]
    fn kimi_takes_shared_skills_but_not_loose_ones() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        let kimi = lookup(&reg, KIMI_CODE).expect("kimi 在 registry 里");

        assert_eq!(
            kimi.supports(&cap_of_kind(Kind::Skill)),
            Support::Native,
            "`cap_of_kind` 造的正是共享库来源，kimi 读得到"
        );

        let loose = FakeAdapter::cap(
            Kind::Skill,
            "x",
            Spec::Skill {
                source: SkillSource::Local {
                    path: PathBuf::from("/x"),
                },
                path: None,
            },
        );
        assert!(
            matches!(kimi.supports(&loose), Support::Unsupported { .. }),
            "local 来源的 skill 在 kimi 上没有落点"
        );
    }

    /// `.claude` 是一个**普通文件**或**断链**时，`exists()` 也返回 true，于是 detect 声称
    /// 「装了 claude」，紧接着 `read_state` 交出空表——「没装」和「装了但读不出」重新变得
    /// 不可区分，这正是 D4 在 scanner 侧刚消掉的形状。契约是 detect **不得**用 `exists()`，
    /// 而按实际承载配置的位置区分目录与文件：harness 可以经由文件探测，只要那确实是它的
    /// 配置所在（claude 的 MCP 在 `~/.claude.json`），但必须 `is_dir()` / `is_file()`，不能
    /// 只看「存在」。
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

    /// 三家各自的目标目录（§9.1：kimi 没有用户级 skills 目录）。
    /// 这条把「谁有目录」钉死，否则一个把 kimi 也填上的实现会让 kimi 的散装 skill
    /// 静默走进交付路径，而它本该被 `supports` 判成 Unsupported。
    #[test]
    fn exactly_claude_and_codex_have_a_skills_target_dir() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        let got: Vec<(&str, bool)> = reg
            .iter()
            .map(|a| (a.name(), a.skills_target_dir().is_some()))
            .collect();
        assert_eq!(
            got,
            vec![(CLAUDE_CODE, true), (CODEX, true), (KIMI_CODE, false)]
        );
    }

    #[test]
    fn skills_target_dirs_point_at_each_harnesss_own_directory() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        assert_eq!(
            lookup(&reg, CLAUDE_CODE)
                .unwrap()
                .skills_target_dir()
                .unwrap(),
            t.home.claude_skills()
        );
        assert_eq!(
            lookup(&reg, CODEX).unwrap().skills_target_dir().unwrap(),
            t.home.codex_skills()
        );
        // 必须是**各家自己的**目录：两处都返回 agents_skills() 这种错法会让
        // 「交付到 claude」把文件放进共享目录，codex 那边看着像已交付。
        assert_ne!(
            lookup(&reg, CLAUDE_CODE).unwrap().skills_target_dir(),
            Some(t.home.agents_skills())
        );
    }

    /// 默认的 `apply` 必须把活交给 `apply::deliver_skills`——包括 kimi 那次
    /// 「什么都不做但不是错误」的调用。这条用 FakeAdapter 驱动，不碰真实配置树。
    #[test]
    fn the_default_apply_delivers_through_the_shared_helper() {
        let t = TempHome::new();
        t.write("src/foo/SKILL.md", "# foo");
        let cap = FakeAdapter::cap(
            crate::capability::Kind::Skill,
            "foo",
            crate::capability::Spec::Skill {
                source: crate::capability::SkillSource::AgentsDir,
                path: None,
            },
        );
        let mut content = crate::store::skills::ContentIndex::default();
        content.sources.insert(
            cap.id.clone(),
            crate::store::skills::ResolvedContent {
                path: t.path("src/foo"),
                hash: "h".into(),
                source_id: "agents-dir".into(),
                stale: None,
            },
        );
        content
            .targets
            .entry("claude-code".into())
            .or_default()
            .insert("foo".into(), crate::store::skills::TargetContent::NoContent);

        let slice = HarnessPlan {
            harness: "claude-code".into(),
            detected: true,
            entries: vec![PlanEntry::new(cap.id.clone(), Action::Install)],
            actual_by_kind: Default::default(),
        };
        let mut ledger = crate::store::provenance::Ledger::default();
        let fake = FakeAdapter::native("claude-code").with_skills_dir(t.path(".claude/skills"));
        let report = fake
            .apply(&slice, std::slice::from_ref(&cap), &content, &mut ledger)
            .expect("default apply must work");
        assert_eq!(report.delivered, vec!["skill/foo".to_string()]);
        assert!(t.path(".claude/skills/foo/SKILL.md").exists());
        assert_eq!(
            ledger.find("claude-code", Kind::Skill, "foo").unwrap().name,
            "foo"
        );

        // 没有目标目录的那家：不交付、不报错、不写账本。
        let kimi = FakeAdapter::native("kimi-code");
        let mut ledger2 = crate::store::provenance::Ledger::default();
        let report = kimi
            .apply(&slice, std::slice::from_ref(&cap), &content, &mut ledger2)
            .expect("kimi 不报错");
        assert!(report.delivered.is_empty());
        assert!(report.failure.is_none());
        assert!(ledger2.entries.is_empty());
    }

    /// §9.6：三家的 MCP 写目标必须**指向读路径读的那个文件**。
    /// 这条测试是「写进去了、读不出来」这个故障类的结构防线——
    /// 读路径用的是 `home.claude_json()` 等三个访问器，写路径拿到的必须是同一批。
    #[test]
    fn every_mcp_target_points_at_the_file_the_reader_reads() {
        let t = TempHome::new();
        let reg = registry(&t.home);
        let by_name = |n: &str| {
            reg.iter()
                .find(|a| a.name() == n)
                .unwrap_or_else(|| panic!("{n} 不在 registry 里"))
                .mcp_target()
                .unwrap_or_else(|| panic!("{n} 应当有 MCP 写目标"))
        };

        let claude = by_name(CLAUDE_CODE);
        assert_eq!(
            claude.path,
            t.home.claude_json(),
            "claude 写在 ~/.claude.json"
        );
        assert_eq!(
            claude.mechanism,
            McpMechanism::ClaudeCli {
                binary: PathBuf::from("claude")
            },
            "生产里必须是裸 `claude`：走 PATH，与用户手敲的是同一件事"
        );

        let codex = by_name(CODEX);
        assert_eq!(codex.path, t.home.codex_config());
        assert_eq!(codex.mechanism, McpMechanism::CodexToml);

        let kimi = by_name(KIMI_CODE);
        assert_eq!(kimi.path, t.home.kimi_mcp());
        assert_eq!(kimi.mechanism, McpMechanism::KimiJson);
    }
}
