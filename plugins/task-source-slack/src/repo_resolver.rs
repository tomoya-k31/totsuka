//! Repository resolution (issue #106): decide *inside the plugin* which local
//! repository a mention concerns, in three stages —
//!
//! 1. `[[channel_groups]]` prefix rules (first match in declaration order)
//!    narrow the candidates; a single survivor resolves immediately. With no
//!    rule matching, `fallback_repo` — when set — *is* that single survivor,
//!    so a channel no rule covers has one deliberate destination instead of
//!    the whole catalogue.
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
/// with no match `unmatched_candidates` below decides.
///
/// A matching group that narrows to *nothing* (empty `repos`, or names that
/// don't exist) is treated as no match at all, rather than stranding the
/// mention behind a picker with no buttons. **A running plugin cannot be in
/// that state**: `static_config_errors` rejects both shapes and `initialize`
/// runs it against the merged candidate list, so an operator sees
/// `CONFIG_INVALID` at startup instead. The branch is kept as a safety net
/// for a caller that has not passed that gate — it is not a supported mode,
/// and nothing here should be read as "a bad `repos` list degrades
/// gracefully at runtime".
pub fn prefix_candidates(config: &SlackConfig, channel_name: &str) -> Vec<RepoInfo> {
    for group in &config.channel_groups {
        // A group may declare several prefixes sharing one `repos` list; any
        // one of them matching is a hit. The one that did is logged, since
        // "which rule caught this channel" is the question a surprising
        // routing raises.
        if let Some(prefix) = group.prefix.matched(channel_name) {
            let narrowed: Vec<RepoInfo> = config
                .repos
                .iter()
                .filter(|r| group.repos.contains(&r.name))
                .cloned()
                .collect();
            if narrowed.is_empty() {
                tracing::warn!(
                    prefix,
                    channel_name,
                    "matching [[channel_groups]] entry narrows to no repository \
                     (fix its `repos` list); continuing as if no rule matched"
                );
                break;
            }
            tracing::debug!(
                prefix,
                channel_name,
                candidates = narrowed.len(),
                "[[channel_groups]] prefix matched"
            );
            return narrowed;
        }
    }
    unmatched_candidates(config, channel_name)
}

/// The candidates for a channel no `[[channel_groups]]` rule covers.
///
/// `fallback_repo` names one of them and short-circuits the classifier
/// (`resolve` resolves a lone candidate outright).
///
/// A name matching no candidate falls through to the full catalogue, on the
/// same safety-net terms as `prefix_candidates` above: `initialize` has
/// already refused to start on that config (`CONFIG_INVALID`), so the branch
/// is unreachable in a running plugin. `config/validate` alone cannot catch
/// it — with `[[repos]]` omitted the candidates are unknown until the
/// Orchestrator supplies them — which is why the check is written to run in
/// both places rather than offline only.
fn unmatched_candidates(config: &SlackConfig, channel_name: &str) -> Vec<RepoInfo> {
    let Some(name) = &config.fallback_repo else {
        return config.repos.clone();
    };
    match config.repos.iter().find(|r| &r.name == name) {
        Some(repo) => {
            tracing::debug!(
                channel_name,
                fallback_repo = name,
                "no [[channel_groups]] match; using `fallback_repo`"
            );
            vec![repo.clone()]
        }
        None => {
            tracing::warn!(
                channel_name,
                fallback_repo = name,
                "`fallback_repo` names no declared repository (fix it); \
                 using every [[repos]] candidate"
            );
            config.repos.clone()
        }
    }
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
    match classify(
        chat,
        llm,
        &config.prompts,
        mention_text,
        thread_context,
        &candidates,
    )
    .await
    {
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

    /// `config` plus a `fallback_repo`, which only applies when no group
    /// matches.
    fn config_with_fallback(groups: serde_json::Value, fallback: &str) -> SlackConfig {
        let mut config = config(groups);
        config.fallback_repo = Some(fallback.to_string());
        config
    }

    #[test]
    fn fallback_repo_is_the_sole_candidate_for_an_unmatched_channel() {
        let config = config_with_fallback(
            json!([{ "prefix": "dev-frontend-", "repos": ["web-app"] }]),
            "backend-api",
        );
        // The whole point: not the three-repo catalogue that the same
        // channel would have produced without the key.
        assert_eq!(
            names(prefix_candidates(&config, "random-talk")),
            vec!["backend-api"]
        );
    }

    #[test]
    fn fallback_repo_never_overrides_a_matching_group() {
        let config = config_with_fallback(
            json!([{ "prefix": "dev-", "repos": ["web-app", "design-system"] }]),
            "backend-api",
        );
        assert_eq!(
            names(prefix_candidates(&config, "dev-infra")),
            vec!["web-app", "design-system"]
        );
    }

    #[test]
    fn a_group_narrowing_to_nothing_lands_on_the_fallback_repo() {
        // Also the safety net (`initialize` rejects both shapes): pinned so
        // that if it is ever reached, the unusable rule reads as "no rule
        // matched" and the fallback decides, instead of the full picker it
        // produced before.
        for groups in [
            json!([{ "prefix": "ops-", "repos": [] }]),
            json!([{ "prefix": "ops-", "repos": ["ghost"] }]),
        ] {
            let config = config_with_fallback(groups, "backend-api");
            assert_eq!(
                names(prefix_candidates(&config, "ops-alerts")),
                vec!["backend-api"]
            );
        }
    }

    #[test]
    fn an_unknown_fallback_repo_degrades_to_every_candidate() {
        // The safety net, not a supported mode: `initialize` rejects this
        // config outright, so a running plugin never reaches here. Pinned so
        // that a caller bypassing that gate still gets a working picker
        // rather than none.
        let config = config_with_fallback(json!([]), "ghost");
        assert_eq!(
            names(prefix_candidates(&config, "random-talk")),
            vec!["web-app", "design-system", "backend-api"]
        );
    }

    #[test]
    fn a_prefix_array_matches_on_any_of_its_entries() {
        // The reason the array form exists: one `repos` list, several
        // prefixes, no copy to keep in step.
        let config = config(json!([
            { "prefix": ["mikasa", "qc-mikasa", "ms-"], "repos": ["web-app", "design-system"] },
        ]));
        for channel in ["mikasa", "mikasa-dev", "qc-mikasa", "ms-kikaku"] {
            assert_eq!(
                names(prefix_candidates(&config, channel)),
                vec!["web-app", "design-system"],
                "{channel}"
            );
        }
        // Nothing in the array matches → the unmatched path, as before.
        assert_eq!(
            names(prefix_candidates(&config, "random-talk")),
            vec!["web-app", "design-system", "backend-api"]
        );
    }

    #[test]
    fn the_single_and_array_forms_are_interchangeable() {
        // Back-compat pinned: an existing `prefix = "…"` config keeps its
        // exact behaviour, and spelling it as a one-element array is the
        // same rule.
        let single = config(json!([{ "prefix": "dev-", "repos": ["web-app"] }]));
        let array = config(json!([{ "prefix": ["dev-"], "repos": ["web-app"] }]));
        for channel in ["dev-infra", "release-notes"] {
            assert_eq!(
                names(prefix_candidates(&single, channel)),
                names(prefix_candidates(&array, channel)),
                "{channel}"
            );
        }
    }

    #[test]
    fn declaration_order_still_decides_across_groups_with_arrays() {
        // First matching *group* wins, whichever of its prefixes hit — the
        // array must not reorder the outer first-match rule.
        let config = config(json!([
            { "prefix": ["dev-frontend-", "ui-"], "repos": ["web-app"] },
            { "prefix": ["dev-", "backend-"], "repos": ["backend-api"] },
        ]));
        assert_eq!(
            names(prefix_candidates(&config, "dev-frontend-general")),
            vec!["web-app"]
        );
        assert_eq!(
            names(prefix_candidates(&config, "dev-infra")),
            vec!["backend-api"]
        );
        assert_eq!(names(prefix_candidates(&config, "ui-kit")), vec!["web-app"]);
    }

    #[tokio::test]
    async fn a_fallback_repo_resolves_without_consulting_the_llm() {
        // `FailingChat` errors on every call, so reaching the classifier
        // would surface as `NeedsSelection`. Resolving proves stage ② was
        // skipped — which is what makes the fallback cost nothing.
        let mut config = config_with_llm();
        config.fallback_repo = Some("backend-api".into());
        let chat = FailingChat(crate::llm::ChatError::transport("must not be called"));
        assert_eq!(
            resolve(&chat, &config, "random-talk", "who owns onboarding?", "").await,
            Resolution::Resolved("backend-api".to_string())
        );
    }
}
