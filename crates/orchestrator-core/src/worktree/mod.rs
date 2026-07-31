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
use crate::paths::Paths;
use crate::ports::git::GitRunner;

/// Default worktree-directory-name template (F-22 addendum).
///
/// The directory name is derived from `(source, task_id)` directly rather than
/// from the branch: the branch is about to stop being something the
/// orchestrator picks, and a path that depends on it could not be rendered
/// before the agent has chosen a name. `(source, task_id)` is known at
/// creation time and is already unique per task, which is all the path needs.
pub const DEFAULT_WORKTREE_NAME_TEMPLATE: &str = "{source}-{task_id}";

/// Default worktree location template — centralized under XDG state (F-22).
///
/// Built from the already-resolved [`Paths`] rather than from a
/// `"${XDG_STATE_HOME}/..."` literal: [`expand_env`] treats an unset variable
/// as an error, so a literal template made the default location unresolvable
/// on any machine without `XDG_STATE_HOME` (the macOS norm), failing every
/// dispatch at worktree creation. [`Paths`] carries the XDG `$HOME/.local/state`
/// fallback, and this mirrors how the other state-directory defaults
/// (`[hooks].socket_path` / `spool_dir`) are already built in the CLI.
///
/// `{repo_name}` / `{worktree_name}` stay as placeholders for
/// [`render_location`]; only the base is pre-resolved.
///
/// The leaf was `{branch}` until the branch became agent-owned. Existing
/// worktrees need no migration regardless: every consumer (cleanup, orphan
/// detection, `doctor`, the dispatch-time reuse guard) reads the path recorded
/// in `state.db`, never a freshly rendered one, so only newly created
/// worktrees pick up the new shape.
pub fn default_location_template(paths: &Paths) -> String {
    format!(
        "{}/worktrees/{{repo_name}}/{{worktree_name}}",
        paths.state_dir().display()
    )
}

/// What `git rev-parse --abbrev-ref HEAD` prints for a detached `HEAD`.
pub const DETACHED_HEAD: &str = "HEAD";

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
    /// The target worktree path is already claimed (retry reuse is #55/#57).
    ///
    /// Path-only: git reports "already exists" for a *plain directory* at the
    /// target as readily as for a registered worktree — the remedy differs
    /// (`rm -rf` vs `git worktree remove`) and the operator cannot act on
    /// either without knowing which path is meant. The branch used to be named
    /// here too, from when the orchestrator generated it; a detached creation
    /// has no branch to collide on.
    #[error("a worktree already exists at {path} → cancel/retry the owning task instead")]
    AlreadyExists {
        /// The worktree path that could not be created.
        path: PathBuf,
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

/// What `git worktree add` should put the new worktree on. See
/// [`branch_source`](WorktreeManager::branch_source) for the priority order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BranchSource {
    /// No branch: check out this commit detached and let the agent name and
    /// create the branch itself. The normal first dispatch.
    Detached(String),
    /// The branch already exists locally — check it out untouched (no `-b`).
    Existing(String),
    /// Re-create this branch at this commit.
    CreateAt(String, String),
}

impl BranchSource {
    /// The branch this puts the worktree on, if any.
    fn branch(&self) -> Option<&str> {
        match self {
            BranchSource::Detached(_) => None,
            BranchSource::Existing(b) | BranchSource::CreateAt(b, _) => Some(b),
        }
    }
}

/// A created worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute worktree path.
    pub path: PathBuf,
    /// The branch checked out in it, or `None` when it is detached.
    pub branch: Option<String>,
    /// The `origin/{default}` commit the worktree was branched from.
    ///
    /// Recorded so cleanup can later ask whether a branch actually descends
    /// from this task's starting point before deleting it — the question that
    /// matters once branch names stop being orchestrator-generated and start
    /// sharing a namespace with the operator's own branches.
    pub base_commit: String,
}

/// Flatten a name into a single path component: `/` becomes `-`, so
/// `agent/github-123` maps to `agent-github-123` rather than nesting one
/// directory deeper than the template says (F-22 addendum).
pub fn sanitize_branch_for_path(branch: &str) -> String {
    branch.replace('/', "-")
}

/// Render a worktree directory name from a template (F-22 addendum).
/// Placeholders: `{source}`, `{task_id}`.
///
/// Uses the git-ref legalization the orchestrator applied to branch names
/// before naming moved to the agent, rather than a path-specific allowlist.
/// Those rules are a strict superset of what a single path component needs —
/// they already remove control characters, whitespace, `:` (which a Slack task
/// id always carries) and a leading `-` (which would be read as an option by
/// any command taking the name). The `/` that git-ref rules deliberately
/// preserve is then folded by [`sanitize_branch_for_path`], because a path
/// component must not nest.
pub fn render_worktree_name(template: &str, source: &str, task_id: &str) -> String {
    sanitize_branch_for_path(&render_legalized(template, source, task_id))
}

/// Substitute `{source}` / `{task_id}` and legalize the result for git.
///
/// Task ids are source-defined and may carry characters `git
/// check-ref-format` forbids (Slack ids are `{channel}:{ts}`), so the
/// git-level constraint is enforced once here at the git boundary rather than
/// in every plugin.
fn render_legalized(template: &str, source: &str, task_id: &str) -> String {
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
/// `env`; `{repo}` / `{repo_name}` / `{worktree_name}` / `{task_id}` /
/// `{source}` are substituted.
///
/// `worktree_name` is expected to come from [`render_worktree_name`], which is
/// what makes it safe as a path component. `{task_id}` and `{source}` are
/// substituted raw — they are an escape hatch for operators who want a
/// different shape, and normalizing them here would silently change the
/// meaning of an existing custom template.
pub fn render_location(
    template: &str,
    ctx: &LocationContext<'_>,
    worktree_name: &str,
    env: &HashMap<String, String>,
) -> Result<PathBuf, WorktreeError> {
    let expanded = expand_env(template, &|k: &str| env.get(k).cloned())?;
    let rendered = expanded
        .replace("{repo}", &ctx.repo_path.display().to_string())
        .replace("{repo_name}", ctx.repo_name)
        .replace("{worktree_name}", worktree_name)
        .replace("{task_id}", ctx.task_id)
        .replace("{source}", ctx.source);
    // A leading `~` expands to `$HOME` (e.g. `worktree_location = "~/.worktrees/{worktree_name}"`).
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
    /// The branch this task is already known to be on, from a previous run.
    ///
    /// `None` — the normal first dispatch — creates the worktree **detached**
    /// at the base commit and leaves naming to the agent, which is the only
    /// party that can read the repository's own convention. `Some` re-creates
    /// over a branch a previous run produced (#254), which is what keeps
    /// committed work from being stranded when a cleaned-up task is dispatched
    /// again.
    pub existing_branch: Option<&'a str>,
    /// Worktree directory-name template (use
    /// [`DEFAULT_WORKTREE_NAME_TEMPLATE`]); fills `{worktree_name}` in
    /// `location_template`.
    pub name_template: &'a str,
    /// Location template (use [`default_location_template`] for the default).
    pub location_template: &'a str,
    /// Base branch override; `None` detects `origin`'s default (F-25).
    pub base_branch: Option<&'a str>,
    /// Environment for `${ENV}` in the location template.
    pub env: &'a HashMap<String, String>,
}

/// Request to clean up a worktree.
#[derive(Debug, Clone, Copy)]
pub struct CleanupRequest<'a> {
    /// Absolute repository clone path.
    pub repo_path: &'a Path,
    /// Absolute worktree path.
    pub worktree_path: &'a Path,
    /// The branch the worktree is on, if any.
    pub branch: Option<&'a str>,
    /// The commit the worktree was created from, if one was recorded. Proves
    /// the branch is this task's before anything deletes it.
    pub base_commit: Option<&'a str>,
    /// Policy for this workflow mode (F-23, F-85).
    pub policy: CleanupPolicy,
    /// When the task reached a terminal state (RFC 3339 UTC).
    pub finished_at: Option<&'a str>,
    /// Now (RFC 3339 UTC).
    pub now: &'a str,
}

impl<G: GitRunner> WorktreeManager<G> {
    /// Build a manager over a git runner.
    pub fn new(git: G) -> Self {
        Self { git }
    }

    /// Create a worktree: `git fetch` (F-25), branch from `origin/{default}`,
    /// then `git worktree add`. Serialization-free; git-lock contention is
    /// absorbed by a short retry (§5.5).
    ///
    /// Also handles **re-creation** after a cleanup (#254): an already-existing
    /// branch is checked out rather than re-created, and a stale registration
    /// left by a manual directory removal is pruned. Re-creating at the same
    /// path does not break agent resume — Claude Code stores sessions outside
    /// the worktree, under `~/.claude/projects/<encoded-cwd>/`, keyed by the
    /// working directory — which is why the caller re-renders the *same* path.
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

        let worktree_name = render_worktree_name(req.name_template, req.source, req.task_id);
        let ctx = LocationContext {
            repo_path: req.repo_path,
            repo_name: req.repo_name,
            source: req.source,
            task_id: req.task_id,
        };
        let path = render_location(req.location_template, &ctx, &worktree_name, req.env)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let path_str = path.display().to_string();
        // Re-creation must work, not just first creation (#254): a task whose
        // worktree was cleaned up is dispatched again on retry or on a new
        // message in the same conversation.
        let mut source = self.branch_source(req.repo_path, req.existing_branch, &base_commit)?;
        let mut add = self.worktree_add(req.repo_path, &path_str, &source)?;
        if !add.success() && is_stale_registration(&add.stdout, &add.stderr) {
            // The directory vanished without `git worktree remove` (a manual
            // `rm -rf`, or a crash mid-cleanup), leaving the registration in
            // `.git/worktrees`. Drop those stale entries — `prune` only touches
            // registrations whose directory is already gone — and add again.
            let prune = self.run_with_transient_retry(req.repo_path, &["worktree", "prune"])?;
            if prune.success() {
                add = self.worktree_add(req.repo_path, &path_str, &source)?;
            } else {
                // `prune` acts on the whole repository, so a failure here is
                // worth seeing even though the `worktree add` error below is
                // what the caller ultimately gets.
                tracing::warn!(
                    repo = %req.repo_path.display(),
                    stderr = %prune.stderr,
                    "`git worktree prune` failed while recovering a stale registration"
                );
            }
        }
        if let BranchSource::CreateAt(branch, _) = &source
            && !add.success()
            && self.ref_exists(req.repo_path, &format!("refs/heads/{branch}"))?
        {
            // `git worktree add -b` is **not atomic**: it can create the branch
            // and then fail. `run_with_transient_retry` re-runs the whole
            // command, so a parallel creation losing the `commondir` race
            // (§5.5) leaves the second attempt dying on the branch its own
            // first attempt just made — "a branch named `X` already exists".
            // The branch state we read before the add is therefore stale;
            // re-read it and check out what is actually there.
            source = BranchSource::Existing(branch.clone());
            add = self.worktree_add(req.repo_path, &path_str, &source)?;
        }
        if !add.success() {
            let combined = format!("{}{}", add.stdout, add.stderr);
            if combined.contains("already exists") || combined.contains("already used") {
                return Err(WorktreeError::AlreadyExists { path });
            }
            return Err(WorktreeError::Git {
                command: "worktree add".to_string(),
                stderr: add.stderr,
            });
        }

        Ok(Worktree {
            path,
            branch: source.branch().map(str::to_string),
            base_commit,
        })
    }

    /// What a (re-)created worktree should be put on (#254).
    ///
    /// With no `existing_branch` — the normal first dispatch — the answer is
    /// always detached. Nothing here picks a name: the repository's naming
    /// convention lives inside the repository, so the agent working in the
    /// worktree is the only party able to follow it.
    ///
    /// With one, this is the re-creation path and the priority is:
    ///
    /// 1. **A surviving local branch** → check it out untouched. `remove`
    ///    deletes branches only best-effort, and it keeps any branch carrying
    ///    commits that are not on `origin` — precisely the branch worth
    ///    keeping. Resetting it to `base_commit` would destroy exactly the work
    ///    that made cleanup spare it.
    /// 2. **A surviving remote branch** → re-create at `origin/{branch}`. This
    ///    is not hypothetical: cleanup deletes a branch once every commit on it
    ///    is also on `origin`, so a published branch is *precisely* the case
    ///    where the commits survive only on the remote. Branching from
    ///    `origin/{default}` here would strand that published work.
    /// 3. **Neither** → detached at the fresh `origin/{default}` commit. The
    ///    recorded name refers to nothing that still exists, and resurrecting
    ///    an empty branch under it would assert a continuity that is not there;
    ///    the agent names the work again.
    fn branch_source(
        &self,
        repo_path: &Path,
        existing_branch: Option<&str>,
        base_commit: &str,
    ) -> Result<BranchSource, WorktreeError> {
        let Some(branch) = existing_branch else {
            return Ok(BranchSource::Detached(base_commit.to_string()));
        };
        if self.ref_exists(repo_path, &format!("refs/heads/{branch}"))? {
            return Ok(BranchSource::Existing(branch.to_string()));
        }
        // Resolve to a commit rather than passing `origin/{branch}`: branching
        // off a remote-tracking ref sets up upstream tracking, which writes
        // `.git/config` and contends under parallel creation (§5.5) — the same
        // reason `base_commit` is pre-resolved above.
        let remote = format!("refs/remotes/origin/{branch}");
        if self.ref_exists(repo_path, &remote)? {
            let rev = self.git.run(
                repo_path,
                &["rev-parse", "--verify", &format!("{remote}^{{commit}}")],
            )?;
            if rev.success() {
                return Ok(BranchSource::CreateAt(
                    branch.to_string(),
                    rev.stdout.trim().to_string(),
                ));
            }
        }
        Ok(BranchSource::Detached(base_commit.to_string()))
    }

    /// `git worktree add`, detached or onto a branch.
    fn worktree_add(
        &self,
        repo_path: &Path,
        path_str: &str,
        source: &BranchSource,
    ) -> Result<crate::ports::git::GitOutput, WorktreeError> {
        let args: Vec<&str> = match source {
            BranchSource::Detached(commit) => {
                vec!["worktree", "add", "--detach", path_str, commit]
            }
            BranchSource::Existing(branch) => vec!["worktree", "add", path_str, branch],
            BranchSource::CreateAt(branch, commit) => {
                vec!["worktree", "add", path_str, "-b", branch, commit]
            }
        };
        self.run_with_transient_retry(repo_path, &args)
    }

    /// Whether a fully-qualified ref exists in the repo.
    fn ref_exists(&self, repo_path: &Path, refname: &str) -> Result<bool, WorktreeError> {
        let out = self
            .git
            .run(repo_path, &["show-ref", "--verify", "--quiet", refname])?;
        Ok(out.success())
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

    /// The branch a worktree currently has checked out, or `None` when it is
    /// detached (or the question could not be answered).
    ///
    /// How the orchestrator learns the branch at all: creation hands the
    /// worktree over detached and the agent names and creates the branch, and
    /// there is no channel from the agent back to totsuka beyond the status
    /// marker and the final message. `HEAD` is ground truth by construction,
    /// which no self-report would be.
    ///
    /// Best-effort — a git that failed answers `None` and the caller asks
    /// again later, because every consumer of the branch already handles its
    /// absence.
    pub fn head_branch(&self, worktree_path: &Path) -> Option<String> {
        let out = self
            .git
            .run(worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .ok()?;
        if !out.success() {
            tracing::debug!(
                worktree = %worktree_path.display(),
                stderr = %out.stderr.trim(),
                "could not read worktree HEAD"
            );
            return None;
        }
        let head = out.stdout.trim();
        (!head.is_empty() && head != DETACHED_HEAD).then(|| head.to_string())
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
    pub fn cleanup(&self, req: &CleanupRequest<'_>) -> Result<CleanupOutcome, WorktreeError> {
        match self.decide_cleanup(
            req.worktree_path,
            req.base_commit,
            req.policy,
            req.finished_at,
            req.now,
        )? {
            CleanupDecision::Dirty => Ok(CleanupOutcome::DirtySkipped),
            CleanupDecision::Retain => Ok(CleanupOutcome::Retained),
            CleanupDecision::Remove => self.remove(
                req.repo_path,
                req.worktree_path,
                req.branch,
                req.base_commit,
            ),
        }
    }

    /// Decide what a cleanup would do, without doing it: the two data-loss
    /// checks (I/O) first (F-23), then the pure policy judgment
    /// ([`policy_allows_removal`]).
    pub fn decide_cleanup(
        &self,
        worktree_path: &Path,
        base_commit: Option<&str>,
        policy: CleanupPolicy,
        finished_at: Option<&str>,
        now: &str,
    ) -> Result<CleanupDecision, WorktreeError> {
        if self.has_uncommitted_changes(worktree_path)? {
            return Ok(CleanupDecision::Dirty);
        }
        if self.is_detached_with_commits(worktree_path, base_commit)? {
            return Ok(CleanupDecision::Dirty);
        }
        if !policy_allows_removal(policy, finished_at, now) {
            return Ok(CleanupDecision::Retain);
        }
        Ok(CleanupDecision::Remove)
    }

    /// Whether the worktree holds commits that no ref points at.
    ///
    /// The other data-loss guard, [`has_uncommitted_changes`], asks `git status
    /// --porcelain`, which is **empty** for work that was committed onto a
    /// detached `HEAD`. Nothing else would notice: the commits are real and
    /// clean, and `git worktree remove` takes the only reachability they had
    /// with it, leaving them recoverable solely via `git fsck --lost-found`
    /// until gc. This could not happen while the orchestrator put every
    /// worktree on a branch itself; it becomes reachable the moment creation is
    /// detached and the agent may simply not have branched.
    ///
    /// Plan mode lands here every time and is *not* affected — a plan-mode pane
    /// cannot run git at all, so the commit count is zero and cleanup proceeds
    /// as before. What this catches is an implement-mode agent that committed
    /// without branching, which is an anomaly worth a human's eyes.
    ///
    /// Without a recorded `base_commit` the question cannot be asked, so the
    /// answer is "no" — reporting an unprovable loss on every legacy row would
    /// pin every pre-v8 worktree on disk forever.
    fn is_detached_with_commits(
        &self,
        worktree_path: &Path,
        base_commit: Option<&str>,
    ) -> Result<bool, WorktreeError> {
        let Some(base) = base_commit else {
            return Ok(false);
        };
        let head = self
            .git
            .run(worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        if !head.success() || head.stdout.trim() != DETACHED_HEAD {
            return Ok(false);
        }
        let out = self.git.run(
            worktree_path,
            &["rev-list", "--count", &format!("{base}..HEAD")],
        )?;
        // Unparseable means the question was not answered. Treat that as "no
        // commits": the alternative pins the worktree forever on a broken ref,
        // and the uncommitted-changes guard above still covers the common case.
        Ok(out
            .success()
            .then(|| out.stdout.trim().parse::<u64>().ok())
            .flatten()
            .is_some_and(|n| n > 0))
    }

    /// Remove a worktree (and best-effort its branch). Re-checks dirtiness
    /// first: the caller may have closed the task's pane since
    /// [`decide_cleanup`](Self::decide_cleanup), and a worktree that turned
    /// dirty in that window must still be kept — data loss (irreversible)
    /// outranks a lost pane (minor). The next sweep retries.
    ///
    /// `branch` is optional because a worktree need not be on one: nothing
    /// guarantees a branch was ever created, and a `None` here means only
    /// "there is no branch to consider deleting", never "skip the cleanup".
    pub fn remove(
        &self,
        repo_path: &Path,
        worktree_path: &Path,
        branch: Option<&str>,
        base_commit: Option<&str>,
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
        if let Some(branch) = branch {
            self.delete_branch_if_published(repo_path, branch, base_commit)?;
        }
        Ok(CleanupOutcome::Removed)
    }

    /// Delete the task's branch, but only once every commit on it also exists
    /// on `origin` — best-effort, and never at the cost of unpushed work.
    ///
    /// **Why not `git branch -d`** (what this replaced, #266): `-d` decides
    /// "fully merged" against the *local* `HEAD` (or the branch's upstream).
    /// But [`create`](Self::create) deliberately branches from
    /// `origin/{default}`, not from the local default branch — so the moment
    /// the local default is even one commit behind its remote, the task's
    /// branch is not an ancestor of `HEAD` and `-d` refuses. **A local branch
    /// lagging its remote is the normal state**, so cleanup failed almost
    /// every time and `agent/*` branches accumulated without bound (5 of them
    /// on the real machine). Nothing surfaced it: the caller discarded the
    /// result, and the unit test only asserted that the command *ran*.
    ///
    /// The replacement asks the question that actually matters — **is
    /// anything here only local?** — with `rev-list --count <branch> --not
    /// --remotes=origin`:
    ///
    /// - `0` → every commit is reachable from some `origin` ref, so `-D`
    ///   destroys nothing. This covers both real cases: a branch whose work
    ///   was merged into the default branch, and a `pull_request` branch that
    ///   was pushed and still has its PR open (the case `-d` got right via the
    ///   upstream rule — a narrower `merge-base --is-ancestor origin/{default}`
    ///   test would have started *keeping* those).
    /// - `> 0` → committed-but-unpushed work exists. Keep the branch. That is
    ///   the guard `-d` was there for, and the only reason it is preserved.
    ///
    /// A repository with no `origin` refs at all counts every commit as
    /// unpushed and therefore keeps the branch — the conservative answer.
    fn delete_branch_if_published(
        &self,
        repo_path: &Path,
        branch: &str,
        base_commit: Option<&str>,
    ) -> Result<(), WorktreeError> {
        if !self.branch_descends_from_base(repo_path, branch, base_commit)? {
            tracing::info!(
                branch,
                "branch kept: it does not descend from this worktree's base commit, \
                 so it is not this task's to delete"
            );
            return Ok(());
        }
        let unpublished = self.git.run(
            repo_path,
            &["rev-list", "--count", branch, "--not", "--remotes=origin"],
        )?;
        let count = unpublished
            .success()
            .then(|| unpublished.stdout.trim().parse::<u64>().ok())
            .flatten();
        let Some(count) = count else {
            // Could not tell — keep the branch. Being unable to prove the work
            // is safe is not permission to destroy it.
            tracing::debug!(
                branch,
                stderr = %unpublished.stderr.trim(),
                "could not count unpublished commits; keeping the branch"
            );
            return Ok(());
        };
        if count > 0 {
            tracing::info!(
                branch,
                unpublished = count,
                "branch kept: it has commits that are not on origin"
            );
            return Ok(());
        }
        let deleted = self.git.run(repo_path, &["branch", "-D", branch])?;
        if deleted.success() {
            tracing::debug!(branch, "branch deleted with its worktree");
        } else {
            // Already gone, or something is holding it. Not an error — the
            // worktree, which is what cleanup is about, is already removed.
            tracing::debug!(
                branch,
                stderr = %deleted.stderr.trim(),
                "branch not deleted; it may already be gone"
            );
        }
        Ok(())
    }

    /// Whether `branch` contains the commit this task's worktree started from.
    ///
    /// The ownership question that "is every commit on `origin`?" does not
    /// answer. That test was sufficient only while the branch name was
    /// orchestrator-generated and so could not name anything a human made;
    /// once the agent picks the name from the repository's own convention it
    /// lands in the operator's namespace, and "fully pushed" describes plenty
    /// of branches cleanup has no business force-deleting. A branch cut from an
    /// older default branch does not contain this task's base commit.
    ///
    /// `None` — a row written before the base commit was recorded — answers
    /// "no". Being unable to prove ownership is not permission to destroy, the
    /// same way an uncountable commit count already keeps the branch.
    fn branch_descends_from_base(
        &self,
        repo_path: &Path,
        branch: &str,
        base_commit: Option<&str>,
    ) -> Result<bool, WorktreeError> {
        let Some(base) = base_commit else {
            return Ok(false);
        };
        let out = self
            .git
            .run(repo_path, &["merge-base", "--is-ancestor", base, branch])?;
        Ok(out.success())
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

/// Whether git refused the `worktree add` because the target path is still
/// registered while its directory is gone — recoverable with `worktree prune`
/// (#254). git's own message names the remedy:
///
/// ```text
/// fatal: '<path>' is a missing but already registered worktree;
/// use 'add -f' to override, or 'prune' or 'remove' to clear
/// ```
///
/// Matched on git's exact phrasing rather than on looser combinations of
/// "missing" and "registered": a *live* worktree at the same path (an
/// operator's own, or another task's) must error out instead of being pruned
/// away, so a false positive is far worse than a false negative. Should git
/// ever reword this, recovery simply stops happening and the caller gets the
/// plain `worktree add` error — the safe direction to fail in.
fn is_stale_registration(stdout: &str, stderr: &str) -> bool {
    format!("{stdout}{stderr}")
        .to_ascii_lowercase()
        .contains("missing but already registered")
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
    fn stale_registration_is_distinguished_from_a_live_one() {
        assert!(is_stale_registration(
            "Preparing worktree\n",
            "fatal: '/s/wt/agent-slack-C1-1.2' is a missing but already registered worktree;\nuse 'add -f' to override, or 'prune' or 'remove' to clear\n"
        ));
        // A *live* worktree at that path is not prunable — it belongs to
        // someone, so it must surface as an error instead.
        assert!(!is_stale_registration(
            "",
            "fatal: '/s/wt/agent-slack-C1-1.2' already exists\n"
        ));
        assert!(!is_stale_registration(
            "",
            "fatal: 'agent/slack-C1-1.2' is already used by worktree at '/s/wt/other'\n"
        ));
    }

    #[test]
    fn render_legalizes_forbidden_ref_characters() {
        // Slack task ids are `{channel}:{ts}` — `:` is invalid in a git ref
        // and unwelcome in a path.
        assert_eq!(
            render_legalized("{source}-{task_id}", "slack", "C1:100.1"),
            "slack-C1-100.1"
        );
        assert_eq!(
            render_legalized("{task_id}", "s", "a b\t~^?*[\\c"),
            "a-b-------c"
        );
    }

    #[test]
    fn render_legalizes_forbidden_ref_sequences() {
        // check-ref-format rejects more than single characters: `..`, `@{`,
        // a lone `@`, `.lock` suffixes, dot-led/dot-trailed components,
        // empty components, and a dash-led name (option injection).
        assert_eq!(render_legalized("{task_id}", "s", "a..b...c"), "a--b--.c");
        assert_eq!(render_legalized("{task_id}", "s", "x.lock"), "x-lock");
        assert_eq!(render_legalized("{task_id}", "s", "a@{b/@"), "a-{b/-");
        // Dot-led becomes dash-led, which then gets the option-injection
        // guard's `b` prefix (a dash-led *name* is also rejected by git).
        assert_eq!(render_legalized("{task_id}", "s", ".hidden."), "b-hidden-");
        assert_eq!(render_legalized("a//{task_id}/", "s", "1"), "a/1");
        assert_eq!(render_legalized("{task_id}", "s", "///"), "task");
        assert_eq!(render_legalized("{task_id}", "s", "-rf"), "b-rf");
    }

    #[test]
    fn renders_default_location() {
        let name = render_worktree_name(DEFAULT_WORKTREE_NAME_TEMPLATE, "github", "123");
        assert_eq!(name, "github-123");

        let ctx = LocationContext {
            repo_path: Path::new("/repos/totsuka"),
            repo_name: "totsuka",
            source: "github",
            task_id: "123",
        };
        // An operator-written template with a `${ENV}` reference — the shape
        // the built-in default used to have, kept here because user config
        // still supports it.
        let loc = render_location(
            "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{worktree_name}",
            &ctx,
            &name,
            &env(&[("XDG_STATE_HOME", "/state")]),
        )
        .unwrap();
        assert_eq!(
            loc,
            PathBuf::from("/state/totsuka/worktrees/totsuka/github-123")
        );
    }

    #[test]
    fn worktree_name_is_legalized_and_flattened() {
        // The `:` of a Slack task id would otherwise reach the filesystem,
        // where it is a separator in `PATH`-shaped variables and a host/path
        // delimiter to `scp`/`rsync`.
        assert_eq!(
            render_worktree_name(
                DEFAULT_WORKTREE_NAME_TEMPLATE,
                "slack",
                "C0ABCDEF12:1720000000.123456"
            ),
            "slack-C0ABCDEF12-1720000000.123456"
        );
        // Any `/` a task id carries is folded rather than nesting the worktree
        // one directory deeper than the template says.
        assert_eq!(
            render_worktree_name(DEFAULT_WORKTREE_NAME_TEMPLATE, "notion", "a/b"),
            "notion-a-b"
        );
        // The option-injection and empty-render guards apply here too.
        assert_eq!(render_worktree_name("{task_id}", "s", "-rf"), "b-rf");
        assert_eq!(render_worktree_name("{task_id}", "s", "///"), "task");
    }

    #[test]
    fn default_location_template_is_preresolved_and_needs_no_env() {
        let paths = Paths::from_env(|k| match k {
            "HOME" => Some("/home/t".to_string()),
            _ => None,
        })
        .unwrap();
        let template = default_location_template(&paths);
        // No `${ENV}` left to expand — this is the whole point: `expand_env`
        // errors on an unset variable, so a `${XDG_STATE_HOME}` default broke
        // every dispatch on a machine that does not set it.
        assert_eq!(
            template,
            "/home/t/.local/state/totsuka/worktrees/{repo_name}/{worktree_name}"
        );

        let ctx = LocationContext {
            repo_path: Path::new("/repos/totsuka"),
            repo_name: "totsuka",
            source: "slack",
            task_id: "C1:100.1",
        };
        let name = render_worktree_name(DEFAULT_WORKTREE_NAME_TEMPLATE, ctx.source, ctx.task_id);
        // Rendering succeeds against an *empty* environment.
        let loc = render_location(&template, &ctx, &name, &HashMap::new()).unwrap();
        assert_eq!(
            loc,
            PathBuf::from("/home/t/.local/state/totsuka/worktrees/totsuka/slack-C1-100.1")
        );
    }

    #[test]
    fn default_location_base_still_honours_xdg_state_home() {
        // The pre-resolved base must keep matching what `${XDG_STATE_HOME}`
        // would have expanded to. Only the *leaf* moved from `{branch}` to
        // `{worktree_name}`; if the base drifted too, every existing install
        // would find its worktree tree relocated wholesale rather than just
        // naming new directories differently.
        let paths = Paths::from_env(|k| match k {
            "HOME" => Some("/home/t".to_string()),
            "XDG_STATE_HOME" => Some("/state".to_string()),
            _ => None,
        })
        .unwrap();
        let ctx = LocationContext {
            repo_path: Path::new("/repos/totsuka"),
            repo_name: "totsuka",
            source: "github",
            task_id: "123",
        };
        let name = render_worktree_name(DEFAULT_WORKTREE_NAME_TEMPLATE, ctx.source, ctx.task_id);
        let new = render_location(
            &default_location_template(&paths),
            &ctx,
            &name,
            &HashMap::new(),
        )
        .unwrap();
        let expected = render_location(
            "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{worktree_name}",
            &ctx,
            &name,
            &env(&[("XDG_STATE_HOME", "/state")]),
        )
        .unwrap();
        assert_eq!(new, expected);
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
            "{repo}/../.worktrees/{worktree_name}",
            &ctx,
            "github-1",
            &env(&[]),
        )
        .unwrap();
        assert_eq!(loc, PathBuf::from("/repos/totsuka/../.worktrees/github-1"));
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
        /// What `rev-list --count` reports: the number of commits on the
        /// branch that are on no `origin` ref. `"0"` = fully published.
        unpublished: std::cell::RefCell<&'static str>,
        /// What `rev-parse --abbrev-ref HEAD` reports. `"HEAD"` = detached.
        head: std::cell::RefCell<&'static str>,
        /// What `rev-list --count <base>..HEAD` reports.
        ahead_of_base: std::cell::RefCell<&'static str>,
        /// Whether `merge-base --is-ancestor <base> <branch>` exits 0.
        descends_from_base: std::cell::RefCell<bool>,
    }

    impl ScriptedGit {
        fn new(statuses: &[&'static str]) -> Self {
            let mut statuses: Vec<&'static str> = statuses.to_vec();
            statuses.reverse(); // pop() yields them in the given order
            Self {
                statuses: std::cell::RefCell::new(statuses),
                commands: std::cell::RefCell::new(Vec::new()),
                unpublished: std::cell::RefCell::new("0"),
                head: std::cell::RefCell::new("a-branch"),
                ahead_of_base: std::cell::RefCell::new("0"),
                descends_from_base: std::cell::RefCell::new(true),
            }
        }

        /// The branch has `count` commits that are not on origin.
        fn with_unpublished(self, count: &'static str) -> Self {
            *self.unpublished.borrow_mut() = count;
            self
        }

        /// `rev-parse --abbrev-ref HEAD` answers this (`"HEAD"` = detached).
        fn with_head(self, head: &'static str) -> Self {
            *self.head.borrow_mut() = head;
            self
        }

        /// The worktree has `count` commits beyond its base commit.
        fn with_commits_beyond_base(self, count: &'static str) -> Self {
            *self.ahead_of_base.borrow_mut() = count;
            self
        }

        /// Whether `merge-base --is-ancestor <base> <branch>` succeeds.
        fn not_descended_from_base(self) -> Self {
            *self.descends_from_base.borrow_mut() = false;
            self
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
            let mut status = Some(0);
            let stdout = if args.first() == Some(&"status") {
                self.statuses
                    .borrow_mut()
                    .pop()
                    .expect("more `git status` calls than scripted outputs")
                    .to_string()
            } else if args == ["rev-parse", "--abbrev-ref", "HEAD"] {
                self.head.borrow().to_string()
            } else if args.first() == Some(&"merge-base") {
                if !*self.descends_from_base.borrow() {
                    status = Some(1);
                }
                String::new()
            } else if args.first() == Some(&"rev-list") {
                // Two different counts share the subcommand: "beyond the base
                // commit" (`<base>..HEAD`) and "not on origin"
                // (`--not --remotes=origin`).
                if args.iter().any(|a| a.contains("..")) {
                    self.ahead_of_base.borrow().to_string()
                } else {
                    self.unpublished.borrow().to_string()
                }
            } else {
                String::new()
            };
            Ok(crate::ports::git::GitOutput {
                status,
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
                .decide_cleanup(Path::new("/wt"), Some("base"), *policy, finished, now)
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
                Some("base"),
                CleanupPolicy::Immediate,
                None,
                "2026-07-12T00:00:00Z"
            )
            .unwrap(),
            CleanupDecision::Remove
        );
        let outcome = mgr
            .remove(
                Path::new("/repo"),
                Path::new("/wt"),
                Some("b"),
                Some("base"),
            )
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
            .remove(
                Path::new("/repo"),
                Path::new("/wt"),
                Some("b"),
                Some("base"),
            )
            .unwrap();
        assert_eq!(outcome, CleanupOutcome::Removed);
        assert!(git.ran("worktree remove"));
        assert!(git.ran("branch -D b"));
    }

    #[test]
    fn a_detached_worktree_with_commits_is_not_removed() {
        // The gap between the two data-loss guards: `git status --porcelain`
        // is empty, so the dirty check passes, but the commits are reachable
        // from nothing once the worktree goes.
        let git = ScriptedGit::new(&[""])
            .with_head(DETACHED_HEAD)
            .with_commits_beyond_base("3");
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.decide_cleanup(
                Path::new("/wt"),
                Some("base"),
                CleanupPolicy::Immediate,
                None,
                "2026-07-12T00:00:00Z"
            )
            .unwrap(),
            CleanupDecision::Dirty
        );
    }

    #[test]
    fn a_detached_worktree_with_no_commits_is_removed() {
        // Plan mode's permanent state: detached, and unable to run git at all,
        // so it commits nothing. It must keep being cleaned up.
        let git = ScriptedGit::new(&[""])
            .with_head(DETACHED_HEAD)
            .with_commits_beyond_base("0");
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.decide_cleanup(
                Path::new("/wt"),
                Some("base"),
                CleanupPolicy::Immediate,
                None,
                "2026-07-12T00:00:00Z"
            )
            .unwrap(),
            CleanupDecision::Remove
        );
    }

    #[test]
    fn a_branch_that_does_not_descend_from_the_base_commit_is_kept() {
        // Fully published, so the "nothing would be lost" test says delete —
        // but it is not this task's branch, which is the question that test
        // never asked.
        let git = ScriptedGit::new(&[""]).not_descended_from_base();
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.remove(
                Path::new("/repo"),
                Path::new("/wt"),
                Some("b"),
                Some("base")
            )
            .unwrap(),
            CleanupOutcome::Removed,
            "the worktree still goes — only the branch is spared"
        );
        assert!(!git.ran("branch -D"), "{:?}", git.commands.borrow());
    }

    #[test]
    fn a_branch_with_no_recorded_base_commit_is_kept() {
        // A pre-v8 row. Being unable to prove ownership is not permission to
        // destroy, the same way an uncountable commit count already keeps it.
        let git = ScriptedGit::new(&[""]);
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.remove(Path::new("/repo"), Path::new("/wt"), Some("b"), None)
                .unwrap(),
            CleanupOutcome::Removed
        );
        assert!(!git.ran("branch -D"), "{:?}", git.commands.borrow());
    }

    #[test]
    fn head_branch_distinguishes_a_branch_from_a_detached_head() {
        let git = ScriptedGit::new(&[]).with_head("feat/agent-picked-this");
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.head_branch(Path::new("/wt")).as_deref(),
            Some("feat/agent-picked-this")
        );

        let git = ScriptedGit::new(&[]).with_head(DETACHED_HEAD);
        let mgr = WorktreeManager::new(&git);
        assert_eq!(mgr.head_branch(Path::new("/wt")), None);
    }

    #[test]
    fn a_branch_with_unpushed_commits_is_kept() {
        // The guard the old `-d` existed for, and the only reason the new
        // check is not an unconditional `-D`: work that lives nowhere but
        // this branch must survive the worktree it was written in (#266).
        let git = ScriptedGit::new(&[""]).with_unpublished("2");
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.remove(
                Path::new("/repo"),
                Path::new("/wt"),
                Some("b"),
                Some("base")
            )
            .unwrap(),
            CleanupOutcome::Removed,
            "the worktree still goes — only the branch is spared"
        );
        assert!(git.ran("worktree remove"));
        assert!(
            !git.ran("branch -D"),
            "unpushed commits must not be force-deleted: {:?}",
            git.commands.borrow()
        );
    }

    #[test]
    fn an_uncountable_branch_is_kept() {
        // `rev-list` answered something unparseable (a broken ref, a git that
        // failed). Not being able to prove the work is safe is not permission
        // to destroy it.
        let git = ScriptedGit::new(&[""]).with_unpublished("not a number");
        let mgr = WorktreeManager::new(&git);
        assert_eq!(
            mgr.remove(
                Path::new("/repo"),
                Path::new("/wt"),
                Some("b"),
                Some("base")
            )
            .unwrap(),
            CleanupOutcome::Removed
        );
        assert!(!git.ran("branch -D"), "{:?}", git.commands.borrow());
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
            "~/.worktrees/{worktree_name}",
            &ctx,
            "github-1",
            &env(&[("HOME", "/home/alice")]),
        )
        .unwrap();
        assert_eq!(loc, PathBuf::from("/home/alice/.worktrees/github-1"));
        // `~/` with no HOME is an error, not a literal directory.
        assert!(render_location("~/{worktree_name}", &ctx, "b", &env(&[])).is_err());
    }

    #[test]
    fn unknown_env_in_location_errors() {
        let ctx = LocationContext {
            repo_path: Path::new("/r"),
            repo_name: "r",
            source: "s",
            task_id: "1",
        };
        assert!(render_location("${MISSING}/{worktree_name}", &ctx, "b", &env(&[])).is_err());
    }
}
