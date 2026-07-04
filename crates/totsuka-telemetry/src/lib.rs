#![forbid(unsafe_code)]
pub mod log;
pub use log::init_tracing;

pub mod disconnect;
pub use disconnect::is_benign_disconnect;

pub mod http;
pub mod request_id;
pub use http::{router as health_router, HealthState};

pub mod notify;
pub use notify::{
    default_dedup_ttl, default_routing, LogSink, Notifier, NotifyPayload, NotifySink, SinkError,
    SinkId, SlackSink,
};
