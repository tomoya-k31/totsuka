//! Turning the Orchestrator's resolved launch (`tool_launch`, #196) into the
//! one string `orca terminal create --command` accepts.
//!
//! # What orca does with `--command` (measured on orca 1.4.205)
//!
//! It **types the text into the terminal's interactive shell**, the operator's
//! login shell with their rc files. It does not exec it. Two consequences
//! shape the string built here:
//!
//! - **Everything is quoted.** The argv arrives as opaque values (hook
//!   settings paths, a `--resume` id) and must reach the
//!   program byte for byte, so each word is single-quoted. `'…'\''…'` is the
//!   one quoting form sh, bash, zsh and fish all read the same way.
//! - **It starts with `exec`.** Typed as-is, the command runs *under* the shell
//!   and the shell stays when it exits — so `terminal wait --for exit` would
//!   never fire and the state stream's deadman would be blind. `exec` replaces
//!   the shell, so the terminal's life is the agent's life (measured: an
//!   `exec`ed process exiting ends the terminal; a plain one leaves the prompt).
//!
//! **The env is never typed** (#744). `terminal create` takes no env — the
//! same gap herdr's `agent.start` has, which herdr closes on
//! `workspace.create` — and an `env K=V …` prefix put every value (the hook
//! token, the prompt context, `env_file` secrets) on the screen and in the
//! scrollback. The env goes through a FIFO instead ([`crate::handoff`]), and
//! the typed text only reads it:
//!
//! ```text
//! exec sh -c 'e=$(cat "$1") || exit 1; rm -f "$1"; eval "$e" || exit 1; shift; exec "$@"' \
//!     sh '<fifo>' '<program>' '<args>'…
//! ```
//!
//! `cat` + `eval` rather than `. "$1"`: macOS's `/bin/sh` is bash 3.2, whose
//! `.` sizes the file with `stat` and so reads **nothing** from a FIFO —
//! measured, the variables arrived empty. `|| exit 1` keeps a shell that runs
//! after the writer gave up (FIFO already removed) from starting the agent
//! with no env. The script holds no `'`, so it survives the same single
//! quoting as every other word.

use std::path::Path;

/// The script `sh -c` runs: read the env from `$1`, delete it, apply it, and
/// replace itself with the program.
const READ_ENV_SCRIPT: &str =
    r#"e=$(cat "$1") || exit 1; rm -f "$1"; eval "$e" || exit 1; shift; exec "$@""#;

/// The `--command` text for launching `program args…`, taking its env from
/// the FIFO at `env_fifo` when there is one.
pub fn launch_command(program: &str, args: &[String], env_fifo: Option<&Path>) -> String {
    let mut words = vec!["exec".to_string()];
    if let Some(fifo) = env_fifo {
        let fifo = shell_quote(&fifo.to_string_lossy());
        words.extend([
            "sh".into(),
            "-c".into(),
            shell_quote(READ_ENV_SCRIPT),
            "sh".into(),
            fifo,
        ]);
    }
    words.push(shell_quote(program));
    words.extend(args.iter().map(|a| shell_quote(a)));
    words.join(" ")
}

/// Single-quote `word` for a POSIX-style shell (and fish).
pub fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fifo_means_only_the_reader_and_the_program_are_typed() {
        let args = vec!["--settings".to_string(), "/p/hooks.json".to_string()];
        assert_eq!(
            launch_command(
                "claude",
                &args,
                Some(Path::new("/run/totsuka/orca-env/1-0"))
            ),
            format!(
                "exec sh -c {} sh '/run/totsuka/orca-env/1-0' 'claude' '--settings' \
                 '/p/hooks.json'",
                shell_quote(READ_ENV_SCRIPT)
            )
        );
        assert!(
            !READ_ENV_SCRIPT.contains('\''),
            "the script must survive single quoting"
        );
    }

    #[test]
    fn no_fifo_means_a_bare_exec() {
        assert_eq!(launch_command("codex", &[], None), "exec 'codex'");
    }

    #[test]
    fn a_single_quote_survives() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    /// The quoting is only worth anything if a real shell reads it back
    /// unchanged — including the characters a shell would otherwise act on.
    #[test]
    fn sh_reads_the_quoted_words_back_verbatim() {
        let tricky = ["it's", "$HOME", "a b", "`x`", "\\n", "\n", "!", "*"];
        let script = format!(
            "printf '%s\\0' {}",
            tricky
                .iter()
                .map(|w| shell_quote(w))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let out = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .expect("sh runs");
        let words: Vec<&str> = std::str::from_utf8(&out.stdout)
            .unwrap()
            .split_terminator('\0')
            .collect();
        assert_eq!(words, tricky);
    }
}
