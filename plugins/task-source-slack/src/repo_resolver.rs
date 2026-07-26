//! Repository resolution (issue #106): decide *inside the plugin* which local
//! repository a mention concerns, in three stages —
//!
//! 1. `[[channel_groups]]` prefix rules (first match in declaration order)
//!    narrow the candidates; a single survivor resolves immediately.
//! 2. The plugin's own LLM classifier ([`crate::llm`]) picks among several
//!    candidates; a verdict at or above the confidence threshold resolves.
//! 3. Otherwise the operator picks via an in-thread ephemeral (handled by
//!    the pipeline; this module only reports that a selection is needed).
//!
//! The submitted task therefore always carries a final `repo_hint`, which the
//! orchestrator's F-10 rule resolves instantly — core LLM repo selection is
//! never involved.

use crate::config::{RepoInfo, SlackConfig};
use crate::llm::{ChatTransport, classify};

/// The outcome of stages ① + ②.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A single repository was determined; submit the task with this hint.
    Resolved(String),
    /// Stages ① and ② could not decide — ask the operator via an ephemeral
    /// offering these candidate names.
    NeedsSelection(Vec<String>),
}

/// Stage ①: the candidates after applying the channel-prefix rules. The
/// first `[[channel_groups]]` whose `prefix` matches `channel_name` wins;
/// with no match every configured repository is a candidate. A matching
/// group that narrows to *nothing* (empty `repos`, or names that don't
/// exist — `config/validate` flags both, but `initialize` does not re-run
/// it) falls back to every repository rather than stranding the mention
/// behind a picker with no buttons.
pub fn prefix_candidates(config: &SlackConfig, channel_name: &str) -> Vec<RepoInfo> {
    for group in &config.channel_groups {
        if channel_name.starts_with(&group.prefix) {
            let narrowed: Vec<RepoInfo> = config
                .repos
                .iter()
                .filter(|r| group.repos.contains(&r.name))
                .cloned()
                .collect();
            if narrowed.is_empty() {
                tracing::warn!(
                    prefix = group.prefix,
                    channel_name,
                    "matching [[channel_groups]] entry narrows to no repository \
                     (fix its `repos` list); using every [[repos]] candidate"
                );
                break;
            }
            return narrowed;
        }
    }
    config.repos.clone()
}

/// Run stages ① and ② for one mention. Never errors: every failure mode of
/// stage ② (low confidence, malformed verdict after retry, API failure,
/// missing `[llm]` table) degrades to [`Resolution::NeedsSelection`].
pub async fn resolve<C: ChatTransport>(
    chat: &C,
    config: &SlackConfig,
    channel_name: &str,
    mention_text: &str,
    thread_context: &str,
) -> Resolution {
    let candidates = prefix_candidates(config, channel_name);
    if let [only] = candidates.as_slice() {
        return Resolution::Resolved(only.name.clone());
    }
    let names: Vec<String> = candidates.iter().map(|r| r.name.clone()).collect();

    let Some(llm) = &config.llm else {
        // Multiple candidates but no classifier (config validation warns
        // about this shape) — fall through to the operator.
        return Resolution::NeedsSelection(names);
    };
    match classify(chat, llm, mention_text, thread_context, &candidates).await {
        Ok(verdict) => {
            tracing::info!(
                repo = verdict.repo,
                confidence = verdict.confidence,
                reason = verdict.reason,
                "repository resolved by the LLM classifier"
            );
            Resolution::Resolved(verdict.repo)
        }
        // A rejected API key is not an inconclusive answer: it is a broken
        // configuration that degrades *every* new conversation to the picker
        // and spends a doomed round-trip each time. Left at `info!` it reads
        // as mildly inconvenient normal operation — which is exactly how a
        // dead OpenRouter key went unnoticed until someone happened to read
        // the log (#267). `totsuka doctor --online` catches it up front.
        Err(e) if e.is_auth_failure() => {
            tracing::warn!(
                error = %e,
                "the LLM provider rejected the API key; repository selection \
                 falls back to the operator picker for every new conversation \
                 until it is fixed — check [llm].api_key_ref and run \
                 `totsuka doctor --online`"
            );
            Resolution::NeedsSelection(names)
        }
        Err(e) => {
            tracing::info!(error = %e, "LLM classification inconclusive; asking the operator");
            Resolution::NeedsSelection(names)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(groups: serde_json::Value) -> SlackConfig {
        serde_json::from_value(json!({
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME",
            "channel_groups": groups,
            "repos": [
                { "name": "web-app" },
                { "name": "design-system" },
                { "name": "backend-api" },
            ]
        }))
        .unwrap()
    }

    fn names(candidates: Vec<RepoInfo>) -> Vec<String> {
        candidates.into_iter().map(|r| r.name).collect()
    }

    /// A [`ChatTransport`] that always fails the same way.
    struct FailingChat(crate::llm::ChatError);

    impl ChatTransport for FailingChat {
        async fn complete(
            &self,
            _config: &crate::config::LlmConfig,
            _body: serde_json::Value,
        ) -> Result<serde_json::Value, crate::llm::ChatError> {
            Err(self.0.clone())
        }
    }

    /// The same config plus an `[llm]` table, so stage ② actually runs.
    fn config_with_llm() -> SlackConfig {
        let mut config = config(json!([]));
        config.llm = Some(crate::config::LlmConfig {
            base_url: "https://llm.test/v1".into(),
            model: "test-model".into(),
            api_key: "sk-dead".into(),
            confidence_threshold: 0.6,
        });
        config
    }

    #[tokio::test]
    async fn a_rejected_key_still_degrades_to_the_picker() {
        // The louder log (#267) must not change the behaviour: an unusable
        // classifier always falls through to the operator, never strands or
        // guesses at the mention.
        let config = config_with_llm();
        for error in [
            crate::llm::ChatError::http(401, r#"{"error":{"message":"User not found."}}"#),
            crate::llm::ChatError::transport("connection refused"),
        ] {
            let chat = FailingChat(error);
            assert_eq!(
                resolve(&chat, &config, "random-talk", "fix the button", "").await,
                Resolution::NeedsSelection(vec![
                    "web-app".to_string(),
                    "design-system".to_string(),
                    "backend-api".to_string(),
                ])
            );
        }
    }

    #[test]
    fn first_matching_group_wins_in_declaration_order() {
        let config = config(json!([
            { "prefix": "dev-frontend-", "repos": ["web-app", "design-system"] },
            { "prefix": "dev-", "repos": ["backend-api"] },
        ]));

        // Both prefixes match; the earlier declaration decides.
        assert_eq!(
            names(prefix_candidates(&config, "dev-frontend-general")),
            vec!["web-app", "design-system"]
        );
        // Only the broader one matches.
        assert_eq!(
            names(prefix_candidates(&config, "dev-infra")),
            vec!["backend-api"]
        );
    }

    #[test]
    fn unmatched_channel_yields_all_repos() {
        let config = config(json!([
            { "prefix": "dev-frontend-", "repos": ["web-app"] },
        ]));
        assert_eq!(
            names(prefix_candidates(&config, "random-talk")),
            vec!["web-app", "design-system", "backend-api"]
        );
    }

    #[test]
    fn multiple_groups_partition_channels() {
        let config = config(json!([
            { "prefix": "team-a-", "repos": ["web-app"] },
            { "prefix": "team-b-", "repos": ["design-system", "backend-api"] },
        ]));
        assert_eq!(
            names(prefix_candidates(&config, "team-a-dev")),
            vec!["web-app"]
        );
        assert_eq!(
            names(prefix_candidates(&config, "team-b-dev")),
            vec!["design-system", "backend-api"]
        );
    }

    #[test]
    fn group_narrowing_to_nothing_falls_back_to_all_repos() {
        // An empty `repos` list and one referencing only unknown names both
        // slip past initialize (which does not re-run config/validate); the
        // mention must still get a full picker, not a skip-only one.
        for groups in [
            json!([{ "prefix": "ops-", "repos": [] }]),
            json!([{ "prefix": "ops-", "repos": ["ghost"] }]),
        ] {
            let config = config(groups);
            assert_eq!(
                names(prefix_candidates(&config, "ops-alerts")),
                vec!["web-app", "design-system", "backend-api"]
            );
        }
    }
}
