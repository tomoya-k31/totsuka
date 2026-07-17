//! Repository auto-selection: rules first, LLM fallback (F-10–F-15).
//!
//! 1. If the task carries a `repo_hint` that resolves to a configured
//!    repository, use it (F-10).
//! 2. Otherwise classify with an OpenAI-compatible model, giving it each
//!    repository's summary + README head as candidates (F-11). Low confidence
//!    or an unusable answer (after one retry) falls back to `pending` for a
//!    human to confirm (F-14); a permanent API failure fails the task (§5.3).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use plugin_protocol::Task;

use crate::ports::llm::{ChatRequest, LlmError, LlmRouter};

/// A repository the task could target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCandidate {
    /// Repository name (config `[[repositories]].name`).
    pub name: String,
    /// Free-text summary for the LLM (F-61).
    pub summary: Option<String>,
    /// README head lines (F-11).
    pub readme_head: Option<String>,
}

/// Tuning for the selection pipeline.
#[derive(Debug, Clone)]
pub struct SelectConfig {
    /// Minimum self-reported confidence to accept an LLM answer (F-14).
    pub confidence_threshold: f64,
    /// Max output tokens for the classification call.
    pub max_tokens: Option<u32>,
}

impl Default for SelectConfig {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.6,
            max_tokens: Some(256),
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
        /// Why (rule match or LLM reason).
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
pub async fn select_repo<L: LlmRouter>(
    task: &Task,
    candidates: &[RepoCandidate],
    llm: &L,
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

    // F-11/F-14: classify with the LLM, retrying once on an unusable answer.
    let request = build_request(task, candidates, config);
    let mut last_error = String::new();
    for _ in 0..2 {
        match llm.chat_json(&request).await {
            Ok(value) => match parse_classification(&value, candidates) {
                Ok(c) => {
                    if c.confidence < config.confidence_threshold {
                        return RepoDecision::Pending {
                            reason: format!(
                                "low confidence {:.2} for `{}`: {}",
                                c.confidence, c.repo, c.reason
                            ),
                        };
                    }
                    return RepoDecision::Selected {
                        repo: c.repo,
                        reason: c.reason,
                    };
                }
                Err(why) => last_error = why, // unusable -> retry once
            },
            // A schema deviation / unparseable content is a *bad answer*, not a
            // transport failure: retry once then fall back to pending (F-14).
            Err(e @ LlmError::InvalidResponse(_)) => last_error = e.to_string(),
            // A genuine transport/status/timeout failure — already retried with
            // backoff inside the router — fails the task (§5.3).
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

/// A parsed LLM classification.
struct Classification {
    repo: String,
    confidence: f64,
    reason: String,
}

/// Resolve a repo hint to a candidate: exact name, or the last `/`-segment
/// (e.g. `owner/repo` → `repo`).
fn resolve_hint<'a>(hint: &str, candidates: &'a [RepoCandidate]) -> Option<&'a RepoCandidate> {
    let tail = hint.rsplit('/').next().unwrap_or(hint);
    candidates.iter().find(|c| c.name == hint || c.name == tail)
}

/// Build the classification request (prompt + JSON schema).
fn build_request(task: &Task, candidates: &[RepoCandidate], config: &SelectConfig) -> ChatRequest {
    let mut user = format!("Task: {}\n", task.title);
    if let Some(body) = &task.body {
        user.push_str(&format!("Description: {body}\n"));
    }
    user.push_str("\nCandidate repositories:\n");
    for c in candidates {
        user.push_str(&format!("- {}", c.name));
        if let Some(s) = &c.summary {
            user.push_str(&format!(": {s}"));
        }
        if let Some(r) = &c.readme_head {
            user.push_str(&format!("\n  README: {}", r.replace('\n', " ")));
        }
        user.push('\n');
    }
    user.push_str("\nChoose the single most appropriate repository.");

    let names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
    let json_schema = json!({
        "type": "object",
        "properties": {
            "repo": { "type": "string", "enum": names },
            "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
            "reason": { "type": "string" },
        },
        "required": ["repo", "confidence", "reason"],
        "additionalProperties": false,
    });

    ChatRequest {
        system: "You route a software task to the correct repository. Respond only with the \
                 structured JSON. Pick from the candidate names exactly."
            .to_string(),
        user,
        json_schema,
        max_tokens: config.max_tokens,
    }
}

/// Parse and validate an LLM classification against the candidate set.
fn parse_classification(
    value: &Value,
    candidates: &[RepoCandidate],
) -> Result<Classification, String> {
    let repo = value["repo"].as_str().ok_or("missing `repo`")?.to_string();
    if !candidates.iter().any(|c| c.name == repo) {
        return Err(format!("unknown repository `{repo}`"));
    }
    let confidence = value["confidence"]
        .as_f64()
        .ok_or("missing/invalid `confidence`")?;
    if !(0.0..=1.0).contains(&confidence) {
        return Err(format!("`confidence` out of range [0,1]: {confidence}"));
    }
    // `reason` is required by the schema and surfaced in --dry-run; a missing or
    // non-string value is a schema deviation, so route it to retry→pending (F-14)
    // rather than silently accepting an empty rationale.
    let reason = value["reason"]
        .as_str()
        .ok_or("missing/invalid `reason`")?
        .to_string();
    Ok(Classification {
        repo,
        confidence,
        reason,
    })
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
    use crate::ports::llm::LlmError;
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
            thread_key: None,
        }
    }

    fn candidates() -> Vec<RepoCandidate> {
        vec![
            RepoCandidate {
                name: "web".into(),
                summary: Some("frontend".into()),
                readme_head: None,
            },
            RepoCandidate {
                name: "api".into(),
                summary: Some("backend".into()),
                readme_head: None,
            },
        ]
    }

    /// A mock router returning a queue of canned results.
    struct MockRouter {
        results: Mutex<std::collections::VecDeque<Result<Value, LlmError>>>,
    }
    impl MockRouter {
        fn new(results: Vec<Result<Value, LlmError>>) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
            }
        }
    }
    impl LlmRouter for MockRouter {
        fn chat_json(
            &self,
            _request: &ChatRequest,
        ) -> impl std::future::Future<Output = Result<Value, LlmError>> + Send {
            let next =
                self.results
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Err(LlmError::InvalidResponse(
                        "no more mock results".into(),
                    )));
            async move { next }
        }
    }

    #[tokio::test]
    async fn repo_hint_is_used_without_llm() {
        // owner/repo form resolves to `api`; router would error if called.
        let llm = MockRouter::new(vec![Err(LlmError::Transport(
            "should not be called".into(),
        ))]);
        let decision = select_repo(
            &task(Some("myorg/api")),
            &candidates(),
            &llm,
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
    }

    #[tokio::test]
    async fn llm_unique_high_confidence_selects() {
        let llm = MockRouter::new(vec![Ok(json!({
            "repo": "api", "confidence": 0.92, "reason": "login is a backend concern"
        }))]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert_eq!(
            decision,
            RepoDecision::Selected {
                repo: "api".into(),
                reason: "login is a backend concern".into()
            }
        );
    }

    #[tokio::test]
    async fn low_confidence_becomes_pending() {
        let llm = MockRouter::new(vec![Ok(json!({
            "repo": "api", "confidence": 0.3, "reason": "unsure"
        }))]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert!(
            matches!(decision, RepoDecision::Pending { .. }),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn unknown_repo_retries_then_pending() {
        // Two invalid answers (unknown repo) -> pending after the single retry.
        let llm = MockRouter::new(vec![
            Ok(json!({"repo": "nope", "confidence": 0.9, "reason": "x"})),
            Ok(json!({"repo": "still-nope", "confidence": 0.9, "reason": "y"})),
        ]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert!(
            matches!(decision, RepoDecision::Pending { ref reason } if reason.contains("unknown repository")),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn retry_recovers_after_one_bad_answer() {
        let llm = MockRouter::new(vec![
            Ok(json!({"repo": "nope", "confidence": 0.9, "reason": "x"})),
            Ok(json!({"repo": "web", "confidence": 0.8, "reason": "frontend task"})),
        ]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert_eq!(
            decision,
            RepoDecision::Selected {
                repo: "web".into(),
                reason: "frontend task".into()
            }
        );
    }

    #[tokio::test]
    async fn missing_reason_retries_then_pending() {
        // Schema deviation (no `reason`) must not be accepted as an empty
        // rationale; both attempts deviate -> pending (F-14).
        let llm = MockRouter::new(vec![
            Ok(json!({"repo": "api", "confidence": 0.9})),
            Ok(json!({"repo": "api", "confidence": 0.9})),
        ]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert!(
            matches!(decision, RepoDecision::Pending { ref reason } if reason.contains("reason")),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn out_of_range_confidence_retries_then_pending() {
        let llm = MockRouter::new(vec![
            Ok(json!({"repo": "api", "confidence": 1.5, "reason": "x"})),
            Ok(json!({"repo": "api", "confidence": -0.1, "reason": "y"})),
        ]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert!(
            matches!(decision, RepoDecision::Pending { ref reason } if reason.contains("out of range")),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn permanent_api_failure_fails_the_task() {
        let llm = MockRouter::new(vec![Err(LlmError::Transport("connection refused".into()))]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert!(
            matches!(decision, RepoDecision::Failed { .. }),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn malformed_response_retries_then_pending_not_failed() {
        // Schema deviation (non-JSON content) is a bad answer -> retry once ->
        // pending, NOT a permanent failure (F-14).
        let llm = MockRouter::new(vec![
            Err(LlmError::InvalidResponse("not json".into())),
            Err(LlmError::InvalidResponse("still not json".into())),
        ]);
        let decision =
            select_repo(&task(None), &candidates(), &llm, &SelectConfig::default()).await;
        assert!(
            matches!(decision, RepoDecision::Pending { .. }),
            "got {decision:?}"
        );
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
