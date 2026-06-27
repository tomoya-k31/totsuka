use crate::schema::Config;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ValidationError {
    #[error("repo {repo}: worktree_subdir and worktree_path are mutually exclusive (both set)")]
    WorktreeBothSet { repo: String },
    #[error("repo {repo}: must set exactly one of worktree_subdir or worktree_path (none set)")]
    WorktreeNeitherSet { repo: String },
    #[error("repo {repo}: description is required (empty)")]
    RepoDescriptionEmpty { repo: String },
    #[error("github.columns must cover all 8 ColumnId values (have {0}, need 8)")]
    ColumnsCoverage(usize),
    #[error("default_mode must be 'auto' or 'delegated' (got {0})")]
    InvalidQaMode(String),
    #[error("classifier.provider must be anthropic|openai|openrouter|litellm|openai_compatible (got {0})")]
    InvalidProvider(String),
    #[error("classifier.api_base required for provider {0}")]
    ApiBaseRequired(String),
    #[error("classifier.confidence_threshold must be in [0.0, 1.0] (got {0})")]
    InvalidThreshold(f64),
    #[error("supervisor.heartbeat.restart_policy must be one of on-dead-only|on-unhealthy|never (got {0})")]
    InvalidRestartPolicy(String),
    #[error("agent_adapter.uds_path and orchestrator.uds_path must differ")]
    UdsCollision,
}

impl Config {
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errs = Vec::new();

        // worktree 排他 + description 必須
        for (repo, r) in &self.agent_adapter.repos {
            if r.description.is_empty() {
                errs.push(ValidationError::RepoDescriptionEmpty { repo: repo.clone() });
            }
            match (&r.worktree_subdir, &r.worktree_path) {
                (Some(_), Some(_)) => {
                    errs.push(ValidationError::WorktreeBothSet { repo: repo.clone() })
                }
                (None, None) => {
                    errs.push(ValidationError::WorktreeNeitherSet { repo: repo.clone() })
                }
                _ => {}
            }
        }

        // ColumnId 8 値カバレッジ
        if self.github.columns.len() != 8 {
            errs.push(ValidationError::ColumnsCoverage(self.github.columns.len()));
        }

        // qa default_mode
        if !matches!(self.qa_service.default_mode.as_str(), "auto" | "delegated") {
            errs.push(ValidationError::InvalidQaMode(
                self.qa_service.default_mode.clone(),
            ));
        }

        // provider
        let p = &self.qa_service.classifier.provider;
        if !matches!(
            p.as_str(),
            "anthropic" | "openai" | "openrouter" | "litellm" | "openai_compatible"
        ) {
            errs.push(ValidationError::InvalidProvider(p.clone()));
        }
        if matches!(p.as_str(), "litellm" | "openai_compatible")
            && self.qa_service.classifier.api_base.is_empty()
        {
            errs.push(ValidationError::ApiBaseRequired(p.clone()));
        }

        // threshold range
        let th = self.qa_service.classifier.confidence_threshold;
        if !(0.0..=1.0).contains(&th) {
            errs.push(ValidationError::InvalidThreshold(th));
        }

        // restart_policy
        let rp = &self.supervisor.heartbeat.restart_policy;
        if !matches!(rp.as_str(), "on-dead-only" | "on-unhealthy" | "never") {
            errs.push(ValidationError::InvalidRestartPolicy(rp.clone()));
        }

        // UDS 衝突
        if self.agent_adapter.uds_path == self.orchestrator.uds_path {
            errs.push(ValidationError::UdsCollision);
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Config, RepoSection};

    fn baseline() -> Config {
        let toml = include_str!("test_min.toml");
        Config::from_toml_str(toml).unwrap()
    }

    #[test]
    fn baseline_validates() {
        baseline().validate().expect("baseline should validate");
    }

    #[test]
    fn worktree_both_set_errors() {
        let mut c = baseline();
        c.agent_adapter.repos.insert(
            "org/r".into(),
            RepoSection {
                description: "x".into(),
                repo_path: None,
                worktree_subdir: Some(".w".into()),
                worktree_path: Some("/tmp".into()),
                default_branch: None,
            },
        );
        let e = c.validate().unwrap_err();
        assert!(e.contains(&ValidationError::WorktreeBothSet {
            repo: "org/r".into()
        }));
    }

    #[test]
    fn worktree_neither_errors() {
        let mut c = baseline();
        c.agent_adapter.repos.insert(
            "org/r".into(),
            RepoSection {
                description: "x".into(),
                repo_path: None,
                worktree_subdir: None,
                worktree_path: None,
                default_branch: None,
            },
        );
        assert!(c
            .validate()
            .unwrap_err()
            .contains(&ValidationError::WorktreeNeitherSet {
                repo: "org/r".into()
            }));
    }

    #[test]
    fn empty_description_errors() {
        let mut c = baseline();
        c.agent_adapter.repos.insert(
            "org/r".into(),
            RepoSection {
                description: "".into(),
                repo_path: None,
                worktree_subdir: Some(".w".into()),
                worktree_path: None,
                default_branch: None,
            },
        );
        assert!(c
            .validate()
            .unwrap_err()
            .iter()
            .any(|e| matches!(e, ValidationError::RepoDescriptionEmpty { .. })));
    }

    #[test]
    fn invalid_provider_errors() {
        let mut c = baseline();
        c.qa_service.classifier.provider = "bogus".into();
        assert!(c
            .validate()
            .unwrap_err()
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidProvider(_))));
    }

    #[test]
    fn litellm_requires_api_base() {
        let mut c = baseline();
        c.qa_service.classifier.provider = "litellm".into();
        c.qa_service.classifier.api_base = "".into();
        assert!(c
            .validate()
            .unwrap_err()
            .iter()
            .any(|e| matches!(e, ValidationError::ApiBaseRequired(_))));
    }

    #[test]
    fn threshold_range_errors() {
        let mut c = baseline();
        c.qa_service.classifier.confidence_threshold = 1.5;
        assert!(c
            .validate()
            .unwrap_err()
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidThreshold(_))));
    }

    #[test]
    fn restart_policy_validates() {
        let mut c = baseline();
        c.supervisor.heartbeat.restart_policy = "wrong".into();
        assert!(c
            .validate()
            .unwrap_err()
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidRestartPolicy(_))));
    }

    #[test]
    fn uds_collision_errors() {
        let mut c = baseline();
        c.agent_adapter.uds_path = "/same".into();
        c.orchestrator.uds_path = "/same".into();
        assert!(c
            .validate()
            .unwrap_err()
            .contains(&ValidationError::UdsCollision));
    }
}
