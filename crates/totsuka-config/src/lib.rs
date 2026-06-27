#![forbid(unsafe_code)]
pub mod expand;
pub mod schema;
pub use expand::{expand_vars, ExpandError};
pub use schema::Config;
