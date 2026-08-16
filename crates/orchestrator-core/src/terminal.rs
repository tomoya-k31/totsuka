//! Making externally-authored text safe to *print* (#280, #297).
//!
//! Task titles, bodies, authors, urls and source ids are written by whoever
//! can post in the connected Slack channel or file the GitHub issue. They are
//! stored verbatim (the audit trail must show what was actually posted) and
//! only become dangerous on the way to a terminal — which is where [`safe`]
//! sits. The threat model, the escape-not-strip rule and the reason `--json`
//! never goes through here live in
//! `ai-docs/security/terminal-output-sanitization.md`.
//!
//! This lives in core rather than in the CLI because two crates print such
//! text: the CLI's human renderings (`task show`, `status`, `logs`, `doctor`)
//! and core's own human log layer on stderr
//! ([`logging::layer`](crate::logging::layer)), which `--debug` turns on for
//! every command. `orchestrator_cli::common::safe` re-exports this one, so
//! there is still a single implementation to reason about.

use std::borrow::Cow;

/// True for characters that let text rewrite the screen rather than appear on
/// it (#280).
///
/// [`char::is_control`] covers C0, DEL and C1 — the escape-sequence
/// introducers. The bidi overrides and isolates are *not* control characters
/// by that definition (they are `Cf`), but they forge the reading order of a
/// line, which is the same attack on the same screen, so they go through the
/// same door.
fn is_screen_control(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Render `text` safely for a terminal: control characters become their
/// visible escaped form (`\u{1b}`, `\n`, `\t`, …), so the string can only be
/// *read*, never *executed* by the terminal.
///
/// Titles, bodies, authors, urls and source ids are written by whoever can
/// post in the connected Slack channel or file the GitHub issue — which may
/// include people outside the team. Printed verbatim they can repaint the
/// operator's screen: `ESC[2J` clears it, `ESC[1A` walks the cursor back over
/// a row already printed (so one task's state can be made to show another's),
/// OSC 8 makes the displayed text and the real link disagree, and OSC 52
/// writes to the clipboard on terminals that honour it. None of that is code
/// execution, but all of it breaks the assumption that reading CLI output
/// tells you the truth.
///
/// Escaping rather than stripping is deliberate: a deleted character is a
/// character the operator cannot see was ever there, which is its own way of
/// lying about the content.
///
/// **Human output only.** `--json` goes through
/// `orchestrator_cli::common::print_json`, and the JSON log file goes through
/// `serde_json`; both already escape control characters as `\u00xx`, so
/// sending them through here as well would double-escape values that a
/// machine, not a terminal, is going to read.
pub fn safe(text: &str) -> Cow<'_, str> {
    // Almost every title is clean, and `task list` calls this once per row —
    // borrow rather than allocate when there is nothing to rewrite.
    if !text.contains(is_screen_control) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_screen_control(c) {
            // `escape_debug` spells the familiar ones `\n` / `\r` / `\t` and
            // falls back to `\u{...}` for everything else.
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of `safe` is that the string can still be *read* — escaping
    /// must not become its own way of destroying the content.
    #[test]
    fn safe_leaves_ordinary_text_exactly_as_it_was() {
        for text in [
            "リポジトリ選択のバグを直す",
            "deploy 🚀 done — 100% ✅",
            "https://example.com/a?b=c&d=e",
            "slack:C0123ABC:1700000000.000100",
            r"C:\Users\dev\repo",
            "",
        ] {
            assert_eq!(safe(text), text, "rewrote clean text: {text:?}");
            // Clean text is borrowed, not rebuilt — `task list` calls this
            // once per row.
            assert!(matches!(safe(text), Cow::Borrowed(_)), "{text:?}");
        }
    }

    /// The three sequences named in #280, plus the bidi override. Each must
    /// survive as visible text (nothing silently dropped) and must not carry
    /// a live control byte through to the terminal.
    #[test]
    fn safe_defuses_the_sequences_that_repaint_a_terminal() {
        let esc = char::from_u32(0x1b).unwrap();

        // ESC[2J — clear screen. ESC[1A — cursor up, i.e. overwrite the row
        // already printed above (forging another task's state).
        let clear = format!("title{esc}[2J");
        let up = format!("title{esc}[1A{esc}[2Kfake");
        // OSC 8 — displayed text and real link disagree.
        let osc8 = format!("{esc}]8;;https://evil.example{esc}\\click me{esc}]8;;{esc}\\");
        // CR alone rewrites the current row from column 0.
        let cr = "real title\rFORGED";
        // U+202E flips the reading order of everything after it.
        let bidi = "invoice\u{202e}gnp.exe";

        for raw in [
            clear.as_str(),
            up.as_str(),
            osc8.as_str(),
            cr,
            bidi,
            "bell\u{7}",
            "nul\u{0}",
        ] {
            let out = safe(raw);
            assert!(
                !out.chars().any(is_screen_control),
                "a live control character survived: {out:?}"
            );
            // The visible payload is still there to be read.
            assert!(out.len() >= raw.len(), "text shrank: {raw:?} -> {out:?}");
        }

        // Spot-check the actual rendering rather than only the property.
        assert_eq!(safe("real title\rFORGED"), r"real title\rFORGED");
        assert_eq!(safe(&format!("x{esc}y")), r"x\u{1b}y");
        assert_eq!(safe("a\nb"), r"a\nb");
        assert_eq!(safe("a\tb"), r"a\tb");
        assert_eq!(safe("invoice\u{202e}gnp.exe"), r"invoice\u{202e}gnp.exe");
    }

    /// A row must stay a row: whatever we print, the terminal sees exactly one
    /// line, so a table cannot be made to grow or shrink rows.
    #[test]
    fn safe_output_is_always_a_single_line() {
        let esc = char::from_u32(0x1b).unwrap();
        for raw in [
            "one\ntwo\nthree",
            "one\r\ntwo",
            &format!("a{esc}[2Jb"),
            "trailing\n",
        ] {
            let out = safe(raw);
            assert_eq!(out.lines().count().max(1), 1, "row split: {out:?}");
            assert!(!out.contains('\n'), "row split: {out:?}");
        }
    }
}
