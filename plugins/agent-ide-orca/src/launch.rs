//! Turning the Orchestrator's resolved launch (`tool_launch`, #196) into the
//! one string `orca terminal create --command` accepts.
//!
//! # What orca does with `--command` (measured on orca 1.4.205)
//!
//! It **types the text into the terminal's interactive shell**, the operator's
//! login shell with their rc files. It does not exec it. Two consequences
//! shape the string built here:
//!
//! - **Everything is quoted.** The argv and env arrive as opaque values
//!   (hook settings paths, a `--resume` id, a job id) and must reach the
//!   program byte for byte, so each word is single-quoted. `'…'\''…'` is the
//!   one quoting form sh, bash, zsh and fish all read the same way.
//! - **It starts with `exec`.** Typed as-is, the command runs *under* the shell
//!   and the shell stays when it exits — so `terminal wait --for exit` would
//!   never fire and the state stream's deadman would be blind. `exec` replaces
//!   the shell, so the terminal's life is the agent's life (measured: an
//!   `exec`ed process exiting ends the terminal; a plain one leaves the prompt).
//!
//! The env rides on `env K=V …` rather than on the terminal, because
//! `terminal create` takes none — the same gap herdr's `agent.start` has,
//! closed differently (herdr puts it on `workspace.create`).

use std::collections::BTreeMap;

/// The `--command` text for launching `program args…` with `env` set.
pub fn shell_command(program: &str, args: &[String], env: &BTreeMap<String, String>) -> String {
    let mut words = vec!["exec".to_string()];
    if !env.is_empty() {
        words.push("env".to_string());
        words.extend(env.iter().map(|(k, v)| shell_quote(&format!("{k}={v}"))));
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
    fn execs_env_then_the_program_with_every_word_quoted() {
        let env = BTreeMap::from([
            ("TOTSUKA_JOB_ID".to_string(), "7.3".to_string()),
            ("TOTSUKA_HOOK_TOKEN".to_string(), "a b".to_string()),
        ]);
        let args = vec!["--settings".to_string(), "/p/hooks.json".to_string()];
        assert_eq!(
            shell_command("claude", &args, &env),
            "exec env 'TOTSUKA_HOOK_TOKEN=a b' 'TOTSUKA_JOB_ID=7.3' 'claude' '--settings' \
             '/p/hooks.json'"
        );
    }

    #[test]
    fn no_env_means_no_env_word() {
        assert_eq!(
            shell_command("codex", &[], &BTreeMap::new()),
            "exec 'codex'"
        );
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
