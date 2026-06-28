use toml::Value;

/// `TOTSUKA__<SECTION>__<KEY>=value` を TOML Value に差し込む。
/// 値は文字列・整数・bool として best-effort で解釈し、それ以外は文字列のままセット。
pub fn apply_env_overrides<I>(mut root: Value, env: I) -> Value
where
    I: IntoIterator<Item = (String, String)>,
{
    for (k, v) in env {
        let Some(path) = k.strip_prefix("TOTSUKA__") else {
            continue;
        };
        let parts: Vec<&str> = path.split("__").collect();
        let lowered: Vec<String> = parts.iter().map(|p| p.to_ascii_lowercase()).collect();
        let parsed = parse_scalar(&v);
        set_path(&mut root, &lowered, parsed);
    }
    root
}

fn parse_scalar(v: &str) -> Value {
    if let Ok(i) = v.parse::<i64>() {
        return Value::Integer(i);
    }
    if let Ok(f) = v.parse::<f64>() {
        return Value::Float(f);
    }
    if v.eq_ignore_ascii_case("true") {
        return Value::Boolean(true);
    }
    if v.eq_ignore_ascii_case("false") {
        return Value::Boolean(false);
    }
    Value::String(v.to_string())
}

fn set_path(root: &mut Value, path: &[String], val: Value) {
    if path.is_empty() {
        return;
    }
    let table = match root.as_table_mut() {
        Some(t) => t,
        None => return, // root が table でなければ無視
    };
    if path.len() == 1 {
        table.insert(path[0].clone(), val);
        return;
    }
    let entry = table
        .entry(path[0].clone())
        .or_insert_with(|| Value::Table(Default::default()));
    set_path(entry, &path[1..], val);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_scalar() {
        let base: Value = toml::from_str(
            r#"
[bus]
batch_size = 16
"#,
        )
        .unwrap();
        let v = apply_env_overrides(base, vec![("TOTSUKA__BUS__BATCH_SIZE".into(), "64".into())]);
        assert_eq!(v["bus"]["batch_size"].as_integer(), Some(64));
    }

    #[test]
    fn creates_missing_path() {
        let base: Value = toml::from_str("[totsuka]\nstate_dir=\"/x\"\n").unwrap();
        let v = apply_env_overrides(
            base,
            vec![(
                "TOTSUKA__TELEMETRY__OTLP_ENDPOINT".into(),
                "http://otel:4317".into(),
            )],
        );
        assert_eq!(
            v["telemetry"]["otlp_endpoint"].as_str(),
            Some("http://otel:4317")
        );
    }

    #[test]
    fn ignores_non_totsuka_env() {
        let base: Value = toml::from_str("[bus]\nbatch_size=16\n").unwrap();
        let v = apply_env_overrides(base, vec![("HOME".into(), "/h".into())]);
        assert_eq!(v["bus"]["batch_size"].as_integer(), Some(16));
    }
}
