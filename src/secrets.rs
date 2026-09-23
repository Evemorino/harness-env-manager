//! `${VAR}` 引用的校验与存在性检查。
//!
//! 密钥永远以引用形态流转：本模块只判断「语法是否为纯引用」与「变量是否存在」，
//! **绝不展开值进任何产物**（spec §3 决策 3）。

/// `"${FOO}"` → `Some("FOO")`；任何非纯引用形态 → `None`。
/// 手写校验而非 regex，以守住「不引入 regex 依赖」的约束。
pub fn parse_ref(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("${")?.strip_suffix('}')?;
    // 标识符规则本身就排除了 `$ { }`（它们都不在 [A-Za-z0-9_] 里），
    // 所以嵌套与拼接形态由下面这段一并拒掉，不需要单独的守卫。
    // 空串也不用单独判：`chars.next()?` 就是那条规则的唯一实现。
    let mut chars = inner.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(inner)
}

/// 校验单个 env 值。错误信息包含 key 名但**绝不回显 value**（防止密钥进日志/终端）。
/// 措辞里不出现任何可能被当作 fixture 值的词（`literal` / `secret` 等），下游测试
/// 会用「值不得出现在消息里」做断言。
pub fn validate_env(key: &str, value: &str) -> Result<(), String> {
    match parse_ref(value) {
        Some(_) => Ok(()),
        None => Err(format!(
            "env `{key}` must be a bare ${{VAR}} reference (value not shown)"
        )),
    }
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
        const WANT: &str = "env `K` must be a bare ${VAR} reference (value not shown)";
        for v in ["sk-live-123", "s", "", "K", "reference", "  spaced  "] {
            assert_eq!(
                validate_env("K", v).unwrap_err(),
                WANT,
                "value {v:?} leaked or message drifted"
            );
        }
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
