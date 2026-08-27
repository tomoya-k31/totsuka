//! `totsuka menu` — the menu-bar view (F-109).
//!
//! Two channels, deliberately: the **glyph** says whether the orchestrator can
//! make progress at all, and the **number** says how many tasks are waiting on
//! a human. They are orthogonal facts, so folding them into a single priority
//! ladder would drop one of them exactly when both are true ("stopped, and 3
//! tasks left waiting").
//!
//! ## Model, then serializer
//!
//! [`MenuModel`] is the thing worth testing — which rows, in which section, in
//! which order. The SwiftBar line syntax is one serialization of it, reached
//! by the default output; `--json` emits the model itself. Keeping them apart
//! means the tests assert about the model and the escaping, not about a
//! third-party DSL.
//!
//! ## Why the escaping is the point
//!
//! Task titles are written by whoever can post in the connected Slack channel
//! or file the GitHub issue, and SwiftBar reads a row through **three** layers
//! of syntax, not one. All three were found on real hardware, one at a time,
//! after the unit tests were green:
//!
//! 1. `text | key=value …` — `|` separates a row's text from its parameters,
//!    so an unescaped one lets its author append arbitrary parameters, `bash=`
//!    included. Closed by [`menu_text`].
//! 2. **backslash escapes inside the text.** Measured on SwiftBar 2.1.1: a
//!    `\n` in the output becomes a real line break, and an escape it does not
//!    know loses its backslash (`\u{7c}` printed as `u{7c}`). The first
//!    shipped version escaped control characters with [`safe`] and stopped
//!    there, so a title containing a newline had that newline **restored** by
//!    SwiftBar — splitting one row into three, forged `---` separator
//!    included. Closed by the doubling step in [`menu_text`].
//! 3. **`:name:` expansion.** `symbolize` and `emojize` default to true, so
//!    SwiftBar swaps `:checkmark.seal.fill:` for an SF Symbol image and
//!    `:mushroom:` for an emoji — written characters simply **vanish**.
//!    **This one cannot be escaped**: no spelling of `:mushroom:` survives as
//!    text. Closed by [`RENDER_AS_TEXT`], repeated on every row that carries
//!    externally-authored text.
//!
//! The pattern across all three is worth naming: making text safe for one
//! consumer does not make it safe for the next one down. `safe` renders
//! control characters harmless *to a terminal*, and every layer above read
//! its output as syntax again.
//!
//! Both layers are the same class of problem as #280 (see
//! `ai-docs/security/terminal-output-sanitization.md`), which is why the
//! rendering lives here in Rust and not in a shell/jq formatter: moving it out
//! would move the defence somewhere with no types and no tests.
//!
//! ## Why it never fails
//!
//! A menu-bar plugin that exits non-zero renders as a broken item, so every
//! degraded outcome — no state DB, pending migrations, XDG paths that will not
//! resolve — is a **row in the menu** and a clean exit, the same contract
//! `totsuka focus` keeps for the same reason (it is the click target of a
//! notification).
//!
//! That contract reaches further than the body of [`run`]: `main` dispatches
//! `menu` **before** the shared `Cx::resolve`, because a `?` on a line outside
//! this module would end the process non-zero without ever reaching the code
//! that turns a failure into a row. Writing is deliberate too — `println!`
//! *panics* on a write error, so a closed pipe (`totsuka menu | head`, which
//! the operations guide suggests for debugging) would exit 101. The same
//! lesson is already pinned for `task export`.

use std::io::Write;
use std::path::Path;

use orchestrator_core::domain::state::TaskState;
use serde::Serialize;

use crate::common::{CliError, Cx, live_health, lock_status, safe, stale_health_message};

/// How many characters of a task title reach the menu before it is elided.
const TITLE_BUDGET: usize = 60;

/// Parameters every row carrying externally-authored text must repeat, so
/// SwiftBar renders that text *as text*.
///
/// `symbolize` and `emojize` both default to **true** upstream: SwiftBar
/// replaces a `:name:` sequence in a row's text with an SF Symbol or an emoji
/// (`:sun.max:`, `:mushroom:`). Task titles are written by whoever can post in
/// the connected Slack channel, so leaving those on lets their author make
/// written characters **disappear into a glyph** — precisely what
/// escape-not-strip exists to prevent, and a way to forge what a row appears
/// to say.
///
/// **This layer cannot be escaped away.** Unlike `|` and backslashes there is
/// no spelling of `:mushroom:` that survives as text; the parameter is the
/// only switch. That is why it lives here and not in [`menu_text`].
const RENDER_AS_TEXT: &str = "symbolize=false emojize=false";

/// Whether the orchestrator can make progress right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// A live `run` holds the lock and reports nothing wrong.
    Ok,
    /// A live `run`, but it cannot do its whole job (F-110) — the case worth
    /// its own glyph, because the process looks fine from the outside.
    Degraded,
    /// No `run` is holding the lock — including a stale lock from a crashed
    /// one. Nothing moves until it is started again.
    Down,
}

impl Availability {
    /// The glyph shown in the menu bar itself.
    fn glyph(self) -> &'static str {
        match self {
            Availability::Ok => "○",
            Availability::Degraded => "⚠",
            Availability::Down => "✕",
        }
    }
}

/// Which section of the menu a task belongs in.
///
/// **This is the one place the 要対応 (Attention) set is defined** — the
/// glossary concept of the same name (`ai-docs/glossary/attention.md`). The
/// match is exhaustive on purpose: adding a [`TaskState`] must not silently
/// default to "hidden", it must fail to compile until someone decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Cannot move again until a human acts.
    Attention,
    /// An agent is working on it; nothing for a human to do.
    Working,
    /// Not shown at all.
    Hidden,
}

/// Classify one task. `has_wait_reason` is whether the orchestrator recorded a
/// note explaining why it is not starting (#407) — the only thing that
/// separates a queued task waiting its turn from one that is stuck.
fn classify(state: TaskState, has_wait_reason: bool) -> Section {
    match state {
        // Waiting on a human, always.
        TaskState::Pending
        | TaskState::WaitingInput
        | TaskState::Verifying
        | TaskState::Escalated => Section::Attention,
        // Queued *with* a recorded reason is stuck; queued without one is
        // simply next in line and will start on its own.
        TaskState::Queued if has_wait_reason => Section::Attention,
        TaskState::Queued => Section::Hidden,
        TaskState::Dispatched | TaskState::Running | TaskState::Publishing => Section::Working,
        // Terminal states are never counted. `StateDb::list_tasks` returns
        // every task ever ingested, with no filter and no limit, so counting
        // them would make the badge grow monotonically and never return to
        // zero — a badge you cannot clear is a badge you stop reading.
        TaskState::Done | TaskState::Failed | TaskState::Cancelled | TaskState::Skipped => {
            Section::Hidden
        }
    }
}

/// One task row of the menu.
#[derive(Debug, Serialize)]
pub struct MenuRow {
    /// Task id — also the argument `totsuka focus` is invoked with.
    pub task_id: i64,
    /// The task's state, spelled exactly as `status` and `--json` spell it.
    pub state: String,
    /// Matched workflow name.
    pub workflow: String,
    /// Task title, **verbatim** (`--json` stays byte-exact, #280).
    pub title: String,
}

/// The whole menu, independent of how it is rendered.
#[derive(Debug, Serialize)]
pub struct MenuModel {
    /// Availability glyph channel.
    pub availability: Availability,
    /// Badge number channel — `attention.len()`, surfaced so a consumer does
    /// not have to count.
    pub attention_count: usize,
    /// Tasks that cannot move until a human acts.
    pub attention: Vec<MenuRow>,
    /// Tasks an agent is working on.
    pub working: Vec<MenuRow>,
    /// What the live `run` cannot currently do (F-110), already worded for a
    /// human. Empty when it is healthy — or when nothing is running, in which
    /// case the glyph is `✕` and a stale document would only mislead.
    pub degraded: Vec<String>,
    /// Why the menu could not be built, if it could not be. The model is
    /// still emitted (with `availability: down`) so both output modes stay
    /// parseable and the exit code stays 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl MenuModel {
    /// The model to show when the menu itself could not be built — distinct
    /// from the `degraded` field, which is a *live* run's own report about
    /// what it cannot do.
    fn unavailable(error: String) -> Self {
        Self {
            availability: Availability::Down,
            attention_count: 0,
            attention: Vec::new(),
            working: Vec::new(),
            degraded: Vec::new(),
            error: Some(error),
        }
    }
}

/// Execute `totsuka menu`.
///
/// Always returns `Ok`, and resolves its own paths rather than taking a [`Cx`]:
/// see the module docs on why a menu-bar plugin must not exit non-zero, and why
/// that has to hold outside this function's body as well.
pub fn run(config: Option<&Path>, json: bool) -> Result<(), CliError> {
    let model = Cx::resolve(config)
        .and_then(|cx| build(&cx))
        .unwrap_or_else(|e| MenuModel::unavailable(e.to_string()));
    let body = if json {
        // Serializing our own types cannot fail; if it somehow did, saying so
        // in the document beats exiting non-zero.
        serde_json::to_string_pretty(&model)
            .unwrap_or_else(|e| format!("{{\"error\":\"could not serialize the menu: {e}\"}}"))
            + "\n"
    } else {
        render_swiftbar(&model, &binary_path())
    };
    // **Not `print!`.** The `print*!` macros panic on a write error, so a
    // closed pipe would exit 101 — the failure mode this command exists to
    // avoid. A reader that went away has already stopped caring.
    let _ = std::io::stdout().write_all(body.as_bytes());
    let _ = std::io::stdout().flush();
    Ok(())
}

/// Read the state and assemble the model.
fn build(cx: &Cx) -> Result<MenuModel, CliError> {
    let lock = lock_status(cx);
    // `run.lock` decides availability first: a run that was killed leaves its
    // health file behind, and trusting it would paint `⚠` over a `✕`.
    let degraded: Vec<String> = live_health(cx, &lock)
        .map(|live| {
            let mut reasons: Vec<String> =
                live.health.degraded.iter().map(|d| d.message()).collect();
            // A run whose pid is alive but which has stopped publishing is
            // its own kind of wrong, and the one the four re-askable facts
            // cannot report — a wedged run cannot tell you it is wedged.
            if live.stale {
                reasons.push(stale_health_message(&live.health));
            }
            reasons
        })
        .unwrap_or_default();
    let db = cx.open_state_db()?;
    let notes = db.open_notes()?;

    let mut attention = Vec::new();
    let mut working = Vec::new();
    for task in db.list_tasks()? {
        let section = classify(task.state, notes.contains_key(&task.id));
        let target = match section {
            Section::Attention => &mut attention,
            Section::Working => &mut working,
            Section::Hidden => continue,
        };
        target.push(MenuRow {
            task_id: task.id,
            state: task.state.to_string(),
            workflow: task.workflow,
            title: task.title,
        });
    }

    Ok(MenuModel {
        availability: match (lock.running, degraded.is_empty()) {
            (false, _) => Availability::Down,
            (true, true) => Availability::Ok,
            (true, false) => Availability::Degraded,
        },
        attention_count: attention.len(),
        attention,
        working,
        degraded,
        error: None,
    })
}

/// Absolute path of the running binary, for the `bash=` parameters.
///
/// **The `PATH` a SwiftBar plugin runs with is not predictable**, so a bare
/// `totsuka` would work from a terminal and break once SwiftBar is the one
/// launching it. Measured on this machine it held Homebrew and the mise shims
/// but *not* `/usr/local/bin` — the shims come from `.zshenv`, which every
/// zsh reads, while `/usr/local/bin` is added by `/etc/zprofile`'s
/// `path_helper`, which only a login shell runs. And it depends on how
/// SwiftBar itself was started: with `launchctl getenv PATH` unset, an app
/// launched by launchd gets `/usr/bin:/bin:/usr/sbin:/sbin` and nothing else.
/// `current_exe` sidesteps the whole question. If it is unavailable there is
/// nothing better to fall back to than the name.
fn binary_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "totsuka".to_string())
}

/// Make externally-authored text safe to place in a SwiftBar line.
///
/// **Three steps, and the order is the whole point** — see the module docs for
/// the two layers of SwiftBar syntax this is defending against:
///
/// 1. [`safe`] turns terminal control characters into their visible escaped
///    form (#280/#297). On its own this is **not** enough: the `\n` it writes
///    for a newline is something SwiftBar turns back into a line break.
/// 2. Every backslash is doubled — including the ones step 1 just produced.
///    This is what makes step 1 survive contact with SwiftBar.
/// 3. `|` is escaped, because it is what separates a row's text from its
///    parameters. Written pre-doubled, so the operator reads `\u{7c}`. The
///    escaped spelling is deliberate: the same escape-don't-strip rule as
///    `safe` — a deleted character is one the operator cannot see was ever
///    there.
///
/// Step 2 was missing in the first shipped version, which is how a title with
/// a newline in it split one row into three. Removing it again would reopen
/// that: `every_backslash_reaches_swiftbar_doubled` fails if you do.
fn menu_text(text: &str) -> String {
    // 1. Control characters become their visible escaped form (#280/#297).
    //    This is also where a real newline turns into the two characters
    //    `\` and `n` — which, on its own, is *not* enough (step 2).
    let visible = safe(text);
    // 2. **Double every backslash.** SwiftBar processes backslash escapes in a
    //    row's text itself: measured on SwiftBar 2.1.1, a `\n` in the output
    //    is rendered as a real line break and `\u{7c}` prints as `u{7c}` with
    //    the backslash eaten. So step 1's escaping is undone by the very thing
    //    it was meant to protect — a title with a newline in it split one row
    //    into three, complete with a forged `---` separator. Doubling makes
    //    SwiftBar print one literal backslash and stop there.
    let doubled = visible.replace('\\', "\\\\");
    // 3. `|` separates a row's text from its parameters, so it must never
    //    reach SwiftBar as itself. Written pre-doubled, so what the operator
    //    sees is `\u{7c}` — escape-not-strip, as everywhere else.
    doubled.replace('|', "\\\\u{7c}")
}

/// Elide `title` to [`TITLE_BUDGET`] characters *before* escaping, so a cut
/// can never land inside an escape sequence produced by [`menu_text`].
fn elide(title: &str) -> String {
    let mut chars = title.chars();
    let head: String = chars.by_ref().take(TITLE_BUDGET).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// The glyph shown next to a task row.
fn row_glyph(state: &str) -> &'static str {
    match state {
        "verifying" => "🔍",
        "waiting_input" => "⏸",
        "escalated" => "🚨",
        "pending" => "◇",
        "queued" => "⛔",
        _ => "▶",
    }
}

/// Render one task row, with `totsuka focus <id>` as its click action.
fn render_row(row: &MenuRow, binary: &str) -> String {
    format!(
        "{glyph} #{id} {state}  {workflow} — {title} | bash=\"{binary}\" param1=focus param2={id} terminal=false {RENDER_AS_TEXT}\n",
        glyph = row_glyph(&row.state),
        id = row.task_id,
        state = menu_text(&row.state),
        workflow = menu_text(&row.workflow),
        title = menu_text(&elide(&row.title)),
        binary = menu_text(binary),
    )
}

/// Serialize the model as a SwiftBar plugin's stdout.
fn render_swiftbar(model: &MenuModel, binary: &str) -> String {
    let mut out = String::new();

    // Menu-bar title: glyph always, count only when there is one.
    out.push_str(model.availability.glyph());
    if model.attention_count > 0 {
        out.push_str(&format!(" {}", model.attention_count));
    }
    out.push('\n');
    out.push_str("---\n");

    match &model.error {
        Some(error) => out.push_str(&format!("{} | {RENDER_AS_TEXT}\n", menu_text(error))),
        None => out.push_str(match model.availability {
            Availability::Ok => "totsuka: running\n",
            Availability::Degraded => "totsuka: running, degraded\n",
            Availability::Down => "totsuka: not running\n",
        }),
    }

    // Ahead of the task sections: when both are present, the degradation is
    // why the tasks are not moving.
    for reason in &model.degraded {
        out.push_str(&format!("• {} | {RENDER_AS_TEXT}\n", menu_text(reason)));
    }

    if !model.attention.is_empty() {
        out.push_str("---\n");
        out.push_str("Needs you\n");
        for row in &model.attention {
            out.push_str(&render_row(row, binary));
        }
    }
    if !model.working.is_empty() {
        out.push_str("---\n");
        out.push_str("Working\n");
        for row in &model.working {
            out.push_str(&render_row(row, binary));
        }
    }

    out.push_str("---\n");
    let binary = menu_text(binary);
    out.push_str(&format!(
        "Open logs | bash=\"{binary}\" param1=logs terminal=true\n"
    ));
    out.push_str(&format!(
        "Run doctor | bash=\"{binary}\" param1=doctor terminal=true\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(task_id: i64, state: &str, title: &str) -> MenuRow {
        MenuRow {
            task_id,
            state: state.to_string(),
            workflow: "implement".to_string(),
            title: title.to_string(),
        }
    }

    fn model(attention: Vec<MenuRow>, working: Vec<MenuRow>) -> MenuModel {
        MenuModel {
            availability: Availability::Ok,
            attention_count: attention.len(),
            attention,
            working,
            degraded: Vec::new(),
            error: None,
        }
    }

    fn degraded_model(reasons: Vec<&str>) -> MenuModel {
        MenuModel {
            availability: Availability::Degraded,
            attention_count: 0,
            attention: Vec::new(),
            working: Vec::new(),
            degraded: reasons.into_iter().map(str::to_string).collect(),
            error: None,
        }
    }

    /// The case the warning glyph exists for: the process is alive and looks
    /// healthy, but it cannot do its whole job.
    #[test]
    fn a_degraded_run_gets_its_own_glyph_and_lists_why() {
        let out = render_swiftbar(
            &degraded_model(vec![
                "the hook receiver could not bind /x → nothing completes",
            ]),
            "/usr/local/bin/totsuka",
        );
        assert_eq!(out.lines().next(), Some("⚠"), "{out}");
        assert!(out.contains("totsuka: running, degraded"), "{out}");
        assert!(out.contains("could not bind /x"), "{out}");
    }

    /// A degradation reason is prose assembled by core, but it still reaches a
    /// SwiftBar line — so it goes through the same escaping as a title.
    #[test]
    fn a_degradation_reason_is_escaped_too() {
        let out = render_swiftbar(
            &degraded_model(vec!["broken | bash=/bin/sh param1=-c"]),
            "/usr/local/bin/totsuka",
        );
        let line = out
            .lines()
            .find(|l| l.contains("broken"))
            .expect("the reason is rendered");
        // One `|` — the separator we emit before our own parameters. The
        // injected one is escaped and stays in the text half.
        assert_eq!(line.matches('|').count(), 1, "{line}");
        let (text, params) = line.split_once('|').expect("one separator");
        assert!(text.contains("\\u{7c}"), "{line}");
        assert_eq!(params.trim(), RENDER_AS_TEXT);
    }

    /// The 要対応 set is exactly five states — the badge's whole contract.
    /// Spelled out rather than derived so a change to `classify` has to change
    /// this list too.
    #[test]
    fn attention_is_exactly_the_five_stuck_states() {
        for state in [
            TaskState::Pending,
            TaskState::WaitingInput,
            TaskState::Verifying,
            TaskState::Escalated,
        ] {
            assert_eq!(classify(state, false), Section::Attention, "{state}");
        }
        assert_eq!(classify(TaskState::Queued, true), Section::Attention);
    }

    /// A queued task with no recorded reason is next in line, not stuck.
    #[test]
    fn queued_without_a_wait_reason_is_hidden() {
        assert_eq!(classify(TaskState::Queued, false), Section::Hidden);
    }

    /// Terminal states must never reach the badge: `list_tasks` returns every
    /// task ever ingested, so counting them would make it grow forever.
    #[test]
    fn terminal_states_are_never_counted() {
        for state in [
            TaskState::Done,
            TaskState::Failed,
            TaskState::Cancelled,
            TaskState::Skipped,
        ] {
            assert_eq!(classify(state, false), Section::Hidden, "{state}");
            assert_eq!(classify(state, true), Section::Hidden, "{state} with note");
        }
    }

    #[test]
    fn in_flight_states_are_working() {
        for state in [
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::Publishing,
        ] {
            assert_eq!(classify(state, false), Section::Working, "{state}");
        }
    }

    /// Exactly what `menu_text` emits, pinned by equality rather than by
    /// `contains` — a substring check passes on both the doubled and the
    /// single-backslash form, so it cannot tell the fix from the bug it
    /// replaced. **Measured against SwiftBar 2.1.1**: it un-doubles `\\` to
    /// one backslash, turns a lone `\n` into a real line break, and drops the
    /// backslash of an escape it does not know (`\u{7c}` printed as `u{7c}`).
    /// So the doubling is what makes step 1's escaping survive at all.
    #[test]
    fn every_backslash_reaches_swiftbar_doubled() {
        // A real newline: `safe` makes it `\n`, and the doubling is what stops
        // SwiftBar turning that back into a line break. Before this, one task
        // title split a row into three, forged `---` separator included.
        assert_eq!(menu_text("a\nb"), r"a\\nb");
        // A backslash the author typed themselves.
        assert_eq!(menu_text(r"a\b"), r"a\\b");
        // `|` is written pre-doubled so the operator reads `\u{7c}`.
        assert_eq!(menu_text("a|b"), r"a\\u{7c}b");
        // Ordinary text is untouched — escaping must not become its own way of
        // mangling the content.
        assert_eq!(menu_text("リポジトリ選択のバグ"), "リポジトリ選択のバグ");
    }

    /// The property behind the equalities above: nothing reaches SwiftBar with
    /// an odd run of backslashes, because an odd run is exactly what leaves it
    /// an escape to act on.
    #[test]
    fn no_odd_run_of_backslashes_survives() {
        for title in [
            "a\nb",
            "a\tb",
            r"a\b",
            r"a\\b",
            r"a\\\b",
            "a|b",
            "a\u{1b}[31mred\u{1b}[0m",
            "plain",
        ] {
            let rendered = menu_text(title);
            let mut run = 0usize;
            for c in rendered.chars() {
                if c == '\\' {
                    run += 1;
                } else {
                    assert_eq!(
                        run % 2,
                        0,
                        "odd backslash run in {rendered:?} (from {title:?})"
                    );
                    run = 0;
                }
            }
            assert_eq!(run % 2, 0, "odd trailing backslash run in {rendered:?}");
        }
    }

    /// **The third layer, and the one escaping cannot reach.** `symbolize` and
    /// `emojize` default to true upstream, so SwiftBar swaps a `:name:` in a
    /// row's text for a glyph. Measured on 2.1.1: a title containing
    /// `:checkmark.seal.fill:` had those 21 written characters **replaced by an
    /// SF Symbol image**. There is no spelling that survives as text, so every
    /// row carrying externally-authored text must repeat the parameters.
    #[test]
    fn every_row_with_external_text_switches_symbol_expansion_off() {
        let m = MenuModel {
            availability: Availability::Degraded,
            attention_count: 1,
            attention: vec![row(1, "verifying", "a :mushroom: title")],
            working: vec![row(2, "running", "another :sun.max: title")],
            degraded: vec!["plugin `:sparkles:` is down".to_string()],
            error: None,
        };
        let out = render_swiftbar(&m, "/usr/local/bin/totsuka");
        for line in out
            .lines()
            .filter(|l| l.contains("title") || l.starts_with('•'))
        {
            assert!(
                line.contains(RENDER_AS_TEXT),
                "a row carrying source-controlled text must disable symbol \
                 expansion: {line}"
            );
        }
        // The failure row too — its message embeds paths from the environment.
        let failed = render_swiftbar(
            &MenuModel::unavailable("no state database at /x/:sun.max:/y".to_string()),
            "/usr/local/bin/totsuka",
        );
        assert!(
            failed
                .lines()
                .any(|l| l.contains("no state database") && l.contains(RENDER_AS_TEXT)),
            "{failed}"
        );
    }

    /// The reason this rendering lives in Rust: a `|` in a source-controlled
    /// title must not be able to append parameters to the row.
    #[test]
    fn a_pipe_in_a_title_cannot_add_parameters() {
        let m = model(
            vec![row(7, "verifying", "innocent | bash=/bin/sh param1=-c")],
            vec![],
        );
        let out = render_swiftbar(&m, "/usr/local/bin/totsuka");
        let task_line = out
            .lines()
            .find(|l| l.contains("#7"))
            .expect("the task row is rendered");
        // Exactly one `|` — the one we emitted ourselves. The injected text
        // survives (escape, don't strip: a deleted character is one the
        // operator cannot see was ever there) but lands entirely in the
        // *text* half of the row, so SwiftBar never parses it as parameters.
        assert_eq!(task_line.matches('|').count(), 1, "{task_line}");
        assert!(task_line.contains("\\u{7c}"), "{task_line}");
        let (text, params) = task_line.split_once('|').expect("one separator");
        assert!(text.contains("bash=/bin/sh"), "{task_line}");
        assert_eq!(
            params.trim(),
            format!(
                "bash=\"/usr/local/bin/totsuka\" param1=focus param2=7 terminal=false {RENDER_AS_TEXT}"
            )
        );
    }

    /// A newline in a title must not become a second menu row.
    #[test]
    fn a_newline_in_a_title_cannot_add_a_row() {
        let m = model(
            vec![row(
                8,
                "escalated",
                "line one\n---\nInjected | bash=/bin/sh",
            )],
            vec![],
        );
        let out = render_swiftbar(&m, "/usr/local/bin/totsuka");
        assert_eq!(
            out.lines().filter(|l| l.contains("Injected")).count(),
            1,
            "the injected text stays inside the one row: {out}"
        );
        assert!(!out.lines().any(|l| l.trim() == "Injected"), "{out}");
    }

    /// The binary path is interpolated into `bash=` too, so it goes through
    /// the same escaping.
    #[test]
    fn the_binary_path_is_escaped_as_well() {
        let m = model(vec![row(9, "verifying", "t")], vec![]);
        let out = render_swiftbar(&m, "/opt/wei|rd/totsuka");
        for line in out.lines().filter(|l| l.contains("bash=")) {
            assert_eq!(line.matches('|').count(), 1, "{line}");
        }
    }

    /// Eliding happens before escaping, so a cut can never split an escape.
    #[test]
    fn a_long_title_is_elided_at_a_char_boundary() {
        let title = "あ".repeat(TITLE_BUDGET + 20);
        let elided = elide(&title);
        assert_eq!(elided.chars().count(), TITLE_BUDGET + 1);
        assert!(elided.ends_with('…'));
        assert_eq!(elide("short"), "short");
    }

    /// Zero attention means no number at all — an idle menu bar shows one
    /// glyph and nothing else.
    #[test]
    fn an_idle_menu_bar_shows_only_the_glyph() {
        let out = render_swiftbar(&model(vec![], vec![]), "/usr/local/bin/totsuka");
        assert_eq!(out.lines().next(), Some("○"));
        assert!(!out.contains("Needs you"), "{out}");
        assert!(!out.contains("Working"), "{out}");
    }

    #[test]
    fn the_badge_carries_the_attention_count() {
        let m = model(
            vec![row(1, "verifying", "a"), row(2, "escalated", "b")],
            vec![row(3, "running", "c")],
        );
        let out = render_swiftbar(&m, "/usr/local/bin/totsuka");
        assert_eq!(out.lines().next(), Some("○ 2"), "working is not counted");
    }

    /// A failure is a row, not an exit code.
    #[test]
    fn a_failure_renders_as_a_menu_row() {
        let m = MenuModel::unavailable(
            "state database not found at /x → run `totsuka run` at least once".to_string(),
        );
        let out = render_swiftbar(&m, "/usr/local/bin/totsuka");
        assert_eq!(out.lines().next(), Some("✕"));
        assert!(out.contains("state database not found"), "{out}");
    }

    /// `--json` is byte-exact: the model keeps the title as it was stored,
    /// because the machine reading it is not a terminal (#280).
    #[test]
    fn the_model_keeps_titles_verbatim() {
        let m = model(
            vec![row(1, "verifying", "raw | title\nwith newline")],
            vec![],
        );
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(
            json["attention"][0]["title"],
            serde_json::json!("raw | title\nwith newline")
        );
        assert_eq!(json["attention_count"], serde_json::json!(1));
        assert_eq!(json["availability"], serde_json::json!("ok"));
        assert!(
            json.get("error").is_none(),
            "no error key when there is none"
        );
    }
}
