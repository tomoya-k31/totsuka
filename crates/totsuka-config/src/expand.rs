use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use toml::Value;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ExpandError {
    #[error("undefined variable: {0}")]
    Undefined(String),
    #[error("undefined env variable: {0}")]
    UndefinedEnv(String),
    #[error("cyclic reference involving: {0}")]
    Cycle(String),
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

/// Walk a `toml::Value` tree and expand `${var}` / `${env:VAR}` references in every
/// string leaf.
///
/// - Variables defined in `vars` are expanded recursively (cycles return `ExpandError::Cycle`).
/// - Environment variables are resolved via `std::env::var`.
/// - References that are undefined in `vars` or unset in the environment are left **as-is**
///   (lenient mode), so cross-section references like `${totsuka.state_dir}` can live in
///   documentation configs without causing errors.
/// - Only `ExpandError::Cycle` is propagated; `Undefined` / `UndefinedEnv` are swallowed.
pub fn expand_toml_value(
    val: &mut Value,
    vars: &HashMap<String, String>,
) -> Result<(), ExpandError> {
    match val {
        Value::String(s) => {
            let expanded = expand_string_lenient(s, vars)?;
            *s = expanded;
        }
        Value::Table(t) => {
            for (_, v) in t.iter_mut() {
                expand_toml_value(v, vars)?;
            }
        }
        Value::Array(a) => {
            for v in a.iter_mut() {
                expand_toml_value(v, vars)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn expand_string_lenient(
    input: &str,
    vars: &HashMap<String, String>,
) -> Result<String, ExpandError> {
    let env_lookup = |k: &str| std::env::var(k).ok();
    expand_inner_lenient(input, vars, &env_lookup, &mut HashSet::new())
}

fn expand_inner_lenient<F>(
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
            // Leave undefined env vars as-is
            env_lookup(env_name).unwrap_or_else(|| format!("${{env:{env_name}}}"))
        } else if let Some(v) = vars.get(key) {
            if !visiting.insert(key.to_string()) {
                return Err(ExpandError::Cycle(key.into()));
            }
            let r = expand_inner_lenient(v, vars, env_lookup, visiting)?;
            visiting.remove(key);
            r
        } else {
            // Leave undefined vars as-is
            format!("${{{key}}}")
        };
        out.push_str(&replaced);
        last = m.end();
    }
    out.push_str(&input[last..]);
    Ok(out)
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
}
