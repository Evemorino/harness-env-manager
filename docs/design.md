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
   机制」不成立。**决策（已确认）**：v1 以 **bundle manifest 对账**为主、SHA 优先；仅当目标
   harness 缺对应 bundle manifest 时才降级拆成散装 skill/MCP——否则会把 kimi 的原生安装拆成
   符号链接，是降级而非升级。
   注：kimi 的 bundle manifest 有两种形态——`.kimi-plugin/plugin.json`（superpowers）与根目录
   `kimi.plugin.json`（modern-web-guidance），探测须覆盖两者。

3. **version 字符串不可作为身份。** claude 插件缓存目录名实测出现 `6.3.0` / `6.4.1` / `1.0.0` /
   `b36e0829c6d0`（git sha）/ `unknown` 五种形态。claude 记 superpowers=`6.4.1`、kimi 记
   `v6.3.0`，但两者 `gitCommitSha` / `installedSha` 同为
   `b36e0829c6d0140e93cfef2ca599b1b07d4a7797`。
   **决策**：`diff` 身份比较 **SHA 优先、version 兜底**；一侧有 SHA 另一侧只有 version 时判
   `Unknown` 并按 in-sync 处理（只读工具不应误报）。plugin 的跨 harness 身份用 **entry 名**
   （如 `superpowers`）；marketplace 各家不同，属 Spec 细节而非身份。

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
      skills.rs      # ~/.agents/skills 共享目录约定（symlink/复制）
      plugins.rs     # plugin 捆绑包 → 能力束的拆解器
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
2. **apply = f(desired, actual) 纯函数**：先 `read_state` 再算
   `Plan{install, update, remove, undeliverable}`，幂等可重跑；`scan` 是同一管道的
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
    source: agents-dir          # 已在 ~/.agents/skills，只做登记 + 链接分发
  superpowers:
    source: git
    repo: obra/superpowers
    path: skills                # 仓库内子目录（monorepo 型）
    ref: v6.3.0                 # 钉版本；ref 就是锁
  my-draft:
    source: local
    path: ~/dev/skills/my-draft # 本地开发中的 skill，直接链接

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
    degrade:
      hooks: report             # 拆出后装不下的能力：report（进 doctor）| skip（静默丢弃）
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
  按 harness 分组执行，adapter 内部依赖序：先 skill 内容落 ~/.agents/skills，
  再链接进各家目录，最后写 MCP/plugin 配置

scan（收养，反向管道）
  各 adapter.read_state() → 归一化 → 与 env.yaml 比对
  新发现的能力 → 生成条目追加进 env.yaml（Origin::Adopted）
  只追加不修改已有条目；--dry-run 只打印

doctor（体检）
  diff 的 undeliverable + 环境校验：
  ${VAR} 引用的变量当前是否存在、~/.agents/skills 链接是否断链、
  各 harness 配置文件是否可解析、detect() 失败的 enabled harness
```

### drift 的处理方向

actual 有而 desired 无的能力，`diff` 报告但 **apply 默认不动它**（可能是临时试验品，
工具不该悄悄删）。删除需显式 `apply --prune`。清单定义"必须有的"，不定义"不许有的"。

### 部分失败与事务性

不做跨 harness 全局事务（文件系统写入无法原子），做**单 harness 内尽量原子 + 失败隔离**：

- 写配置文件：临时文件写好 → fsync → rename；rename 前落 `.bak`，单文件层面原子
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

Plan: 3 install, 1 update, 0 remove, 1 undeliverable, 1 drift
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
- **skill 内容安装**：v1 可 shell out 到 `npx skills`（已验证的生态）或自行
  git clone；接口上收敛在 `store/skills.rs`，两者可替换。

## 8. 技术选型清单

| 用途 | crate |
|---|---|
| CLI | `clap`（derive） |
| 清单/配置解析 | `serde` + `serde_yaml` + `serde_json` + `toml`（`toml_edit` 保注释 merge） |
| 错误 | `anyhow`（应用层） |
| diff 输出 | `similar` |
| 快照测试 | `insta` |
| 临时目录 | `tempfile` |
| git 操作 | `git2` 或 shell out `git`（实现时定，倾向 shell out 减依赖面） |

注：codex `config.toml` 的 merge 用 `toml_edit` 而非 `toml`，以保留用户注释与格式。
