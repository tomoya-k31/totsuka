//! The `permissions.deny` sets a profile earns in Claude's `--settings` file
//! (#395, [#393](https://github.com/tomoya-k31/totsuka/issues/393) D4).
//!
//! # Why this is not configurable
//!
//! These lists decide what an agent may do, so they live in Rust and nowhere
//! else. A config key that composed deny rules from strings would let prose —
//! a value that reads like documentation — hand out permissions. That is the
//! same conclusion, for the same reason, that
//! [ADR-0023](https://github.com/tomoya-k31/totsuka/blob/main/docs/decisions/adr-0023-configurable-prompt-surface.md)
//! reached about opencode's deny map.
//!
//! # Why a deny list is worth anything
//!
//! From Claude Code's permission model (`code.claude.com/docs/en/permissions`):
//!
//! - deny rules **merge across scopes**, and a tool denied anywhere cannot be
//!   allowed by any scope's allow list. The rules totsuka passes through
//!   `--settings` therefore beat the target repository's own
//!   `.claude/settings.json`.
//! - "Permission rules are enforced by Claude Code, not by the model.
//!   Instructions in your prompt or CLAUDE.md … don't change what Claude Code
//!   allows." That sentence is the whole point: [#378] was a plan-mode task
//!   that branched, committed, pushed and opened a PR because the target
//!   repository's `CLAUDE.md` told it to. Prose could not be answered with
//!   prose.
//! - deny applies in every permission mode, so it composes with
//!   `--permission-mode plan` rather than replacing it.
//!
//! # What this does and does not guarantee
//!
//! | layer | mechanism | strength |
//! |---|---|---|
//! | 1 | `--permission-mode plan` | a flag; the model can be talked around it |
//! | 2 | bare tool names (`Edit` / `Write` / `NotebookEdit`) | **effectively guarantees no file edits** — the tool is removed, not filtered |
//! | 3 | `Bash(...)` patterns | best effort only (see below) |
//! | 4 | the branch-detection warning (#385) | detection after the fact |
//!
//! **`Bash(...)` is a literal prefix match on the command string.** `Bash(git
//! push *)` does not stop `/usr/bin/git push`, `sh -c "git push"`, or a `git
//! push` in the middle of a chain. Layer 3 reduces accidents; it is not a
//! boundary. A hard guarantee needs a sandbox, which is out of scope here.
//!
//! Two format traps, both load-bearing:
//!
//! - **Path-scoped `Write(path)` / `NotebookEdit(path)` rules are accepted and
//!   then never consulted** (Claude Code warns about it as of v2.1.210). Only
//!   `Edit(path)` / `Read(path)` work path-scoped. Everything here uses **bare
//!   tool names**, which have no such caveat and no version requirement.
//! - The space matters: `Bash(git *)` and `Bash(git*)` are different rules, and
//!   the latter also matches `gitk`.

use crate::config::Profile;

/// The tools an agent is not given at all in a read-only profile.
///
/// Bare names rather than path-scoped rules: this removes the tool, which is
/// the one layer here that actually holds.
const DENY_FILE_EDITS: &[&str] = &["Edit", "Write", "NotebookEdit"];

/// History-rewriting and publishing git commands. Best-effort (layer 3).
///
/// `git switch -c` / `git checkout -b` rather than bare `git switch` /
/// `git checkout`: reading another ref is legitimate work for a design task,
/// and denying it would push the agent into workarounds instead of stopping
/// anything.
const DENY_GIT_WRITES: &[&str] = &[
    "Bash(git commit *)",
    "Bash(git push *)",
    "Bash(git switch -c *)",
    "Bash(git checkout -b *)",
    "Bash(git branch *)",
    "Bash(git merge *)",
    "Bash(git rebase *)",
    "Bash(git reset *)",
    "Bash(git tag *)",
];

/// Pull-request commands, denied in every non-`implement` profile: opening a
/// PR is what `implement` is for.
const DENY_PR: &[&str] = &[
    "Bash(gh pr create *)",
    "Bash(gh pr merge *)",
    "Bash(gh pr close *)",
];

/// Repository administration. Denied even where the agent is expected to write
/// through `gh`, because nothing a `triage` or `design` task legitimately does
/// requires deleting or renaming a repository.
const DENY_REPO_ADMIN: &[&str] = &["Bash(gh repo delete *)", "Bash(gh repo rename *)"];

/// `gh api`, denied in **every** read-only profile.
///
/// It reaches every REST and GraphQL endpoint, so leaving it open while denying
/// `gh repo delete` and `gh pr create` would make those rules decorative — the
/// same operations are a `gh api -X DELETE repos/{owner}/{repo}` or
/// `gh api -X POST repos/{owner}/{repo}/pulls` away. A deny list that reads
/// stronger than it is, is worse than a short one.
///
/// The cost is real: the pattern cannot tell a `GET` from a `POST`, so
/// read-only API calls go with it. `gh issue view` / `gh pr view` / `gh search`
/// cover the reads these profiles need. **If a workflow turns out to need
/// GraphQL** — Projects v2 fields and draft issues have no `gh` subcommand —
/// that is the signal to revisit this rule deliberately, not to quietly leave
/// the hatch open.
const DENY_GH_API: &[&str] = &["Bash(gh api *)"];

/// Writing to GitHub's issue surface.
///
/// Denied for `answer` only. `triage` and `design` write their artifact this
/// way, so denying it there would deny the profile's whole purpose.
const DENY_GH_ARTIFACTS: &[&str] = &[
    "Bash(gh issue create *)",
    "Bash(gh issue comment *)",
    "Bash(gh issue edit *)",
    "Bash(gh issue close *)",
    "Bash(gh repo *)",
];

/// The deny rules for `profile`, or `None` when the profile is meant to write
/// (only `implement`).
///
/// Assembled from the shared fragments above rather than written out per
/// profile, so the two read-only sets cannot drift apart when one is edited —
/// their difference is exactly the GitHub-artifact commands, which
/// `answer_denies_a_superset_of_the_external_write_profiles` pins.
pub fn deny_rules(profile: Profile) -> Option<Vec<&'static str>> {
    let mut rules: Vec<&'static str> = Vec::new();
    match profile {
        // Answers go back through the source plugin's approval gate, so the
        // agent needs no write of any kind.
        Profile::Answer => {
            rules.extend(DENY_FILE_EDITS);
            rules.extend(DENY_GIT_WRITES);
            rules.extend(DENY_PR);
            rules.extend(DENY_GH_API);
            rules.extend(DENY_GH_ARTIFACTS);
        }
        // The worktree stays read-only, but the agent files the issue / writes
        // the design comment itself (#393 D2), so `gh issue …` stays open —
        // and only that. `gh api` goes with the rest: it would reach the same
        // endpoints the rules above deny.
        Profile::Triage | Profile::Design => {
            rules.extend(DENY_FILE_EDITS);
            rules.extend(DENY_GIT_WRITES);
            rules.extend(DENY_PR);
            rules.extend(DENY_GH_API);
            rules.extend(DENY_REPO_ADMIN);
        }
        Profile::Implement => return None,
    }
    Some(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two read-only sets must differ by the GitHub-artifact commands and
    /// nothing else. Written out per profile they drifted the moment one was
    /// edited; this pins the relationship rather than the lists.
    #[test]
    fn answer_denies_a_superset_of_the_external_write_profiles() {
        let answer = deny_rules(Profile::Answer).unwrap();
        let design = deny_rules(Profile::Design).unwrap();

        for rule in &design {
            // `gh repo delete`/`rename` are covered by `answer`'s broader
            // `Bash(gh repo *)`, so they are the one legitimate absence.
            if rule.starts_with("Bash(gh repo ") {
                assert!(
                    answer.contains(&"Bash(gh repo *)"),
                    "answer must still cover `{rule}` through the broader rule"
                );
                continue;
            }
            assert!(
                answer.contains(rule),
                "`{rule}` is denied for design but not for answer, which is strictly stricter"
            );
        }

        // And the difference is only about writing to GitHub.
        for rule in &answer {
            if !design.contains(rule) {
                assert!(
                    rule.starts_with("Bash(gh "),
                    "answer denies `{rule}` for a reason unrelated to GitHub writes; \
                     if that is intended, the fragment split above needs updating"
                );
            }
        }
    }

    #[test]
    fn triage_and_design_share_one_set() {
        assert_eq!(deny_rules(Profile::Triage), deny_rules(Profile::Design));
    }

    #[test]
    fn implement_denies_nothing() {
        assert_eq!(deny_rules(Profile::Implement), None);
    }

    /// Every read-only profile must deny the file-editing tools by their **bare
    /// names**. A path-scoped `Write(path)` is accepted by Claude Code and then
    /// never consulted, so a "fix" that scoped these to the worktree would read
    /// as tighter and enforce nothing.
    #[test]
    fn file_edit_tools_are_denied_by_bare_name() {
        for profile in [Profile::Answer, Profile::Triage, Profile::Design] {
            let rules = deny_rules(profile).unwrap();
            for tool in ["Edit", "Write", "NotebookEdit"] {
                assert!(
                    rules.contains(&tool),
                    "{profile:?} must deny the bare `{tool}`"
                );
            }
        }
    }

    /// `Bash(git *)` would also match `gitk`; `Bash(git commit*)` would miss
    /// nothing but reads as a different rule. The space before `*` is part of
    /// the semantics, so it is pinned rather than left to a later tidy-up.
    #[test]
    fn bash_rules_keep_the_space_before_the_wildcard() {
        for profile in [Profile::Answer, Profile::Triage, Profile::Design] {
            for rule in deny_rules(profile).unwrap() {
                let Some(inner) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')'))
                else {
                    continue;
                };
                assert!(
                    inner.ends_with(" *"),
                    "`{rule}`: a wildcard must be preceded by a space, or the rule also \
                     matches commands that merely start with the same letters"
                );
            }
        }
    }

    /// **A denied command must not be reachable through `gh api`.**
    ///
    /// `gh api` speaks to every REST and GraphQL endpoint, so
    /// `Bash(gh repo delete *)` means nothing next to an allowed
    /// `gh api -X DELETE repos/{owner}/{repo}`, and `Bash(gh pr create *)`
    /// means nothing next to `gh api -X POST .../pulls`. Any profile that
    /// denies a `gh` command has to deny the hatch as well, or the list reads
    /// stronger than it enforces.
    #[test]
    fn no_profile_denies_a_gh_command_while_leaving_gh_api_open() {
        for profile in [Profile::Answer, Profile::Triage, Profile::Design] {
            let rules = deny_rules(profile).unwrap();
            let denies_a_gh_command = rules
                .iter()
                .any(|r| r.starts_with("Bash(gh ") && !r.starts_with("Bash(gh api"));
            if denies_a_gh_command {
                assert!(
                    rules.contains(&"Bash(gh api *)"),
                    "{profile:?} denies specific `gh` commands but leaves `gh api` open, \
                     which reaches the same endpoints"
                );
            }
        }
    }

    /// The `implement` profile is the only one that may open a PR — that is
    /// what distinguishes it. If a read-only profile ever stopped denying this,
    /// the symptom would be a PR nobody asked for.
    #[test]
    fn only_implement_may_open_a_pull_request() {
        for profile in [Profile::Answer, Profile::Triage, Profile::Design] {
            assert!(
                deny_rules(profile)
                    .unwrap()
                    .contains(&"Bash(gh pr create *)"),
                "{profile:?} must not be able to open a PR"
            );
        }
    }
}
