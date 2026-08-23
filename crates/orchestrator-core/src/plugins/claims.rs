//! Repository → tracker routing, assembled from what the task sources claimed
//! at `initialize` (#542, [ADR-0056]).
//!
//! [ADR-0056]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0056-multi-tracker-routing.md
//!
//! The Orchestrator holds **no** repository → tracker configuration of its own.
//! Each task_source knows which repositories it is the tracker for and answers
//! `initialize` with a `claimed_repos` list; this module is only the union of
//! those answers, plus the one question no single plugin can answer — whether
//! two plugins claimed the same repository.
//!
//! ## What an absent claim means
//!
//! Nothing claiming a repository means **no configured source is its tracker**,
//! which is the normal state for anyone who has not set one up, and also what a
//! plugin predating protocol 0.5.1 looks like. Callers must therefore treat it
//! as "say nothing extra", never as an error.

use std::collections::HashMap;

use plugin_protocol::methods::ClaimedRepo;

/// Two sources claiming the same repository (#542).
///
/// Reported rather than resolved. Picking a winner would route half the
/// operator's tasks somewhere they did not intend and say nothing about it;
/// with the conflict named, the fix is one line of config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimConflict {
    /// The contested repository (`[[repositories]].name`).
    pub repo: String,
    /// The plugin names that claimed it, in the order they were seen.
    pub sources: Vec<String>,
}

impl std::fmt::Display for ClaimConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "repository `{}` is claimed as a tracker target by {} → \
             a repository may have exactly one tracker, so remove it from all but one of them",
            self.repo,
            self.sources
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(" and "),
        )
    }
}

/// Where a new item for each repository goes, keyed by repository name.
#[derive(Debug, Default, Clone)]
pub struct ClaimRegistry {
    /// repository → (claiming plugin, destination prose).
    ///
    /// On a conflict the **first** claim wins this map, and the conflict is
    /// reported separately. Dropping both would turn a config mistake into
    /// silently unrouted tasks — strictly worse than routing to one of the two
    /// places the operator actually configured, while the warning says which.
    by_repo: HashMap<String, (String, String)>,
    conflicts: Vec<ClaimConflict>,
}

impl ClaimRegistry {
    /// Build the registry from each source's `initialize` answer.
    ///
    /// `sources` is `(plugin name, its claims)`. **Iteration order decides
    /// which claim wins a conflict**, so the caller must pass a deterministic
    /// order — the engine sorts by plugin name, because the map it reads from
    /// is a `HashMap` and an arbitrary order would route a conflicted
    /// repository to a different tracker between runs of the same config.
    pub fn from_sources<'a, I>(sources: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a [ClaimedRepo])>,
    {
        let mut by_repo: HashMap<String, (String, String)> = HashMap::new();
        let mut contested: Vec<(String, Vec<String>)> = Vec::new();
        for (name, claims) in sources {
            for claim in claims {
                match by_repo.get(&claim.repo) {
                    Some((first, _)) => {
                        let first = first.clone();
                        match contested.iter_mut().find(|(repo, _)| repo == &claim.repo) {
                            // A third source claiming the same repository joins
                            // the existing report rather than opening a second
                            // one about the same repository.
                            Some((_, sources)) => sources.push(name.to_string()),
                            None => {
                                contested.push((claim.repo.clone(), vec![first, name.to_string()]));
                            }
                        }
                    }
                    None => {
                        by_repo.insert(
                            claim.repo.clone(),
                            (name.to_string(), claim.destination.clone()),
                        );
                    }
                }
            }
        }
        // Sorted so the operator sees the same list in the same order every
        // run; `HashMap` order is not stable and this text is read by humans.
        contested.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            by_repo,
            conflicts: contested
                .into_iter()
                .map(|(repo, sources)| ClaimConflict { repo, sources })
                .collect(),
        }
    }

    /// Where an item for `repo` goes, or `None` when no source claims it.
    pub fn destination(&self, repo: &str) -> Option<&str> {
        self.by_repo.get(repo).map(|(_, dest)| dest.as_str())
    }

    /// The plugin that is `repo`'s tracker, or `None`.
    pub fn source_for(&self, repo: &str) -> Option<&str> {
        self.by_repo.get(repo).map(|(name, _)| name.as_str())
    }

    /// Repositories claimed by more than one source, in repository order.
    pub fn conflicts(&self) -> &[ClaimConflict] {
        &self.conflicts
    }

    /// How many repositories have a tracker.
    pub fn len(&self) -> usize {
        self.by_repo.len()
    }

    /// Whether no source claimed anything.
    pub fn is_empty(&self) -> bool {
        self.by_repo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(repo: &str, destination: &str) -> ClaimedRepo {
        ClaimedRepo {
            repo: repo.into(),
            destination: destination.into(),
        }
    }

    #[test]
    fn a_repository_resolves_to_the_source_that_claimed_it() {
        let github = [
            claim("totsuka", "Project #7"),
            claim("dotfiles", "Project #7"),
        ];
        let notion = [claim("web-app", "Database DB2")];
        let registry =
            ClaimRegistry::from_sources([("github", &github[..]), ("notion", &notion[..])]);

        assert_eq!(registry.destination("totsuka"), Some("Project #7"));
        assert_eq!(registry.source_for("totsuka"), Some("github"));
        assert_eq!(registry.destination("web-app"), Some("Database DB2"));
        assert_eq!(registry.source_for("web-app"), Some("notion"));
        assert_eq!(registry.len(), 3);
        assert!(registry.conflicts().is_empty());
    }

    #[test]
    fn an_unclaimed_repository_is_absent_not_an_error() {
        let github = [claim("totsuka", "Project #7")];
        let registry = ClaimRegistry::from_sources([("github", &github[..])]);
        // The normal state for anyone who has not configured a tracker, and
        // also what a plugin predating protocol 0.5.1 produces.
        assert_eq!(registry.destination("unconfigured"), None);
    }

    #[test]
    fn no_sources_at_all_is_an_empty_registry() {
        let registry = ClaimRegistry::from_sources(std::iter::empty());
        assert!(registry.is_empty());
        assert!(registry.conflicts().is_empty());
    }

    /// The cross-source case no single plugin can see: each one's own
    /// `config/validate` is happy, and only the union is wrong.
    #[test]
    fn two_sources_claiming_one_repository_is_reported() {
        let github = [claim("shared", "Project #7")];
        let notion = [claim("shared", "Database DB1")];
        let registry =
            ClaimRegistry::from_sources([("github", &github[..]), ("notion", &notion[..])]);

        assert_eq!(registry.conflicts().len(), 1);
        assert_eq!(registry.conflicts()[0].repo, "shared");
        assert_eq!(registry.conflicts()[0].sources, ["github", "notion"]);
        // The first claim still routes: unrouted tasks would be worse than
        // tasks routed to one of the two configured places, and the conflict
        // is reported alongside.
        assert_eq!(registry.destination("shared"), Some("Project #7"));

        let message = registry.conflicts()[0].to_string();
        assert!(message.contains("shared"), "{message}");
        assert!(
            message.contains("`github`") && message.contains("`notion`"),
            "{message}"
        );
    }

    #[test]
    fn a_third_claimant_joins_the_same_report() {
        let a = [claim("shared", "A")];
        let b = [claim("shared", "B")];
        let c = [claim("shared", "C")];
        let registry = ClaimRegistry::from_sources([("a", &a[..]), ("b", &b[..]), ("c", &c[..])]);
        // One report about one repository, not two.
        assert_eq!(registry.conflicts().len(), 1);
        assert_eq!(registry.conflicts()[0].sources, ["a", "b", "c"]);
    }

    #[test]
    fn conflicts_are_ordered_by_repository_name() {
        let a = [claim("zebra", "A"), claim("alpha", "A")];
        let b = [claim("zebra", "B"), claim("alpha", "B")];
        let registry = ClaimRegistry::from_sources([("a", &a[..]), ("b", &b[..])]);
        let repos: Vec<&str> = registry
            .conflicts()
            .iter()
            .map(|c| c.repo.as_str())
            .collect();
        // Stable across runs: `HashMap` iteration is not, and this text is
        // read by a human comparing one run's output with another's.
        assert_eq!(repos, ["alpha", "zebra"]);
    }
}
