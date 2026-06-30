use std::time::Duration;
use tokio::sync::mpsc;
use totsukactl::sock_api::ControlMsg;
use totsukactl::supervisor::replace_closed_ctl_rx;

#[tokio::test]
async fn replaced_rx_blocks_when_old_was_closed() {
    let (tx, rx) = mpsc::channel::<ControlMsg>(1);
    drop(tx); // old channel: receiver would yield None
    let mut new_rx = replace_closed_ctl_rx(rx).await;

    // Old behavior under test: new_rx must NOT yield None within 100 ms.
    let outcome = tokio::time::timeout(Duration::from_millis(100), new_rx.recv()).await;
    assert!(
        outcome.is_err(),
        "new receiver should block; got {outcome:?} instead of timeout"
    );
}

#[tokio::test]
async fn replaced_rx_does_not_panic_on_drop() {
    let (tx, rx) = mpsc::channel::<ControlMsg>(1);
    drop(tx);
    let new_rx = replace_closed_ctl_rx(rx).await;
    drop(new_rx); // exercise the held-sender's task termination path
                  // Sleep briefly to let the held-sender task notice the drop (it never will, but verify no panic).
    tokio::time::sleep(Duration::from_millis(50)).await;
}
