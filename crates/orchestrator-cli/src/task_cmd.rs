//! `totsuka task ...` — per-task operations (§5.1): list / show / cancel /
//! retry / export.
//!
//! `cancel` / `retry` are state-machine transitions on the DB (#48); the agent
//! session and slots are reconciled by the next `totsuka run` (recovery/retry
//! reuse, F-44).

use std::io::Write;

use clap::Subcommand;
use orchestrator_core::adapters::state_db::EventExportFilter;
use orchestrator_core::domain::state::{TaskEvent, TaskState};
use serde::Serialize;
use serde_json::Value;

use crate::common::{CliError, Cx, JsonFlag, print_json, safe};

/// Task subcommands.
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List all tasks.
    List {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Show one task: state, its conversation, sessions, worktree, and full
    /// event history.
    Show {
        /// Task id (see `totsuka status`).
        id: i64,
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Cancel a task (any non-finished state).
    Cancel {
        /// Task id.
        id: i64,
    },
    /// Re-queue a failed or cancelled task (reuses its worktree/session when
    /// possible, F-44).
    Retry {
        /// Task id.
        id: i64,
    },
    /// Stream the audit log as NDJSON — one JSON event per line, oldest first
    /// (#463).
    ///
    /// The state of record lives in SQLite, which no other tool can read
    /// without `sqlite3` and the schema. This is its flat-text escape hatch:
    /// `events` is append-only, so the export composes with `jq`, `grep`, and
    /// an incremental cursor.
    Export {
        /// Only events after this `event_id` — the cursor for an incremental
        /// export (take the last `event_id` of the previous run).
        #[arg(long, value_name = "EVENT_ID")]
        since: Option<i64>,
        /// Only this task's events.
        #[arg(long, value_name = "ID")]
        task: Option<i64>,
        /// Omit the `detail` field.
        ///
        /// This is NOT a redaction feature — the same content is already
        /// reachable through `task show --json`. It is here because `detail`
        /// carries the agent's accumulated terminal output on the publish
        /// transitions, which makes rows arbitrarily large.
        //
        // No markdown emphasis in help text: clap prints doc comments
        // verbatim, so `**...**` would paint literal asterisks in a terminal.
        // No other command's `--help` in this binary does that.
        #[arg(long)]
        no_detail: bool,
    },
    /// Approve or reject a task awaiting human verification
    /// (`verification = "human"`, #131 D-01).
    Verify {
        /// Task id (must be in the `verifying` state).
        id: i64,
        /// Approve the self-reported completion → publish on the next run.
        #[arg(long, conflicts_with = "fail")]
        pass: bool,
        /// Reject the completion → back to running for correction in the pane.
        #[arg(long, requires = "reason")]
        fail: bool,
        /// Reason for rejection (required with `--fail`).
        #[arg(long)]
        reason: Option<String>,
    },
}

impl TaskCommand {
    /// Whether this subcommand was invoked with `--json` (drives the JSON
    /// error envelope in `main`).
    pub fn wants_json(&self) -> bool {
        // `export` has no `--json` to set: NDJSON is its only output, so its
        // errors belong in the JSON envelope unconditionally.
        if matches!(self, Self::Export { .. }) {
            return true;
        }
        matches!(
            self,
            Self::List { json } | Self::Show { json, .. } if json.json
        )
    }
}

/// A `task show --json` document.
#[derive(Debug, Serialize)]
struct TaskDetail {
    id: i64,
    source: String,
    source_task_id: String,
    workflow: String,
    mode: String,
    state: String,
    repo: Option<String>,
    priority: i64,
    title: String,
    url: Option<String>,
    worktree_path: Option<String>,
    branch: Option<String>,
    finished_at: Option<String>,
    created_at: String,
    updated_at: String,
    /// The conversation, oldest first (#242). Empty for a task from a source
    /// that has one message per task.
    messages: Vec<MessageRow>,
    sessions: Vec<SessionRow>,
    events: Vec<EventRow>,
}

/// One message of the conversation. The body is **not** truncated here — the
/// JSON document is what an audit reads; only the terminal rendering clips.
#[derive(Debug, Serialize)]
struct MessageRow {
    message_key: String,
    author: Option<String>,
    body: String,
    url: Option<String>,
    received_at: String,
    /// When the message was handed to the agent; `null` while it is still
    /// queued. Messages dispatched together share one timestamp.
    processed_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionRow {
    plugin: String,
    session_id: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct EventRow {
    from: Option<String>,
    to: String,
    occurred_at: String,
    detail: Option<Value>,
}

/// Dispatch a task subcommand.
pub fn run(cx: &Cx, command: TaskCommand) -> Result<(), CliError> {
    match command {
        TaskCommand::List { json } => list(cx, json.json),
        TaskCommand::Show { id, json } => show(cx, id, json.json),
        TaskCommand::Cancel { id } => cancel(cx, id),
        TaskCommand::Retry { id } => retry(cx, id),
        TaskCommand::Export {
            since,
            task,
            no_detail,
        } => export(cx, since, task, no_detail),
        TaskCommand::Verify {
            id,
            pass,
            fail,
            reason,
        } => verify(cx, id, pass, fail, reason),
    }
}

fn list(cx: &Cx, json: bool) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let tasks = db.list_tasks()?;
    if json {
        let rows: Vec<Value> = tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id, "state": t.state.to_string(), "source": t.source,
                    "source_task_id": t.source_task_id, "workflow": t.workflow,
                    "repo": t.repo, "title": t.title, "updated_at": t.updated_at,
                })
            })
            .collect();
        return print_json(&rows);
    }
    if tasks.is_empty() {
        println!("no tasks yet → `totsuka run` ingests them from your task sources");
        return Ok(());
    }
    println!(
        "{:<5} {:<14} {:<10} {:<12} TITLE",
        "ID", "STATE", "SOURCE", "WORKFLOW"
    );
    for t in &tasks {
        // `title` is whatever the source let someone type (#280) — it is the
        // last column precisely so it cannot push the others around, but it
        // still has to be stripped of its ability to move the cursor.
        println!(
            "{:<5} {:<14} {:<10} {:<12} {}",
            t.id,
            t.state,
            t.source,
            t.workflow,
            safe(&t.title)
        );
    }
    Ok(())
}

fn show(cx: &Cx, id: i64, json: bool) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    let detail = TaskDetail {
        id: task.id,
        source: task.source,
        source_task_id: task.source_task_id,
        workflow: task.workflow,
        mode: task.mode,
        state: task.state.to_string(),
        repo: task.repo,
        priority: task.priority,
        title: task.title,
        url: task.url,
        worktree_path: task.worktree_path,
        branch: task.branch,
        finished_at: task.finished_at,
        created_at: task.created_at,
        updated_at: task.updated_at,
        messages: db
            .list_task_messages(id)?
            .into_iter()
            .map(|m| MessageRow {
                message_key: m.message_key,
                author: m.author,
                body: m.body,
                url: m.url,
                received_at: m.received_at,
                processed_at: m.processed_at,
            })
            .collect(),
        sessions: db
            .list_sessions(id)?
            .into_iter()
            .map(|s| SessionRow {
                plugin: s.plugin,
                session_id: s.session_id,
                created_at: s.created_at,
            })
            .collect(),
        events: db
            .list_events(id)?
            .into_iter()
            .map(|e| EventRow {
                from: e.from_state.map(|s| s.to_string()),
                to: e.to_state.to_string(),
                occurred_at: e.occurred_at,
                detail: e.detail,
            })
            .collect(),
    };
    if json {
        return print_json(&detail);
    }
    // Everything the source controls goes through `safe` (#280): `title`,
    // `source_task_id`, `url`, and the conversation rows below. `state` /
    // `workflow` / `mode` / `repo` are ours or the config's, so they stay
    // verbatim — running them through would only risk mangling our own text.
    println!("task {}: {}", detail.id, safe(&detail.title));
    println!("  state:     {}", detail.state);
    println!(
        "  source:    {}#{}",
        detail.source,
        safe(&detail.source_task_id)
    );
    println!("  workflow:  {} (mode {})", detail.workflow, detail.mode);
    println!("  repo:      {}", detail.repo.as_deref().unwrap_or("-"));
    if let Some(url) = &detail.url {
        // A URL is the one field where a forged OSC 8 hyperlink would be most
        // convincing, since the operator is about to click it.
        println!("  url:       {}", safe(url));
    }
    if let Some(path) = &detail.worktree_path {
        // Both are fed by the external `source_task_id`. `render_branch`
        // folds `char::is_control()` to `-` on the way in, but that is `Cc`
        // only — a bidi override is `Cf` and goes straight through it, and
        // the path is built from the branch. So neither is sanitised at the
        // source, and this call is load-bearing rather than belt-and-braces.
        println!(
            "  worktree:  {} [{}]",
            safe(path),
            safe(detail.branch.as_deref().unwrap_or("-"))
        );
    }
    if !detail.messages.is_empty() {
        let queued = detail
            .messages
            .iter()
            .filter(|m| m.processed_at.is_none())
            .count();
        println!(
            "  conversation ({} message(s){}):",
            detail.messages.len(),
            if queued > 0 {
                format!(", {queued} not yet sent to the agent")
            } else {
                String::new()
            }
        );
        for m in &detail.messages {
            // `→` = handed to the agent, `·` = still queued. The marker leads
            // the line so a long conversation can be scanned down one column.
            let mark = if m.processed_at.is_some() {
                "→"
            } else {
                "·"
            };
            println!(
                "    {mark} {} {} {}",
                m.received_at,
                safe(m.author.as_deref().unwrap_or("-")),
                one_line(&m.body, BODY_PREVIEW_CHARS)
            );
        }
    }
    if !detail.sessions.is_empty() {
        println!("  sessions (newest first):");
        for s in &detail.sessions {
            // The session id is minted by the agent plugin, not by us — a
            // narrower door than the task source, but still not our text.
            println!(
                "    {} @ {} ({})",
                safe(&s.session_id),
                s.plugin,
                s.created_at
            );
        }
    }
    println!("  history:");
    for e in &detail.events {
        let from = e.from.as_deref().unwrap_or("(ingest)");
        println!("    {} {} → {}", e.occurred_at, from, e.to);
    }
    Ok(())
}

/// `totsuka task export` — stream the audit log as NDJSON (#463).
///
/// One compact JSON document per line, oldest first, written straight through
/// a buffered stdout. Nothing accumulates in memory: a single `detail` can
/// carry the agent's whole terminal output (`publish_artifact`), so collecting
/// the walk first would make the memory cost of an export proportional to
/// every byte every agent has ever printed.
///
/// Output is **not** passed through [`safe`]: like every other `--json` path,
/// `serde_json` already escapes control characters, and re-escaping would
/// corrupt the machine-readable values (see the note on external text in
/// `orchestrator-cli`'s component doc).
fn export(cx: &Cx, since: Option<i64>, task: Option<i64>, no_detail: bool) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    // An unknown `--task` is a user error, not an empty answer, and is
    // rejected the way `show` / `cancel` / `retry` reject it. An exhausted
    // `--since` cursor is the opposite case — a real answer that legitimately
    // yields nothing — so it stays silent. Without this, a script passing a
    // stale id writes an empty archive that reads as "this task did nothing"
    // rather than "this task does not exist".
    if let Some(id) = task
        && db.get_task(id)?.is_none()
    {
        return Err(not_found(id));
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let filter = EventExportFilter {
        after_id: since,
        task_id: task,
        without_detail: no_detail,
    };

    let result: Result<(), CliError> =
        db.for_each_exported_event(filter, |event| write_ndjson_line(&mut out, &event));

    // `totsuka task export | head -5` closes the pipe on us. Rust ignores
    // SIGPIPE, so that surfaces as an `EPIPE` write error instead of killing
    // the process — and reporting it as a failure would make the command
    // unusable in exactly the pipelines it exists for. The reader left; that
    // is success, not an error.
    match result {
        Err(e) if is_broken_pipe(&e) => return Ok(()),
        other => other?,
    }
    match out.flush() {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

/// Serialize one event straight into `out`, followed by the NDJSON newline.
///
/// `to_writer` rather than `to_string` + `writeln!`: it serializes into the
/// buffer instead of building an intermediate `String` per row, which for a
/// row carrying a multi-megabyte `publish_artifact` is the difference between
/// one copy and two.
///
/// A `serde_json` failure caused by the writer is unwrapped back into the
/// underlying [`std::io::Error`] so the caller can still see a `BrokenPipe`;
/// `serde_json::Error` hides the kind behind
/// [`io_error_kind`](serde_json::Error::io_error_kind).
fn write_ndjson_line<W: Write>(
    out: &mut W,
    event: &orchestrator_core::adapters::state_db::ExportedEvent,
) -> Result<(), CliError> {
    serde_json::to_writer(&mut *out, event).map_err(|e| match e.io_error_kind() {
        Some(kind) => std::io::Error::new(kind, e),
        None => std::io::Error::other(e),
    })?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Whether this error is a downstream reader closing the pipe.
fn is_broken_pipe(e: &CliError) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
}

fn cancel(cx: &Cx, id: i64) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    if task.state.is_terminal() {
        // The advice has to match what `retry` actually accepts: it refuses a
        // `done` task, and since #242 the way to carry a finished conversation
        // forward is another message in it, not a re-run of the old one.
        let next = if task.state == TaskState::Done {
            "it finished; send another message in the conversation (the reply in its thread/issue) to continue it".to_string()
        } else {
            format!("use `totsuka task retry {id}` to re-run it")
        };
        return Err(format!(
            "task {id} is already {} → nothing to cancel; {next}",
            task.state
        )
        .into());
    }
    db.apply_event(
        id,
        TaskEvent::Cancel,
        Some(serde_json::json!({ "kind": "cli", "command": "task cancel" })),
    )?;
    println!("task {id} cancelled");
    if matches!(
        task.state,
        TaskState::Dispatched
            | TaskState::Running
            | TaskState::WaitingInput
            | TaskState::Publishing
    ) {
        println!(
            "note: the worktree is kept per the cleanup policy; the pane is not closed here — `totsuka doctor` lists it"
        );
    }
    Ok(())
}

fn retry(cx: &Cx, id: i64) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    if !matches!(
        task.state,
        TaskState::Failed | TaskState::Cancelled | TaskState::Skipped
    ) {
        let action = if task.state == TaskState::Done {
            // Since #242 `done` means "no unprocessed messages", not "closed
            // forever": a new message reopens the conversation. Re-running the
            // same instructions is a different thing, and not what anyone
            // asking about a finished task wants.
            "it finished; send another message in the conversation (the reply in its thread/issue) to continue it — a re-run of the same instructions is not what `retry` is for"
        } else {
            "only failed/cancelled/skipped tasks can be retried; `totsuka task cancel` it first if you want a re-run"
        };
        return Err(format!("task {id} is {} → {action}", task.state).into());
    }
    // A skipped task (#556) is another member's: they claimed it, this
    // instance stepped aside. Retrying is the deliberate override, so say
    // what it re-enters rather than refusing.
    if task.state == TaskState::Skipped {
        println!(
            "task {id} was skipped because another member claimed it — retrying re-enters \
             the claim: it runs here only if they have since released the task"
        );
    }
    // `retry_task`, not `apply_event(Retry)`: requeueing the task without the
    // messages its failed run was given would dispatch an empty prompt (#242).
    let (_, requeued) = db.retry_task(
        id,
        Some(serde_json::json!({ "kind": "cli", "command": "task retry" })),
    )?;
    println!(
        "task {id} re-queued → `totsuka run` dispatches it (reusing its worktree/session when possible)"
    );
    if requeued > 0 {
        println!("  {requeued} message(s) from the last dispatch will be sent again");
    }
    Ok(())
}

fn verify(
    cx: &Cx,
    id: i64,
    pass: bool,
    fail: bool,
    reason: Option<String>,
) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    if task.state != TaskState::Verifying {
        return Err(format!(
            "task {id} is {} → `totsuka task verify` applies only to a task awaiting human verification (state `verifying`)",
            task.state
        )
        .into());
    }
    if pass {
        // ApproveVerification → Publishing; the next `totsuka run` recover cycle
        // finalizes it via the existing Publishing-restore path (#131 D-01).
        db.apply_event(
            id,
            TaskEvent::ApproveVerification,
            Some(serde_json::json!({ "kind": "cli", "command": "task verify --pass" })),
        )?;
        println!("task {id} verification passed → `totsuka run` publishes it on the next cycle");
    } else if fail {
        // VerificationFailed → Running; the human gives corrective instructions
        // directly in the agent pane (D-07).
        let reason = reason.unwrap_or_default();
        db.apply_event(
            id,
            TaskEvent::VerificationFailed,
            Some(serde_json::json!({
                "kind": "cli", "command": "task verify --fail", "reason": reason,
            })),
        )?;
        println!(
            "task {id} verification failed → back to running; give corrective instructions in the agent pane"
        );
    } else {
        return Err(
            "specify --pass to approve, or --fail --reason <text> to reject → see `totsuka task verify --help`"
                .into(),
        );
    }
    Ok(())
}

/// How much of a message body the terminal listing shows. The whole text is
/// always in `--json`.
const BODY_PREVIEW_CHARS: usize = 72;

/// `body` as one line, clipped to `limit` **characters** (not bytes — Slack
/// bodies are routinely Japanese, and slicing by byte would panic mid-glyph).
///
/// The three steps are ordered, not interchangeable (#280):
///
/// 1. **fold whitespace** — `split_whitespace` disposes of `\n`, `\r` and
///    `\t` by turning them into the single space that joins the tokens, so
///    they never reach step 2 and never need escaping.
/// 2. **escape what is left** — ESC, BEL and friends are not whitespace, so
///    step 1 passes them straight through; [`safe`] is what stops them.
/// 3. **clip** — last, so `limit` bounds the width actually painted on the
///    terminal. Clipping before escaping would let 72 ESC bytes expand into
///    432 characters of `\u{1b}` afterwards and wrap the row anyway.
fn one_line(body: &str, limit: usize) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = safe(&flat);
    // One pass: take the head, then ask the *same* iterator whether anything
    // is left. Counting the whole string first would walk a long body twice
    // only to learn it is long.
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(limit).collect();
    match chars.next() {
        Some(_) => format!("{head}…"),
        None => head,
    }
}

fn not_found(id: i64) -> CliError {
    format!("task {id} not found → `totsuka task list` shows known ids").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_clips_by_character_and_survives_odd_bodies() {
        // Multibyte throughout: clipping by byte would panic mid-glyph, and
        // Slack bodies are routinely Japanese.
        let body = "日本語".repeat(40);
        let clipped = one_line(&body, 5);
        assert_eq!(clipped, "日本語日本…");

        // Exactly at the limit is not clipped — the ellipsis must mean
        // "there is more", never "there was exactly this much".
        assert_eq!(one_line("あいうえお", 5), "あいうえお");
        assert_eq!(one_line("あいうえおか", 5), "あいうえお…");

        // A long unbroken token has no whitespace to fold, and still clips.
        assert_eq!(one_line(&"x".repeat(100), 3), "xxx…");

        // Nothing to show: an empty or whitespace-only body renders as an
        // empty cell rather than a stray ellipsis.
        assert_eq!(one_line("", 5), "");
        assert_eq!(one_line("   \n\t ", 5), "");
    }

    /// A message body is the most attacker-controlled string the CLI prints
    /// (#280): anyone who can post in the channel writes it. The escaping has
    /// to happen *between* the whitespace fold and the clip — see `one_line`.
    #[test]
    fn one_line_defuses_a_body_that_tries_to_repaint_the_row() {
        let esc = char::from_u32(0x1b).unwrap();

        // Whitespace controls are folded away by step 1, so they never show
        // up as escapes — the row just becomes one line.
        assert_eq!(one_line("real\rFORGED", 72), "real FORGED");
        assert_eq!(one_line("a\nb\tc", 72), "a b c");

        // ESC is not whitespace, so step 2 is what stops it.
        assert_eq!(one_line(&format!("hi{esc}[2J"), 72), r"hi\u{1b}[2J");

        // Clipping happens after escaping, so the printed width still obeys
        // the limit even when the body is nothing but escape sequences.
        let hostile = format!("{esc}[1A").repeat(40);
        let clipped = one_line(&hostile, 20);
        assert_eq!(clipped.chars().count(), 21, "{clipped:?}"); // 20 + ellipsis
        assert!(!clipped.chars().any(char::is_control), "{clipped:?}");
    }

    #[test]
    fn one_line_folds_a_multi_line_body_into_one_row() {
        assert_eq!(
            one_line("追記: ログです\n\n    ERROR foo\n    ERROR bar", 72),
            "追記: ログです ERROR foo ERROR bar"
        );
    }
}
