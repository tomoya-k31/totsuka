#![forbid(unsafe_code)]
pub mod expand;
pub use expand::{expand_vars, ExpandError};
