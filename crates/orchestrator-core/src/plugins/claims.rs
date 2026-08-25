//! Repository → tracker routing, assembled from what the task sources
//! claimed at `initialize` (#542, #554).
//!
//! The Orchestrator decides **which** plugin is a repository's tracker — that
//! is `[[repositories]].project` naming a `[[projects]]` entry, and the entry
//! naming its `source` (#554). What it cannot produce is the *destination
//! prose*: "file the issue, then `gh project item-add 7 …`" is the tracker's
//! own vocabulary, addressed to an agent, and rendering it would require the
//! Orchestrator to know each tracker's shape. So the plugin answers
//! `initialize` with one line per repository it was given, and this module is
//! the union of those answers.
//!
//! ## Two claims for one repository are no longer possible
//!
//! Until #554 the mapping lived the other way round — a `repos = [...]` list
//! inside each source's own config ([ADR-0056]) — where two plugins could name
//! the same repository, so this module detected that and reported it. A
//! repository now names **one** project and a project names **one** source, so
//! the state is unrepresentable and the detection is gone.
//!
//! ## What an absent claim means
//!
//! Nothing claiming a repository means **no configured source is its tracker**,
//! which is the normal state for anyone who has not set one up. Callers must
//! treat it as "say nothing extra", never as an error.
//!
//! [ADR-0056]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0056-multi-tracker-routing.md

use std::collections::HashMap;

use plugin_protocol::methods::ClaimedRepo;

/// Where a new item for each repository goes, keyed by repository name.
#[derive(Debug, Default, Clone)]
pub struct ClaimRegistry {
    /// repository → (claiming plugin, destination prose).
    by_repo: HashMap<String, (String, String)>,
}

impl ClaimRegistry {
    /// Build the registry from each source's `initialize` answer.
    ///
    /// `sources` is `(plugin name, its claims)`. Two sources cannot claim one
    /// repository any more (see the module docs), so the iteration order no
    /// longer decides anything; a later claim for a repository already present
    /// would mean a plugin claimed one it was not given, and taking the first
    /// keeps that from changing where tasks are routed.
    pub fn from_sources<'a, I>(sources: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a [ClaimedRepo])>,
    {
        let mut by_repo: HashMap<String, (String, String)> = HashMap::new();
        for (name, claims) in sources {
            for claim in claims {
                by_repo
                    .entry(claim.repo.clone())
                    .or_insert_with(|| (name.to_string(), claim.destination.clone()));
            }
        }
        Self { by_repo }
    }

    /// Where an item for `repo` goes, or `None` when no source claims it.
    pub fn destination(&self, repo: &str) -> Option<&str> {
        self.by_repo.get(repo).map(|(_, dest)| dest.as_str())
    }

    /// The plugin that is `repo`'s tracker, or `None`.
    pub fn source_for(&self, repo: &str) -> Option<&str> {
        self.by_repo.get(repo).map(|(name, _)| name.as_str())
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
    }

    /// A repository can only be claimed once now (#554): it names one
    /// `[[projects]]` entry and the entry names one source, so a second claim
    /// means a plugin claimed a repository it was never given. Taking the
    /// first keeps a misbehaving plugin from moving where tasks are routed.
    #[test]
    fn a_second_claim_for_one_repository_does_not_take_over() {
        let github = [claim("shared", "Project #7")];
        let rogue = [claim("shared", "Somewhere else")];
        let registry =
            ClaimRegistry::from_sources([("github", &github[..]), ("rogue", &rogue[..])]);
        assert_eq!(registry.destination("shared"), Some("Project #7"));
        assert_eq!(registry.source_for("shared"), Some("github"));
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
    }
}
