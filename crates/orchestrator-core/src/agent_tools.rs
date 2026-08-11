//! Which external tools a profile's agent needs, and whether they are usable
//! (#399, [#393](https://github.com/tomoya-k31/totsuka/issues/393) D9).
//!
//! Since [#398](https://github.com/tomoya-k31/totsuka/issues/398) the agent
//! writes its own deliverable, so a task can now fail for a reason that has
//! nothing to do with the task: the tool it needs is not authenticated. The
//! failure mode is bad — the agent starts, discovers `gh` cannot talk to
//! GitHub, and sits in the pane until someone looks. Checking first turns that
//! into a task that waits and a notification that says why.
//!
//! # What is checked, and the large part that is not
//!
//! **Only `implement`, and only `gh`.** Opening a pull request needs `gh`
//! whatever the task came from, so that requirement is knowable here.
//!
//! `triage` and `design` also write externally, but *where* depends on the
//! source — `gh issue comment` for GitHub, the Notion MCP server for Notion —
//! and the Orchestrator cannot tell those apart. `[[workflows]].source` is a
//! user-chosen instance name (`github`, but equally `gh-work`), and guessing
//! from it would mean **blocking a task that would have run**. A gate that
//! guesses wrong is worse than no gate, so those profiles are not checked.
//! `doctor` says so rather than leaving the silence to be read as a pass.
//!
//! Notion MCP is unreachable from here for a second reason: it is configured
//! on the *agent's* side, in a file whose location depends on which tool the
//! workflow resolves to, and it is a different credential from the `notion`
//! plugin's own token.
//!
//! # Why the check is local-only
//!
//! [`available`] stats a file and looks up a binary; it never runs
//! `gh auth status`. This is on the dispatch path, which runs every cycle: a
//! network probe there would add its latency and its flakiness to every
//! dispatch decision. `doctor --online` is where the live check belongs.
//!
//! The cost is that a **valid-looking but expired** credential passes here.
//! That is the right trade: the check exists to catch "never set up", which is
//! the common case, and an expired token still fails visibly in the pane the
//! way it does today.
//!
//! # The environment mismatch, which is real
//!
//! This runs in the Orchestrator's process. The agent runs in a pane with the
//! user's shell profile applied (`.zshenv`, `mise activate`, herdr's workspace
//! env). A `gh` reachable only from that environment reads as missing here.
//!
//! That is why a failed check **skips** rather than fails: the task stays
//! `Queued` and runs as soon as the check passes, so a false negative delays
//! work instead of destroying it. If it turns out to bite in practice, the fix
//! is to probe through the agent plugin — a separate change, not a knob.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::Profile;

/// How long an availability answer is reused.
///
/// The dispatch loop ticks every 200 ms and re-examines every queued task, so
/// an uncached check would `stat` in a hot loop. Five minutes also bounds how
/// long a task waits after the operator fixes the credential — long enough to
/// be worth caching, short enough that "run `gh auth login` and wait" is a
/// complete instruction.
const CACHE_TTL: Duration = Duration::from_secs(300);

/// An external tool the agent needs in its own environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentTool {
    /// The GitHub CLI, authenticated.
    Gh,
}

impl AgentTool {
    /// The name shown to an operator.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentTool::Gh => "gh",
        }
    }

    /// The tool a name written by [`AgentTool::as_str`] refers to.
    ///
    /// Needed because the wait reason is *recorded* as names (#407) and
    /// rendered later, possibly by a build that knows more tools than the one
    /// that wrote the note — hence `Option` rather than a panic.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "gh" => Some(AgentTool::Gh),
            _ => None,
        }
    }

    /// What to do about it being missing.
    pub fn remedy(self) -> &'static str {
        match self {
            AgentTool::Gh => "install the GitHub CLI and run `gh auth login`",
        }
    }
}

/// The [`NOTE_KEY`](crate::adapters::state_db::NOTE_KEY) value for "this task
/// cannot start because a tool it needs is unusable in this environment"
/// (#407). The note also carries `missing`, a list of [`AgentTool::as_str`]
/// names.
pub const BLOCKED_NOTE: &str = "blocked_agent_tools";

/// The operator-facing explanation for a task waiting on `missing`.
///
/// Shared by the notification that fires once (#399) and by `totsuka status`,
/// which answers the same question minutes later (#407), so the two cannot
/// drift apart on the remedy or on the false-negative caveat — the caveat is
/// the whole reason this check skips instead of failing.
///
/// Names that this build does not recognise still appear; only their remedy is
/// omitted. A note is data written by some version of totsuka, not necessarily
/// this one.
pub fn blocked_reason(missing: &[&str]) -> String {
    let remedies: Vec<&str> = missing
        .iter()
        .filter_map(|n| AgentTool::parse(n))
        .map(AgentTool::remedy)
        .collect();
    let remedy = if remedies.is_empty() {
        String::new()
    } else {
        format!(" → {}", remedies.join("; "))
    };
    format!(
        "{} unavailable in the orchestrator's environment{remedy}. \
         The task stays queued and starts on its own once this resolves \
         (checked every few minutes). If the tool is only reachable from \
         the agent's pane, this check is a false negative — see the \
         agent-tools note in `totsuka doctor`.",
        missing.join(", ")
    )
}

/// The tools `profile` needs before its tasks can usefully start.
///
/// Empty for everything except `implement` — see the module docs for why
/// `triage` and `design` are deliberately unchecked rather than guessed at.
pub fn required(profile: Option<Profile>) -> &'static [AgentTool] {
    match profile {
        // The deliverable is a pull request, whatever the source.
        Some(Profile::Implement) => &[AgentTool::Gh],
        Some(Profile::Triage | Profile::Design | Profile::Answer) | None => &[],
    }
}

/// Caches [`available`] answers so the dispatch loop does not re-stat every
/// tick.
#[derive(Debug, Default)]
pub struct ToolCache {
    entries: HashMap<AgentTool, (Instant, bool)>,
}

impl ToolCache {
    /// Whether `tool` looks usable, answering from cache within
    /// `CACHE_TTL` (5 minutes).
    ///
    /// `now` is passed in rather than read here so the expiry is testable
    /// without sleeping (the same reason the engine takes a `Clock`).
    pub fn available(&mut self, tool: AgentTool, now: Instant) -> bool {
        if let Some((checked, answer)) = self.entries.get(&tool)
            && now.duration_since(*checked) < CACHE_TTL
        {
            return *answer;
        }
        let answer = available(tool);
        self.entries.insert(tool, (now, answer));
        answer
    }

    /// The tools of `required` that are not usable, in declaration order.
    pub fn missing(&mut self, profile: Option<Profile>, now: Instant) -> Vec<AgentTool> {
        required(profile)
            .iter()
            .copied()
            .filter(|t| !self.available(*t, now))
            .collect()
    }
}

/// Whether `tool` looks usable **right now, from this process**.
///
/// Local checks only (see the module docs): a binary on `PATH` and the file the
/// tool writes when it authenticates.
pub fn available(tool: AgentTool) -> bool {
    match tool {
        AgentTool::Gh => on_path("gh") && gh_hosts_file().is_some(),
    }
}

/// Whether `program` resolves on this process's `PATH`.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

/// `gh`'s credential store, if it exists.
///
/// Its **presence** is the signal, never its contents — this file holds OAuth
/// tokens, and nothing here should be in a position to leak one into a log.
fn gh_hosts_file() -> Option<PathBuf> {
    let base = std::env::var_os("GH_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("gh")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/gh")))?;
    let hosts = base.join("hosts.yml");
    hosts.is_file().then_some(hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_implement_requires_a_tool() {
        assert_eq!(required(Some(Profile::Implement)), &[AgentTool::Gh]);
        // Not an oversight — see the module docs. `triage`/`design` write
        // externally too, but where depends on a source the Orchestrator
        // cannot identify, and a gate that guesses wrong blocks work that
        // would have run.
        for profile in [Profile::Triage, Profile::Design, Profile::Answer] {
            assert!(required(Some(profile)).is_empty(), "{profile:?}");
        }
        // A workflow written in the spelled-out notation has no profile, so
        // nothing here applies to it — the same line #395 draws.
        assert!(required(None).is_empty());
    }

    #[test]
    fn an_answer_is_cached_until_the_ttl_expires() {
        let mut cache = ToolCache::default();
        let start = Instant::now();
        let first = cache.available(AgentTool::Gh, start);

        // Poison the entry: a cached read must not consult the filesystem.
        cache.entries.insert(AgentTool::Gh, (start, !first));
        assert_eq!(
            cache.available(AgentTool::Gh, start + CACHE_TTL / 2),
            !first,
            "within the TTL the cached answer is used"
        );
        assert_eq!(
            cache.available(AgentTool::Gh, start + CACHE_TTL + Duration::from_secs(1)),
            first,
            "past the TTL the check runs again — this is how a task resumes \
             after the operator authenticates"
        );
    }

    /// Every variant. `as_str` / `remedy` are exhaustive matches so the
    /// compiler catches a new variant there; `parse` matches on `&str` and
    /// cannot, so this list is what keeps it honest — extend it with the
    /// variant.
    const ALL: &[AgentTool] = &[AgentTool::Gh];

    #[test]
    fn every_tool_name_round_trips() {
        // `blocked_reason` reads names back out of a recorded note (#407), so
        // a tool whose name does not parse would lose its remedy.
        for tool in ALL {
            assert_eq!(AgentTool::parse(tool.as_str()), Some(*tool));
        }
    }

    #[test]
    fn a_reason_carries_the_remedy_and_the_false_negative_caveat() {
        let reason = blocked_reason(&["gh"]);
        assert!(reason.starts_with("gh unavailable"), "{reason}");
        assert!(reason.contains("gh auth login"), "{reason}");
        assert!(reason.contains("false negative"), "{reason}");

        // A note written by a build that knows a tool this one does not: the
        // name still has to reach the operator, because dropping it would
        // render as "blocked for no reason".
        let reason = blocked_reason(&["nonesuch"]);
        assert!(reason.starts_with("nonesuch unavailable"), "{reason}");
        assert!(!reason.contains(" → "), "no remedy invented: {reason}");
    }

    #[test]
    fn a_profile_needing_nothing_reports_nothing_missing() {
        let mut cache = ToolCache::default();
        assert!(
            cache
                .missing(Some(Profile::Answer), Instant::now())
                .is_empty()
        );
        assert!(cache.missing(None, Instant::now()).is_empty());
    }
}
