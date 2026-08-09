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
//!   allows." Read that narrowly: a **denied rule stays denied** no matter what
//!   the repository's `CLAUDE.md` says. It does **not** say the agent will stop
//!   pursuing what `CLAUDE.md` asked for — it will reach for whatever is still
//!   allowed, which is the whole of
//!   [#410](https://github.com/tomoya-k31/totsuka/issues/410) below. This was
//!   first written here as the answer to
//!   [#378](https://github.com/tomoya-k31/totsuka/issues/378) (a plan-mode task
//!   that branched, committed, pushed and opened a PR because the target
//!   repository's `CLAUDE.md` told it to). It is not that answer, and #378 is
//!   not fixed.
//! - deny applies in every permission mode, so it composes with
//!   `--permission-mode plan` rather than replacing it.
//!
//! # This is not a read-only guarantee. It was documented as one, and it is not.
//!
//! **[#410](https://github.com/tomoya-k31/totsuka/issues/410): a live `answer`
//! task branched, committed, pushed and opened a pull request with every rule
//! below correctly generated and applied.** The rules fired — `Write` was gone,
//! `git switch -c` was denied — and the agent went around all of them, through
//! `Bash`.
//!
//! Only one mechanism here survived that: **taking the tool away**. Patterns
//! did not. So the profiles now differ in *kind*, not in list length:
//!
//! | | `answer` | `triage` / `design` |
//! |---|---|---|
//! | edit tools | removed | removed |
//! | `Bash` | **removed** | present, filtered by `Bash(...)` patterns (weak — see below) |
//! | `--permission-mode plan` | not passed | not passed (#409) |
//! | what bypassed #410 | unavailable — there is no shell | **still open** |
//!
//! `answer` can no longer run a command at all, which is what closes both of
//! the routes #410 used:
//!
//! 1. **Writing a file through the shell.** `cat > file`, `python3 - <<EOF`,
//!    `tee` — the set of ways to do this is not closed, so it was never going
//!    to be enumerable as `Bash(...)` rules. Removing `Bash` is the only move
//!    that covers it.
//! 2. **Compound and piped commands.** In #410, with their rules present,
//!    `git add -A && git commit`, `git push … | tail -5` and
//!    `gh pr create --fill | tail -5` all ran.
//!
//!    *That* much is observed. **The mechanism is not.** The obvious
//!    explanation is that the rule is matched against the command string as a
//!    whole, so a string starting with `git add` is never tested against
//!    `Bash(git commit *)` — but Claude Code might equally be splitting the
//!    compound and evaluating each part, and failing for some other reason.
//!    Nobody has measured which. Do not build a fix on the explanation until
//!    someone does: asserting a mechanism from an outcome is the same mistake
//!    this module is retracting.
//!
//! **`triage` and `design` still have both routes open, and that is now a
//! decision rather than a gap (#409).** They write their artifact with
//! `gh issue comment`, so the shell cannot simply go. Fencing it by inspecting
//! commands was considered and **rejected**: telling
//! `gh issue comment 31 --body 'use A && B'` (harmless — the `&&` is inside
//! the text being posted) from `gh issue comment 31 && git push` needs a
//! quoting-aware shell parser, and shipping an imperfect one under a name like
//! "command safety check" would be the third thing here that reads stronger
//! than it is. Their `Bash(...)` rules remain what #410 proved insufficient;
//! what changed is that a read-only profile which ends up on a branch now
//! **fails instead of publishing** (`run::read_only_side_effect`). That does
//! not prevent a push — by then it has happened — but it stops the silent
//! success #410 produced. The real boundary is a sandbox
//! ([#418](https://github.com/tomoya-k31/totsuka/issues/418)).
//!
//! **Dropping plan mode for `answer` has an unmeasured edge.** What was
//! measured is narrow: plan mode did not stop a `Bash` file write. It does not
//! follow that plan mode did *nothing* — the pane now starts in the ambient
//! default mode instead, and nothing here replaces plan's blanket over tools
//! this list never names (`WebFetch`, `WebSearch`, `mcp__*`). Two shapes to
//! watch for in the next live run: a target repo whose own settings allow a
//! write-capable MCP tool, and a read-only tool that used to be free under
//! plan now raising a permission prompt an unattended pane cannot answer. The
//! second is at least loud (it escalates), not silent.
//!
//! **This is still not a read-only guarantee, for any profile.** Removing
//! `Bash` closes the routes that were *measured*; it says nothing about MCP
//! tools, subagents, or a tool added later. A hard guarantee needs a sandbox.
//! **Do not write anywhere that these rules make a profile read-only** — that
//! sentence was in the security policy and the ADR before #410 disproved it,
//! and a documented promise nobody can keep is worse than no promise at all.
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

/// The edit tools an agent is not given at all in a read-only profile.
///
/// Bare names rather than path-scoped rules — a path-scoped `Write(path)` is
/// accepted and then never consulted.
///
/// **On its own this does not stop the agent writing files.** #410 observed
/// `cat >`, `cat >>` and `python3 - <<EOF` writing the worktree in a session
/// where `Write` was correctly removed. It closes the direct route only; the
/// profile has to close `Bash` too ([`DENY_SHELL`]) for that to mean anything.
const DENY_FILE_EDITS: &[&str] = &["Edit", "Write", "NotebookEdit"];

/// The shell, removed as a tool rather than filtered by command pattern.
///
/// This is the one rule here that is a boundary rather than a speed bump, and
/// it is a boundary for the same reason [`DENY_FILE_EDITS`] is: **the tool is
/// gone**, so there is no string to get around. #410 got past every
/// `Bash(...)` pattern with `&&`, a pipe, and a heredoc, in one session; none
/// of that is available when `Bash` itself is not offered.
///
/// The cost is that the agent cannot run *anything* — no `git log`, no `gh
/// issue view`, no test suite. That is affordable for `answer`, whose job is
/// to read and reply, because reading is [`Read`]/`Grep`/`Glob` and the reply
/// goes back through the source plugin's approval gate. It is **not**
/// affordable for `triage`/`design`, which write their artifact with
/// `gh issue comment`; those need a `PreToolUse` hook that can inspect the
/// whole command instead (#409).
///
/// [`Read`]: https://code.claude.com/docs/en/settings
const DENY_SHELL: &[&str] = &["Bash"];

/// History-rewriting and publishing git commands. Pattern-based, and
/// therefore best-effort — see the module header. Reached only by the profiles
/// that still have a shell (`triage` / `design`).
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

/// Pull-request commands. Opening a PR is what `implement` is for.
///
/// Carried by `triage` / `design` only — **not** by `answer`, which has no
/// shell for them to apply to. Do not restore the words "every non-implement
/// profile" here: a comment that claims wider coverage than the code has is
/// the failure this module keeps re-learning (#410).
const DENY_PR: &[&str] = &[
    "Bash(gh pr create *)",
    "Bash(gh pr merge *)",
    "Bash(gh pr close *)",
];

/// Repository administration. Denied even where the agent is expected to write
/// through `gh`, because nothing a `triage` or `design` task legitimately does
/// requires deleting or renaming a repository. (`answer` has no shell, so it
/// does not carry this either.)
const DENY_REPO_ADMIN: &[&str] = &["Bash(gh repo delete *)", "Bash(gh repo rename *)"];

/// `gh api`, denied in the read-only profiles that still have a shell
/// (`triage` / `design`). `answer` denies `Bash` instead, so it never reaches
/// a `gh` of any kind.
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

// There is no `DENY_GH_ARTIFACTS` fragment any more (#410). It listed
// `gh issue create|comment|edit|close` and `gh repo *`, and existed to stop
// `answer` writing to GitHub while letting `triage`/`design` do exactly that.
// `answer` now denies `Bash` outright, which covers all of it and cannot be
// worked around with `&&`; the profiles that *need* those commands were never
// covered by it.

/// Whether Claude's `--permission-mode plan` would add nothing but its
/// approval gate for `profile`, so the dispatch is better off without it
/// (#410/#409).
///
/// True for every read-only profile. That is a **reversal** of the narrower
/// rule this shipped with, which asked whether the rules removed every write
/// tool and therefore covered `answer` only. What changed is not the evidence
/// about `answer` but the accounting for `triage`/`design`:
///
/// - plan mode's contribution to *enforcement* is unmeasured at best — a live
///   session wrote a file with `cat >` while `permissionMode` was still `plan`
/// - plan mode's contribution to *breakage* is certain: `ExitPlanMode` is a
///   human approval gate, and a live `design` task sat at it for 14 minutes
///   until a human answered ([#409](https://github.com/tomoya-k31/totsuka/issues/409))
///
/// Trading a certain hang for a speculative nudge is not a trade. The profiles
/// that still have a shell get their (weak) protection from the `Bash(...)`
/// patterns and, since #409, from a read-only violation failing the task
/// instead of publishing it.
///
/// A workflow with **no** profile is excluded: it receives no deny rules at
/// all, so dropping the flag would leave it with nothing. `implement` is
/// excluded because it is not read-only in the first place.
pub fn plan_mode_only_adds_the_gate(profile: Option<Profile>) -> bool {
    profile.is_some_and(Profile::is_read_only)
}

/// The deny rules for `profile`, or `None` when the profile is meant to write
/// (only `implement`).
///
/// Assembled from the shared fragments above rather than written out per
/// profile, so the read-only sets cannot drift apart when one is edited.
pub fn deny_rules(profile: Profile) -> Option<Vec<&'static str>> {
    let mut rules: Vec<&'static str> = Vec::new();
    match profile {
        // Answers go back through the source plugin's approval gate, so the
        // agent needs no write of any kind — not even a shell.
        //
        // The `Bash(...)` patterns the other profiles carry are deliberately
        // **absent** here rather than kept "for documentation": `Bash` itself
        // is denied, so every one of them is unreachable, and a rule list that
        // reads stronger than it is was the whole failure of #410.
        Profile::Answer => {
            rules.extend(DENY_FILE_EDITS);
            rules.extend(DENY_SHELL);
        }
        // The edit tools are denied (which is not the same as a read-only
        // worktree — see the module header), but the agent files the issue /
        // writes the design comment itself (#393 D2), so `gh issue …` stays
        // open — and only that. `gh api` goes with the rest: it would reach
        // the same endpoints the rules above deny.
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

    /// `answer` must be strictly the stricter of the two read-only shapes, and
    /// the way it is stricter must be **tool removal**, not a longer pattern
    /// list. #410 is what happens when a rule list reads stronger than it is.
    #[test]
    fn answer_is_stricter_than_the_external_write_profiles_by_removing_the_shell() {
        let answer = deny_rules(Profile::Answer).unwrap();
        let design = deny_rules(Profile::Design).unwrap();

        assert!(
            answer.contains(&"Bash"),
            "answer must deny the shell itself"
        );
        assert!(
            !design.contains(&"Bash"),
            "design writes its artifact with `gh issue comment`, so it cannot \
             deny the shell — it needs a PreToolUse hook instead (#409)"
        );

        // Everything `design` blocks by pattern, `answer` blocks by not having
        // the tool at all. Asserted as an implication rather than a list, so a
        // new `Bash(...)` fragment cannot silently escape it.
        for rule in &design {
            if rule.starts_with("Bash(") {
                assert!(
                    answer.contains(&"Bash"),
                    "`{rule}` is unreachable for answer only because `Bash` is denied"
                );
            } else {
                assert!(
                    answer.contains(rule),
                    "`{rule}` is denied for design but not for answer, which is strictly stricter"
                );
            }
        }

        // And answer carries no `Bash(...)` pattern of its own: with the tool
        // gone they would all be dead text that reads like protection.
        for rule in &answer {
            assert!(
                !rule.starts_with("Bash("),
                "answer denies `Bash` outright, so the pattern `{rule}` is unreachable — \
                 delete it rather than leaving a rule that cannot fire"
            );
        }
    }

    /// Every read-only profile drops the plan flag (#409). `implement` is not
    /// read-only, and a workflow with **no** profile receives no deny rules at
    /// all — dropping the flag there would leave the dispatch with nothing.
    #[test]
    fn only_the_read_only_profiles_drop_the_plan_flag() {
        for profile in [Profile::Answer, Profile::Triage, Profile::Design] {
            assert!(plan_mode_only_adds_the_gate(Some(profile)), "{profile:?}");
        }
        assert!(!plan_mode_only_adds_the_gate(Some(Profile::Implement)));
        assert!(!plan_mode_only_adds_the_gate(None));
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
    ///
    /// Stated as "cannot reach `gh pr create`" rather than "contains the
    /// pattern", because there are two ways to be unable to run it and #410
    /// showed the pattern is the weaker one.
    #[test]
    fn only_implement_may_open_a_pull_request() {
        for profile in [Profile::Answer, Profile::Triage, Profile::Design] {
            let rules = deny_rules(profile).unwrap();
            assert!(
                rules.contains(&"Bash") || rules.contains(&"Bash(gh pr create *)"),
                "{profile:?} must not be able to open a PR — deny `Bash`, or the command"
            );
        }
    }
}
