//! Repository auto-selection: rules first, classifier fallback (F-10–F-15).
//!
//! 1. If the task carries a `repo_hint` that resolves to a configured
//!    repository, use it (F-10).
//! 2. Otherwise ask the [`RepoClassifier`], giving it each repository's
//!    summary + README head as candidates (F-11). Low confidence or an
//!    unusable answer (after one retry) falls back to `pending` for a human to
//!    confirm (F-14); a permanent API failure fails the task (§5.3).
//!
//! How the classifier is asked — prompt, schema, wire format — is the
//! classifier's business (`repo-classifier`). What happens with its answer is
//! decided here.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use plugin_protocol::Task;

use crate::ports::llm::{Candidate, Classification, ClassifyRequest, RepoClassifier};

/// Tuning for the selection pipeline.
#[derive(Debug, Clone)]
pub struct SelectConfig {
    /// Minimum classifier confidence to accept a verdict without asking a
    /// human (F-14). `[llm].confidence_threshold`.
    pub confidence_threshold: f64,
}

impl SelectConfig {
    /// The threshold when `[llm]` sets none.
    pub const DEFAULT_CONFIDENCE_THRESHOLD: f64 = 0.6;
}

impl Default for SelectConfig {
    fn default() -> Self {
        Self {
            confidence_threshold: Self::DEFAULT_CONFIDENCE_THRESHOLD,
        }
    }
}

/// The outcome of repository selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoDecision {
    /// A repository was chosen. `reason` is exposed by `--dry-run` (#64).
    Selected {
        /// Chosen repository name.
        repo: String,
        /// Why (rule match or classifier reason).
        reason: String,
    },
    /// Needs human confirmation (F-14): ambiguous/low-confidence/unusable.
    Pending {
        /// Why it is pending.
        reason: String,
    },
    /// The classification failed permanently (§5.3).
    Failed {
        /// The failure cause.
        reason: String,
    },
}

/// Select the repository for `task` among `candidates` (F-10–F-14).
pub async fn select_repo<C: RepoClassifier>(
    task: &Task,
    candidates: &[Candidate],
    classifier: &C,
    config: &SelectConfig,
) -> RepoDecision {
    // F-10: an explicit repo hint wins.
    if let Some(hint) = &task.repo_hint
        && let Some(candidate) = resolve_hint(hint, candidates)
    {
        return RepoDecision::Selected {
            repo: candidate.name.clone(),
            reason: format!("repository hint `{hint}` matched `{}`", candidate.name),
        };
    }

    match candidates {
        [] => {
            return RepoDecision::Pending {
                reason: "no repositories are configured to choose from".to_string(),
            };
        }
        [only] => {
            return RepoDecision::Selected {
                repo: only.name.clone(),
                reason: "only one configured repository".to_string(),
            };
        }
        _ => {}
    }

    // F-11/F-14: classify, asking once more on an unusable answer.
    let request = classify_request(task, candidates);
    let mut last_error = String::new();
    for _ in 0..2 {
        match classifier.classify(&request).await {
            Ok(Classification::Repo {
                repo,
                confidence,
                reason,
            }) if confidence < config.confidence_threshold => {
                return RepoDecision::Pending {
                    reason: format!("low confidence {confidence:.2} for `{repo}`: {reason}"),
                };
            }
            Ok(Classification::Repo { repo, reason, .. }) => {
                return RepoDecision::Selected { repo, reason };
            }
            // The classifier says no candidate fits: a human decides, and
            // asking again would only repeat the verdict.
            Ok(Classification::NoneFits { reason }) => {
                return RepoDecision::Pending {
                    reason: format!("no configured repository fits: {reason}"),
                };
            }
            // A bad answer (unreadable, schema deviation, not a candidate) is
            // worth one more question, then a human (F-14).
            Err(e) if e.is_bad_answer() => last_error = e.to_string(),
            // A genuine transport/status/timeout failure — already retried with
            // backoff inside the classifier — fails the task (§5.3).
            Err(e) => {
                return RepoDecision::Failed {
                    reason: e.to_string(),
                };
            }
        }
    }
    RepoDecision::Pending {
        reason: format!("could not determine a repository: {last_error}"),
    }
}

/// Resolve a repo hint to a candidate: exact name, or the last `/`-segment
/// (e.g. `owner/repo` → `repo`).
fn resolve_hint<'a>(hint: &str, candidates: &'a [Candidate]) -> Option<&'a Candidate> {
    let tail = hint.rsplit('/').next().unwrap_or(hint);
    candidates.iter().find(|c| c.name == hint || c.name == tail)
}

/// The classification question for `task`.
fn classify_request(task: &Task, candidates: &[Candidate]) -> ClassifyRequest {
    let mut subject = vec![("Task".to_string(), task.title.clone())];
    if let Some(body) = &task.body {
        subject.push(("Description".to_string(), body.clone()));
    }
    ClassifyRequest {
        subject,
        candidates: candidates.to_vec(),
        chat_prompt: None,
    }
}

/// README head extraction with a content-hash cache (F-15).
///
/// v1 stores the first N lines keyed by the README's SHA-256, so an unchanged
/// README reuses the cached head (the hook where LLM summarization plugs in
/// later). Cache is under `$XDG_CACHE_HOME/totsuka/readme/`.
#[derive(Debug, Clone)]
pub struct ReadmeCache {
    dir: PathBuf,
}

impl ReadmeCache {
    /// A cache rooted at `cache_dir` (usually `$XDG_CACHE_HOME/totsuka`).
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: cache_dir.into().join("readme"),
        }
    }

    /// The first `lines` of `repo_path`'s README, or `None` if there is none.
    /// Cached by README content hash.
    pub fn head(&self, repo_path: &Path, lines: usize) -> Option<String> {
        let contents = read_readme(repo_path)?;
        let hash: String = Sha256::digest(contents.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let cache_file = self.dir.join(format!("{hash}-{lines}"));

        if let Ok(cached) = fs::read_to_string(&cache_file) {
            return Some(cached);
        }
        let head = head_lines(&contents, lines);
        if fs::create_dir_all(&self.dir).is_ok() {
            let _ = fs::write(&cache_file, &head);
        }
        Some(head)
    }
}

/// Read a repository's README (`README.md`, `README`, or `readme.md`).
fn read_readme(repo_path: &Path) -> Option<String> {
    for name in ["README.md", "README", "readme.md", "Readme.md"] {
        if let Ok(contents) = fs::read_to_string(repo_path.join(name)) {
            return Some(contents);
        }
    }
    None
}

/// The first `lines` lines of `text`.
fn head_lines(text: &str, lines: usize) -> String {
    text.lines().take(lines).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::llm::ClassifyError;
    use std::sync::Mutex;

    fn task(repo_hint: Option<&str>) -> Task {
        Task {
            id: "1".into(),
            source: "github".into(),
            title: "Fix the login bug".into(),
            body: Some("Users cannot sign in".into()),
            repo_hint: repo_hint.map(str::to_string),
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            message_key: None,
            instructions: None,
            handle: None,
            branch_hint: None,
        }
    }

    fn candidates() -> Vec<Candidate> {
        vec![
            Candidate {
                name: "web".into(),
                summary: Some("frontend".into()),
                readme_head: None,
            },
            Candidate {
                name: "api".into(),
                summary: Some("backend".into()),
                readme_head: None,
            },
        ]
    }

    fn verdict(repo: &str, confidence: f64, reason: &str) -> Result<Classification, ClassifyError> {
        Ok(Classification::Repo {
            repo: repo.into(),
            confidence,
            reason: reason.into(),
        })
    }

    /// Answers from a queue of canned results and records every request.
    struct Canned {
        results: Mutex<std::collections::VecDeque<Result<Classification, ClassifyError>>>,
        requests: Mutex<Vec<ClassifyRequest>>,
    }

    impl Canned {
        fn new(results: Vec<Result<Classification, ClassifyError>>) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl RepoClassifier for Canned {
        async fn classify(
            &self,
            request: &ClassifyRequest,
        ) -> Result<Classification, ClassifyError> {
            self.requests.lock().unwrap().push(request.clone());
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(ClassifyError::InvalidResponse(
                    "no more canned results".into(),
                )))
        }
    }

    async fn select(results: Vec<Result<Classification, ClassifyError>>) -> RepoDecision {
        let classifier = Canned::new(results);
        select_repo(
            &task(None),
            &candidates(),
            &classifier,
            &SelectConfig::default(),
        )
        .await
    }

    #[tokio::test]
    async fn repo_hint_is_used_without_the_classifier() {
        // owner/repo form resolves to `api`; the classifier would error if called.
        let classifier = Canned::new(vec![Err(ClassifyError::Transport(
            "should not be called".into(),
        ))]);
        let decision = select_repo(
            &task(Some("myorg/api")),
            &candidates(),
            &classifier,
            &SelectConfig::default(),
        )
        .await;
        assert_eq!(
            decision,
            RepoDecision::Selected {
                repo: "api".into(),
                reason: "repository hint `myorg/api` matched `api`".into()
            }
        );
        assert!(classifier.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_task_is_asked_about_every_candidate() {
        let classifier = Canned::new(vec![verdict("api", 0.9, "backend")]);
        select_repo(
            &task(None),
            &candidates(),
            &classifier,
            &SelectConfig::default(),
        )
        .await;
        let requests = classifier.requests.lock().unwrap();
        assert_eq!(
            requests[0].subject,
            vec![
                ("Task".to_string(), "Fix the login bug".to_string()),
                (
                    "Description".to_string(),
                    "Users cannot sign in".to_string()
                ),
            ]
        );
        assert_eq!(requests[0].candidates, candidates());
        assert!(requests[0].chat_prompt.is_none(), "the built-in prompt");
    }

    #[tokio::test]
    async fn a_confident_verdict_selects() {
        assert_eq!(
            select(vec![verdict("api", 0.92, "login is a backend concern")]).await,
            RepoDecision::Selected {
                repo: "api".into(),
                reason: "login is a backend concern".into()
            }
        );
    }

    #[tokio::test]
    async fn low_confidence_becomes_pending() {
        let decision = select(vec![verdict("api", 0.3, "unsure")]).await;
        assert!(
            matches!(decision, RepoDecision::Pending { ref reason } if reason.contains("low confidence 0.30")),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn the_threshold_is_configurable() {
        let classifier = Canned::new(vec![verdict("api", 0.7, "fairly sure")]);
        let strict = SelectConfig {
            confidence_threshold: 0.8,
        };
        let decision = select_repo(&task(None), &candidates(), &classifier, &strict).await;
        assert!(
            matches!(decision, RepoDecision::Pending { .. }),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn none_fitting_is_pending_without_asking_again() {
        let classifier = Canned::new(vec![Ok(Classification::NoneFits {
            reason: "jev: none p=0.98".into(),
        })]);
        let decision = select_repo(
            &task(None),
            &candidates(),
            &classifier,
            &SelectConfig::default(),
        )
        .await;
        assert_eq!(
            decision,
            RepoDecision::Pending {
                reason: "no configured repository fits: jev: none p=0.98".into()
            }
        );
        assert_eq!(classifier.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn two_bad_answers_become_pending_not_failed() {
        let decision = select(vec![
            Err(ClassifyError::UnknownRepo("nope".into())),
            Err(ClassifyError::InvalidResponse("not json".into())),
        ])
        .await;
        assert!(
            matches!(decision, RepoDecision::Pending { ref reason } if reason.contains("not json")),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn one_bad_answer_is_asked_again() {
        assert_eq!(
            select(vec![
                Err(ClassifyError::UnknownRepo("nope".into())),
                verdict("web", 0.8, "frontend task"),
            ])
            .await,
            RepoDecision::Selected {
                repo: "web".into(),
                reason: "frontend task".into()
            }
        );
    }

    #[tokio::test]
    async fn a_permanent_api_failure_fails_the_task() {
        for failure in [
            ClassifyError::Transport("connection refused".into()),
            ClassifyError::status(401, ""),
        ] {
            let decision = select(vec![Err(failure)]).await;
            assert!(
                matches!(decision, RepoDecision::Failed { .. }),
                "got {decision:?}"
            );
        }
    }

    #[test]
    fn readme_cache_returns_head_and_caches_by_hash() {
        let base = std::env::temp_dir().join(format!("totsuka-readme-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("README.md"), "line1\nline2\nline3\nline4\n").unwrap();

        let cache = ReadmeCache::new(base.join("cache"));
        let head = cache.head(&repo, 2).unwrap();
        assert_eq!(head, "line1\nline2");
        // Second call hits the cache (still correct).
        assert_eq!(cache.head(&repo, 2).unwrap(), "line1\nline2");
        // No README -> None.
        assert!(cache.head(&base.join("empty"), 2).is_none());

        let _ = fs::remove_dir_all(&base);
    }
}
