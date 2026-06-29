//! Path-shaped string helpers. Pure functions only — callers supply env lookups.

/// Expand a leading `~/` (or bare `~`) using the supplied home directory.
/// Returns `raw` unchanged if `home` is `None`, if `raw` doesn't start with a
/// tilde token (`~/` or exactly `~`), or if `raw` is empty.
pub fn resolve_tilde(raw: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return raw.to_string();
    };
    if raw == "~" {
        return home.to_string();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let mut out = String::with_capacity(home.len() + 1 + rest.len());
        out.push_str(home);
        out.push('/');
        out.push_str(rest);
        return out;
    }
    raw.to_string()
}
