//! Recovering an agent's final answer from the agent's **own** transcript.
//!
//! herdr's pane content cannot serve as the answer (#124): `pane.read` returns
//! a copy of the screen with no scrollback, so a long reply loses its head and
//! carries TUI chrome. The agent's own conversation log has the exact text, so
//! that is what the `output = source` publish artifact is built from.
//!
//! Every agent herdr integrates stores that log in its own place and format, so
//! this module is a **per-agent seam**: [`for_agent`] resolves the reader for
//! the agent herdr names in `pane.agent_session.agent`. Claude Code is the only
//! reader implemented today (it is the agent this plugin launches by default);
//! adding another means adding one [`TranscriptReader`] here — callers do not
//! change, and an agent with no reader degrades to screen extraction rather
//! than failing.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The agent session herdr reports for a pane (`pane.get` → `pane.agent_session`),
/// populated by the agent's herdr integration hook.
#[derive(Debug, Clone)]
pub struct AgentSession {
    /// The agent herdr detected (`claude`, `codex`, …) — selects the reader.
    pub agent: String,
    /// What [`value`](Self::value) is: `id` (a session id) is what integrations
    /// report today; anything else is left to the reader to interpret.
    pub kind: String,
    /// The agent's own session identifier.
    pub value: String,
}

impl AgentSession {
    /// Parse the `agent_session` object of a herdr pane record, if present and
    /// complete enough to identify a session.
    pub fn from_pane(pane: &Value) -> Option<Self> {
        let session = pane.get("agent_session")?;
        let value = session.get("value").and_then(Value::as_str)?;
        if value.is_empty() {
            return None;
        }
        Some(Self {
            agent: session
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind: session
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            value: value.to_string(),
        })
    }
}

/// Reads an agent's last answer out of that agent's transcript.
pub trait TranscriptReader: Send + Sync {
    /// The `agent_session.agent` value this reader handles.
    fn agent(&self) -> &'static str;

    /// The agent's most recent answer text, or `None` when the transcript is
    /// missing/unreadable/empty (the caller then falls back to the screen).
    /// `cwd` is the pane's working directory — agents commonly key their logs
    /// by it.
    fn last_answer(&self, session: &AgentSession, cwd: &Path) -> Option<String>;
}

/// Every reader this build knows about. One entry per integrated agent.
const READERS: &[&dyn TranscriptReader] = &[&ClaudeTranscript];

/// The reader for `agent`, if this build has one.
pub fn for_agent(agent: &str) -> Option<&'static dyn TranscriptReader> {
    READERS.iter().copied().find(|r| r.agent() == agent)
}

/// Claude Code: JSONL transcripts under `<config>/projects/<encoded cwd>/<session id>.jsonl`.
struct ClaudeTranscript;

impl TranscriptReader for ClaudeTranscript {
    fn agent(&self) -> &'static str {
        "claude"
    }

    fn last_answer(&self, session: &AgentSession, cwd: &Path) -> Option<String> {
        // `id` is what the hook reports; a future `kind` may not be a session id.
        if !session.kind.is_empty() && session.kind != "id" {
            return None;
        }
        let path = claude_transcript_path(&session.value, cwd)?;
        let body = std::fs::read_to_string(path).ok()?;
        last_assistant_text(&body)
    }
}

/// Claude's config root (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
fn claude_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".claude"))
}

/// Locate `<session_id>.jsonl`: the encoded-`cwd` directory first, then a scan
/// of every project directory. The scan keeps this working when the encoding
/// changes — the session id is a uuid, so a hit is unambiguous.
fn claude_transcript_path(session_id: &str, cwd: &Path) -> Option<PathBuf> {
    // A session id lands in a path; refuse anything that could escape it.
    if session_id.is_empty() || session_id.contains(['/', '\\', '.']) {
        return None;
    }
    let projects = claude_config_dir()?.join("projects");
    let file = format!("{session_id}.jsonl");

    let direct = projects.join(encode_cwd(cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(&file))
        .find(|candidate| candidate.is_file())
}

/// Claude's project-directory name for `cwd`: path separators and dots become
/// `-` (`/w/repo/.worktrees/x` → `-w-repo--worktrees-x`).
fn encode_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy().replace(['/', '.'], "-")
}

/// The last non-empty assistant message in a Claude JSONL transcript.
///
/// Not every `assistant` entry is the agent talking. The CLI logs its own
/// errors as synthetic assistant turns — a rate limit becomes
/// `{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit",
/// content:[{"text":"You've hit your session limit · resets 4:10pm"}]}` — and
/// the CLI then returns to idle, which is exactly the completion signal the
/// state stream watches for. Published unfiltered, that error text would become
/// the task's answer (a Slack reply reading "You've hit your session limit"),
/// so synthetic turns are skipped: with no real answer the caller falls back to
/// the screen rather than publishing the CLI's complaint.
fn last_assistant_text(transcript: &str) -> Option<String> {
    transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("assistant"))
        .filter(|entry| !is_synthetic(entry))
        .filter_map(|entry| {
            let content = entry.get("message")?.get("content")?.as_array()?.clone();
            let text = content
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            let text = text.trim().to_string();
            (!text.is_empty()).then_some(text)
        })
        .next_back()
}

/// Whether a transcript entry is the CLI speaking as the agent rather than the
/// agent itself: an API error it surfaced (`isApiErrorMessage`), bookkeeping it
/// injected (`isMeta`), or a turn it generated locally (`model: "<synthetic>"`).
/// Any of these marks the entry as not an answer.
fn is_synthetic(entry: &Value) -> bool {
    let flagged = |key: &str| entry.get(key).and_then(Value::as_bool).unwrap_or(false);
    let synthetic_model = entry
        .get("message")
        .and_then(|m| m.get("model"))
        .or_else(|| entry.get("model"))
        .and_then(Value::as_str)
        .is_some_and(|model| model.starts_with('<'));
    flagged("isApiErrorMessage") || flagged("isMeta") || synthetic_model
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_the_pane_agent_session() {
        let pane = json!({
            "pane_id": "w1:p1",
            "agent_session": { "source": "herdr:claude", "agent": "claude", "kind": "id", "value": "abc-123" },
        });
        let session = AgentSession::from_pane(&pane).expect("a session");
        assert_eq!(session.agent, "claude");
        assert_eq!(session.kind, "id");
        assert_eq!(session.value, "abc-123");

        // No session reported yet (the hook has not fired), or an empty id.
        assert!(AgentSession::from_pane(&json!({ "pane_id": "w1:p1" })).is_none());
        assert!(
            AgentSession::from_pane(&json!({ "agent_session": { "value": "" } })).is_none(),
            "an empty session id identifies nothing"
        );
    }

    #[test]
    fn resolves_readers_by_agent_name() {
        assert_eq!(for_agent("claude").map(|r| r.agent()), Some("claude"));
        // An agent this build has no reader for degrades to the screen.
        assert!(for_agent("codex").is_none());
        assert!(for_agent("").is_none());
    }

    #[test]
    fn encodes_cwd_like_claude_does() {
        assert_eq!(
            encode_cwd(Path::new("/Users/me/Workspace/dotfiles")),
            "-Users-me-Workspace-dotfiles"
        );
        // Dots collapse too, so worktrees under a dot-directory still resolve.
        assert_eq!(
            encode_cwd(Path::new("/w/repo/.worktrees/qa-1.2")),
            "-w-repo--worktrees-qa-1-2"
        );
    }

    #[test]
    fn takes_the_last_assistant_text() {
        let transcript = [
            r#"{"type":"user","message":{"content":[{"type":"text","text":"question"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first answer"}]}}"#,
            // Tool-use blocks carry no text and must not become the answer.
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"final answer\nsecond line"}]}}"#,
            "not json at all",
        ]
        .join("\n");
        assert_eq!(
            last_assistant_text(&transcript).as_deref(),
            Some("final answer\nsecond line")
        );
    }

    #[test]
    fn skips_the_clis_own_error_turns() {
        // A rate limit is logged as a synthetic assistant turn and the CLI then
        // goes idle — the same signal a finished answer gives. Publishing it
        // would put "You've hit your session limit" in the Slack reply.
        let transcript = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"the real answer"}]}}"#,
            r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","apiErrorStatus":429,"message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your session limit · resets 4:10pm (Asia/Tokyo)"}]}}"#,
        ]
        .join("\n");
        assert_eq!(
            last_assistant_text(&transcript).as_deref(),
            Some("the real answer"),
            "the CLI's error turn must not shadow the agent's answer"
        );

        // With no real answer at all, there is nothing to publish — the caller
        // falls back to the screen instead of publishing the complaint.
        let only_error = r#"{"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"You've hit your session limit"}]}}"#;
        assert_eq!(last_assistant_text(only_error), None);

        // The other synthetic markers are skipped the same way.
        let meta = r#"{"type":"assistant","isMeta":true,"message":{"content":[{"type":"text","text":"caveat: …"}]}}"#;
        assert_eq!(last_assistant_text(meta), None);
        let synthetic_model = r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No conversation found"}]}}"#;
        assert_eq!(last_assistant_text(synthetic_model), None);
    }

    #[test]
    fn no_assistant_text_yields_nothing() {
        assert_eq!(last_assistant_text(""), None);
        assert_eq!(
            last_assistant_text(
                r#"{"type":"user","message":{"content":[{"type":"text","text":"q"}]}}"#
            ),
            None
        );
    }

    #[test]
    fn rejects_session_ids_that_would_escape_the_projects_dir() {
        assert!(claude_transcript_path("../../etc/passwd", Path::new("/w")).is_none());
        assert!(claude_transcript_path("", Path::new("/w")).is_none());
    }
}
