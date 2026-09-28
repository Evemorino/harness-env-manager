//! `${VAR}` 引用的校验与存在性检查。
//!
//! 密钥永远以引用形态流转：本模块只判断「语法是否为纯引用」与「变量是否存在」，
//! **绝不展开值进任何产物**（spec §3 决策 3）。

/// 标识符规则：`[A-Za-z_][A-Za-z0-9_]*`。`$` `{` `}` 都不在里面，所以嵌套与拼接形态
/// 由这一条一并拒掉，不需要单独的守卫；空串也由 `chars.next()` 挡下。
/// 手写校验而非 regex，以守住「不引入 regex 依赖」的约束。
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `"${FOO}"` → `Some("FOO")`；任何非纯引用形态 → `None`。
///
/// 这是**目标 harness 的取值字段能表达的那种引用**：codex 的 `env_vars` /
/// `env_http_headers` 收的是变量名，`${FOO:-}` 的「缺失时取空值」语义在那儿没有落点，
/// 所以翻译层与写入器用严格版。**清单门禁**用的是 [`parse_manifest_ref`]。
pub fn parse_ref(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("${")?.strip_suffix('}')?;
    is_ident(inner).then_some(inner)
}

/// **清单门禁**的规则：`"${FOO}"` 与 `"${FOO:-}"`（**空**默认值）→ `Some("FOO")`，
/// 其余任何形态 → `None`。
///
/// 单给空默认值开一个口，是因为真实世界的配置里有它——claude 自己那份 context7 plugin
/// 就写着 `${CONTEXT7_API_KEY:-}`，把它照抄进清单不该报错。**非空**默认值照旧拒绝：
/// `${FOO:-sk-live-abc}` 会把密钥从后门带进清单，那正是这道闸要挡的东西；空默认值不带
/// 任何字面量，语义只是「这个变量可以缺」。
///
/// 注意它同时是 `diff` 渲染摘要时的判据（见 `capability::McpTransport::summary`）：
/// 认得出的引用照原样显示，认不出的一律 `***`。
pub fn parse_manifest_ref(value: &str) -> Option<&str> {
    if let Some(var) = parse_ref(value) {
        return Some(var);
    }
    let inner = value.strip_prefix("${")?.strip_suffix(":-}")?;
    is_ident(inner).then_some(inner)
}

/// `"Bearer ${FOO}"` / `"Bearer ${FOO:-}"` → `Some("FOO")`；其余 → `None`。
///
/// 这个前缀是**一等写法**，不是「任意字面量 + 引用」的开口：它一一对应 codex 的
/// `bearer_token_env_var` 与 kimi 的 `bearerTokenEnvVar`（两家都发
/// `Authorization: Bearer <token>`）。没有它，那两家原生的 bearer 通道就没有入口——
/// 而那是 kimi 唯一能从环境取凭据的方式（§9.6）。
pub fn parse_bearer_ref(value: &str) -> Option<&str> {
    parse_manifest_ref(value.strip_prefix("Bearer ")?)
}

/// 严格版：只吃 `Bearer ${FOO}`（不带默认值）。codex / kimi 的 bearer 字段收的是**变量名**，
/// 「变量缺失时取空值」那层语义在它们那儿没有落点，所以翻译层用这一版。
pub fn parse_bearer_var(value: &str) -> Option<&str> {
    parse_ref(value.strip_prefix("Bearer ")?)
}

/// **门禁的完整判据**：`${VAR}`、`${VAR:-}`，以及带 `Bearer ` 前缀的那两种。
/// 清单校验（[`validate_env`]）与 CLI 的 `--env` / `--header` 共用它——两层曾经各判一套，
/// 症状是命令行放行、落盘才报（§9.6）。
pub fn is_manifest_value(value: &str) -> bool {
    parse_manifest_ref(value).is_some() || parse_bearer_ref(value).is_some()
}

/// 校验单个 env / header 值。错误信息包含 key 名但**绝不回显 value**（防止密钥进日志/终端）。
/// 措辞里不出现任何可能被当作 fixture 值的词（`literal` / `secret` 等），下游测试
/// 会用「值不得出现在消息里」做断言。
pub fn validate_env(key: &str, value: &str) -> Result<(), String> {
    if is_manifest_value(value) {
        return Ok(());
    }
    Err(format!(
        "env `{key}` must be a ${{VAR}} reference, optionally prefixed with `Bearer `; \
         `${{VAR:-}}` with an empty default is allowed too (value not shown)"
    ))
}

/// 返回在环境中缺失（或为空）的变量名，保持输入顺序。
pub fn missing(vars: &[String], lookup: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    vars.iter()
        .filter(|v| lookup(v).map(|s| s.is_empty()).unwrap_or(true))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wellformed_reference() {
        assert_eq!(parse_ref("${CONTEXT7_API_KEY}"), Some("CONTEXT7_API_KEY"));
        assert_eq!(parse_ref("${_X1}"), Some("_X1"));
    }

    #[test]
    fn rejects_literal_secret() {
        assert_eq!(parse_ref("sk-abc123"), None);
        assert_eq!(parse_ref(""), None);
    }

    #[test]
    fn rejects_malformed_reference_syntax() {
        assert_eq!(parse_ref("${}"), None);
        assert_eq!(parse_ref("$VAR}"), None);
        assert_eq!(parse_ref("${VAR"), None);
        assert_eq!(parse_ref("${VAR} "), None);
        assert_eq!(parse_ref(" ${VAR}"), None);
        assert_eq!(parse_ref("${A-B}"), None);
        assert_eq!(parse_ref("${1ABC}"), None);
        // 嵌套/拼接一律拒绝，避免半展开
        assert_eq!(parse_ref("${A}${B}"), None);
        assert_eq!(parse_ref("prefix-${A}"), None);
        // 包裹形态是唯一合法外壳：少 $、少 {}、多一个 } 都不算引用
        assert_eq!(parse_ref("$FOO"), None);
        assert_eq!(parse_ref("{FOO}"), None);
        assert_eq!(parse_ref("${FOO}}"), None);
        assert_eq!(parse_ref("${FOO}${BAR}"), None);
    }

    #[test]
    fn validate_env_names_the_offending_key() {
        assert!(validate_env("K", "${K}").is_ok());
        let err = validate_env("CONTEXT7_API_KEY", "sk-live-123").unwrap_err();
        assert!(
            err.contains("CONTEXT7_API_KEY"),
            "error names the key: {err}"
        );
        assert!(
            !err.contains("sk-live-123"),
            "error must not echo the secret: {err}"
        );
    }

    /// 上面那条只挡得住「整串回显」：把值截断成前缀 `sk-` 或首字符塞进消息，
    /// `!err.contains("sk-live-123")` 照样绿，而密钥已经进日志了。威胁模型是日志与
    /// 终端回滚缓冲，所以这里把**整条消息**钉死，并跨一组取值复用同一条断言——
    /// 其中 `"reference"` 让「把值拼进消息」的写法几乎不可能凑巧等于 WANT。
    #[test]
    fn validate_env_message_is_pinned_and_never_carries_the_value() {
        const WANT: &str = "env `K` must be a ${VAR} reference, optionally prefixed with `Bearer \
                            `; `${VAR:-}` with an empty default is allowed too (value not shown)";
        for v in ["sk-live-123", "s", "", "K", "reference", "  spaced  "] {
            assert_eq!(
                validate_env("K", v).unwrap_err(),
                WANT,
                "value {v:?} leaked or message drifted"
            );
        }
    }

    /// 门禁收哪些形态：裸引用，以及**空**默认值（claude 自己那份 context7 plugin 就写着
    /// `${CONTEXT7_API_KEY:-}`，照抄进清单不该报错）。
    #[test]
    fn an_empty_default_is_accepted_and_a_non_empty_one_is_not() {
        assert_eq!(parse_manifest_ref("${K}"), Some("K"));
        assert_eq!(parse_manifest_ref("${K:-}"), Some("K"));
        assert!(validate_env("K", "${K:-}").is_ok());

        // 非空默认值会把密钥从后门带进清单——照旧拒。
        assert_eq!(parse_manifest_ref("${K:-sk-live-abc}"), None);
        assert!(validate_env("K", "${K:-sk-live-abc}").is_err());
        // 严格版（翻译层用的那个）不吃 `:-`：codex 的取值字段收的是变量名。
        assert_eq!(parse_ref("${K:-}"), None);
        assert_eq!(parse_ref("${K}"), Some("K"));
    }

    /// `Bearer ${VAR}` 是**一等写法**：它一一对应 codex 的 `bearer_token_env_var` 与
    /// kimi 的 `bearerTokenEnvVar`（两家都发 `Authorization: Bearer <token>`），
    /// 而后者是 kimi 唯一能从环境取凭据的通道。
    #[test]
    fn a_bearer_prefix_is_a_first_class_shape_not_an_arbitrary_literal() {
        // 门禁收两种默认值形态的 bearer。
        assert_eq!(parse_bearer_ref("Bearer ${K}"), Some("K"));
        assert_eq!(parse_bearer_ref("Bearer ${K:-}"), Some("K"));
        assert!(validate_env("Authorization", "Bearer ${K}").is_ok());
        assert!(validate_env("Authorization", "Bearer ${K:-}").is_ok());
        // 翻译层的严格版不吃默认值：bearer 字段收的是变量名。
        assert_eq!(parse_bearer_var("Bearer ${K}"), Some("K"));
        assert_eq!(parse_bearer_var("Bearer ${K:-}"), None);

        // 只认这一个前缀，别的词一律不认——否则「引用 + 任意字面量」就从这道闸漏进来了。
        assert_eq!(parse_bearer_ref("Basic ${K}"), None);
        assert_eq!(parse_bearer_ref("bearer ${K}"), None);
        assert_eq!(parse_bearer_ref("Bearer sk-live-abc"), None);
        assert_eq!(parse_bearer_ref("Bearer ${K:-sk-live-abc}"), None);
        assert!(validate_env("Authorization", "Basic ${K}").is_err());
        assert!(validate_env("Authorization", "Bearer sk-live-abc").is_err());
    }

    #[test]
    fn missing_reports_only_absent_vars_in_input_order() {
        let vars = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let got = missing(&vars, &|k| if k == "B" { Some("x".into()) } else { None });
        assert_eq!(got, vec!["A".to_string(), "C".to_string()]);
    }

    #[test]
    fn missing_treats_empty_value_as_absent() {
        let vars = vec!["A".to_string()];
        let got = missing(&vars, &|_| Some(String::new()));
        assert_eq!(got, vec!["A".to_string()]);
    }

    /// 原 fixture 是 `["A","B","C"]`——本身升序、单字符、全大写，于是「先排序再过滤」
    /// 「按长度排序」「查表前统一大写」三个变异体全部存活，只有「反转」被抓。这里换成
    /// 非升序、混合大小写、含下划线与多字符的名字，并且断言**送进 lookup 的名字序列**
    /// 本身：名字在被查之前就变形（大写、trim、截断），这条也立刻变红。
    #[test]
    fn missing_preserves_input_order_and_name_exactly() {
        // `missing` 收的是 `&dyn Fn`，闭包不能直接 mutate 捕获的 Vec（E0596），
        // 所以 spy 用 RefCell 装：只借 `&seen`，`Fn` 照样成立。
        let vars = ["Zeta-key", "alpha", "MIX", "b", "_tail"];
        let seen: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let got = missing(
            &vars.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &|k| {
                seen.borrow_mut().push(k.to_string());
                None
            },
        );
        assert_eq!(
            seen.into_inner(),
            vars,
            "lookup 必须按输入顺序、原样收到每个名字"
        );
        assert_eq!(got, vars, "全部缺失时应原序原样返回");
    }
}
