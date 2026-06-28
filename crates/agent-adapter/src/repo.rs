//! Resolved per-repo configuration cache. Atomic swap on reload (spec §11
//! hot-reload requirement); lookups never block.

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use totsuka_config::schema::{AgentAdapterSection, RepoSection};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoKey(String);

impl RepoKey {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct RepoEntry {
    pub description: String,
    pub repo_path: PathBuf,
    pub worktree_root: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReloadReport {
    pub added: Vec<RepoKey>,
    pub removed: Vec<RepoKey>,
}

pub struct RepoRegistry {
    map: ArcSwap<HashMap<RepoKey, RepoEntry>>,
}

impl RepoRegistry {
    pub fn new() -> Self {
        Self {
            map: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    pub fn resolve(&self, key: &RepoKey) -> Option<RepoEntry> {
        self.map.load().get(key).cloned()
    }

    pub fn reload(&self, cfg: &AgentAdapterSection) -> ReloadReport {
        let next: HashMap<RepoKey, RepoEntry> = cfg
            .repos
            .iter()
            .map(|(k, v)| {
                (
                    RepoKey::new(k.clone()),
                    resolve_entry(k, v, &cfg.repos_root),
                )
            })
            .collect();
        let prev_keys: Vec<RepoKey> = self.map.load().keys().cloned().collect();
        let next_keys: Vec<RepoKey> = next.keys().cloned().collect();
        let added: Vec<RepoKey> = next_keys
            .iter()
            .filter(|k| !prev_keys.contains(k))
            .cloned()
            .collect();
        let removed: Vec<RepoKey> = prev_keys
            .iter()
            .filter(|k| !next_keys.contains(k))
            .cloned()
            .collect();
        self.map.store(Arc::new(next));
        // Stable order for deterministic tests / logs.
        let mut report = ReloadReport { added, removed };
        report.added.sort_by(|a, b| a.0.cmp(&b.0));
        report.removed.sort_by(|a, b| a.0.cmp(&b.0));
        report
    }
}

impl Default for RepoRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_entry(key: &str, r: &RepoSection, repos_root: &str) -> RepoEntry {
    let repo_path = r
        .repo_path
        .clone()
        .unwrap_or_else(|| format!("{}/{}", repos_root.trim_end_matches('/'), key))
        .into();
    let worktree_root = if let Some(abs) = &r.worktree_path {
        PathBuf::from(abs)
    } else {
        let sub = r.worktree_subdir.as_deref().unwrap_or(".worktree");
        let mut p: PathBuf = (&repo_path as &PathBuf).clone();
        p.push(sub);
        p
    };
    RepoEntry {
        description: r.description.clone(),
        repo_path,
        worktree_root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use totsuka_config::schema::{AgentAdapterSection, RepoSection};

    fn cfg(repos_root: &str, repos: &[(&str, RepoSection)]) -> AgentAdapterSection {
        AgentAdapterSection {
            uds_path: "/tmp/u.sock".into(),
            tcp_bind: String::new(),
            herdr_socket: "/tmp/h.sock".into(),
            node_capacity: 8,
            repos_root: repos_root.into(),
            auto_clone: false,
            worktree_failed_ttl_hours: 72,
            worktree_orphan_scan_interval_secs: 3600,
            vars: HashMap::new(),
            repos: repos
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    fn repo(desc: &str, subdir: Option<&str>, abs: Option<&str>) -> RepoSection {
        RepoSection {
            description: desc.into(),
            repo_path: None,
            worktree_subdir: subdir.map(String::from),
            worktree_path: abs.map(String::from),
            default_branch: None,
        }
    }

    #[test]
    fn resolves_known_repo_via_repos_root_subdir() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg(
            "/work/repos",
            &[("x/y", repo("Y", Some(".worktree"), None))],
        ));
        let e = reg.resolve(&RepoKey::new("x/y".into())).unwrap();
        assert_eq!(e.repo_path, std::path::Path::new("/work/repos/x/y"));
        assert_eq!(
            e.worktree_root,
            std::path::Path::new("/work/repos/x/y/.worktree")
        );
    }

    #[test]
    fn explicit_worktree_path_overrides_subdir() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg(
            "/work/repos",
            &[("x/y", repo("Y", None, Some("/fast/worktrees/y")))],
        ));
        let e = reg.resolve(&RepoKey::new("x/y".into())).unwrap();
        assert_eq!(e.worktree_root, std::path::Path::new("/fast/worktrees/y"));
    }

    #[test]
    fn unknown_repo_returns_none() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg("/r", &[]));
        assert!(reg.resolve(&RepoKey::new("nope/none".into())).is_none());
    }

    #[test]
    fn reload_returns_diff_report() {
        let reg = RepoRegistry::new();
        reg.reload(&cfg("/r", &[("x/a", repo("A", Some(".w"), None))]));
        let rep = reg.reload(&cfg(
            "/r",
            &[
                ("x/a", repo("A", Some(".w"), None)),
                ("x/b", repo("B", Some(".w"), None)),
            ],
        ));
        assert_eq!(rep.added, vec![RepoKey::new("x/b".into())]);
        assert!(rep.removed.is_empty());
    }
}
