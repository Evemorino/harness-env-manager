//! `${VAR}` 引用的校验与存在性检查。
//!
//! 密钥永远以引用形态流转：本模块只判断「语法是否为纯引用」与「变量是否存在」，
//! **绝不展开值进任何产物**（spec §3 决策 3）。

/// `"${FOO}"` → `Some("FOO")`；任何非纯引用形态 → `None`。
/// 手写校验而非 regex，以守住「不引入 regex 依赖」的约束。
pub fn parse_ref(value: &str) -> Option<&str> {
    let inner = value.strip_prefix("${")?.strip_suffix('}')?;
    if inner.is_empty() {
        return None;
    }
    // 标识符规则本身就排除了 `$ { }`（它们都不在 [A-Za-z0-9_] 里），
    // 所以嵌套与拼接形态由下面这段一并拒掉，不需要单独的守卫。
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
pub fn validate_env(key: &str, value: &str) -> Result<(), String> {
    match parse_ref(value) {
        Some(_) => Ok(()),
        None => Err(format!(
            "env `{key}` must be a bare ${{VAR}} reference; literal secrets are rejected"
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
}
