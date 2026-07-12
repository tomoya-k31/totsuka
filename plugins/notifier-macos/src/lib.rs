//! `notifier-macos`: a totsuka notifier plugin that delivers Orchestrator events
//! (`waiting_input` / `done` / `failed` / `pending`) to the macOS Notification
//! Center (F-90〜F-93).
//!
//! `notify` is a JSON-RPC **notification** (no response). Delivery is
//! **fire-and-forget** (F-93): a send failure is logged and never propagated, so
//! a missing/broken notifier cannot affect task execution. Events can be filtered
//! per workflow × event kind (F-92).
//!
//! v1 sends via `osascript -e 'display notification …'` (simplest signing /
//! permission story). The send path is behind a [`sender::NotificationSender`]
//! trait so a future UNUserNotificationCenter backend can drop in.

pub mod config;
pub mod error;
pub mod sender;
pub mod server;
