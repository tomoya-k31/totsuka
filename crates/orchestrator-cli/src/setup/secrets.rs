//! Which secret store the generated config points at.
//!
//! `setup` never handles a secret **value** (F-65): it writes a reference and
//! prints the command that registers the value behind it. The backend decides
//! what that reference looks like and what the command is.
//!
//! The account names (`github-token`, `slack-user`, …) are conventional rather
//! than asked. They only have to agree between `config.toml` and the printed
//! commands, and inventing a naming scheme is not a decision worth a prompt.

use std::fmt;
use std::str::FromStr;

/// A secret store `setup` can write references for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum SecretBackend {
    /// 1Password (`op://<vault>/<item>/<field>`).
    ///
    /// The default: it is cross-platform, it is a real secret store, and the
    /// `op read` URI is the one reference form that is a genuine URI rather
    /// than a scheme totsuka invented.
    #[default]
    #[value(name = "op")]
    OnePassword,
    /// Bitwarden (`bw:<item>/<field>`).
    ///
    /// One item **per account**, unlike 1Password's one item with a field per
    /// account: `bw:` deliberately does not reach custom fields, and every
    /// secret here is a token, which maps to the same `password` object.
    #[value(name = "bw")]
    Bitwarden,
    /// macOS Keychain (`keychain:<service>/<account>`). **macOS only.**
    #[value(name = "keychain")]
    Keychain,
    /// Another tool's CLI, run on each resolution (`cmd:<command>`).
    #[value(name = "cmd")]
    Cmd,
    /// Environment variables (`${TOTSUKA_SECRET_...}`).
    ///
    /// The `TOTSUKA_SECRET_` prefix keeps the names in totsuka's namespace
    /// without colliding with the config overrides, which own `TOTSUKA_*` and
    /// warn about every name they do not recognise
    /// (`orchestrator_core::config::env_overrides::SECRET_PREFIX`).
    #[value(name = "env")]
    Env,
}

impl SecretBackend {
    /// The reference string this backend uses for `account`.
    pub fn reference(self, account: &str) -> String {
        match self {
            SecretBackend::OnePassword => format!("op://Dev/totsuka/{account}"),
            SecretBackend::Bitwarden => format!("bw:totsuka-{account}/password"),
            SecretBackend::Keychain => format!("keychain:totsuka/{account}"),
            SecretBackend::Cmd => match account {
                // The two accounts with a real command behind them. `cmd:` is
                // for credentials another tool owns and rotates, so where such
                // a tool exists, naming it is the whole value of the form.
                "github-token" => "cmd:gh auth token".to_string(),
                "notion-token" => "cmd:ntn auth token --plain".to_string(),
                _ => format!("cmd:<command that prints the {account}>"),
            },
            SecretBackend::Env => format!(
                "${{{}{}}}",
                orchestrator_core::config::env_overrides::SECRET_PREFIX,
                account.to_uppercase().replace('-', "_")
            ),
        }
    }

    /// The other four forms, named, for the comment above a reference.
    ///
    /// The file has to stay usable by someone who picked the wrong backend, or
    /// moves machines: they should be able to switch without going to the docs.
    pub fn other_forms(self, account: &str) -> String {
        let all = [
            (SecretBackend::OnePassword, "op://<vault>/<item>/<field>"),
            (SecretBackend::Bitwarden, "bw:<item>/<field>"),
            (
                SecretBackend::Keychain,
                "keychain:<service>/<account> (macOS only)",
            ),
            (SecretBackend::Cmd, "cmd:<command>"),
            (SecretBackend::Env, "${ENV_VAR}"),
        ];
        let _ = account;
        all.iter()
            .filter(|(backend, _)| *backend != self)
            .map(|(_, form)| *form)
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// A copy-pasteable command that registers `account`, when the backend has
    /// one. `Env` and `Cmd` have none — the value lives elsewhere by design.
    pub fn register_command(self, account: &str) -> Option<String> {
        match self {
            SecretBackend::Keychain => Some(format!(
                "security add-generic-password -U -s totsuka -a {account} -w '<paste the value>'"
            )),
            SecretBackend::OnePassword => Some(format!(
                "op item edit totsuka {account}='<paste the value>'   # or create the item first"
            )),
            // No `op item edit` counterpart exists: `bw` only creates and
            // edits items from encoded JSON, so the one-liner needs `jq`.
            // It always *creates*, which is why the caveat is not optional —
            // a duplicate item makes `bw get` fail with "more than one
            // result", i.e. the command would walk the operator straight into
            // our own error path.
            SecretBackend::Bitwarden => Some(format!(
                "bw get template item | jq '.name=\"totsuka-{account}\" | \
                 .login.password=\"<paste the value>\"' | bw encode | bw create item   \
                 # needs jq; creates a NEW item — edit the existing one instead if it is there"
            )),
            SecretBackend::Cmd | SecretBackend::Env => None,
        }
    }

    /// What to say instead, when there is no register command.
    pub fn register_note(self) -> Option<&'static str> {
        match self {
            SecretBackend::Cmd => Some(
                "`cmd:` resolves by running another tool, so there is nothing to register — \
                 make sure each command prints the secret and nothing else.",
            ),
            SecretBackend::Env => Some(
                "`${ENV_VAR}` reads the environment, so there is nothing to register — \
                 export each variable in the shell that starts `totsuka run`.",
            ),
            _ => None,
        }
    }
}

impl fmt::Display for SecretBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SecretBackend::OnePassword => "op",
            SecretBackend::Bitwarden => "bw",
            SecretBackend::Keychain => "keychain",
            SecretBackend::Cmd => "cmd",
            SecretBackend::Env => "env",
        })
    }
}

impl FromStr for SecretBackend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Kept in step with the `#[value(name = …)]` spellings above: clap
        // parses `--secret-backend` through the derived `ValueEnum`, so an
        // alias only this impl knew would be advertised nowhere and rejected
        // on the command line.
        match s {
            "op" => Ok(SecretBackend::OnePassword),
            "bw" => Ok(SecretBackend::Bitwarden),
            "keychain" => Ok(SecretBackend::Keychain),
            "cmd" => Ok(SecretBackend::Cmd),
            "env" => Ok(SecretBackend::Env),
            other => Err(format!(
                "unknown secret backend `{other}` → use one of op / bw / keychain / cmd / env"
            )),
        }
    }
}

/// What each account is for, for the checklist.
pub fn purpose_of(account: &str) -> &'static str {
    match account {
        "github-token" => "reads the Project board and writes results back",
        "notion-token" => "reads the database and writes results back",
        "slack-user" => "posts the reply under your own name",
        "slack-app" => "opens the Socket Mode connection",
        "slack-bot" => "sends the notification nudge (self-replies raise none)",
        "discord-bot-token" => "reads and posts as the Discord app",
        "llm-api-key" => "picks which repository a task belongs to",
        _ => "referenced by the config setup just wrote",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_1password() {
        assert_eq!(SecretBackend::default(), SecretBackend::OnePassword);
    }

    /// Round-trips, so `--secret-backend` and what is printed back agree.
    #[test]
    fn every_backend_parses_from_what_it_displays() {
        for backend in [
            SecretBackend::OnePassword,
            SecretBackend::Bitwarden,
            SecretBackend::Keychain,
            SecretBackend::Cmd,
            SecretBackend::Env,
        ] {
            assert_eq!(backend.to_string().parse(), Ok(backend));
        }
    }

    #[test]
    fn an_unknown_backend_names_the_five() {
        let err = "vault".parse::<SecretBackend>().unwrap_err();
        assert!(err.contains("op / bw / keychain / cmd / env"), "{err}");
    }

    /// **The alternatives never include the one in use.** Listing the chosen
    /// form as an "other form" reads as a second, different option.
    #[test]
    fn other_forms_omits_the_chosen_one() {
        assert!(
            !SecretBackend::Keychain
                .other_forms("x")
                .contains("keychain:")
        );
        assert!(SecretBackend::Keychain.other_forms("x").contains("op://"));
    }

    /// The env backend stays out of the config-override namespace, which warns
    /// about every `TOTSUKA_*` name it does not recognise.
    #[test]
    fn env_references_live_under_the_reserved_secret_prefix() {
        let reference = SecretBackend::Env.reference("github-token");
        assert_eq!(reference, "${TOTSUKA_SECRET_GITHUB_TOKEN}");
        assert!(
            reference.contains(orchestrator_core::config::env_overrides::SECRET_PREFIX),
            "the prefix must come from core, so the exemption cannot drift"
        );
    }

    /// `cmd:` earns its place on the accounts where another tool really does
    /// own the credential; elsewhere it is honest about being a placeholder.
    #[test]
    fn cmd_names_the_owning_tool_where_there_is_one() {
        assert_eq!(
            SecretBackend::Cmd.reference("github-token"),
            "cmd:gh auth token"
        );
        assert!(
            SecretBackend::Cmd
                .reference("slack-user")
                .contains("<command that prints"),
            "an account with no owning tool must not pretend to have one"
        );
    }

    /// Backends with nothing to register say so instead of printing nothing.
    #[test]
    fn backends_without_a_register_step_explain_themselves() {
        for backend in [SecretBackend::Cmd, SecretBackend::Env] {
            assert!(backend.register_command("github-token").is_none());
            assert!(backend.register_note().is_some());
        }
        assert!(SecretBackend::Keychain.register_note().is_none());
    }
}
