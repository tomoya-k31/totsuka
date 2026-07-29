//! Output policy execution (#65): the concrete meaning of a task's
//! `publishing` state (F-83, F-86, F-07).
//!
//! Three policies, chosen per workflow:
//!
//! - **`pull_request`**: the agent's work ended at a commit (F-86); the
//!   Orchestrator pushes the branch and opens a pull request. Branch push and
//!   the commits-exist check go through the [`GitRunner`](crate::ports::git)
//!   seam ([`WorktreeManager`](crate::worktree)); PR creation goes through the
//!   [`PrCreator`] seam so it is testable without hitting GitHub.
//! - **`source`**: the produced artifact is written back to the task source via
//!   the plugin's `result/publish` (F-07). The Orchestrator does not push.
//! - **`none`**: nothing to publish.
//!
//! Publishing failures are recoverable: the caller keeps the worktree and
//! commits and fails the task so `task retry` can resume (issue #65).

use std::process::Command;

use crate::template;

/// A pull request to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRequest {
    /// Absolute worktree path (the PR tool runs here to resolve the remote).
    pub worktree_path: std::path::PathBuf,
    /// The already-pushed head branch.
    pub head_branch: String,
    /// PR title.
    pub title: String,
    /// PR body.
    pub body: String,
}

/// Errors from opening a pull request.
#[derive(Debug, thiserror::Error)]
pub enum PrError {
    /// The PR tool (e.g. `gh`) could not be run.
    #[error("failed to run the pull-request tool: {0}")]
    Spawn(#[from] std::io::Error),
    /// The PR tool ran but reported failure.
    #[error("pull-request creation failed: {0}")]
    Failed(String),
}

/// Opens pull requests. The real implementation shells out to `gh`; tests use a
/// recording fake so push + PR flow is exercised without network access.
pub trait PrCreator: Send + Sync {
    /// Open a PR for an already-pushed branch, returning its URL.
    fn create_pr(&self, req: &PrRequest) -> Result<String, PrError>;
}

/// Opens PRs via the GitHub CLI (`gh pr create`). Falls back to nothing else in
/// v1 — a `gh` that is not on `PATH` surfaces as [`PrError::Spawn`], and one
/// that runs but rejects the request (unauthenticated, etc.) as
/// [`PrError::Failed`] whose message names the fix (§7).
#[derive(Debug, Default, Clone, Copy)]
pub struct GhPrCreator;

impl PrCreator for GhPrCreator {
    fn create_pr(&self, req: &PrRequest) -> Result<String, PrError> {
        // `gh` infers owner/repo and the base branch from the worktree's
        // `origin` remote; `--head` names the branch we just pushed.
        let output = Command::new("gh")
            .current_dir(&req.worktree_path)
            .args([
                "pr",
                "create",
                "--head",
                &req.head_branch,
                "--title",
                &req.title,
                "--body",
                &req.body,
            ])
            .output()?;
        if output.status.success() {
            // `gh pr create` prints the PR URL on stdout.
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(PrError::Failed(format!(
                "{} → ensure `gh` is installed and authenticated (`gh auth login`)",
                stderr.trim()
            )))
        }
    }
}

/// Default PR title template. Placeholders: `{title}` `{task_id}` `{source}`.
pub const DEFAULT_PR_TITLE_TEMPLATE: &str = "{title}";

/// Default PR body template. Placeholders: `{title}` `{url}` `{source}`
/// `{task_id}` `{summary}`.
pub const DEFAULT_PR_BODY_TEMPLATE: &str = "\
Automated by totsuka for task **{title}**.

Source: {url}

{summary}";

/// Fields available to PR templates.
#[derive(Debug, Clone)]
pub struct PrContext<'a> {
    /// Task title.
    pub title: &'a str,
    /// Source URL (empty string if none).
    pub url: &'a str,
    /// Source plugin instance name.
    pub source: &'a str,
    /// Source task id.
    pub task_id: &'a str,
    /// Agent output summary (may be empty).
    pub summary: &'a str,
}

/// Render a PR template by substituting `{placeholder}` tokens in a **single
/// pass**, so a substituted value that itself contains `{token}` text (e.g. a
/// task title literally containing `{summary}`) is never re-expanded — that
/// would be template injection from untrusted task data.
///
/// The substitution itself lives in [`template::render`]; this only names the
/// fields a PR template may reference.
pub fn render_template(template: &str, ctx: &PrContext<'_>) -> String {
    template::render(
        template,
        &[
            ("title", ctx.title),
            ("url", ctx.url),
            ("source", ctx.source),
            ("task_id", ctx.task_id),
            ("summary", ctx.summary),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_substitute_every_placeholder() {
        let ctx = PrContext {
            title: "Fix login",
            url: "https://example.com/issues/1",
            source: "github",
            task_id: "1",
            summary: "Added a null check.",
        };
        assert_eq!(
            render_template(DEFAULT_PR_TITLE_TEMPLATE, &ctx),
            "Fix login"
        );
        let body = render_template(DEFAULT_PR_BODY_TEMPLATE, &ctx);
        assert!(body.contains("Fix login"));
        assert!(body.contains("https://example.com/issues/1"));
        assert!(body.contains("Added a null check."));
        // A custom template can reference any field.
        assert_eq!(
            render_template("{source}#{task_id}: {title}", &ctx),
            "github#1: Fix login"
        );
    }

    #[test]
    fn substituted_values_are_not_re_expanded() {
        // A title containing a placeholder token must be inserted literally,
        // not treated as another template directive (injection guard).
        let ctx = PrContext {
            title: "Handle {summary} field",
            url: "u",
            source: "s",
            task_id: "1",
            summary: "SECRET",
        };
        assert_eq!(
            render_template("{title}", &ctx),
            "Handle {summary} field",
            "the title's literal {{summary}} must not expand to the summary"
        );
        // Unknown placeholders are kept verbatim; unbalanced braces are literal.
        assert_eq!(render_template("{bogus}", &ctx), "{bogus}");
        assert_eq!(render_template("a { b", &ctx), "a { b");
    }
}
