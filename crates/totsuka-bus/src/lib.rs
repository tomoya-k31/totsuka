#![forbid(unsafe_code)]
pub mod pgmq;
pub use pgmq::{create_queue, delete, read_one, send_json, BusError, PgmqMessage};
