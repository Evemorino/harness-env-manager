# henv — 跨 harness 能力环境管理器设计

日期：2026-09-23
状态：已与用户逐节确认（架构 / 清单 schema / 数据流与错误处理）；测试策略一节随本文档首次呈现，待评审。

## 1. 问题与目标

同一台机器上多个 agent harness（claude-code、codex、kimi-code，未来更多）各自维护
skills / MCP / plugins 配置，格式互不兼容。任务写到一半切换 harness 时，能力静默失效
（典型：claude-code 的 plugin 是 skills+MCP+hooks 捆绑包，kimi-code 没有 plugin 机制）。

**henv 的目标**：一份声明式清单作为事实源，识别各 harness 现状，切换 harness 时保证
能力环境一致；skills 与 harness 解耦管理；plugin 按能力拆解后降级分发。

### 已确认的产品决策

| 决策点 | 结论 |
|---|---|
| 定位 | 个人工具优先，初期支持 claude-code + codex + kimi-code，adapter 层可插拔扩展 |
| 一致性语义 | 能力拆解降级：plugin 拆为 skill/MCP/command/agent/hook，目标 harness 支持什么装什么 |
| 事实源与方向 | env.yaml 清单为源，单向 apply；`scan` 命令可把已有安装收养进清单 |
| 交付方式 | **真拷贝，不用软链**（2026-09-25 定）。harness 的 skills 目录是 apply 的**构建产物**，与「单向 apply」自洽；软链会让它变成源目录的实时视图。见 §2.2 第 6 条、§9.3 |
| 实现语言 | Rust（单静态二进制，零运行时依赖；机器上工具 shell 缺 mise/proxy，Python CLI 不可靠） |

### 非目标（v1 明确不做）

- 不做 lockfile / semver 解析（git skill 用 `ref` 钉死；marketplace plugin 以 `scan` 快照记录版本）
- 不做动态加载 `.so` 式第三方插件（见 §7 扩展路径）
- 不管理 harness 本身的安装、模型/凭证配置（只管能力：skills/MCP/plugins 拆出物）
- 不做 GUI

## 2. 本机现状（设计依据，2026-09-23 盘点）

- **skills 已有事实标准**：`~/.agents/skills/` 为共享目录（vercel-labs `npx skills` 约定），
  `~/.claude/skills`、`~/.qoder-cn/skills`、`~/.trae-cn/skills` 以符号链接接入。
  henv 复用此约定作为 skill 内容存储，不自造。
- **MCP 三家三格式**：claude-code `settings.json`（JSON）、codex `config.toml`（TOML）、
  kimi-code `mcp.json`（JSON，schema 不同）。
- **plugins 三家互不兼容**：claude-code（marketplace + `installed_plugins.json`）、
  kimi-code（`installed.json` + `managed/`）、codex（`plugins/`）。
- 用户已有 symlink 同步实践：`~/.codex/AGENTS.md`、`~/.kimi-code/AGENTS.md` →
  `~/.claude/CLAUDE.md`。

### 2.1 实测修正（2026-09-23，写实现计划前盘点本机）

以下 5 条经实测，修正或补充 §1–§3、§8 的前提。**实现计划以本节为准**；与上文冲突处本节胜出。

1. **claude-code 用户级 MCP 不在 `settings.json`，而在 `~/.claude.json` 顶层 `mcpServers`。**
   该文件 71KB、权限 0600、被 claude-code 频繁重写（含 `numStartups` / `pluginUsage` /
   `skillUsage` / `projects.*`）。`settings.json` 实际持有 `enabledPlugins`、
   `extraKnownMarketplaces`、`hooks`、`env`、`permissions`，**无** `mcpServers`。
   **决策（已确认）**：写路径 shell out 到 `claude mcp add-json`，不直接 merge 该文件——避免与
   harness 并发写竞争丢数据。读路径仍直接解析 `~/.claude.json`。

2. **三家都有 plugin 机制，且 bundle 自带 per-harness manifest。** superpowers 6.3.0 内含
   `.claude-plugin/plugin.json`、`.codex-plugin/plugin.json`、`.kimi-plugin/plugin.json`
   （另有 `.cursor-plugin` / `.hermes-plugin` / `.devin-plugin` / `.pi` / `.opencode` / `.agents`）；
   kimi-code 已从 GitHub 原生安装 superpowers（tag `v6.3.0`）。故 §1 所举「kimi-code 没有 plugin
   机制」不成立。**决策（已确认）**：v1 以 **bundle manifest 对账**为主（对账逻辑见 §9.4）；仅当目标
   harness 缺对应 bundle manifest 时才降级拆成散装 skill/MCP——否则会把 kimi 的原生安装拆成
   符号链接，是降级而非升级。
   注：kimi 的 bundle manifest 有两种形态——`.kimi-plugin/plugin.json`（superpowers）与根目录
   `kimi.plugin.json`（modern-web-guidance），探测须覆盖两者。

3. **plugin 身份用 entry 名，版本比较用「version 形」的 version 字段。** claude 插件缓存目录名
   实测出现 `6.3.0` / `6.4.1` / `1.0.0` / `b36e0829c6d0`（git sha）/ `unknown` 五种形态——所以
   version 串**不能作跨 harness 身份**（五种形态互不可比），身份用 entry 名（如 `superpowers`）；
   但**同一 harness 内**、清单期望的 `ref` 与现状的 `version` 同源，可比（§2.2 第 7 条）。
   **决策（2026-09-25 修订，判据按下文三条；实测依据与覆盖面见 §2.2 第 7 条）**：
   1. 两边都有 SHA 且**不相等** ⇒ `Different`——内容不同，最硬也最便宜的一刀（期望侧生产里
      恒无 SHA，此分支主要供单元测试与未来写路径）；
   2. 否则看 `version` 字段，但**只有两边都是「version 形」才可比**——去前导 `v` 后**含小数点**
      （`6.3.0` ≡ `v6.3.0`），归一化后相等 ⇒ `Same`、不等 ⇒ `Different`；期望侧 `version` 来自
      清单 `ref`，现状侧来自 harness 记账（claude 的 `version`、kimi 的 `github.ref.value`）；
   3. 其余 ⇒ `Unknown`（sha 形目录名、无 `ref` 的条目、非版本形 `ref` 如 `main` 等），且
      **`Unknown` 必须有可见出口**，不得再折进 in-sync。

   **为什么记账 `version` 是可信证人、无需再读安装自带 manifest**（§2.2 第 7 条）：claude 的
   `version` 是**定位符**——它必须等于生效目录名（19/19 实测），陈旧不了；`gitCommitSha` 才是
   无人依赖刷新的陈旧元数据。两者虽同在一个 JSON 记录里，可信度不同。kimi 的 `version` 位存
   `github.ref.value`，与期望侧 `ref` 本就是同一套拼写。
   **原判据「SHA 优先、version 兜底；Unknown 当 in-sync」为什么错**：它信了陈旧的 `gitCommitSha`
   作优先证人；而期望侧恒无 SHA，生产里 sha 分支不可达，比对落 `Unknown`，又被 `diff.rs` 与
   `Same` 一并折成 `Action::InSync`（`Action` 无「判不了」变体）——于是 plugin 版本检查等于关闭
   （实测 claude 装 6.4.1、清单 `ref: v6.3.0`，报 InSync）。修订要同时改判据（上面三条）和补出口。
   plugin 的跨 harness 身份仍用 **entry 名**；marketplace 各家不同，属 Spec 细节而非身份。
   （承载：判据落在 `compare_versions` 的 plugin 分支，`Spec::Plugin` **不加字段**；`Unknown` 的
   出口是 `Action` 的新变体；实现细节见随后的实现计划。）

4. **`~/.agents/skills` 共享约定只部分成立。** 实测存在反向链接与绕过：
   - `~/.agents/skills/mcp-builder → ~/.claude/skills/mcp-builder`（指回 harness）
   - `~/.agents/skills/superpowers → ~/.codex/superpowers/skills`
   - `~/.codex/skills/brainstorming → ~/.claude/plugins/cache/claude-plugins-official/superpowers/b36e0829c6d0/skills/brainstorming`
     （codex 直接链进 claude 插件缓存，完全绕过 `.agents`）
   - `~/.claude/skills/{trim-cot-leakage,webapp-testing}` 是不属于任何共享库的真实目录
   - `~/.kimi-code` **没有** skills 目录；kimi 的 skill 只随 plugin bundle 分发
     （`plugins/managed/<id>/skills/`）

   **决策**：`read_state` 必须传递解析多跳符号链接、识别断链与环，并对每个条目分类
   `Shared | HarnessLocal | PluginCache | Other`。该分类是 drift 判定（插件缓存内的 skill 不计
   drift）与 doctor 断链报告的基础。kimi 无用户级 skills 目录 ⇒ 散装 skill 对 kimi 判
   `Unsupported`，这正是 diff 输出里 `!` 行的真实来源。

5. **`serde_yaml` 已弃用**（crates.io 实测 `0.9.34+deprecated`）。§8 选型改用维护中的分支
   `serde_yaml_ng = "0.10"`（0.9 的直接 fork，API 兼容）。

### 2.2 实测修正（2026-09-25，写 Plan B 设计前盘点本机）

Plan B 是写路径，以下 7 条经实测，修正或补充 §2.1 与 §5–§8 的前提。**与上文冲突处本节胜出。**

1. **`~/.codex/skills` 那批链接已经大面积失效——26 条里断 20 条。** 它们直接指向 claude 插件缓存中
   **带构建哈希的目录**（`.../superpowers/b36e0829c6d0/skills/brainstorming`），而缓存现在的目录名是
   `6.3.0` / `6.4.1`。§2.1 第 4 条记下了这类链接的存在，没记下它们**已经断了**；断链是静默的，
   报告里那些 skill 看起来仍在，实际内容为零。
   ⇒ **链接必须指向稳定路径**，不能指向随构建变化的路径——这是 §9.5 拆解不做「直链 bundle」
   的原因。**进一步：本设计索性不建链**（第 6 条），所以这条失效模式整类消失。

2. **`~/.agents/skills` 有它自己的账本。** 库根存在 `~/.agents/.skill-lock.json`（`npx skills` 的
   lockfile，version 3），逐 skill 记 `source` / `sourceUrl` / `skillPath`（到 SKILL.md 的仓库内路径）/
   `skillFolderHash`（commit sha）/ `installedAt` / `updatedAt`，外加一个 `dismissed` 表。它已经解决了
   monorepo 子目录问题。**但库里 17 个条目只有 1 条在这个账上**（`microsoft-foundry`），其余是人手放的。
   ⇒ henv **不写**这个文件——它是别人的账本格式，且没有「来自本机某个 bundle」这个概念；
   henv 自己记账（§9.3）。同理，§4 里 git 源的 `repo`/`path`/`ref` 三件套与它的
   `sourceUrl`/`skillPath`/`skillFolderHash` 是**两套并行的事实**，本轮不与它对齐。

3. **`~/.codex/config.toml` 的 merge 风险不是「注释」。** 实测 35 行、**注释 0 行**，但含
   `experimental_bearer_token`、`[projects."/Users/nava/Code/llm/rag/first-rag"]`（**带引号、含点、含空格**
   的表头）以及 `[hooks.state."/Users/nava/.codex/hooks.json:stop:0:0"]` 这类结构。
   ⇒ 风险在**表结构与字面量形态**，不在注释。§8 选 `toml_edit` 做**子树级**改动（只碰
   `[mcp_servers]`，其余字节原样）是对的；整文件重新序列化会重排键序、改写内联表形态。

4. **`~/.kimi-code/mcp.json` 结构简单**：`{ mcpServers: { <name>: { command, args } } }`，无嵌套怪形。
   普通 JSON merge 足够，不需要特殊处理；只需保留未知的顶层键。

5. **两家配置目录都没有 `.bak` 残留** ⇒ `.bak` 是 Plan B 新引入的约定，**没有历史包袱要兼容**，
   命名与轮转规则可以自由定（§9.7）。

6. **同一个 plugin 在本机有 3 份已安装副本、2 个版本——「按名字建一个共享条目」这个前提是错的。**
   实测 obra/superpowers 的五处落点：

   | 位置 | 版本 | 来源 |
   |---|---|---|
   | claude 缓存 `…/superpowers/6.4.1` | **6.4.1** | claude marketplace，**claude 当下运行的就是它** |
   | claude 缓存 `…/superpowers/6.3.0` | 6.3.0 | 旧版本残留 |
   | `~/.codex/superpowers`（git checkout） | 6.3.0 @ `b36e082` | 手工 clone |
   | `~/.kimi-code/plugins/managed/superpowers` | 6.3.0 @ `b36e082` | kimi 装 github tag，`installedSha` 记在 `installed.json` |
   | `~/.agents/skills/superpowers` → 第 3 项 | 6.3.0 | 反向软链 |

   同一上游仓库、同一批 skill 名、**两个钉死的版本**；codex 与 kimi 停在同一个 commit。
   （副产物：第 1 条那些断链里的 `b36e0829c6d0` 正是 kimi 记的 `installedSha`——claude 的缓存
   目录曾按 sha 命名，后改为版本号命名，这是断链的完整机理。）

   ⇒ **§9.5 说的「内容来源：已安装的 bundle」在这些机器上不唯一。** 若 store 按裸名建一个
   `superpowers` 条目，拆解时只能挑一个版本，等于**静默给某一家换版**（把 claude 降到 6.3.0，
   或把 codex/kimi 升到 6.4.1）——正是本项目立身要防的事。两条结论：
   **(a) 拆解的来源 harness 必须在 manifest 里显式写死**，不许从「哪些 harness 装了它」推断；
   **(b) 交付不用共享条目**，逐 harness 从各自的源拷（§9.3）。

7. **claude 的 `version` 是定位符、`gitCommitSha` 才是陈旧字段——判据应信前者。** 实测
   （2026-09-25）：

   | 证据 | claude | kimi | codex |
   |---|---|---|---|
   | 生效安装 | `…/cache/claude-plugins-official/superpowers/6.4.1` | `…/managed/superpowers` | `…/plugins/cache/openai-api-curated/superpowers/1dc19589` |
   | 记账 `version` | **6.4.1** | `v6.3.0`（=`github.ref.value`） | 无（`version: None`） |
   | 记账 sha | `gitCommitSha = b36e082…` | `installedSha = b36e082…` | 无（`sha: None`） |

   **同一份 JSON 记录里的两个字段，可信度不同**，三处实测：

   - **`version` 是定位符，陈旧不了**：claude 的 `version` 必须等于生效目录名（`installPath`
     的末段），否则 claude 找不到自己的安装；实测 **19/19 全部相等**。目录名又来自内容本身——
     `superpowers/6.4.1/` 那份树内的 `plugin.json` 就写 6.4.1，旁边还并存 `6.3.0/`。
   - **`gitCommitSha` 是元数据，无人依赖它刷新**：claude↔kimi 两边 sha 同为 `b36e082…`，但
     生效安装是 6.4.1 / 6.3.0——**sha 相同、内容不同**，claude 记的 sha 不描述它装的内容。
     机理：插件缓存是版本仓库（`superpowers/{6.3.0,6.4.1}` 并存），`installPath` 指向生效那份，
     6.3.0 那份仍在盘上，「`gitCommitSha` 是 6.3.0 时期写下、升级时未随 `installPath` 刷新」
     是可依的机制（机制为推断，字段与内容不符为实测）。
   - **kimi 的 `version` 位存 `github.ref.value`**，即「当初从哪个 ref 装的」，与期望侧的 `ref`
     本就是同一套拼写，不需任何中间人即可比。

   **后果（两条，第 2 条更要紧）**：§2.1 第 3 条的原判据「SHA 优先」把 `gitCommitSha` 当可信
   证人，于是 sha 相同 ⇒ `Same`、报 InSync。但**更关键的是**：期望侧从不记 sha（`lower` 恒
   `sha: None`），所以生产里那个 sha 分支根本**不可达**；plugin 比对实际落进 `Unknown`，而
   `diff.rs` 把 `VersionMatch::Same | Unknown` 一并映射成 `Action::InSync`、`Action` 又只有五个
   变体**没有「判不了」出口**——于是 plugin 的版本检查在 claude/kimi 上**等于关闭**（实测：claude
   装 6.4.1、清单 `ref: v6.3.0`，diff 报 InSync）。判据修订要同时修两处：改用 `version` 判据
   （第 3 条），并补 `Unknown` 的可见出口。

   **覆盖面（实测）**：改用 `version` 后，可比的是「两边都是 version 形（含小数点）」的情形——
   claude 的 11 个 semver 形目录名、kimi 的 2 个有 `ref` 的，都可直接比；落 `Unknown` 的是
   claude 的 8 个 sha 形目录名（如 `db467cc56673`，离线无可比之物）、kimi 的 3 个 zip 装的
   （无 `github` 块 ⇒ `version: None`）、codex 全部（`config.toml` 不记版本）、以及清单里无 `ref`
   或非版本形 `ref`（如 `main`）的条目。⇒ 残差非 0，第 3 条那个可见出口是配套的必需品，不是
   锦上添花。

## 3. 架构

单 crate 二进制 `henv`，内部五层：

```text
henv/
  src/
    main.rs          # clap 入口：scan / diff / apply / doctor
    manifest.rs      # env.yaml 反序列化 + 校验（serde）
    capability.rs    # ★ 归一化能力模型 —— 全系统的通用语
    adapters/
      mod.rs         # 编译期注册表：name -> Box<dyn HarnessAdapter>
      claude_code.rs # settings.json + plugins/ 拆解
      codex.rs       # config.toml
      kimi_code.rs   # mcp.json + plugins/installed.json
    store/
      skills.rs      # skill 源解析与交付：~/.agents/skills 约定 + git 缓存 + 逐 harness 拷贝
      plugins.rs     # plugin 捆绑包 → 能力束的拆解器（含「来源 harness」消歧）
    diff.rs          # desired vs actual → Plan
    secrets.rs       # ${ENV_VAR} 引用的保留与存在性校验
```

### 核心类型

```rust
/// 归一化能力：所有 harness 的差异都被压平到这个模型上
struct Capability {
    kind: Kind,            // Skill | Mcp | Command | Agent | Hook
    name: String,
    origin: Origin,        // Manifest | Plugin { name } | Adopted { harness }
    spec: Spec,            // Skill{path} / Mcp{command,args,env_refs} / Hook{...}
}

/// 每家 harness 对一个能力的接纳度 —— 「拆解降级」的判定点
enum Support {
    Native,                       // 原生支持（claude 的 plugin、各家的 skill/MCP）
    Degraded { as_kind: Kind },   // 降级安装（plugin 里的 MCP → 写进 mcp.json）
    Unsupported { reason: String }, // 装不了（hooks → kimi-code），进 doctor 报告
}

trait HarnessAdapter {
    fn name(&self) -> &str;
    fn detect(&self) -> bool;                                  // 本机是否装了这家
    fn read_state(&self) -> anyhow::Result<Vec<Capability>>;   // 扫描 → 归一化
    fn supports(&self, cap: &Capability) -> Support;
    fn apply(&self, plan: &Plan) -> anyhow::Result<ApplyReport>;
}
```

### 三个架构决策

1. **plugin 不是特殊公民**：manifest 里的 plugin 条目在解析阶段就被 `store/plugins.rs`
   拆成一组带 `Origin::Plugin{name}` 的 Capability。之后 diff/apply/doctor 只认
   Capability，adapter 不需要理解 plugin 概念——kimi-code adapter 面对的就是几条
   skill 和几条 MCP，天然实现降级。
2. **规划是纯函数，执行是另一层**：`plan_apply(desired, actual) -> Plan` 不碰盘，
   `Plan{install, update, remove, undeliverable}` 幂等可重跑；执行层是唯一碰盘的地方
   （Plan A 的形状 `diff::compute` 同构，Plan B 按 §9.2 补执行层）。`scan` 是同一管道的
   反向——actual 归一化后 merge 回 env.yaml。
3. **写配置用 merge 不用整文件覆盖**：各 harness 配置文件里用户手写的其他字段必须保留；
   写前落 `.bak`。密钥永远以 `${VAR}` 引用形态流转，`secrets.rs` 只校验存在性、
   绝不展开进任何产物。

## 4. env.yaml 清单 schema

```yaml
version: 1

harnesses:                      # 参与分发的目标；detect() 不到会警告而非报错
  claude-code: { enabled: true }
  codex:       { enabled: true }
  kimi-code:   { enabled: true }

skills:
  dingtalk-mail:
    source: agents-dir          # 已在 ~/.agents/skills，只做登记 + 拷贝分发
  superpowers:
    source: git
    repo: obra/superpowers
    path: skills                # 仓库内子目录（monorepo 型）
    ref: v6.3.0                 # 钉版本；ref 就是锁
  my-draft:
    source: local
    path: ~/dev/skills/my-draft # 本地开发中的 skill，改完源要跑 apply 才分发出去

mcps:
  context7:
    command: npx
    args: ["-y", "@upstash/context7-mcp"]
    env:
      CONTEXT7_API_KEY: ${CONTEXT7_API_KEY}   # 只允许引用，字面量密钥校验报错
    targets: all                # all 或显式列表 [claude-code, codex]

plugins:
  superpowers-bundle:
    source: claude-marketplace  # 只有 claude-code 原生理解它
    marketplace: obra/superpowers-marketplace
    entry: superpowers
    source_harness: claude-code # 拆解时从哪一家的已安装 bundle 取内容。**必填**，见 §9.5 ——
                                # 本机同一 plugin 有 3 份、2 个版本，不写死就会挑错版本
    degrade:
      hooks: report             # 拆出后装不下的能力：report（进报告）| skip（静默丢弃）
      commands: report
```

Schema 决策：

1. **三类条目、一个出口**：`skills`/`mcps`/`plugins` 是用户书写视角；解析后全部变成
   Capability 流，`targets`、`degrade` 字段语义在三类上统一。
2. **密钥语法即门禁**：`env` 值必须匹配 `${VAR}`，`manifest.rs` 校验阶段拒绝任何字面量。
3. **degrade 策略按 plugin 条目声明**，不做全局开关：不同 bundle 对 hooks 的依赖程度不同。
4. **`targets: all` 是默认值**：显式列表只用于定向分发场景。

## 5. 数据流与错误处理

### 四个子命令的管道

```text
diff（只读，一切的基础）
  env.yaml ─parse→ desired: Vec<Capability>（plugin 已拆解）
  各 adapter.read_state() ─→ actual[harness]
  对每个 (capability, harness)：
    targets 不含它 → 忽略
    supports() = Native/Degraded → 与 actual 比对 → install | update | in-sync
    supports() = Unsupported → undeliverable（按 degrade 策略 report/skip）
  actual 有、desired 无 → drift
  输出 Plan + 人类可读表格

apply = diff → 确认 → 执行
  默认打印 Plan 后要求确认（--yes 跳过）
  依赖序：先全局解析源（git fetch 到缓存、定位各 bundle），再按 harness 分组执行——
  每家先拷贝 skill 内容进自己的 skills 目录，最后写自己的 MCP/plugin 配置（§9.2）

scan（收养，反向管道）
  各 adapter.read_state() → 归一化 → 与 env.yaml 比对
  新发现的能力 → 生成条目追加进 env.yaml（Origin::Adopted）
  只追加不修改已有条目；--dry-run 只打印

doctor（体检）
  diff 的 undeliverable + 环境校验：
  ${VAR} 引用的变量当前是否存在、henv 交付的拷贝是否与源脱节（§9.4）、
  各 harness 配置文件是否可解析、detect() 失败的 enabled harness
```

### drift 的处理方向

actual 有而 desired 无的能力，`diff` 报告但 **apply 默认不动它**（可能是临时试验品，
工具不该悄悄删）。删除需显式 `apply --prune`。清单定义"必须有的"，不定义"不许有的"。

### 部分失败与事务性

不做跨 harness 全局事务（文件系统写入无法原子），做**单 harness 内尽量原子 + 失败隔离**：

- 写配置文件：临时文件写好 → fsync → rename；rename 前落 `.bak`，单文件层面原子
  （**`.bak` 只适用于配置文件**——skill 拷贝交付不落 `.bak`，理由见 §9.7）
- 一个 adapter 中途失败：该 harness 记为 `partial`，其余照常执行，`ApplyReport` 汇总
- 重跑即恢复：apply 是 f(desired, actual)，失败后再 apply 就是修复路径，无 rollback 命令
- skill 内容下载（git clone/fetch）失败：该 capability 标记 `failed`，不阻塞其他条目

### diff 输出形态

```text
$ henv diff
claude-code   ✓ in-sync  skills:42 mcps:6 plugins:3
codex         + install  mcp/context7, mcp/browser-use
              ~ update   skill/dingtalk-mail (ref drift: v2.1 → v2.2)
              ! undeliverable  hook/superpowers:pre-commit (no hook support)
kimi-code     + install  mcp/context7
              - drift    mcp/experimental-thing (未在清单中，--prune 可删)

Plan: 0 conflict, 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift, 0 unknown
```

切 harness 前跑一眼 `henv diff`，`!` 行就是"什么会失效"。

## 6. 测试策略

- **fixture 假 HOME**：`tests/fixtures/` 存三家 harness 的脱敏配置树快照
  （settings.json / config.toml / mcp.json / plugins 目录）。adapter 测试在
  `tempfile::TempDir` 里重建假 HOME 后读写，绝不触碰真实 `~`。
- **roundtrip 性质**：每个 adapter 满足 `read_state(apply(caps)) ⊇ caps`
  （在该 harness 可表达的范围内）——装进去再扫出来必须还在。
- **幂等测试**：同一 Plan apply 两次，第二次必须 no-op（diff 全 in-sync）。
- **merge 安全**：fixture 配置文件里预埋手写无关字段，apply 后断言原样保留。
- **golden diff**：固定的 desired+actual 组合 → 期望 Plan 与期望终端输出
  （`insta` 快照），diff 报告格式变化会被显式捕捉。
- **secrets 校验单测**：字面量密钥进 `env` 必须解析报错；`${VAR}` 引用在
  apply/doctor 全程保持未展开。
- **真实环境冒烟**（`#[ignore]`，手动跑）：对真 HOME 执行 `henv diff`（只读），
  确认三家现状识别正确。

## 7. 扩展路径（预记录，不预支实现）

- **新增 harness** = `adapters/` 新模块实现 trait + `mod.rs` 注册一行，不碰核心。
- **第三方扩展**（远期）：把 trait 输入输出序列化为协议，外置成子命令式 adapter
  （类似 git/gh 的 `xxx-helper` 模式）。v1 不做。
- **skill 内容安装**：~~v1 可 shell out 到 `npx skills`（已验证的生态）或自行
  git clone~~ → **已定：自行 git clone（shell out 到系统 `git`），不用 `npx skills`**。
  接口上收敛在 `store/skills.rs`，见 §9.12。放弃 `npx skills` 的原因是它把 node 变成
  运行时依赖，与 §1 的「单静态二进制、零运行时依赖」冲突。

## 8. 技术选型清单

| 用途 | crate |
|---|---|
| CLI | `clap`（derive） |
| 清单/配置解析 | `serde` + `serde_yaml_ng` + `serde_json` + `toml`；Plan B 加 `toml_edit`（保注释 merge） |
| 错误 | `anyhow`（应用层） |
| diff 输出 | ~~`similar`~~（Plan A 未用；render 直接拼字符串，Plan B 同样不需要） |
| 快照测试 | `insta` |
| 临时目录 | `tempfile` |
| git 操作 | **shell out 到系统 `git`**（已定，见 §9.12；不引入 `git2`） |

注 1：codex `config.toml` 的 merge 用 `toml_edit` 而非 `toml`，以保留用户注释与格式。
注 2：**Plan A 已交付，实际引入的依赖比上表少**——`similar` 未使用（render 直接拼字符串），
`git2` 未使用。`toml_edit` 在 Plan A 被明令禁止（原话是「**本计划**都不需要」），**该禁令是
Plan A 范围内的，不是永久封禁**；Plan B 按上表解禁，用于 §9.6 的子树级 merge。
注 3：Plan B 的 git 操作定为 **shell out 到系统 `git`**，不引入 `git2`（§9.12）。也不用
`npx skills`——它会给「单静态二进制、零运行时依赖」重新引入 node 运行时（§9.12）。

## 9. Plan B：写路径（apply）

**日期：2026-09-25。状态：已与用户逐节确认。** 本节兑现 §3 里
`store/skills.rs`、`store/plugins.rs` 与 `HarnessAdapter::apply` 三个已占位未实现的东西。

**落地进度（2026-09-26）：本节分 B1/B2/B3 三块做，B1 与 B2 已完成。** B1 落的是**骨架与 skills
交付**：`henv apply` 子命令（渲染计划 → 冲突则中止 → 一次 y/N 确认，`--yes` 跳过）、规划与
执行分离（`plan_for` + `apply::execute`）、`agents-dir` 与 `local` 两种 skill 源的**逐 harness
真拷贝交付**、`~/.local/state/henv/provenance.json` 账本（记「往哪家放了什么、内容哈希是多少」）、
只读守卫改成 `src/apply/` + `src/store/` 写入白名单，以及 §9.3 的断链自愈（`SkillEntry::content_path`）。

**B2 落的是 git 源与 `henv skills list`**（2026-09-26）：`source: git` 的解析（`store::git::resolve`，
shell out 系统 git、不引 `git2`，缓存 `~/.cache/henv/git/<repo-hash>/`，`owner/repo` 简写展开到
`https://github.com/<owner>/<repo>.git`）、判定模式穿线（**`diff` 只读缓存不联网 / `apply` fetch**）、
来源标识 `repo@ref@sha`、§9.4 的 `stale`（上游刷不动就沿用缓存，标出来而不是报失败）、
`henv skills list`（§9.8），以及一条 B1 遗留缺陷的收口：**没解析出来的源不再被静默跳过**
（skill 的 install 报成可见的 `undeliverable`，apply 侧也报失败）。只读守卫因此**收窄而非删除**：
`Command::new(` 在 `src/store/git.rs` 与 `src/testutil.rs` 之外仍然禁止（§9.9）。

**2026-09-26 终审后的两处修正**（都是接口层的事实错误，不是设计变更）：§9.4 的解析配方原本
写的是裸 `rev-parse <ref>`，而 `fetch` 不移动本地分支——照那个配方实现会让**默认分支静默停在
旧提交上并报「新鲜」**、非默认分支则永远解不出来；现已改为按 `origin/<ref>` 解析。以及
`skills list` 的长选项缺省被 clap 生成为 `--git-ref`，与本节承诺的 `--ref` 不符，现已对齐。

**B2 未做的 / B3**：MCP 三家写入、plugin bundle 拆解、`degrade:` 真正生效（§9.1 的声明性占位，
至今仍不改变报告）、`--prune`、`--harness <name>`。因此本节的 `.bak`、失败隔离与事务性（§9.7）
在 B2 结束时有 skills 交付那一条路径实装，配置文件的写路径尚未涉及。

**B3 拆成三份做**（2026-09-26 定的边界）：**B3a = MCP 三家写入**（§9.6，含其新增的所有权
裁决与账本键修订）；**B3b = plugin bundle 拆解 + `degrade:` 生效**（§9.5，会连带 §2.2 第 7 条
的 kimi `installed.json` 解析与 `optional_bool`）；**B3c = `--prune` 与 `--harness <name>`**。
拆开的理由是 B3a 独立于另外两份——它只碰配置文件、不碰 bundle，而 §9.8 的 `--prune` 要删东西、
语义最重，不适合与「第一次改用户配置文件」混在一次改动里。

### 9.1 范围

**做**：skills 四种来源的交付（`agents-dir` / `local` / `git` / plugin bundle 拆解）；MCP 三家写入；
`plan_apply` 规划层 + 执行层、`.bak`、失败隔离；逐 harness 拷贝交付与来源记账；只读守卫改造成写入白名单
（Plan A 移交清单里的硬触发项）。

**不做**（明确留给后续）：plugin 原生安装（claude marketplace / kimi `installed.json`+`managed/` /
codex `plugins/`）；`scan`；`doctor`；为 kimi 构造 bundle；除 `git` 外的网络获取；
**把既有软链批量迁移成拷贝**（§9.3 已说明为什么不做）。

> **本节多处提到 `doctor`，那指的都是后续的 doctor，不是本计划要建的。** 本计划的义务是
> **把信息产在报告里**（`ApplyReport` / diff 输出），使 doctor 落地时不必回头改数据结构。
> 读到时不要把「doctor 里要报 X」理解成「本计划要实现 doctor」。

**kimi 的散装 skill 与拆解出的 skill 仍判 undeliverable**——kimi 没有用户级 skills 目录
（§2.1 第 4 条），**没有可交付的目标**。这是设计约束不是遗漏，**报告里必须写清楚**。

### 9.2 架构：规划与执行分离

```text
源解析（唯一的 I/O 前置）: git fetch / 定位 bundle → ResolvedSources
plan_apply(desired, actual_by_harness, ledger, resolved) -> ApplyPlan   // 纯函数，不碰盘
apply::execute(plan, &Home)                              -> ApplyReport // 唯一落盘的地方
```

与 Plan A 的 `diff::compute` 同构。**规划层可上 insta golden**，「apply 该做什么」能在不写任何
字节的前提下测到底。`resolved` 作为入参而非在规划内解析，是为了让「staleness 判定」（§9.4）也
落在纯函数里可测——I/O 全部挤在那一步前置里。

执行序**固定**，且源解析不属于任何 adapter：

1. **源解析**（全局一次）：git 源 fetch 到 `~/.cache/henv/git/`、定位各 plugin 的已安装 bundle。
   只读 + 只写缓存，产出「这个能力的内容在本地哪个目录」。
2. **每家 adapter 交付**：把内容**拷贝**进自己的 skills 目录，再写自己的配置文件。

`HarnessAdapter::apply()` 只负责第 2 步；它拿到的是解析好的本地内容路径，不需要知道内容
来自 git 还是 bundle 拆解。

**没有「store 物化」这一步**——这是与上一版设计最大的差别。`~/.agents/skills` 退为
**源库**（`agents-dir` 源的内容就在那里），不再是交付的中转站；交付一律从源**直接拷进
目标 harness**（§9.3）。

### 9.3 交付：逐 harness 真拷贝

`~/.agents/skills` 是 §2 的事实标准，henv **复用它当源库，但不把它当交付目标**——
`agents-dir` 源的内容本来就在那里，其余源的交付不再经由它中转（§9.2）。

**不变式一：henv 在 harness 的 skills 目录里从不创建软链，一律真拷贝。**

| 来源 | 内容从哪来 | 交付到 |
|---|---|---|
| `agents-dir` | `~/.agents/skills/<name>` | 各目标 harness 的 skills 目录（拷贝） |
| `local` | manifest 声明的本地路径 | 同上 |
| `git` | `~/.cache/henv/git/<repo-hash>/<path>` | 同上 |
| bundle 拆解 | 该条目**显式指定**的那家 harness 的已安装 bundle | 同上 |

**为什么不建链**（三条，按分量排序）：

1. **henv 的模型是「清单为源、单向 apply」（§1）。** 软链让 harness 目录变成源目录的实时
   视图，等于在执行期偷偷开了一条反向通道——改源不跑 apply 也生效。拷贝让
   「harness 目录 = apply 的构建产物」字面成立，两种状态下 `diff` 的语义一致。
2. **§2.2 第 6 条证明「一个名字对应一份内容」不成立。** 同一个 plugin 在 claude 和 kimi 上
   是 6.4.1 与 6.3.0。共享条目按名字做键就必须挑一个版本，必然静默给某一家换版；
   逐 harness 拷贝天然容得下两个版本，不需要额外机制。
3. **§9.11 的未验证假设随之消失**——「codex 会不会加载软链形式的 skill」不必再赌。

代价（**已确认接受**）：`agents-dir` 与 `local` 这类可变源，改完源目录要跑 `henv apply`
才在 harness 里生效。本机现有 14 个 dingtalk skill 目前是软链、即时生效，改造后不再即时。

**不变式二：henv 只动自己记过账的条目。**

**这条在拷贝模式下从「谨慎」升级为承重。** 旧设计里覆盖一条软链是廉价操作；现在覆盖的是
**用户目录里的真实内容**（本机就有 `~/.claude/skills/mcp-builder`、`trim-cot-leakage`、
`webapp-testing` 三个手工放的真目录），覆盖即毁数据。规则不变但不再有折扣：
**只碰账本里记着的条目，其余一律不动**，报告里列出来供人决定；
**同名冲突时停下报错，绝不覆盖**。

**账本**：`~/.local/state/henv/provenance.json`——搬到 henv 自己的家。旧位置的理由是
「挨着 store」，而 store 已不是交付目标；账本记的是**逐 harness 的交付**，属于 henv 的
机器状态，放 XDG state 才对。逐条目记：目标 harness、目标路径、来源类型、来源标识
（git 的 `repo@ref@sha` / bundle 的 `bundle@from@version@sha`）、交付内容哈希、交付时间。

> 它**不是** §1「非目标」里说的 lockfile，别混淆：它不解版本、不做依赖求解，只回答
> **「这个文件是不是 henv 放的、从哪来、放的时候是什么内容」**。它是不变式二的落实载体，
> 没有它就无法区分「henv 放的」和「用户手放的」。文件丢失的后果是退化成「什么都不敢动」
> （安全的一侧），不是误删。

**位置为什么是 XDG state 而不是 `~/.agents/`**（那里有 `.skill-lock.json` 这个就地先例，
是个很容易被想到的落点）：账本记的不止 `agents-dir`，还有 henv 往 `~/.claude/skills`、
`~/.codex/skills` 的每一次交付（以及将来的 MCP 写入）——把「harness 侧交付」记在
`~/.agents/` 里是类别错误。更要紧的是**生命周期耦合**：`~/.agents` 是用户随时可能整体
重建的「skill 库」，一旦重建，henv 对 claude/codex 做过什么就被一起忘掉了。**账本必须活得比
任何一个内容目录长**，所以它不放在任何内容目录里。

**另一种做法（已否决）**：不要中央账本，改成在每个交付目录里放一个标记文件，让
「这目录是不是 henv 的」就地可答——账本丢了也不要紧。否决理由：它要求「harness 会忽略
skill 目录里多出来的点文件」，而这正是 §9.11 刚记下的那类赌注，且赌输的代价是 harness
把交付目录解析坏。中央账本对 harness **零足迹**，它赌输时只是「什么都不敢动」。

**判定单位是「内容是否等价」，不是「形态是否相同」**——这条把不变式一和不变式二缝在一起。
`diff` 问的是「目标位置上的内容是否等于该源的当前内容」，而**不看它是真目录、软链还是别的**：

- 等价 ⇒ `in-sync`。**本机那 14 个 dingtalk skill 正落在这里**：它们是指向 `agents-dir` 源的
  软链，内容等价，所以判 in-sync，henv 不写任何字节。它们不在账本里（不是 henv 建的），
  按不变式二本来也不该动——两条规则给出的答案一致，不需要为它们开特例。
- 不等价/缺失 ⇒ `install`/`update`，`apply` 交付**拷贝**。所以 §2.2 第 1 条那种断链若被
  清单声明，下次 `apply` 会被修成真拷贝而**不是修回软链**——旧的脆弱形态在重跑中自愈，
  不会因为「用户以前建过」而被永久保留。

**由此产生一个刻意的边界**：henv **只保证自己交付的是拷贝，不对不是自己建的东西做形态迁移**。
本机那 14 条软链因此会**原样留着**，终态可能是「一半拷贝一半软链」。想让它们变拷贝，需要一次
显式的迁移动作——**本轮不做**，`scan` 也不做（scan 的产物是清单条目，不是形态改写）。
这个取舍是刻意的：批量改写用户既有目录是本设计里最危险的操作，宁可留着不一致，也不主动
去动一批自己没建过的东西。等哪条链断了、被 apply 修成拷贝，它会自然替换掉。

这条判据有一个刻意的取舍：**用户手改过 henv 交付的副本时，`apply` 仍会覆盖**（它是构建产物，
`cargo build` 语义），但账本里的交付哈希对不上，报告里显式列出 `modified` 一行。不做
「脏副本保护」——那会让 apply 不再是幂等函数，且 `--force` 之类的逃生门又是新的状态。

### 9.4 过期与重建

拷贝相对直链多付出的代价，必须真的实现，否则 harness 里的副本会**静默过期**——用户改了源、
以为已生效，实际没有。三种源各有判据：

- **git 源**：账本记着上次解析该 `ref` 得到的 sha。重跑时 `git fetch` 后按 **`origin/<ref>`**
  解析比对，**不一致才重拷**。这里必须是 `origin/<ref>`：`fetch` 只更新远程跟踪引用、
  **不移动本地分支**，对着本地分支解出来的是 **clone 那天**的提交——上游前进了也照样报
  「新鲜」，正是本条要防的静默过期；非默认分支更是本地根本没有，只活在 `origin/<ref>` 里。
  裸 `<ref>` 只作为回退，覆盖 `fetch` 管不到的形状（tag 进的是 `refs/tags/`、裸提交 sha）。
  网络不可达则沿用缓存内容、把该条目标 `stale`，**不报失败**——离线也要能用。
- **bundle 拆解**：比对账本记的 `bundle@from@version@sha` 与该 bundle 当前的值。
  **纯本地比对**，不需要网络。**「当前的值」取安装自带的 manifest 声明，不取 harness 的
  `gitCommitSha`**——§2.2 第 7 条实测 claude 的 `gitCommitSha` 陈旧（与安装内容不符），拿它比对
  会让一个过期的副本看起来是新的。（`version` 定位符本身可信，但「过期」要回答的是「副本内容
  对不对」，内容的权威声明在副本自己的 manifest 里，不在 harness 的记账里。）
- **`agents-dir` / `local`**：直接对源目录内容算哈希与账本比。这同时覆盖了「改了源忘了 apply」
  ——`diff` 报 `update`。

重新拷贝一律「先拷进同层临时目录 → rename 到位」，失败时**旧内容原样保留**。

**过期是逐 harness 独立的**（拷贝模式独有的性质）：claude 的副本可以过期而 kimi 的是新的。
这既是代价也是能力——§2.2 第 6 条那种「claude 6.4.1 / kimi 6.3.0」在两个 harness 各说各话时，
用拷贝能如实表达，用共享条目不能。报告里按 harness 分行列出各家的过期状态。

### 9.5 bundle 拆解

**与 §2.1 第 2 条决策的关系（正面处理，避免读成自相矛盾）**：§2.1 第 2 条说「v1 以 bundle
manifest 对账为主，**仅当目标 harness 缺对应 manifest 时才降级拆解**」——它反对的是**自动**
降级（那会把 kimi 的原生安装拆成符号链接，是降级而非升级）。本节要的是**用户显式请求**的拆解：
manifest 里写了 `degrade:` 块，或该 plugin 条目的 `targets` 含一个装不了它的 harness。
两者不冲突——前者禁止自动，后者是声明式意图。**没有显式意图就绝不拆解。**

- **来源 harness 必须在 manifest 里显式写死**（新增字段 `source_harness:`，与 `degrade:` 同级；
  §4 需相应补一行）。**禁止**从「哪些 harness 装了它」反推来源：§2.2 第 6 条实测到同一个 plugin
  在本机有 3 份、2 个版本，反推必然挑错版本，把某一家静默换版。写死之后语义才唯一。
- **内容来源**：`source_harness:` 那家的**已安装 bundle**（adapter 已能定位：claude 的
  `installed_plugins.json` 与缓存路径、codex 的 `plugins/`、kimi 的 `managed/` + `installed.json`）。
  **不从 marketplace 或远端重新拉取**——那是 plugin 原生安装的活，超出 §9.1 的范围。
- **交付**：把该 bundle 里 `skills/<name>/` 直接拷进目标 harness 的 skills 目录。
  **不经过 `~/.agents/skills` 中转**（§9.3）。
- **装不下的部分**（hooks / commands / agents / MCP）按该 plugin 条目的 `degrade:` 策略
  `report`（进报告）或 `skip`（静默丢弃）。**这是 `degrade:` 第一次真实生效**——
  Plan A 里它只是声明性占位。
- **拆解目标是 kimi 时**：仍 undeliverable（§9.1）——kimi 没有用户级 skills 目录。
  注意这与「来源是 kimi」是两件事：`source_harness: kimi-code` 而目标是 claude 是
  **本设计最想要的用法**（把 kimi 独有的 plugin 携带的 skill 给 claude 用）。

### 9.6 MCP 三家三种写路径

| harness | 机制 | 理由 |
|---|---|---|
| claude-code | shell out `claude mcp add-json -s user` | §2.1 第 1 条：`~/.claude.json` 71KB、0600、被 harness 频繁重写，直接 merge 会与它并发写竞争丢数据 |
| codex | `toml_edit` **只改** `[mcp_servers]` 子树 | §2.2 第 3 条：其余字节原样，表结构与字面量形态不动 |
| kimi-code | serde_json 改 `mcpServers` 键，保留其他顶层键 | §2.2 第 4 条：结构简单，无需特殊处理 |

`env` 一律以 `${VAR}` 引用形态写入，**永不展开**（§3 决策 3）。

**claude 那条必须显式指定 user 级 scope——已核实，不再只是意图。** 本机实测
`claude mcp add-json` 的 `-s, --scope <scope>` 取 `local|user|project`，**默认 `local`**；
而 `local` 落在 `projects.<cwd>.mcpServers`，正是读路径明确不读的项目级位置（§2.1 第 1 条）。
⇒ 实现必须写死 `-s user`。漏了它的后果不是报错而是**静默无用**：写进去的东西 henv 自己
都看不见，下一次 `diff` 照旧报 `install`，而用户以为已经装好了。兜底测试断言 argv 含
`-s user` 且子命令名是 `add-json`——**这条不能靠「跑在某个 cwd 恰好也行」蒙过去**，
`local` 与 `user` 的差别只在配置落在哪个文件里，与 cwd 无关地恒真。

**claude 的 `update` 是两条命令：先 `remove -s user <name>`，再 `add-json -s user <name> <json>`。**
实测（隔离 HOME）：`add-json` 对**已存在**的名字直接失败——`MCP server <name> already exists in user
config`，退出码非 0，没有 `--force` 之类的覆盖开关；`remove` 只摘掉 `mcpServers` 里那一个键，其余
顶层键（`numStartups` 等）原样留下。所以「改一个 henv 已经交付过的 MCP」在 claude 上必须走两步，
而 `remove` 对不存在的名字会以 `No MCP server named "<name>" in user scope` 退出 1——**不吞它**：
走到 `update` 说明 diff 刚看到过它，这时 remove 失败是真故障，照 §9.7 报 partial。

**写路径与读路径必须同址。** adapter 只暴露「这家 MCP 配置在哪、什么格式」
（`mcp_target() -> Option<McpTarget { path, format }>`，与 `skills_target_dir()` 同一形状），
字节一律由 `src/apply/` 写。这样 `read_mcps` 与写入指向**同一个文件**——「写进去了、
读不出来」这类自相矛盾不是被测试抓出来的，是结构上不可能的。claude 是唯一的例外形状：
位置由 `claude` 自己决定，但仍钉在 `~/.claude.json` 顶层（`-s user`），与读路径一致。

**只覆盖账本里记着的条目（§9.3 不变式二在 MCP 上的落实）。** `diff` 对 MCP 的裁决因此
不只看 spec 是否相同，还要问「这条是不是 henv 放的」：

| 现状 vs 清单 | 账本 | 裁决 |
|---|---|---|
| 相同 | 有/无 | `in-sync`——与 skills 那条一致：内容相同就不看账本（本机那 14 条用户自建软链靠的就是这条，§9.3） |
| 不同 | **无** | `conflict`——同名但不是 henv 放的，`apply` 拒绝并点名 |
| 不同 | 有 | `update`；账本哈希与现状对不上时额外注记「被手改过」（仍然覆盖，§9.3 已声明该取舍） |

没有这一条，MCP 就是不变式二的缺口：用户在 `~/.claude.json` 里自己配的 `ctx` 会被清单里
同名的 `ctx` **静默覆盖**——而那是用户的配置，不是 henv 的构建产物。今天 `diff` 走的是
`compare_versions`、完全不看账本，所以报的是 `update`；B3a 把这个裁决点补上。
代价（**已接受**）：用户想让 henv 接管一个自己手配过的同名 MCP，得先自己删掉再 `apply`。

`update` 的 from/to 用**可读命令行**（`npx -y @upstash/context7-mcp`）而不是哈希：MCP 的
「内容」就是它的命令行，哈希在这里只是账本用的凭证。凭证由
`store::hash::hash_mcp_spec(command, args, env)`（与 `hash_dir` 并列）产出，读写两侧共用
一份实现——两侧各算各的，正是「账本哈希对不上」这类谜题的来源。

**为什么是三个原生字段而不是 `hash_spec(&Spec)`**（2026-09-26 与实现对齐）：`Spec` 没有
`Serialize`，为哈希去 derive 它会把序列化形状变成对外契约；而 `&Spec` 版本还要为另外六个
变体（`Skill`/`Plugin`/`Hook`/`Command`/`Agent`/`Only`）写永远走不到的 arm。三个原生字段
顺带让「拿一个 skill spec 算 mcp 哈希」在类型上不可表达。**env 的值参与哈希**——与
`mcp_summary`（只渲染键名，§3 决策 3）不同，账本要认得出「同一条 server 换了一个环境变量
引用」是改动。

**账本键是 `(harness, kind, name)`，不是 `(harness, name)`。** 同名跨 kind 是清单明确允许的
（`skills.foo` 与 `mcps.foo` 并存），而 `record` 的语义是「同名覆盖旧的」——键里不带 kind 的话，
写一个 mcp `foo` 会**静默抹掉**同名 skill `foo` 的账本记录，那条 skill 随即从「henv 放的」
退化成「用户手放的」，下一次 `diff` 报 `conflict` 而不是 `in-sync`，`modified` 检测一起失明。
这个缺陷在 B2 之前不暴露，因为那时只有 skill 会被记账。

### 9.7 事务、备份与失败隔离（对 §5 的修订）

§5 的「写配置文件：临时文件写好 → fsync → rename；rename 前落 `.bak`」笼统覆盖了所有写入。
本节收窄，**这是对 §5 的修订**：

- **配置文件**（codex `config.toml`、kimi `mcp.json`）：同目录临时文件 → fsync → rename。
  **`.bak` 只在它还不存在时落一次，名字 `<file>.henv.bak`**（§2.2 第 5 条：两家都没有 `.bak`
  残留，命名无历史包袱）。单槽是刻意的：`.bak` 的语义是「henv 介入**之前**的样子」，
  无条件覆盖会让它退化成「上一次的样子」——第二次 `apply` 之后，盘上再没有任何东西能回到
  原状，而给配置留 `.bak` 的全部理由就是兜住**不可重建**的东西。不加时间戳轮转：henv 没有
  清理机制，会堆积。名字里带 `.henv.` 是为了不踩用户自己的 `<file>.bak`。**文件不存在时
  无需备份**——没有「之前」可言。
  **临时文件的命名**：`.<name>.henv-tmp-<pid>`（与 skill 交付的 `sibling()` 同一形状），
  写在目标同目录、写完 fsync 后 rename 到目标；中途失败就删掉它，不留半截。带 pid 让两次
  并发 `apply` 的临时文件名不同——它们仍会在 `rename` 上竞争（单文件写入无法避免），但
  不会互删对方的临时文件。
  **权限只保留、不放宽**：临时文件按目标的**现有**权限位落盘（目标不存在则 `0600`，
  **不跟 umask**），`rename` 因此不会把用户的 `0600` 悄悄放宽成 `0644`——`~/.codex/config.toml`
  里坐的正是明文 `experimental_bearer_token`。`.bak` 经 `fs::copy` 天然带过原权限位。
  **claude 是例外且无备份**：MCP 走 shell out `claude mcp add-json -s user -- <name> <json>`
  与 `claude mcp remove -s user -- <name>`（§9.6；那个 `--` 不可省——名字来自清单，而清单
  放行 `-h` 这类形状，没有 `--` 时真 CLI 会把它读成 `--help`、打印帮助并退出 0），
  文件由 `claude` 自己写，henv 既不该也不能在旁边落 `.bak`。这是选 shell out 的附带代价
  ——**已接受**，因为另一条路（直接 merge `~/.claude.json`）会与 harness 并发写竞争、
  丢的是更多数据。
- **skills 拷贝交付**：同层临时目录 → rename 到位，**不落 `.bak`**——内容可从来源重建，
  备份它只是噪音；`.bak` 只留给**不可重建**的东西。注意拷贝**替换的是整个目录**：目标已存在
  且是 henv 账上的条目时先 rename 到临时名再换入，不做逐文件 merge（用户手改的副本会被整体
  覆盖，这是 §9.3 已声明的取舍）
- **henv 不创建任何符号链接**（§9.3 不变式一与其后的边界），也不改写既有的。唯一要处置的
  既有链接是**配置文件自己**：`write_config` 的 `rename` 换掉的是链接本身，而备份那步的
  `fs::copy` 反而**跟随**它——两边对链接的处置相反，用户的 chezmoi/stow 关系会被静默销毁、
  内容只以普通文件形式留在 `.bak` 里。所以目标是链接时**拒绝写入并报错**，链条归用户自己。
  父目录是软链不受影响：`rename` 落在解析后的目录里，照常写。
- **失败隔离**：一个 adapter 中途失败 → 该 harness 记 `partial`，其余继续，`ApplyReport` 汇总
- **重跑即修复**，无 rollback 命令（§5 不变）

### 9.8 CLI 面与交互边界

```text
henv apply [-m env.yaml] [--yes] [--prune] [--harness <name>]
henv skills list --repo <url> [--ref <r>] [--path <p>]        ← Plan B 新增
```

`skills list`：列出仓库里所有含 `SKILL.md` 的目录（目录名 + 仓库内相对路径），按相对路径排序，
`--path` 限定只看某个子目录，`--ref` 缺省用仓库默认分支。它是「用户给一个 git 地址」时的第一步。

**只读的准确含义**（2026-09-26 与实现对齐）：它要 clone 才能看，而 clone 必须落在一个目录里。
落在 **henv 自己缓存下的一个进程独占子目录**：`~/.cache/henv/git/list-<repo-hash>-<pid>/`，
命令返回前删掉（成功失败都删）。**除这个临时目录外不写任何东西**——不碰 harness 目录、
不碰账本、不碰 `~/.cache/henv/git/` 下 `resolve` 用的那些缓存条目。放缓存下而不是系统临时
目录，是为了少一处要解释的路径；独占名字（repo-key + pid）让并发的两次 list 不打架。

**`--prune` 受不变式二（§9.3）约束**：它删除的范围**只有 provenance 里记着的条目**。
§5 说「actual 有而 desired 无」就报 drift，但「actual 有」在 harness 目录里包含**用户手建的**
东西——`--prune` 若按字面把它一起删，就正好毁掉 §9.3 承诺绝不碰的那批。所以：
**prune = 撤销 henv 自己做过的事**（删掉自己交付的拷贝、从配置里摘掉 MCP），
**不是**「让 harness 目录等于清单」。非 henv 条目一律只报不删，`--prune` 也不例外。

**交互边界（已确认）**：henv 除 `apply` 的 y/N 确认外**不提问**。「这个 skill 加给哪几家」
这类对话由 agent 承担，产物是 env.yaml 里的一行 `targets:`（§4 已有字段，不引入新概念）。
**理由**：交互式 CLI 的流程难以稳定测试，而 agent 对话正是用户的实际用法。

### 9.9 只读守卫的去向

**不是删掉，是收窄。** Plan A 的 `tests/read_only_guard.rs` 断言「`src/` 里除测试夹具外零写
API」；Plan B 把它改成**白名单**：`src/apply/` 与 `src/store/` 允许写 API，**其余地方一律不许**。
Plan A 那句「绝不改你的配置」于是以更强的形式保住——从「哪里都不许写」变成「只许在这一小块写」。
Plan A 已把 `"Command::new("` 埋进 `FORBIDDEN` 当绊线，**Plan B 的第一行 shell out 会直接撞上
它，届时的正确做法是给写路径圈出允许位，而不是把该模式从 `FORBIDDEN` 里删掉**。

### 9.10 测试策略（对 §6 的补充）

- **规划层纯函数**：`plan_apply` 不碰盘，上 insta golden
- **执行层在 `TempDir` 真跑**：断言文件内容、`.bak` 存在、`partial` 汇总
- **交付形态是真拷贝**：断言目标是**真目录**（`symlink_metadata` 非 link），且内容与源逐字节相同
- **形态无关的等价判定**（§9.3）：预埋一个「指向该 agents-dir 源的软链」，断言 `diff` 报
  in-sync 且 `apply` **不写任何字节**；再把它换成断链，断言转为 `install` 且 apply 交付的是**拷贝**
- **多版本 bundle 消歧**：fixture 放两份同 id、不同版本的已安装 bundle，断言 `source_harness:`
  决定取哪一份，且**不会**因为另一家的版本更新而改动目标
- **roundtrip**：`read_state(apply(caps)) ⊇ caps`（§6 原本就要求）
- **幂等**：apply 两次，第二次的 plan 为空
- **merge-safe**：fixture 预埋 `experimental_bearer_token` 与 `[projects."含点路径"]`，apply 后断言原样
- **git 源不联网**：fixture 用 `git init` 出的本地 `file://` 仓库
- **冲突即停**：预埋一条同名非 henv 条目（**真目录、有内容**），断言 apply 报错且该目录
  **逐字节未被修改**——这条是拷贝模式下最重要的回归测试
- **脏副本**：手改一个已交付的副本，断言 apply 后副本被还原为源内容，且报告里出现 `modified`
- **只读守卫**：白名单之外零写 API；保留 Plan A `scanner_is_not_vacuous` 的反证思路
- **绝不碰真实 `~`**（§6 不变）

### 9.11 一处未验证假设——本设计选择把它设计掉，而不是赌它

上一版设计压在一个无法从数据断言的假设上：**codex 运行时是否真的加载符号链接形式的 skill。**
§2.2 第 1 条显示那批链接断了 20 条而无人察觉，说明「链过去就能用」**从未被验证过，只是被假设**。

当时的处置是加一条 `#[ignore]` 的真机冒烟测试去钉死它。**现在改为不依赖它**：§9.3 定为真拷贝，
「harness 能不能读到」就不再是一个关于软链行为的赌注——读取一个普通目录是无可争议的。这条
假设连同它的冒烟测试需求一并消失。

留这段是为了记下**取舍的方向**：一个压在未验证行为上的设计，先问「能不能不依赖它」，
再问「怎么验证它」。后者的成本（真机冒烟、且失败要返工整条交付路径）远高于前者。

### 9.12 已定的选型（对 §7、§8 的落地）

- **git 源用 shell out 到系统 `git`**，不引入 `git2`。design §7 曾把这条留到实现时定，
  现在定为 shell out：零新依赖，且完全在我们控制下。
- **不用 `npx skills`**。§2 说 `~/.agents/skills` 是它创建的约定，但拿它当安装器会把 node
  变成运行时依赖，与 §1「单静态二进制、零运行时依赖」的定位冲突；且 §2.2 第 2 条显示它的
  账本与本项目要记的东西不是一回事。
- **`toml_edit` 解禁**（§8 注 2），只用于 §9.6 的子树级 merge。
- **git 的 monorepo 子目录抽取**（`repo` + `path` + `ref`）由 henv 自己做，落在
  `store/skills.rs`；缓存放 `~/.cache/henv/git/<repo-hash>/`，避免每次 apply 重新下载。
