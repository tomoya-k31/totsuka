//! git worktree lifecycle management (F-20–F-25, F-85).
//!
//! Implements the "1 task = 1 repo = 1 worktree = 1 branch" normalization:
//! creating worktrees (fetch → branch from `origin/{default}` → `worktree
//! add`), cleaning them up per policy (skipping dirty ones), and detecting
//! orphans. All git access goes through [`GitRunner`]
//! so the pure rendering logic is unit-tested and the git-touching paths are
//! integration-tested against a real repo.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::resolve::{ResolveError, expand_env};
use crate::ports::git::GitRunner;

/// Default branch-name template (F-21).
pub const DEFAULT_BRANCH_TEMPLATE: &str = "agent/{source}-{task_id}";
/// Default worktree location template — centralized under XDG state (F-22).
pub const DEFAULT_LOCATION_TEMPLATE: &str =
    "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{branch}";

/// How many times to retry a git command that hit transient contention
/// (lock files, mid-creation worktree reads), and the backoff.
const GIT_TRANSIENT_RETRIES: u32 = 5;
const GIT_TRANSIENT_BACKOFF_MS: u64 = 50;

/// Errors from worktree operations.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// Running git failed at the process level.
    #[error("failed to run git: {0}")]
    Io(#[from] std::io::Error),
    /// `git fetch` failed (F-25).
    #[error("git fetch failed for {repo}: {stderr} → check network and remote access")]
    Fetch {
        /// Repository path.
        repo: String,
        /// git stderr.
        stderr: String,
    },
    /// The target branch/worktree already exists (retry reuse is #55/#57).
    #[error("worktree or branch `{branch}` already exists → cancel/retry the owning task instead")]
    AlreadyExists {
        /// Branch name.
        branch: String,
    },
    /// A git command failed for another reason.
    #[error("git {command} failed: {stderr}")]
    Git {
        /// The git subcommand.
        command: String,
        /// git stderr.
        stderr: String,
    },
    /// A location template referenced an unset variable.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
}

/// The worktree cleanup policy for a workflow mode (F-23, F-85).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicy {
    /// Remove as soon as the task finishes (default for plan mode).
    Immediate,
    /// Keep for N days after the task finished, then remove.
    RetentionDays(u32),
    /// Never auto-remove; a human cleans up.
    Manual,
}

/// The outcome of a cleanup attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// The worktree (and branch) were removed.
    Removed,
    /// Kept per policy (e.g. retention not elapsed, or manual).
    Retained,
    /// Skipped because it had uncommitted changes (data-loss guard).
    DirtySkipped,
}

/// The decision phase of a cleanup (#210): computed before any side effect so
/// the caller can close the task's pane between deciding and removing —
/// `Remove` is the only decision the pane close may act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupDecision {
    /// Policy allows removal and the worktree is clean.
    Remove,
    /// Kept per policy (retention not elapsed, or manual).
    Retain,
    /// Uncommitted changes present (data-loss guard, F-23).
    Dirty,
}

/// A created worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute worktree path.
    pub path: PathBuf,
    /// The branch checked out in it.
    pub branch: String,
}

/// Sanitize a branch name for use as a single path component: `/` becomes `-`
/// so `agent/github-123` maps to the directory `agent-github-123` (avoids
/// unintended nesting, F-22 addendum).
pub fn sanitize_branch_for_path(branch: &str) -> String {
    branch.replace('/', "-")
}

/// Render a branch name from a template (F-21). Placeholders: `{source}`,
/// `{task_id}`. The result is legalized for git: task ids are
/// source-defined and may carry characters `git check-ref-format` forbids
/// (Slack ids are `{channel}:{ts}`), so the git-level constraint is
/// enforced once here at the git boundary rather than in every plugin.
pub fn render_branch(template: &str, source: &str, task_id: &str) -> String {
    let rendered = template
        .replace("{source}", source)
        .replace("{task_id}", task_id);
    let cleaned: String = rendered
        .chars()
        .map(|c| {
            // `/` stays (hierarchical branch names are the point of the
            // template); the rest of git's forbidden set becomes `-`.
            if c.is_whitespace() || c.is_control() || ":~^?*[\\".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    // Characters alone are not enough: `git check-ref-format` also rejects
    // sequences and affixes — empty components (`//`, leading/trailing `/`),
    // `..`, `@{`, a lone `@`, a `.lock` suffix, and components starting or
    // ending with `.`.
    let legalized = cleaned
        .split('/')
        .filter(|component| !component.is_empty())
        .map(|component| {
            let mut c = component.replace("..", "--").replace("@{", "-{");
            if c == "@" {
                c = "-".to_string();
            }
            if let Some(rest) = c.strip_suffix(".lock") {
                c = format!("{rest}-lock");
            }
            if let Some(rest) = c.strip_prefix('.') {
                c = format!("-{rest}");
            }
            if let Some(rest) = c.strip_suffix('.') {
                c = format!("{rest}-");
            }
            c
        })
        .collect::<Vec<_>>()
        .join("/");
    // Never empty (an all-`/` render) and never dash-led (`git worktree add
    // -b <branch>` would parse it as an option).
    if legalized.is_empty() {
        "task".to_string()
    } else if legalized.starts_with('-') {
        format!("b{legalized}")
    } else {
        legalized
    }
}

/// Context for rendering a worktree location.
#[derive(Debug, Clone, Copy)]
pub struct LocationContext<'a> {
    /// Absolute clone path of the repository.
    pub repo_path: &'a Path,
    /// Repository name (namespaces centralized worktrees).
    pub repo_name: &'a str,
    /// Source plugin name.
    pub source: &'a str,
    /// Task id.
    pub task_id: &'a str,
}

/// Render a worktree location from a template (F-22). `${ENV}` is expanded from
/// `env`; `{repo}` / `{repo_name}` / `{branch}` (sanitized) / `{task_id}` /
/// `{source}` are substituted.
pub fn render_location(
    template: &str,
    ctx: &LocationContext<'_>,
    branch: &str,
    env: &HashMap<String, String>,
) -> Result<PathBuf, WorktreeError> {
    let expanded = expand_env(template, &|k: &str| env.get(k).cloned())?;
    let sanitized = sanitize_branch_for_path(branch);
    let rendered = expanded
        .replace("{repo}", &ctx.repo_path.display().to_string())
        .replace("{repo_name}", ctx.repo_name)
        .replace("{branch}", &sanitized)
        .replace("{task_id}", ctx.task_id)
        .replace("{source}", ctx.source);
    // A leading `~` expands to `$HOME` (e.g. `worktree_location = "~/.worktrees/{branch}"`).
    if let Some(rest) = rendered.strip_prefix("~/") {
        let home = env
            .get("HOME")
            .ok_or_else(|| ResolveError::EnvNotSet("HOME".to_string()))?;
        Ok(PathBuf::from(home).join(rest))
    } else {
        Ok(PathBuf::from(rendered))
    }
}

/// Whether the policy allows removing a worktree given when the task finished.
///
/// `finished_at` / `now` are RFC 3339 UTC. A `RetentionDays` policy with no
/// `finished_at` (or an unparseable one) keeps the worktree (safe default).
pub fn policy_allows_removal(policy: CleanupPolicy, finished_at: Option<&str>, now: &str) -> bool {
    match policy {
        CleanupPolicy::Immediate => true,
        CleanupPolicy::Manual => false,
        CleanupPolicy::RetentionDays(days) => {
            let (Some(finished), Ok(now)) = (finished_at, parse_rfc3339(now)) else {
                return false;
            };
            match parse_rfc3339(finished) {
                Ok(finished) => finished + time::Duration::days(days as i64) <= now,
                Err(_) => false,
            }
        }
    }
}

fn parse_rfc3339(s: &str) -> Result<time::OffsetDateTime, time::error::Parse> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
}

/// Manages worktree lifecycle over a [`GitRunner`].
#[derive(Debug, Clone)]
pub struct WorktreeManager<G> {
    git: G,
}

/// Request to create a worktree.
#[derive(Debug, Clone, Copy)]
pub struct CreateRequest<'a> {
    /// Absolute repository clone path.
    pub repo_path: &'a Path,
    /// Repository name.
    pub repo_name: &'a str,
    /// Source plugin name.
    pub source: &'a str,
    /// Task id.
    pub task_id: &'a str,
    /// Branch template (use [`DEFAULT_BRANCH_TEMPLATE`]).
    pub branch_template: &'a str,
    /// Location template (use [`DEFAULT_LOCATION_TEMPLATE`]).
    pub location_template: &'a str,
    /// Base branch override; `None` detects `origin`'s default (F-25).
    pub base_branch: Option<&'a str>,
    /// Environment for `${ENV}` in the location template.
    pub env: &'a HashMap<String, String>,
}

impl<G: GitRunner> WorktreeManager<G> {
    /// Build a manager over a git runner.
    pub fn new(git: G) -> Self {
        Self { git }
    }

    /// Create a worktree: `git fetch` (F-25), branch from `origin/{default}`,
    /// then `git worktree add`. Serialization-free; git-lock contention is
    /// absorbed by a short retry (§5.5).
    pub fn create(&self, req: &CreateRequest<'_>) -> Result<Worktree, WorktreeError> {
        // F-25: always fetch first so we branch off fresh remote state.
        let fetch = self.run_with_transient_retry(req.repo_path, &["fetch", "origin"])?;
        if !fetch.success() {
            return Err(WorktreeError::Fetch {
                repo: req.repo_path.display().to_string(),
                stderr: fetch.stderr,
            });
        }

        let default_branch = match req.base_branch {
            Some(b) => b.to_string(),
            None => self.detect_default_branch(req.repo_path)?,
        };
        // Resolve `origin/{default}` to a commit and branch from *that*: creating
        // a branch off a remote-tracking ref sets up upstream tracking, which
        // writes `.git/config` and contends under parallel creation (§5.5).
        // Branching from a raw commit avoids that shared-config write entirely.
        let base_ref = format!("origin/{default_branch}^{{commit}}");
        let rev = self
            .git
            .run(req.repo_path, &["rev-parse", "--verify", &base_ref])?;
        if !rev.success() {
            return Err(WorktreeError::Git {
                command: "rev-parse".to_string(),
                stderr: rev.stderr,
            });
        }
        let base_commit = rev.stdout.trim().to_string();

        let branch = render_branch(req.branch_template, req.source, req.task_id);
        let ctx = LocationContext {
            repo_path: req.repo_path,
            repo_name: req.repo_name,
            source: req.source,
            task_id: req.task_id,
        };
        let path = render_location(req.location_template, &ctx, &branch, req.env)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let path_str = path.display().to_string();
        let add = self.run_with_transient_retry(
            req.repo_path,
            &["worktree", "add", &path_str, "-b", &branch, &base_commit],
        )?;
        if !add.success() {
            let combined = format!("{}{}", add.stdout, add.stderr);
            if combined.contains("already exists") || combined.contains("already used") {
                return Err(WorktreeError::AlreadyExists { branch });
            }
            return Err(WorktreeError::Git {
                command: "worktree add".to_string(),
                stderr: add.stderr,
            });
        }

        Ok(Worktree { path, branch })
    }

    /// Whether the worktree's `HEAD` has commits beyond `origin`'s **default
    /// branch** — i.e. the agent actually committed work to publish (F-86).
    ///
    /// The comparison is against `origin/{default}`, deliberately **not**
    /// against all origin remote branches: after a `pull_request` retry the
    /// task's own branch is already on `origin`, so `--remotes=origin` would
    /// count zero and wrongly report "nothing to publish". Comparing to the
    /// default branch stays truthful across a push (the agent's commits are
    /// still not on `main`).
    pub fn has_commits_to_publish(&self, worktree_path: &Path) -> Result<bool, WorktreeError> {
        let default = self.detect_default_branch(worktree_path)?;
        let range = format!("origin/{default}..HEAD");
        let out = self
            .git
            .run(worktree_path, &["rev-list", "--count", &range])?;
        if !out.success() {
            return Err(WorktreeError::Git {
                command: "rev-list".to_string(),
                stderr: out.stderr,
            });
        }
        // Surface an unparseable count as an error rather than silently reading
        // it as "0 commits" (which would wrongly fail publishing).
        let count: u64 = out.stdout.trim().parse().map_err(|_| WorktreeError::Git {
            command: "rev-list".to_string(),
            stderr: format!("unexpected `rev-list --count` output: {:?}", out.stdout),
        })?;
        Ok(count > 0)
    }

    /// Push the worktree's branch to `origin`, setting upstream (F-86). The
    /// Orchestrator — never the agent — performs the push.
    pub fn push_branch(&self, worktree_path: &Path, branch: &str) -> Result<(), WorktreeError> {
        let out =
            self.run_with_transient_retry(worktree_path, &["push", "-u", "origin", branch])?;
        if !out.success() {
            return Err(WorktreeError::Git {
                command: "push".to_string(),
                stderr: out.stderr,
            });
        }
        Ok(())
    }

    /// Detect `origin`'s default branch (e.g. `main`), falling back to `main`.
    fn detect_default_branch(&self, repo_path: &Path) -> Result<String, WorktreeError> {
        let out = self
            .git
            .run(repo_path, &["rev-parse", "--abbrev-ref", "origin/HEAD"])?;
        if out.success()
            && let Some(branch) = out.stdout.trim().strip_prefix("origin/")
            && !branch.is_empty()
        {
            return Ok(branch.to_string());
        }
        Ok("main".to_string())
    }

    /// Clean up a worktree per policy. A worktree with uncommitted changes is
    /// skipped regardless of policy (data-loss guard). Thin wrapper over
    /// [`decide_cleanup`](Self::decide_cleanup) + [`remove`](Self::remove) for
    /// callers with nothing to do between the two.
    pub fn cleanup(
        &self,
        repo_path: &Path,
        worktree_path: &Path,
        branch: &str,
        policy: CleanupPolicy,
        finished_at: Option<&str>,
        now: &str,
    ) -> Result<CleanupOutcome, WorktreeError> {
        match self.decide_cleanup(worktree_path, policy, finished_at, now)? {
            CleanupDecision::Dirty => Ok(CleanupOutcome::DirtySkipped),
            CleanupDecision::Retain => Ok(CleanupOutcome::Retained),
            CleanupDecision::Remove => self.remove(repo_path, worktree_path, branch),
        }
    }

    /// Decide what a cleanup would do, without doing it: dirty check (I/O)
    /// first (F-23), then the pure policy judgment ([`policy_allows_removal`]).
    pub fn decide_cleanup(
        &self,
        worktree_path: &Path,
        policy: CleanupPolicy,
        finished_at: Option<&str>,
        now: &str,
    ) -> Result<CleanupDecision, WorktreeError> {
        if self.has_uncommitted_changes(worktree_path)? {
            return Ok(CleanupDecision::Dirty);
        }
        if !policy_allows_removal(policy, finished_at, now) {
            return Ok(CleanupDecision::Retain);
        }
        Ok(CleanupDecision::Remove)
    }

    /// Remove a worktree (and best-effort its branch). Re-checks dirtiness
    /// first: the caller may have closed the task's pane since
    /// [`decide_cleanup`](Self::decide_cleanup), and a worktree that turned
    /// dirty in that window must still be kept — data loss (irreversible)
    /// outranks a lost pane (minor). The next sweep retries.
    pub fn remove(
        &self,
        repo_path: &Path,
        worktree_path: &Path,
        branch: &str,
    ) -> Result<CleanupOutcome, WorktreeError> {
        if self.has_uncommitted_changes(worktree_path)? {
            tracing::warn!(
                worktree = %worktree_path.display(),
                "worktree turned dirty between the cleanup decision and removal; kept (F-23)"
            );
            return Ok(CleanupOutcome::DirtySkipped);
        }

        let path_str = worktree_path.display().to_string();
        let remove =
            self.run_with_transient_retry(repo_path, &["worktree", "remove", &path_str])?;
        if !remove.success() {
            return Err(WorktreeError::Git {
                command: "worktree remove".to_string(),
                stderr: remove.stderr,
            });
        }
        // Safe branch delete: `-d` refuses to drop a branch with unmerged
        // commits, so committed-but-unpushed work is never force-destroyed
        // (extends the dirty guard to the committed case). Best-effort: a
        // missing or retained branch is not an error.
        let _ = self.git.run(repo_path, &["branch", "-d", branch])?;
        Ok(CleanupOutcome::Removed)
    }

    /// Whether a worktree has uncommitted (staged/unstaged/untracked) changes.
    fn has_uncommitted_changes(&self, worktree_path: &Path) -> Result<bool, WorktreeError> {
        let out = self.git.run(worktree_path, &["status", "--porcelain"])?;
        if !out.success() {
            // Fail closed: if we cannot determine cleanliness, do not proceed
            // to remove (surface the error rather than assuming clean).
            return Err(WorktreeError::Git {
                command: "status".to_string(),
                stderr: out.stderr,
            });
        }
        Ok(!out.stdout.trim().is_empty())
    }

    /// List worktrees with no corresponding known path (orphans, F-24). The main
    /// working tree (the repo itself) is never reported.
    pub fn detect_orphans(
        &self,
        repo_path: &Path,
        known: &HashSet<PathBuf>,
    ) -> Result<Vec<PathBuf>, WorktreeError> {
        let out = self
            .git
            .run(repo_path, &["worktree", "list", "--porcelain"])?;
        if !out.success() {
            return Err(WorktreeError::Git {
                command: "worktree list".to_string(),
                stderr: out.stderr,
            });
        }
        let main = canonical(repo_path);
        // git emits symlink-resolved absolute paths; compare against a
        // canonicalized copy of `known` so callers can pass the (possibly
        // non-canonical) path returned by `create()` without false orphans.
        let known_canonical: HashSet<PathBuf> = known.iter().map(|p| canonical(p)).collect();
        let mut orphans = Vec::new();
        for line in out.stdout.lines() {
            if let Some(raw) = line.strip_prefix("worktree ") {
                let path = PathBuf::from(raw.trim());
                let canonical_path = canonical(&path);
                if canonical_path == main {
                    continue; // never the main working tree
                }
                if !known_canonical.contains(&canonical_path) {
                    orphans.push(path);
                }
            }
        }
        Ok(orphans)
    }

    /// Run a git command, retrying briefly on transient contention (§5.5).
    fn run_with_transient_retry(
        &self,
        cwd: &Path,
        args: &[&str],
    ) -> Result<crate::ports::git::GitOutput, WorktreeError> {
        let mut attempt = 0;
        loop {
            let out = self.git.run(cwd, args)?;
            if out.success()
                || !is_transient_git_error(&out.stderr)
                || attempt >= GIT_TRANSIENT_RETRIES
            {
                return Ok(out);
            }
            attempt += 1;
            std::thread::sleep(std::time::Duration::from_millis(
                GIT_TRANSIENT_BACKOFF_MS * attempt as u64,
            ));
        }
    }
}

/// Whether git stderr indicates transient contention worth retrying: lock
/// files, or a parallel `worktree add` reading a sibling worktree's metadata
/// (`.git/worktrees/<name>/commondir`) before the creator has written it.
fn is_transient_git_error(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("index.lock")
        || s.contains("unable to lock")
        || s.contains("cannot lock ref")
        || s.contains("could not lock")
        || s.contains("another git process")
        || (s.contains("failed to read") && s.contains("commondir"))
}

/// Canonicalize a path, falling back to the original if it does not exist.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn transient_errors_cover_locks_and_commondir_race() {
        assert!(is_transient_git_error(
            "fatal: Unable to create '/r/.git/index.lock': File exists.\nAnother git process seems to be running"
        ));
        // Parallel `worktree add` reading a sibling's not-yet-written metadata.
        assert!(is_transient_git_error(
            "Preparing worktree (new branch 'agent/github-p3')\nfatal: failed to read .git/worktrees/agent-github-p2/commondir: Success\n"
        ));
        assert!(!is_transient_git_error(
            "fatal: 'bogus' is not a commit and a branch 'b' cannot be created from it"
        ));
    }

    #[test]
    fn render_branch_legalizes_forbidden_ref_characters() {
        // Slack task ids are `{channel}:{ts}` — `:` is invalid in a git ref.
        let branch = render_branch(DEFAULT_BRANCH_TEMPLATE, "slack", "C1:100.1");
        assert_eq!(branch, "agent/slack-C1-100.1");
        assert_eq!(
            render_branch("{task_id}", "s", "a b\t~^?*[\\c"),
            "a-b-------c"
        );
    }

    #[test]
    fn render_branch_legalizes_forbidden_ref_sequences() {
        // check-ref-format rejects more than single characters: `..`, `@{`,
        // a lone `@`, `.lock` suffixes, dot-led/dot-trailed components,
        // empty components, and a dash-led name (option injection).
        assert_eq!(render_branch("{task_id}", "s", "a..b...c"), "a--b--.c");
        assert_eq!(render_branch("{task_id}", "s", "x.lock"), "x-lock");
        assert_eq!(render_branch("{task_id}", "s", "a@{b/@"), "a-{b/-");
        // Dot-led becomes dash-led, which then gets the option-injection
        // guard's `b` prefix (a dash-led *name* is also rejected by git).
        assert_eq!(render_branch("{task_id}", "s", ".hidden."), "b-hidden-");
        assert_eq!(render_branch("a//{task_id}/", "s", "1"), "a/1");
        assert_eq!(render_branch("{task_id}", "s", "///"), "task");
        assert_eq!(render_branch("{task_id}", "s", "-rf"), "b-rf");
    }

    #[test]
    fn renders_default_branch_and_location() {
        let branch = render_branch(DEFAULT_BRANCH_TEMPLATE, "github", "123");
        assert_eq!(branch, "agent/github-123");

        let ctx = LocationContext {
            repo_path: Path::new("/repos/totsuka"),
            repo_name: "totsuka",
            source: "github",
            task_id: "123",
        };
        let loc = render_location(
            DEFAULT_LOCATION_TEMPLATE,
            &ctx,
            &branch,
            &env(&[("XDG_STATE_HOME", "/state")]),
        )
        .unwrap();
        // `/` in the branch is sanitized to `-` in the directory name.
        assert_eq!(
            loc,
            PathBuf::from("/state/totsuka/worktrees/totsuka/agent-github-123")
        );
    }

    #[test]
    fn adjacent_location_template_is_supported() {
        let ctx = LocationContext {
            repo_path: Path::new("/repos/totsuka"),
            repo_name: "totsuka",
            source: "github",
            task_id: "1",
        };
        let loc = render_location(
            "{repo}/../.worktrees/{branch}",
            &ctx,
            "agent/github-1",
            &env(&[]),
        )
        .unwrap();
        assert_eq!(
            loc,
            PathBuf::from("/repos/totsuka/../.worktrees/agent-github-1")
        );
    }

    #[test]
    fn sanitize_flattens_slashes() {
        assert_eq!(sanitize_branch_for_path("agent/github-1"), "agent-github-1");
        assert_eq!(sanitize_branch_for_path("plain"), "plain");
    }

    #[test]
    fn policy_immediate_and_manual() {
        let now = "2026-07-12T00:00:00Z";
        assert!(policy_allows_removal(CleanupPolicy::Immediate, None, now));
        assert!(!policy_allows_removal(
            CleanupPolicy::Manual,
            Some(now),
            now
        ));
    }

    #[test]
    fn policy_retention_days() {
        let finished = "2026-07-01T00:00:00Z";
        // 5 days retention, 3 days later -> keep.
        assert!(!policy_allows_removal(
            CleanupPolicy::RetentionDays(5),
            Some(finished),
            "2026-07-04T00:00:00Z"
        ));
        // 5 days retention, 6 days later -> remove.
        assert!(policy_allows_removal(
            CleanupPolicy::RetentionDays(5),
            Some(finished),
            "2026-07-07T00:00:00Z"
        ));
        // No finished_at -> keep (safe).
        assert!(!policy_allows_removal(
            CleanupPolicy::RetentionDays(1),
            None,
            "2026-07-07T00:00:00Z"
        ));
    }

    /// A scripted [`GitRunner`]: `status --porcelain` pops the next canned
    /// output (so cleanliness can flip between calls), every other command
    /// succeeds and is logged.
    struct ScriptedGit {
        statuses: std::cell::RefCell<Vec<&'static str>>,
        commands: std::cell::RefCell<Vec<String>>,
    }

    impl ScriptedGit {
        fn new(statuses: &[&'static str]) -> Self {
            let mut statuses: Vec<&'static str> = statuses.to_vec();
            statuses.reverse(); // pop() yields them in the given order
            Self {
                statuses: std::cell::RefCell::new(statuses),
                commands: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn ran(&self, subcommand: &str) -> bool {
            self.commands
                .borrow()
                .iter()
                .any(|c| c.starts_with(subcommand))
        }
    }

    impl GitRunner for &ScriptedGit {
        fn run(&self, _cwd: &Path, args: &[&str]) -> std::io::Result<crate::ports::git::GitOutput> {
            self.commands.borrow_mut().push(args.join(" "));
            let stdout = if args.first() == Some(&"status") {
                self.statuses
                    .borrow_mut()
                    .pop()
                    .expect("more `git status` calls than scripted outputs")
                    .to_string()
            } else {
                String::new()
            };
            Ok(crate::ports::git::GitOutput {
                status: Some(0),
                stdout,
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn decide_cleanup_covers_the_policy_table() {
        let now = "2026-07-12T00:00:00Z";
        let finished = Some("2026-07-01T00:00:00Z");
        // (clean?, policy, expected decision)
        let cases: &[(&'static str, CleanupPolicy, CleanupDecision)] = &[
            ("", CleanupPolicy::Immediate, CleanupDecision::Remove),
            ("", CleanupPolicy::Manual, CleanupDecision::Retain),
            // 30 days retention, 11 days elapsed → keep.
            (
                "",
                CleanupPolicy::RetentionDays(30),
                CleanupDecision::Retain,
            ),
            // 7 days retention, 11 days elapsed → remove.
            ("", CleanupPolicy::RetentionDays(7), CleanupDecision::Remove),
            // Dirty wins over every policy (F-23).
            (" M file", CleanupPolicy::Immediate, CleanupDecision::Dirty),
            (
                " M file",
                CleanupPolicy::RetentionDays(7),
                CleanupDecision::Dirty,
            ),
        ];
        for (status, policy, expected) in cases {
            let git = ScriptedGit::new(&[status]);
            let mgr = WorktreeManager::new(&git);
            let decision = mgr
                .decide_cleanup(Path::new("/wt"), *policy, finished, now)
                .unwrap();
            assert_eq!(decision, *expected, "policy {policy:?}, status {status:?}");
        }
    }

    #[test]
    fn remove_rechecks_dirtiness_and_skips_when_it_flipped() {
        // TOCTOU guard: clean at decide time, dirty by removal time (the pane
        // close in between is exactly such a window) → DirtySkipped, and the
        // worktree is never touched.
        let git = ScriptedGit::new(&["", " M file"]);
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.decide_cleanup(
                Path::new("/wt"),
                CleanupPolicy::Immediate,
                None,
                "2026-07-12T00:00:00Z"
            )
            .unwrap(),
            CleanupDecision::Remove
        );
        let outcome = mgr
            .remove(Path::new("/repo"), Path::new("/wt"), "b")
            .unwrap();
        assert_eq!(outcome, CleanupOutcome::DirtySkipped);
        assert!(
            !git.ran("worktree remove"),
            "a dirty worktree is never removed"
        );

        // Still clean at removal time → removed (worktree + branch commands ran).
        let git = ScriptedGit::new(&[""]);
        let mgr = WorktreeManager::new(&git);
        let outcome = mgr
            .remove(Path::new("/repo"), Path::new("/wt"), "b")
            .unwrap();
        assert_eq!(outcome, CleanupOutcome::Removed);
        assert!(git.ran("worktree remove"));
        assert!(git.ran("branch -d"));
    }

    #[test]
    fn tilde_in_location_expands_to_home() {
        let ctx = LocationContext {
            repo_path: Path::new("/r"),
            repo_name: "r",
            source: "github",
            task_id: "1",
        };
        let loc = render_location(
            "~/.worktrees/{branch}",
            &ctx,
            "agent/github-1",
            &env(&[("HOME", "/home/alice")]),
        )
        .unwrap();
        assert_eq!(loc, PathBuf::from("/home/alice/.worktrees/agent-github-1"));
        // `~/` with no HOME is an error, not a literal directory.
        assert!(render_location("~/{branch}", &ctx, "b", &env(&[])).is_err());
    }

    #[test]
    fn unknown_env_in_location_errors() {
        let ctx = LocationContext {
            repo_path: Path::new("/r"),
            repo_name: "r",
            source: "s",
            task_id: "1",
        };
        assert!(render_location("${MISSING}/{branch}", &ctx, "b", &env(&[])).is_err());
    }
}
