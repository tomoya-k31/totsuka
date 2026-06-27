#![forbid(unsafe_code)]
pub mod clock;
pub mod error;

pub use clock::{Clock, MockClock, SystemClock};
pub use error::{Error, Result};
