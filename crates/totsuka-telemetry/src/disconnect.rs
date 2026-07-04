//! Classify connection-teardown errors from UDS/HTTP serving.
//!
//! On macOS, a client that closes its end right after reading the response
//! makes hyper's final socket shutdown fail with `ENOTCONN` (os error 57).
//! The request/response completed fine — logging that at WARN floods the
//! logs with one line per health probe. Callers use this to demote such
//! errors to DEBUG while keeping real failures at WARN.

use std::error::Error;
use std::io::ErrorKind;

/// True when the error chain contains an I/O error (first one found wins)
/// that just means "the peer already hung up": NotConnected / BrokenPipe /
/// ConnectionReset / ConnectionAborted.
pub fn is_benign_disconnect(e: &(dyn Error + 'static)) -> bool {
    let mut cur: Option<&(dyn Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                ErrorKind::NotConnected
                    | ErrorKind::BrokenPipe
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
            );
        }
        cur = err.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    /// Stand-in for hyper::Error(Shutdown, io::Error): wraps an io::Error
    /// one level deep in the source chain.
    #[derive(Debug)]
    struct Wrapper(std::io::Error);
    impl fmt::Display for Wrapper {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "wrapper: {}", self.0)
        }
    }
    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn not_connected_nested_is_benign() {
        let e = Wrapper(std::io::Error::new(ErrorKind::NotConnected, "os error 57"));
        assert!(is_benign_disconnect(&e));
    }

    #[test]
    fn direct_broken_pipe_is_benign() {
        let e = std::io::Error::new(ErrorKind::BrokenPipe, "EPIPE");
        assert!(is_benign_disconnect(&e));
    }

    #[test]
    fn other_io_error_is_not_benign() {
        let e = Wrapper(std::io::Error::other("disk on fire"));
        assert!(!is_benign_disconnect(&e));
    }

    #[test]
    fn non_io_error_is_not_benign() {
        #[derive(Debug)]
        struct Plain;
        impl fmt::Display for Plain {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "plain")
            }
        }
        impl Error for Plain {}
        assert!(!is_benign_disconnect(&Plain));
    }
}
