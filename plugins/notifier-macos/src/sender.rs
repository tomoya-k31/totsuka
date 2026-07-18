//! The notification delivery backends.
//!
//! [`Server`](crate::server::Server) is generic over [`NotificationSender`] so
//! delivery is tested against a recording fake, while production shells out to
//! one of two backends selected by `[notifier].backend`:
//!
//! - [`OsascriptSender`] — AppleScript `display notification`. Always
//!   available, but macOS gives the notification to osascript's owner (Script
//!   Editor), so a click can never reach the task's pane.
//! - [`TerminalNotifierSender`] — `terminal-notifier` with `-execute`
//!   (click runs `totsuka focus <task_id>`) and `-activate` (click brings the
//!   GUI terminal to the front): the click-to-focus backend (F-94, ADR-0005).
//!   Falls back to osascript per send when the binary is missing, so
//!   notifications keep flowing on a machine without terminal-notifier.
//!
//! A future UNUserNotificationCenter backend can implement the same trait.

use std::future::Future;

use crate::error::NotifierError;

/// A formatted notification: what the user sees, plus the task correlation a
/// clickable backend needs (`-group` dedup key and the `{task_id}` for the
/// click command).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Headline (bold line).
    pub title: String,
    /// Secondary line.
    pub subtitle: String,
    /// Body text.
    pub body: String,
    /// The related task id, if any (drives `-group` / `-execute`).
    pub task_id: Option<String>,
}

/// Delivers a [`Notice`] to the user.
pub trait NotificationSender: Clone + Send + Sync + 'static {
    /// Post the notice. On the fire-and-forget path (F-93) the error is only
    /// logged, but it is returned so `config/validate` can surface it.
    fn send(&self, notice: Notice) -> impl Future<Output = Result<(), NotifierError>> + Send;

    /// A non-visible check that the backend can run (F-59), used by
    /// `config/validate` so validation doesn't post a user-visible notification.
    fn probe(&self) -> impl Future<Output = Result<(), NotifierError>> + Send;
}

/// The production backend: AppleScript `display notification` via `osascript`.
#[derive(Clone)]
pub struct OsascriptSender {
    bin: String,
}

impl OsascriptSender {
    /// A sender invoking `bin` (usually `osascript`).
    pub fn new(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }
}

impl NotificationSender for OsascriptSender {
    async fn send(&self, notice: Notice) -> Result<(), NotifierError> {
        self.run(osascript_args(&notice)).await
    }

    async fn probe(&self) -> Result<(), NotifierError> {
        // Runs osascript without displaying anything — confirms the tool is
        // present and executable (the common misconfiguration).
        self.run(vec!["-e".into(), "return \"ok\"".into()]).await
    }
}

impl OsascriptSender {
    /// Run `osascript` with `args`, mapping a spawn/exit failure to an error.
    async fn run(&self, args: Vec<String>) -> Result<(), NotifierError> {
        let output = tokio::process::Command::new(&self.bin)
            .args(&args)
            .output()
            .await
            .map_err(|source| NotifierError::Spawn {
                bin: self.bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(NotifierError::Failed {
                bin: self.bin.clone(),
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(300)
                    .collect(),
            });
        }
        Ok(())
    }
}

/// Build the `osascript` arguments for a notice. The user strings are passed as
/// `argv` (via `on run argv`), never interpolated into the AppleScript source,
/// so a title/body containing quotes or `"` cannot break out or inject script.
fn osascript_args(notice: &Notice) -> Vec<String> {
    vec![
        "-e".into(),
        "on run argv".into(),
        "-e".into(),
        "display notification (item 1 of argv) with title (item 2 of argv) subtitle (item 3 of argv)"
            .into(),
        "-e".into(),
        "end run".into(),
        // `--` ends option parsing so a title/body starting with `-` is treated
        // as an argv string, not an osascript flag.
        "--".into(),
        notice.body.clone(),
        notice.title.clone(),
        notice.subtitle.clone(),
    ]
}

/// The click-to-focus backend: `terminal-notifier` (F-94, ADR-0005).
///
/// The click behaviour rides on two flags: `-activate <bundle-id>` brings the
/// GUI terminal to the front natively, and `-execute '<cmd>'` runs the
/// `totsuka focus` command that asks the orchestrator to focus the pane.
/// `-sender` is deliberately **never** used: on macOS Sequoia 15.x+ combining
/// it with `-activate` breaks click-to-focus. `-group totsuka-<task_id>`
/// coalesces repeat notifications per task.
#[derive(Clone)]
pub struct TerminalNotifierSender {
    bin: String,
    activate_bundle_id: Option<String>,
    click_command: String,
    /// Per-send fallback when `bin` is missing: notifications must keep
    /// flowing (un-clickable beats undelivered) on a machine without
    /// terminal-notifier.
    fallback: OsascriptSender,
}

impl TerminalNotifierSender {
    /// A sender invoking `bin`, falling back to `fallback` when it is absent.
    pub fn new(
        bin: impl Into<String>,
        activate_bundle_id: Option<String>,
        click_command: impl Into<String>,
        fallback: OsascriptSender,
    ) -> Self {
        Self {
            bin: bin.into(),
            activate_bundle_id,
            click_command: click_command.into(),
            fallback,
        }
    }

    /// Run `terminal-notifier` with `args`.
    async fn run(&self, args: Vec<String>) -> Result<(), NotifierError> {
        let output = tokio::process::Command::new(&self.bin)
            .args(&args)
            .output()
            .await
            .map_err(|source| NotifierError::Spawn {
                bin: self.bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(NotifierError::Failed {
                bin: self.bin.clone(),
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(300)
                    .collect(),
            });
        }
        Ok(())
    }
}

impl NotificationSender for TerminalNotifierSender {
    async fn send(&self, notice: Notice) -> Result<(), NotifierError> {
        let args = terminal_notifier_args(&notice, &self.activate_bundle_id, &self.click_command);
        match self.run(args).await {
            // The binary is not installed: degrade to osascript so the
            // notification still lands (click-to-focus is simply lost).
            Err(NotifierError::Spawn { ref source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                tracing::warn!(
                    bin = %self.bin,
                    "terminal-notifier not found → falling back to osascript (notification not clickable); \
                     install terminal-notifier or set backend = \"osascript\""
                );
                self.fallback.send(notice).await
            }
            other => other,
        }
    }

    async fn probe(&self) -> Result<(), NotifierError> {
        // `-help` exits 0 without posting anything. Deliberately NOT falling
        // back: `config/validate` must surface a configured-but-missing
        // terminal-notifier as an actionable problem, while `send` degrades.
        self.run(vec!["-help".into()]).await
    }
}

/// The production sender: one of the two backends, selected by
/// `[notifier].backend` (see [`crate::config::Backend`]).
#[derive(Clone)]
pub enum BackendSender {
    /// AppleScript `display notification` (not clickable).
    Osascript(OsascriptSender),
    /// `terminal-notifier` click-to-focus (F-94).
    TerminalNotifier(Box<TerminalNotifierSender>),
}

impl BackendSender {
    /// Build the sender `config` selects.
    pub fn from_config(config: &crate::config::NotifierConfig) -> Self {
        match config.backend {
            crate::config::Backend::Osascript => {
                Self::Osascript(OsascriptSender::new(config.osascript_bin()))
            }
            crate::config::Backend::TerminalNotifier => {
                Self::TerminalNotifier(Box::new(TerminalNotifierSender::new(
                    config.terminal_notifier_bin(),
                    config.activate_bundle_id.clone(),
                    config.click_command.clone(),
                    OsascriptSender::new(config.osascript_bin()),
                )))
            }
        }
    }
}

impl NotificationSender for BackendSender {
    async fn send(&self, notice: Notice) -> Result<(), NotifierError> {
        match self {
            Self::Osascript(sender) => sender.send(notice).await,
            Self::TerminalNotifier(sender) => sender.send(notice).await,
        }
    }

    async fn probe(&self) -> Result<(), NotifierError> {
        match self {
            Self::Osascript(sender) => sender.probe().await,
            Self::TerminalNotifier(sender) => sender.probe().await,
        }
    }
}

/// Build the `terminal-notifier` argv for a notice. Every user string is its
/// own argv element (never interpolated into a shell line by us); the one
/// shell string terminal-notifier itself executes (`-execute`) gets the task
/// id **single-quoted** via [`shell_single_quote`], so an id can never inject
/// shell syntax.
fn terminal_notifier_args(
    notice: &Notice,
    activate_bundle_id: &Option<String>,
    click_command: &str,
) -> Vec<String> {
    let mut args = vec![
        "-title".into(),
        notice.title.clone(),
        "-subtitle".into(),
        notice.subtitle.clone(),
        "-message".into(),
        notice.body.clone(),
        // Coalesce repeats per task (parallel tasks keep distinct
        // notifications, each carrying its own click target).
        "-group".into(),
        match &notice.task_id {
            Some(id) => format!("totsuka-{id}"),
            None => "totsuka".into(),
        },
    ];
    if let Some(bundle_id) = activate_bundle_id {
        args.push("-activate".into());
        args.push(bundle_id.clone());
    }
    if let Some(task_id) = &notice.task_id
        && !click_command.is_empty()
    {
        args.push("-execute".into());
        args.push(click_command.replace("{task_id}", &shell_single_quote(task_id)));
    }
    args
}

/// `s` as a single-quoted POSIX shell word (`'` → `'\''`): whatever the id
/// contains, the shell sees one literal argument.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osascript_args_pass_strings_as_argv_not_source() {
        // A body with quotes must appear verbatim as an argv element, never
        // spliced into the AppleScript program (injection safety).
        let notice = Notice {
            title: "T".into(),
            subtitle: "S".into(),
            body: "say \"hi\" & do bad".into(),
            task_id: Some("42".into()),
        };
        let args = osascript_args(&notice);
        // The program uses argv references, not the literal user text.
        let program = &args[3];
        assert!(program.contains("item 1 of argv"));
        assert!(
            !program.contains("say \"hi\""),
            "user text not in the script source"
        );
        // argv order is body, title, subtitle (the trailing three args).
        assert_eq!(args[args.len() - 3], "say \"hi\" & do bad");
        assert_eq!(args[args.len() - 2], "T");
        assert_eq!(args[args.len() - 1], "S");
    }

    fn notice(task_id: Option<&str>) -> Notice {
        Notice {
            title: "🔔 入力待ち".into(),
            subtitle: "Fix bug · impl".into(),
            body: "question?".into(),
            task_id: task_id.map(str::to_string),
        }
    }

    #[test]
    fn terminal_notifier_args_carry_click_activate_and_group() {
        let args = terminal_notifier_args(
            &notice(Some("42")),
            &Some("org.alacritty".into()),
            "totsuka focus {task_id}",
        );
        let joined = args.join("\u{0}");
        assert!(joined.contains("-title\u{0}🔔 入力待ち"));
        assert!(joined.contains("-group\u{0}totsuka-42"));
        assert!(joined.contains("-activate\u{0}org.alacritty"));
        assert!(joined.contains("-execute\u{0}totsuka focus '42'"));
        // Sequoia 15.x+: -sender combined with -activate breaks click-to-focus.
        assert!(!args.iter().any(|a| a == "-sender"), "never pass -sender");
    }

    #[test]
    fn terminal_notifier_args_degrade_without_task_or_bundle() {
        // No task id → app-level group, no -execute (nothing to focus).
        let args = terminal_notifier_args(&notice(None), &None, "totsuka focus {task_id}");
        let joined = args.join("\u{0}");
        assert!(joined.contains("-group\u{0}totsuka"));
        assert!(!args.iter().any(|a| a == "-execute"));
        assert!(!args.iter().any(|a| a == "-activate"));

        // An empty click_command disables -execute even with a task id.
        let args = terminal_notifier_args(&notice(Some("7")), &None, "");
        assert!(!args.iter().any(|a| a == "-execute"));
    }

    #[test]
    fn click_command_task_id_is_shell_quoted_against_injection() {
        // A hostile id cannot break out of the -execute shell string: the
        // whole value stays inside single quotes.
        let args = terminal_notifier_args(
            &notice(Some("42'; rm -rf ~; echo '")),
            &None,
            "totsuka focus {task_id}",
        );
        let execute = args
            .iter()
            .position(|a| a == "-execute")
            .map(|i| args[i + 1].clone())
            .expect("an -execute value");
        assert_eq!(execute, r#"totsuka focus '42'\''; rm -rf ~; echo '\'''"#);
    }

    #[test]
    fn shell_single_quote_neutralizes_quotes() {
        assert_eq!(shell_single_quote("42"), "'42'");
        assert_eq!(shell_single_quote("a'b"), r"'a'\''b'");
    }
}
