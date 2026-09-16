//! Rendering a `config.toml` out of the shipped skeleton.
//!
//! The skeleton (`crates/orchestrator-cli/templates/config.toml`) carries every
//! setting totsuka understands, commented out, plus a handful of directives
//! that only this module reads. **The directives never reach the generated
//! file**: they are the seam between "one file documents everything" and "the
//! file I just got mentions the plugins I picked".
//!
//! | Directive | Effect |
//! |---|---|
//! | `# totsuka:section plugin=<name> [label=<text>]` | Everything until the next directive belongs to `<name>`, and is dropped unless `<name>` was selected. `label` also names the plugin in the picker |
//! | `# totsuka:section core` | Back to unconditional content |
//! | `# totsuka:secret <account>` | The next line's quoted value is a secret reference; rewrite it for the chosen backend and say what the other forms look like |
//! | `lint:raw` (anywhere on a line) | For `scripts/config-template-lint.sh` only; stripped here |
//!
//! Deriving the plugin roster from the skeleton rather than a list in Rust is
//! deliberate: adding a plugin section is then the *only* edit needed for it to
//! appear in the picker, and the two cannot drift apart.

use std::collections::BTreeSet;

use super::SecretBackend;

/// The skeleton, baked in at build time.
pub const TEMPLATE: &str = include_str!("../../templates/config.toml");

/// A plugin the skeleton knows how to configure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPlugin {
    /// Plugin name, which is also the binary name and the table name.
    pub name: String,
    /// One line for the picker.
    pub label: String,
}

/// What a `# totsuka:section` directive says.
enum Directive {
    Core,
    Plugin { name: String, label: Option<String> },
}

fn parse_section(line: &str) -> Option<Directive> {
    let rest = line.trim().strip_prefix("# totsuka:section ")?.trim();
    if rest == "core" {
        return Some(Directive::Core);
    }
    let rest = rest.strip_prefix("plugin=")?;
    // `label=` is optional and runs to the end of the line, so it is split off
    // first — a label may contain anything, spaces included.
    match rest.split_once(" label=") {
        Some((name, label)) => Some(Directive::Plugin {
            name: name.trim().to_string(),
            label: Some(label.trim().to_string()),
        }),
        None => Some(Directive::Plugin {
            name: rest.trim().to_string(),
            label: None,
        }),
    }
}

/// The plugins the skeleton can configure, in the order it presents them.
///
/// Only sections that carry a `label` count: each plugin also has an unlabelled
/// directive around its `[plugins.<name>]` roster entry, and listing it twice
/// would put the plugin in the picker twice.
pub fn known_plugins() -> Vec<KnownPlugin> {
    TEMPLATE
        .lines()
        .filter_map(parse_section)
        .filter_map(|d| match d {
            Directive::Plugin {
                name,
                label: Some(label),
            } => Some(KnownPlugin { name, label }),
            _ => None,
        })
        .collect()
}

/// The secret accounts a config rendered for `selected` would reference.
///
/// Read off the same directives the rendering uses, so the checklist printed at
/// the end cannot list an account the file does not mention, or miss one it
/// does. Order follows the file; duplicates (the LLM key appears under both
/// `[llm]` and `[slack.llm]`) are collapsed.
pub fn secret_accounts(selected: &BTreeSet<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut accounts = Vec::new();
    for (account, _) in walk(selected) {
        if let Some(account) = account
            && seen.insert(account.clone())
        {
            accounts.push(account);
        }
    }
    accounts
}

/// The secret accounts a stretch of *rendered* text references.
///
/// Read off the rendered form rather than the directives, because what the
/// checklist has to describe is the text that was actually written — on an
/// append that is a subset of what the selection would have produced, and the
/// lines already in the file keep whatever backend wrote them.
pub fn accounts_mentioned_in(rendered: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut accounts = Vec::new();
    for line in rendered.lines() {
        let Some(rest) = line.trim().strip_prefix("# Secret reference for `") else {
            continue;
        };
        let Some(account) = rest.split('`').next() else {
            continue;
        };
        if seen.insert(account.to_string()) {
            accounts.push(account.to_string());
        }
    }
    accounts
}

/// Walk the skeleton, yielding `(secret account, line)` for the lines that
/// survive `selected`. The account is attached to the line whose value it
/// rewrites, not to the directive.
fn walk(selected: &BTreeSet<String>) -> Vec<(Option<String>, &'static str)> {
    let mut out = Vec::new();
    let mut keep = true;
    let mut pending: Option<String> = None;
    for line in TEMPLATE.lines() {
        if let Some(directive) = parse_section(line) {
            keep = match directive {
                Directive::Core => true,
                Directive::Plugin { ref name, .. } => selected.contains(name),
            };
            continue;
        }
        if let Some(account) = line.trim().strip_prefix("# totsuka:secret ") {
            pending = Some(account.trim().to_string());
            continue;
        }
        if !keep {
            pending = None;
            continue;
        }
        out.push((pending.take(), line));
    }
    out
}

/// Render the skeleton for `selected`, writing secret references in `backend`'s
/// form.
pub fn render(selected: &BTreeSet<String>, backend: SecretBackend) -> String {
    let mut out = String::new();
    let mut blanks = 0;
    for (account, line) in walk(selected) {
        // Dropping a plugin's section takes its trailing blank line with it but
        // leaves the one that preceded its banner, so runs build up. Collapse
        // them rather than making every section's spacing load-bearing.
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }

        let line = line.replace("   lint:raw", "").replace(" lint:raw", "");
        // The one key the generated file sets. Writing it means a config that
        // omits `version` — which today is read as "whatever the current
        // version happens to be" — never leaves this command.
        let line = if line.trim() == "# version = 1" {
            "version = 1".to_string()
        } else {
            line
        };

        match account {
            Some(account) => {
                out.push_str(&format!(
                    "# Secret reference for `{account}`. Other forms: {}\n",
                    backend.other_forms(&account)
                ));
                out.push_str(&rewrite_secret(&line, &backend.reference(&account)));
            }
            None => out.push_str(&line),
        }
        out.push('\n');
    }
    out
}

/// Replace the first double-quoted value in `line` with `reference`, keeping
/// everything else — the key, and the trailing comment that explains it.
fn rewrite_secret(line: &str, reference: &str) -> String {
    let Some(open) = line.find('"') else {
        return line.to_string();
    };
    let Some(close) = line[open + 1..].find('"') else {
        return line.to_string();
    };
    let close = open + 1 + close;
    format!("{}\"{reference}\"{}", &line[..open], &line[close + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    /// The picker's list comes from the skeleton, once per plugin.
    #[test]
    fn known_plugins_are_the_labelled_sections() {
        let names: Vec<String> = known_plugins().into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "github", "notion", "slack", "discord", "herdr", "orca", "macos"
            ]
        );
    }

    /// **An unselected plugin leaves no trace.** Not its settings table, not
    /// its roster entry: a `[plugins.notion]` left behind would be a roster
    /// entry for a plugin that was never installed, which `config validate`
    /// reports as a problem the operator did not create.
    #[test]
    fn unselected_plugins_are_absent_entirely() {
        let rendered = render(&selected(&["github"]), SecretBackend::OnePassword);
        assert!(rendered.contains("[plugins.github]"), "{rendered}");
        assert!(rendered.contains("[github]"), "{rendered}");
        for absent in ["notion", "slack", "discord", "herdr", "orca", "macos"] {
            // Header lines only: the recipe section names plugins in prose
            // ("Needs: [plugins.slack], …"), and that is documentation, not a
            // roster entry.
            let header = format!("# [plugins.{absent}]");
            let table = format!("# [{absent}]");
            assert!(
                !rendered.lines().any(|l| l.trim_end() == header),
                "roster entry for unselected `{absent}` survived"
            );
            assert!(
                !rendered.lines().any(|l| l.trim_end() == table),
                "settings table for unselected `{absent}` survived"
            );
        }
    }

    /// Core content is unconditional, and `version` is the one active line.
    #[test]
    fn the_core_skeleton_is_always_written_and_sets_only_version() {
        let rendered = render(&selected(&[]), SecretBackend::OnePassword);
        for core in ["[[repositories]]", "[[workflows]]", "[worktree]", "[hooks]"] {
            assert!(rendered.contains(core), "core section {core} missing");
        }
        let parsed: toml::Table = rendered
            .parse()
            .expect("rendered config must be valid TOML");
        assert_eq!(
            parsed.keys().collect::<Vec<_>>(),
            vec!["version"],
            "everything but `version` must stay commented out"
        );
    }

    /// The directives are a build-time seam, not user-facing text.
    #[test]
    fn directives_never_reach_the_generated_file() {
        let rendered = render(&selected(&["github", "slack"]), SecretBackend::Keychain);
        assert!(!rendered.contains("totsuka:section"), "{rendered}");
        assert!(!rendered.contains("totsuka:secret"), "{rendered}");
        assert!(!rendered.contains("lint:raw"), "{rendered}");
    }

    /// **The backend reaches the file, not just the printed checklist.** A
    /// checklist that says `security add-generic-password` over a config that
    /// says `op://` sends the operator to register a secret nothing will read.
    #[test]
    fn secret_references_are_written_in_the_chosen_backends_form() {
        let rendered = render(&selected(&["github"]), SecretBackend::Keychain);
        assert!(
            rendered.contains(r#"# token = "keychain:totsuka/github-token""#),
            "{rendered}"
        );
        // The line's own explanation survives the rewrite.
        assert!(rendered.contains("it never goes stale"), "{rendered}");
        assert!(
            rendered.contains("Other forms: op://"),
            "the alternatives are named next to the line: {rendered}"
        );
    }

    /// **What the checklist names is what was written.** On an append the
    /// existing reference lines are left alone, so listing every account the
    /// selection *could* reference would print, say, `keychain:` commands over
    /// a file that still says `op://`.
    #[test]
    fn accounts_are_read_back_out_of_the_rendered_text() {
        let rendered = render(&selected(&["github"]), SecretBackend::Keychain);
        assert_eq!(
            accounts_mentioned_in(&rendered),
            secret_accounts(&selected(&["github"])),
            "a full render mentions exactly the accounts the selection implies"
        );
        assert!(
            accounts_mentioned_in("# nothing here\n# token = \"op://x/y/z\"\n").is_empty(),
            "text with no reference marker names no account"
        );
    }

    /// The checklist is read off the same directives the rendering is.
    #[test]
    fn secret_accounts_follow_the_selection() {
        assert_eq!(
            secret_accounts(&selected(&["github"])),
            vec!["llm-api-key", "hook-token", "github-token"]
        );
        assert!(
            secret_accounts(&selected(&[]))
                .iter()
                .all(|a| a != "github-token")
        );
        let slack = secret_accounts(&selected(&["slack"]));
        assert!(slack.contains(&"slack-user".to_string()), "{slack:?}");
        assert_eq!(
            slack.iter().filter(|a| *a == "llm-api-key").count(),
            1,
            "the LLM key appears under both [llm] and [slack.llm] but is one account: {slack:?}"
        );
    }
}
