//! `{placeholder}` substitution for the configurable prompts (#318).
//!
//! A deliberate ~25-line copy of `orchestrator_core::template::render`: plugins
//! may depend only on `plugin-protocol` / `plugin-sdk`
//! (`scripts/arch-lint.sh`), so core's copy is out of reach. It is not
//! promoted to `plugin-sdk` yet because this is the only consumer — orca's
//! plan prefix has no placeholders, and `agent-ide-orca` does not even depend
//! on the SDK. **Promote it when a second plugin needs one.**

/// Substitute `{key}` tokens in a **single pass**: a substituted value that
/// itself contains `{token}` text is never re-expanded.
///
/// The single-pass property is load-bearing here, more so than in core. Three
/// of the variables — the mention text, the thread context, the repository
/// catalog — are **Slack content, chosen by whoever wrote the message**. A
/// mention containing the literal text `{catalog}` must be inserted as-is, not
/// treated as a directive that splices the candidate list into the reply.
///
/// That was already true when these prompts were `format!` calls; making them
/// templates only makes it *look* like a place to reach for a
/// `loop { replace }`. Do not.
///
/// Unknown keys and an unbalanced `{` are emitted verbatim, which makes a
/// typo'd placeholder visible instead of silently deleting text.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Unbalanced `{`: emit the rest literally.
            out.push_str(&rest[open..]);
            return out;
        };
        let key = &after[..close];
        match vars.iter().find(|(name, _)| *name == key) {
            Some((_, value)) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(key);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Every `{name}` token in `template` whose name is a bare identifier, in
/// order. Used at `initialize` to warn about placeholders an overridden
/// template references but nothing supplies.
///
/// Restricted to identifiers (`[A-Za-z_][A-Za-z0-9_]*`) because braces also
/// appear as **content**: `classifier_system` embeds the literal JSON shape
/// `{"repo": string, ...}`. [`render`] already leaves that alone — no var
/// matches, so it is emitted verbatim — and reporting it as an unknown
/// placeholder would be a warning about text that behaves exactly as intended.
pub fn scan(template: &str) -> Vec<&str> {
    let bytes = template.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(rel) = template[i + 1..].find('}')
        {
            let name = &template[i + 1..i + 1 + rel];
            if is_identifier(name) {
                found.push(name);
            }
            i = i + 1 + rel + 1;
            continue;
        }
        i += 1;
    }
    found
}

/// Whether `s` looks like a placeholder name rather than braced content.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_and_keeps_unknown_keys() {
        let vars = [("a", "1")];
        assert_eq!(render("{a}-{b}", &vars), "1-{b}");
        assert_eq!(render("x { y", &vars), "x { y");
    }

    #[test]
    fn render_is_single_pass_on_attacker_controlled_text() {
        // A Slack message whose body is literally `{catalog}` must not splice
        // the repository catalog into the rendered prompt.
        let vars = [("text", "{catalog}"), ("catalog", "SECRET REPO LIST")];
        assert_eq!(render("{text}", &vars), "{catalog}");
    }

    #[test]
    fn scan_lists_placeholders() {
        assert_eq!(scan("{a} と {b}"), vec!["a", "b"]);
        assert_eq!(scan("なし"), Vec::<&str>::new());
    }

    #[test]
    fn scan_ignores_braced_content_that_is_not_an_identifier() {
        // The classifier system prompt embeds this JSON shape literally.
        assert_eq!(
            scan(r#"shape {"repo": string, "confidence": number} and {ok}"#),
            vec!["ok"]
        );
        // `render` agrees: unmatched braces are content, emitted verbatim.
        assert_eq!(
            render(r#"{"repo": string}"#, &[("repo", "X")]),
            r#"{"repo": string}"#
        );
    }
}
