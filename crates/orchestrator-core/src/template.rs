//! `{placeholder}` substitution shared by every user-authored template in the
//! config (#312).
//!
//! Two halves, deliberately paired:
//!
//! - [`render`] substitutes at runtime, keeping unknown keys verbatim so a
//!   typo degrades to visible text rather than a panic.
//! - [`scan`] enumerates the placeholders a template references, so
//!   [`config::validate`](mod@crate::config::validate) can reject the typo up
//!   front instead of leaving it to be discovered in a pane.
//!
//! The two must agree on **what counts as a placeholder**, or validation
//! rejects text that renders perfectly well. They agree on braces being
//! identifier-named (#328) and on an unbalanced `{` ending the scan, exactly as
//! it makes `render` emit the remainder literally. They deliberately do *not*
//! agree on unknown names: `render` is fail-soft there, `scan`'s callers are
//! fail-fast.
//!
//! # Not used by the worktree templates
//!
//! [`worktree::render_location`](crate::worktree::render_location) and
//! `render_branch` deliberately do **not** go through [`render`]. They chain
//! `str::replace` calls (multi-pass — a substituted value *is* re-scanned) and
//! their output feeds git-ref legalization. Their placeholders are also
//! interleaved with `${ENV}` expansion, which this module knows nothing about.
//! Unifying the two would change the worktree contract, so they stay separate;
//! [`scan`]'s `skip_dollar` flag is the only thing the two share.

/// Substitute `{key}` tokens in a **single pass**: a substituted value that
/// itself contains `{token}` text is never re-expanded.
///
/// That single-pass property is load-bearing wherever a template variable can
/// carry untrusted data — a task title (or a Slack message) containing the
/// literal text `{summary}` must be inserted as-is, not treated as another
/// directive. Reaching for a `loop { replace }` here would reintroduce
/// template injection.
///
/// Unknown keys and an unbalanced `{` are emitted verbatim, which makes a
/// typo'd placeholder visible in the output instead of silently deleting text.
/// That is fail-soft, not a substitute for validation: a caller whose template
/// carries load-bearing text is expected to reject the typo up front with
/// [`scan`], the way the worktree templates already do.
///
/// `vars` is a slice rather than a map because every call site has a handful of
/// keys; the linear scan is cheaper than hashing.
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
            // Unknown placeholder: keep it verbatim (helps spot typos).
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
/// order.
///
/// With `skip_dollar`, a `{` immediately preceded by `$` is not a placeholder
/// but the start of a `${...}` env reference (expanded at resolve time, not
/// here) — worktree templates need that, prompt templates do not.
///
/// # Why identifiers only (#328)
///
/// Braces also appear as **content**. A prompt may legitimately show a model
/// the JSON shape it must answer with:
///
/// ```text
/// Answer with ONLY an object of the exact shape {"ok": bool, "why": string}
/// ```
///
/// [`render`] already leaves that alone — nothing in `vars` is named
/// `"ok": bool, "why": string`, so it is emitted verbatim — but before #328
/// `scan` reported it, and every caller turns an unrecognised name into a hard
/// error. A config whose *rendered output was correct* could not start.
///
/// Restricting to `[A-Za-z_][A-Za-z0-9_]*` is what makes this function agree
/// with [`render`]'s *effective* semantics rather than its literal brace
/// scanning. No real placeholder is affected: every key in
/// [`prompts`](crate::prompts) and every worktree placeholder is an
/// identifier.
///
/// **Do not "fix" this by narrowing [`render`] to match.** Its
/// unknown-key-verbatim behaviour is a deliberate fail-soft, documented above.
pub fn scan(template: &str, skip_dollar: bool) -> Vec<&str> {
    let bytes = template.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && !(skip_dollar && i > 0 && bytes[i - 1] == b'$')
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
    fn render_substitutes_known_keys_and_keeps_unknown_ones() {
        let vars = [("a", "1"), ("b", "2")];
        assert_eq!(render("{a}-{b}", &vars), "1-2");
        assert_eq!(render("{a}{bogus}", &vars), "1{bogus}");
        assert_eq!(render("x { y", &vars), "x { y");
        assert_eq!(render("no placeholders", &vars), "no placeholders");
        assert_eq!(render("{a}", &[]), "{a}");
    }

    #[test]
    fn render_is_single_pass() {
        // A value containing a placeholder token must be inserted literally,
        // not re-scanned as another directive (injection guard).
        let vars = [
            ("rubric", "check it {marker_failed}"),
            ("marker_failed", "<<STATUS:FAILED>>"),
        ];
        assert_eq!(
            render("{rubric}", &vars),
            "check it {marker_failed}",
            "a substituted value must not be re-expanded"
        );
        // The same key still expands when it appears in the template itself.
        assert_eq!(render("{marker_failed}", &vars), "<<STATUS:FAILED>>");
    }

    #[test]
    fn scan_finds_every_placeholder_in_order() {
        assert_eq!(scan("{a} and {b}", false), vec!["a", "b"]);
        assert_eq!(scan("none here", false), Vec::<&str>::new());
        // An unbalanced `{` ends the scan, mirroring `render`'s behavior.
        assert_eq!(scan("{a} then { b", false), vec!["a"]);
    }

    #[test]
    fn scan_ignores_braced_content_that_is_not_an_identifier() {
        // A prompt may show the model the JSON shape it must answer with.
        // `render` emits that verbatim, so `scan` must not report it (#328).
        assert_eq!(
            scan(r#"shape {"ok": bool, "why": string} then {rubric}"#, false),
            vec!["rubric"]
        );
        // Whatever `scan` skips, `render` leaves alone — the two agree.
        assert_eq!(render(r#"{"ok": bool}"#, &[("ok", "X")]), r#"{"ok": bool}"#);
        // Digits-first, dots, spaces and hyphens are content, not names.
        assert_eq!(scan("{1st} {a.b} {a b} {a-b}", false), Vec::<&str>::new());
        // Underscores and digits after the first char are fine.
        assert_eq!(scan("{_x} {a1} {A_B2}", false), vec!["_x", "a1", "A_B2"]);
        // An empty name is content too.
        assert_eq!(scan("{}", false), Vec::<&str>::new());
    }

    #[test]
    fn scan_skips_env_references_only_when_asked() {
        assert_eq!(scan("${HOME}/{task_id}", true), vec!["task_id"]);
        assert_eq!(scan("${HOME}/{task_id}", false), vec!["HOME", "task_id"]);
    }
}
