use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Expand a single string leaf. A whole-value `op://...` reference is
/// resolved verbatim via `op_lookup` (no further `${...}`/`~` expansion is
/// applied to the secret it resolves to). Otherwise: first var/env expansion
/// (lenient), then leading-tilde expansion using HOME from `env_lookup`.
fn expand_string_leaf<F, O>(
    s: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
    op_lookup: &O,
) -> Result<String, ExpandError>
where
    F: Fn(&str) -> Option<String>,
    O: Fn(&str) -> Result<String, ExpandError>,
{
    if s.starts_with("op://") {
        return op_lookup(s);
    }
    let expanded = expand_vars_lenient(s, vars, env_lookup)?;
    let home = env_lookup("HOME");
    Ok(crate::path_expand::resolve_tilde(
        &expanded,
        home.as_deref(),
    ))
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ExpandError {
    #[error("undefined variable: {0}")]
    Undefined(String),
    #[error("undefined env variable: {0}")]
    UndefinedEnv(String),
    #[error("cyclic reference involving: {0}")]
    Cycle(String),
    #[error("op CLI could not be executed for {0}: {1}")]
    OpExec(String, String),
    #[error("op read failed for {0}: {1}")]
    OpFailed(String, String),
    #[error("op read returned non-UTF8 output for {0}")]
    OpNonUtf8(String),
}

fn re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\$\{([a-zA-Z0-9_:.\-]+)\}").unwrap())
}

/// `${name}` を vars から、`${env:NAME}` を env_lookup から展開する。
/// vars 内の相互参照も解決する (循環は ExpandError::Cycle)
pub fn expand_vars<F>(
    input: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
) -> Result<String, ExpandError>
where
    F: Fn(&str) -> Option<String>,
{
    expand_inner(input, vars, env_lookup, &mut HashSet::new())
}

fn expand_inner<F>(
    input: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
    visiting: &mut HashSet<String>,
) -> Result<String, ExpandError>
where
    F: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(input.len());
    let mut last = 0usize;
    for cap in re().captures_iter(input) {
        let m = cap.get(0).unwrap();
        let key = cap.get(1).unwrap().as_str();
        out.push_str(&input[last..m.start()]);
        let replaced = if let Some(env_name) = key.strip_prefix("env:") {
            env_lookup(env_name).ok_or_else(|| ExpandError::UndefinedEnv(env_name.into()))?
        } else {
            if !visiting.insert(key.to_string()) {
                return Err(ExpandError::Cycle(key.into()));
            }
            let v = vars
                .get(key)
                .ok_or_else(|| ExpandError::Undefined(key.into()))?;
            let r = expand_inner(v, vars, env_lookup, visiting)?;
            visiting.remove(key);
            r
        };
        out.push_str(&replaced);
        last = m.end();
    }
    out.push_str(&input[last..]);
    Ok(out)
}

/// Walk a `toml::Value` tree and expand `${name}` / `${env:NAME}` / `op://...`
/// references in every string leaf. `vars` should be collected from a
/// top-level `[vars]` table; `env_lookup` provides env-var fallback;
/// `op_lookup` resolves whole-value `op://vault/item/field` references (see
/// `expand_string_leaf`).
///
/// **Lenient mode for unknown `${name}`**: when a `${name}` reference cannot be
/// resolved from `vars`, the original token is left in place rather than
/// raising `ExpandError::Undefined`. This keeps backward compatibility with
/// configs whose variables live in non-top-level sections (e.g.
/// `[agent_adapter.vars]`) — the unresolved leaves still hit `validate()` and
/// the orchestrator can decide what to do with them. Cycles, `${env:NAME}`
/// misses, and `op://` resolution failures remain hard errors.
pub fn expand_toml_value<F, O>(
    value: &mut toml::Value,
    vars: &HashMap<String, String>,
    env_lookup: &F,
    op_lookup: &O,
) -> Result<(), ExpandError>
where
    F: Fn(&str) -> Option<String>,
    O: Fn(&str) -> Result<String, ExpandError>,
{
    match value {
        toml::Value::String(s) => {
            *s = expand_string_leaf(s, vars, env_lookup, op_lookup)?;
        }
        toml::Value::Array(arr) => {
            for v in arr.iter_mut() {
                expand_toml_value(v, vars, env_lookup, op_lookup)?;
            }
        }
        toml::Value::Table(tbl) => {
            for (_, v) in tbl.iter_mut() {
                expand_toml_value(v, vars, env_lookup, op_lookup)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Like `expand_vars` but undefined `${name}` (not `${env:NAME}`) refs are
/// left as the literal `${name}` token instead of erroring. Cycles and
/// undefined env vars still error.
pub fn expand_vars_lenient<F>(
    input: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
) -> Result<String, ExpandError>
where
    F: Fn(&str) -> Option<String>,
{
    expand_lenient_inner(input, vars, env_lookup, &mut HashSet::new())
}

fn expand_lenient_inner<F>(
    input: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
    visiting: &mut HashSet<String>,
) -> Result<String, ExpandError>
where
    F: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(input.len());
    let mut last = 0usize;
    for cap in re().captures_iter(input) {
        let m = cap.get(0).unwrap();
        let key = cap.get(1).unwrap().as_str();
        out.push_str(&input[last..m.start()]);
        if let Some(env_name) = key.strip_prefix("env:") {
            let v =
                env_lookup(env_name).ok_or_else(|| ExpandError::UndefinedEnv(env_name.into()))?;
            out.push_str(&v);
        } else if let Some(v) = vars.get(key) {
            if !visiting.insert(key.to_string()) {
                return Err(ExpandError::Cycle(key.into()));
            }
            let r = expand_lenient_inner(v, vars, env_lookup, visiting)?;
            visiting.remove(key);
            out.push_str(&r);
        } else {
            // Lenient: leave the unresolved token in place.
            out.push_str(&input[m.start()..m.end()]);
        }
        last = m.end();
    }
    out.push_str(&input[last..]);
    Ok(out)
}

/// Flatten every string leaf in the TOML tree into a `section.subsection.key → value`
/// map suitable as a fallback lookup for `${section.key}` expansion.
pub fn flatten_string_leaves(tree: &toml::Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    walk_leaves(tree, &mut Vec::new(), &mut out);
    out
}

fn walk_leaves(value: &toml::Value, path: &mut Vec<String>, out: &mut HashMap<String, String>) {
    match value {
        toml::Value::String(s) => {
            if !path.is_empty() {
                out.insert(path.join("."), s.clone());
            }
        }
        toml::Value::Table(tbl) => {
            for (k, v) in tbl.iter() {
                path.push(k.clone());
                walk_leaves(v, path, out);
                path.pop();
            }
        }
        // Arrays and scalars are not exposed as ${section.key} refs.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty_env() -> impl Fn(&str) -> Option<String> {
        |_| None
    }

    #[test]
    fn plain_passthrough() {
        let vars = HashMap::new();
        assert_eq!(
            expand_vars("/no/vars/here", &vars, &empty_env()).unwrap(),
            "/no/vars/here"
        );
    }

    #[test]
    fn simple_var() {
        let mut v = HashMap::new();
        v.insert("work".into(), "/home/u/work".into());
        assert_eq!(
            expand_vars("${work}/repos", &v, &empty_env()).unwrap(),
            "/home/u/work/repos"
        );
    }

    #[test]
    fn env_var() {
        let v = HashMap::new();
        let env = |k: &str| if k == "HOME" { Some("/h".into()) } else { None };
        assert_eq!(expand_vars("${env:HOME}/x", &v, &env).unwrap(), "/h/x");
    }

    #[test]
    fn nested_vars() {
        let mut v = HashMap::new();
        v.insert("a".into(), "/x/${b}".into());
        v.insert("b".into(), "y".into());
        assert_eq!(expand_vars("${a}", &v, &empty_env()).unwrap(), "/x/y");
    }

    #[test]
    fn undefined_errors() {
        let v = HashMap::new();
        assert_eq!(
            expand_vars("${nope}", &v, &empty_env()).unwrap_err(),
            ExpandError::Undefined("nope".into())
        );
    }

    #[test]
    fn undefined_env_errors() {
        let v = HashMap::new();
        assert_eq!(
            expand_vars("${env:MISSING}", &v, &empty_env()).unwrap_err(),
            ExpandError::UndefinedEnv("MISSING".into())
        );
    }

    #[test]
    fn cycle_errors() {
        let mut v = HashMap::new();
        v.insert("a".into(), "${b}".into());
        v.insert("b".into(), "${a}".into());
        match expand_vars("${a}", &v, &empty_env()).unwrap_err() {
            ExpandError::Cycle(_) => (),
            e => panic!("expected Cycle, got {:?}", e),
        }
    }

    fn ok_op_lookup() -> impl Fn(&str) -> Result<String, ExpandError> {
        |uri: &str| Ok(format!("resolved:{uri}"))
    }

    fn failing_op_lookup() -> impl Fn(&str) -> Result<String, ExpandError> {
        |uri: &str| Err(ExpandError::OpFailed(uri.to_string(), "boom".into()))
    }

    #[test]
    fn op_ref_resolved_via_lookup() {
        let vars = HashMap::new();
        assert_eq!(
            expand_string_leaf(
                "op://Vault/Item/field",
                &vars,
                &empty_env(),
                &ok_op_lookup()
            )
            .unwrap(),
            "resolved:op://Vault/Item/field"
        );
    }

    #[test]
    fn op_ref_failure_propagates() {
        let vars = HashMap::new();
        assert_eq!(
            expand_string_leaf(
                "op://Vault/Item/field",
                &vars,
                &empty_env(),
                &failing_op_lookup()
            )
            .unwrap_err(),
            ExpandError::OpFailed("op://Vault/Item/field".into(), "boom".into())
        );
    }

    #[test]
    fn op_ref_is_not_var_expanded() {
        let mut vars = HashMap::new();
        vars.insert("x".into(), "should-not-appear".into());
        assert_eq!(
            expand_string_leaf(
                "op://Vault/${x}/field",
                &vars,
                &empty_env(),
                &ok_op_lookup()
            )
            .unwrap(),
            "resolved:op://Vault/${x}/field"
        );
    }

    #[test]
    fn non_op_string_unaffected_by_op_lookup_param() {
        let vars = HashMap::new();
        assert_eq!(
            expand_string_leaf("/plain/path", &vars, &empty_env(), &failing_op_lookup()).unwrap(),
            "/plain/path"
        );
    }
}
