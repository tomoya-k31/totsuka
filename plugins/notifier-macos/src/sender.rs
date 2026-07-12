//! The notification delivery backend.
//!
//! [`Server`](crate::server::Server) is generic over [`NotificationSender`] so
//! delivery is tested against a recording fake, while production shells out to
//! `osascript`. v1 uses AppleScript's `display notification`; a future
//! UNUserNotificationCenter backend can implement the same trait.

use std::future::Future;

use crate::error::NotifierError;

/// A formatted notification: what the user sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Headline (bold line).
    pub title: String,
    /// Secondary line.
    pub subtitle: String,
    /// Body text.
    pub body: String,
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
}
