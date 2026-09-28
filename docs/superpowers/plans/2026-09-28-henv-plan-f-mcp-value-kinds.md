# Plan F：MCP 的 env / header 值分「引用」与「字面量」（方案，待确认）

**状态：已实施（2026-09-28）。** 与提案的差别只有两处补充：`McpValue::from_text_or_literal`
（claude 的读路径确实需要「认得出就解析、认不出当字面量」这个语义，测试夹具也用它）与
`McpValue::required_var`（doctor 的缺值判据）。哈希布局另有 `the_hash_layout_is_pinned`
把字节钉死；真机上那份账本在改造后仍全 in-sync，是「不需要迁移」的现场证据。

**Goal：** 让「引用」与「字面量」在模型里就是两种东西，于是**长得像引用的字面量永远不会等于
引用**——修掉 `diff` 会报假 `in-sync` 的那个洞，并让 `Bearer` / `:-` 从字符串前缀升级成模型的
一等属性。

## 1. 洞的证据（2026-09-28 实测）

codex 的 `http_headers` 值是**字面量**（它原样发出去，只有 `env_http_headers` /
`bearer_token_env_var` / `env_vars` 才是「从环境取值」，见 §9.6）。所以下面这份配置是**坏的**：
codex 会把 `${CONTEXT7_API_KEY}` 这段文本当 Authorization 发出去。

```toml
[mcp_servers.context7]
url = "https://mcp.context7.com/mcp"
[mcp_servers.context7.http_headers]
Authorization = "${CONTEXT7_API_KEY}"   # 字面量，不是引用
```

而 henv 现在说它没问题：

```console
$ henv diff -m env.yaml          # 清单里写的是 headers.Authorization: ${CONTEXT7_API_KEY}
codex         ✓ in-sync  mcps:1
```

**为什么会这样**：值在模型里就是 `String`。清单侧的 `${CONTEXT7_API_KEY}` 与配置里那段**文本**
逐字符相等，`mcp_verdict` 的第一条判据（`dt == at`）直接判 `InSync`——读路径当初并没有记住
「这一格是字面量」这件事，因为在它看来两边都是同一串字符。

**两个后果**：

1. **升级前 henv 交付过的条目修不好**：清单没变、diff 说没问题，那段坏文本就一直在那儿。
2. 谁手工往 codex 的 `http_headers` 里写一段像引用的文本，henv 也会说它同步。

（claude 没有这个问题：它的 `env` / `headers` **真的会展开** `${VAR}`，所以那段文本在它那儿
就是对的东西。问题只在「值字段是字面量」的两家上，且只在文本恰好长得像引用时。）

## 2. 方案：值变成两态

```rust
/// 一条 env / header 的值。
pub enum McpValue {
    /// 清单里的写法。三种形态各自记着，渲染/翻译都不再靠解析字符串。
    Ref {
        var: String,
        /// `Bearer ${VAR}` —— 对应 codex 的 `bearer_token_env_var` 与 kimi 的 `bearerTokenEnvVar`
        bearer: bool,
        /// `${VAR:-}` —— 「变量可以缺」
        optional: bool,
    },
    /// 其它一切：**只可能来自读回来的配置**（清单门禁拒字面量）。
    Literal(String),
}
```

配套两个方法：

- `McpValue::to_text()` —— 渲染回清单里的写法（`${K}` / `${K:-}` / `Bearer ${K}` /
  `Bearer ${K:-}`），字面量就是它自己。**哈希用它**（见 §4 的迁移讨论）。
- `McpValue::from_manifest_text(&str)` —— 清单 → `Ref`；解析不了就是 bug（门禁已经拦过），
  返回 `Err` 而不是造一个 `Literal` 出来。

`McpTransport` 的两张表随之变成 `BTreeMap<String, McpValue>`。

### 2.1 语义核心：**读路径按字段决定哪边是引用**

这是整个方案里唯一「必须逐家写清」的地方——同样一串文本，在哪一格是引用、在哪一格是字面量，
由**字段**决定，不是由文本长什么样决定：

| harness | 字段 | 读回来是什么 |
|---|---|---|
| claude-code | `env` / `headers` | claude 会展开，所以**按引用解析**；解析不了（如 `X-Foo: bar`）→ `Literal` |
| codex | `env` / `http_headers` | **一律 `Literal`**（codex 就是原样发） |
| codex | `env_vars = ["K"]` | `Ref { var: K }`（同名转发） |
| codex | `env_http_headers = {H = "V"}` | `Ref { var: V }`（值那一侧是变量名） |
| codex | `bearer_token_env_var = "K"` | `Ref { var: K, bearer: true }` |
| kimi-code | `env` / `headers` | **一律 `Literal`** |
| kimi-code | `bearerTokenEnvVar = "K"` | `Ref { var: K, bearer: true }` |

于是上面那份坏配置读回来是 `Literal("${CONTEXT7_API_KEY}")`，与清单要的
`Ref { var: CONTEXT7_API_KEY }` **结构上不可能相等** → 不再有假 in-sync。

### 2.2 各层怎么变

- **manifest**：YAML 侧照旧是字符串（schema 不变），门禁照旧（§9.6 那条：`${VAR}` /
  `${VAR:-}` / `Bearer ${VAR}` / `Bearer ${VAR:-}`）。变的只是 `lower` 多做一步
  `McpValue::from_manifest_text`。**用户写的东西一个字符都不用改。**
- **writer**：收到的 desired 永远是 `Ref`（清单来的），所以每个 writer 的取值分支从
  「解析字符串」变成「match 枚举」；`Literal` 出现在 desired 里是 bug → 报错，不猜。
  codex / kimi 那几条「表达不了」的判断（`mcp_expressible`）同理变成结构判断，
  比现在的字符串前缀判断短得多。
- **渲染**（`mcp_summary`）：`Ref` → `to_text()`（引用照原样，`Bearer` 与 `:-` 都看得见），
  `Literal` → `***`。`shown_value` 这个「看起来像不像引用」的启发式**删掉**。
- **scan**：`adoptable` 变成「只要有一个 `Literal` 就跳过这条，并说明是哪一格」——
  不用再重新判字符串；`shape()` 写引用时用 `to_text()`。
- **doctor**：只查 `Ref { optional: false, .. }`（`${VAR:-}` 仍然不报）。
- **diff**：判据本来就是整体相等，天然受益；`from`/`to` 用摘要渲染。

## 3. 明确不改的

- 清单 schema、字段名、`targets`、`degrade`、账本键：**一个都不动**。
- 用户要写的东西不变：还是 `` K: ${VAR} ``、`Authorization: Bearer ${VAR}`。
- 「绝不把字面量写进配置」这条不变：`Literal` 永远只出现在**读回来的**那一侧。

## 4. 迁移与兼容（这是要你拍板的地方）

哈希目前对「值文本」取字节。方案里让 `hash_mcp_spec` 对 `to_text()` 取字节——对今天所有
合法形态来说，`to_text()` **逐字节等于**当初写进清单、写进账本的那串文本，所以：

- **账本不用迁移、不会集体报「副本被手改过」**。
- 升级前 henv 交付过的坏条目：账本哈希仍对得上 → `diff` 报 `update` → `apply` 把它改写成
  `env_http_headers` / `bearer_token_env_var`。**自愈，且不需要用户做什么。**
- 别人手工配的、恰好长得像引用的条目：账本里没有 → 报 `conflict`（§9.6 既有语义：同名不是
  henv 放的，`apply` 拒绝并点名）。**从「假 in-sync」变成「要你处理」，这是行为变化**，
  也是对的：henv 不能证明那份配置是它写的，就不该悄悄覆盖。

`Literal` 与「恰好等于引用文本的字面量」在哈希上会撞（同一串字节）——只影响「副本被手改过」
那句注记的精度，不影响该不该改写，故接受。

## 5. 工作量与文件清单

中等改动。改模型 + 三个读路径 + 三个写路径 + 四处消费者（渲染 / scan / doctor / hash），
外加测试夹具（`BTreeMap<String, String>` → `BTreeMap<String, McpValue>`，约 30 处）。

```
src/capability.rs      类型 + summary + 单元测试
src/lower.rs           清单文本 → Ref
src/store/hash.rs      to_text 取字节（保持字节兼容）
src/diff.rs            判据/渲染（改动小）
src/adapters/{claude_code,codex,kimi_code,mod}.rs   读路径 + mcp_expressible
src/apply/{mcp_claude,mcp_json,mcp_toml}.rs         写路径按枚举分派
src/{scan,doctor,testutil}.rs                       消费者与夹具
tests/mcp_end_to_end.rs 等                          夹具与端到端
docs/design.md         §3 决策 3、§9.6 的取值表
```

## 6. 验收（要新增的测试）

1. **假 in-sync 的回归锁**：codex `http_headers.Authorization = "${K}"` + 清单
   `Authorization: ${K}` → 必须报 `update`（今天报 `in-sync`，§1 那条实测就是它）。
2. 同一个 `(kind, name)` 上 `Literal("${K}") ≠ Ref{var: "K"}` —— 结构不等，且**相等性**
   不被字符串撞上（含 `Bearer` 与 `:-` 两种形态）。
3. 读路径逐格：claude 的 `headers` 解析成 `Ref`、`X-Foo: bar` 成 `Literal`；codex 的
   `http_headers` 恒 `Literal`、`env_http_headers` / `env_vars` / `bearer_token_env_var`
   恒 `Ref`；kimi 同理。
4. 账本字节兼容：对今天所有合法形态，新旧 `hash_mcp_spec` 结果**逐一相等**。
5. 端到端：坏条目（有账本）→ `apply` 自愈成正确形态 → 再 `diff` in-sync；坏条目（无账本）
   → `conflict`，`apply` 不动它。
6. 真机：`codex doctor` 对交付结果仍然报 `bearer token env var … is not set` /
   `header env var … is not set`。

## 7. 备选方案（不采用）与被否原因

- **只在 diff 里特判「codex 的 `http_headers` 是字面量」**：读路径早就把字段名丢了，
  不知道这个值是从哪一格来的；要特判就得先记住——那正是本方案。
- **给字面量加转义前缀（如 `\0`）**：把语义编码进字符串，任何一处忘了转义就是静默错误，
  与「结构化」背道而驰。
- **门禁也收字面量**：看起来能让 `scan` 收下一切，但那等于允许把明文密钥写进清单——
  §3 决策 3 的底线。

## 8. 需要你拍板的点

1. 现在做，还是先搁着？（洞目前**只在**「升级前交付过的条目」与「手工写的伪引用」上触发，
   你机器上两种都没有。）
2. `Literal` 出现在 desired 里（正常不可能）→ 报错（我倾向这个），还是给 claude 原样写下去？
3. 字面量的渲染继续用 `***`（我倾向），还是带上长度/前缀之类的提示？
