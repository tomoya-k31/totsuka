//! Prompt template shared by every Classifier impl. Output schema documented
//! in the system message so off-the-rails responses are still parseable.

use super::schema::ClassifyRequest;

pub fn build_prompt(req: &ClassifyRequest, top_n: u32) -> (String, String) {
    let system = format!(
        "You classify a user question to one of the candidate repositories. \
         Return the top {top_n} most-likely repos as JSON: \
         {{\"top_candidates\": [{{\"repo\": \"owner/name\", \"confidence\": 0.0..1.0, \"rationale\": \"...\"}}]}}. \
         Sort by confidence descending. Only choose repos from the candidate list.");

    let mut user = String::new();
    if let Some(ctx) = &req.thread_context {
        user.push_str("Thread context:\n");
        user.push_str(ctx);
        user.push_str("\n\n");
    }
    user.push_str("Question:\n");
    user.push_str(&req.question);
    user.push_str("\n\nCandidate repositories:\n");
    for c in &req.candidates {
        user.push_str(&format!("- {}: {}\n", c.repo, c.description));
    }
    (system, user)
}

#[cfg(test)]
mod tests {
    use super::super::schema::RepoCandidate;
    use super::*;

    #[test]
    fn prompt_contains_top_n_and_all_repos() {
        let req = ClassifyRequest {
            question: "Where is the auth flow?".into(),
            thread_context: Some("Earlier: tried to log in".into()),
            candidates: vec![
                RepoCandidate {
                    repo: "acme/web".into(),
                    description: "frontend".into(),
                },
                RepoCandidate {
                    repo: "acme/api".into(),
                    description: "auth backend".into(),
                },
            ],
        };
        let (sys, user) = build_prompt(&req, 3);
        assert!(sys.contains("top 3"));
        assert!(user.contains("Earlier: tried to log in"));
        assert!(user.contains("Where is the auth flow?"));
        assert!(user.contains("acme/web"));
        assert!(user.contains("acme/api"));
        assert!(user.contains("auth backend"));
    }

    #[test]
    fn prompt_omits_thread_context_block_when_none() {
        let req = ClassifyRequest {
            question: "q".into(),
            thread_context: None,
            candidates: vec![RepoCandidate {
                repo: "a/b".into(),
                description: "x".into(),
            }],
        };
        let (_sys, user) = build_prompt(&req, 1);
        assert!(!user.contains("Thread context:"));
    }
}
