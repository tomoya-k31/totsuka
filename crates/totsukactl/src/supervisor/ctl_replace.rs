//! Helper for `main_loop`'s control-dispatch loop: when `ctl_rx.recv()` returns
//! `None` (because the sock_api task crashed and dropped all `ControlMsg` senders),
//! the supervisor must NOT interpret that as "shutdown requested". Instead, we
//! log loudly and swap the closed receiver for a fresh one whose sender we
//! intentionally hold forever — the new receiver will never yield `None`, so
//! `tokio::select!` keeps polling the signal arms (SIGTERM/SIGINT).
//!
//! Once the new receiver is in place, the supervisor's CLI IPC is dead until
//! restart, but the children remain healthy under signal-driven shutdown.

use crate::sock_api::ControlMsg;
use tokio::sync::mpsc;

pub async fn replace_closed_ctl_rx(_old: mpsc::Receiver<ControlMsg>) -> mpsc::Receiver<ControlMsg> {
    // Dropping `_old` is intentional: the sender side is already gone, so the
    // receiver is no longer useful.
    tracing::error!(
        "sock_api control channel closed unexpectedly; supervisor continuing on signals only \
         (CLI commands via supervisor.sock will not work until restart)"
    );
    let (sender, new_rx) = mpsc::channel::<ControlMsg>(1);
    tokio::spawn(async move {
        // Holding `sender` for the lifetime of the supervisor keeps `new_rx`
        // open (never yields None). `pending::<()>()` is the standard "never
        // resolves" future.
        let _hold = sender;
        std::future::pending::<()>().await;
    });
    new_rx
}
