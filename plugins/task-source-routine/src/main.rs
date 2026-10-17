//! Binary entrypoint: the SDK stdio runtime over [`Server`].
//!
//! The local time-zone offset is read **before** the tokio runtime starts:
//! `time` refuses to read it once the process has more than one thread
//! (ADR-0097). It is read once, so a daylight-saving change takes effect at
//! the next start.

use task_source_routine::server::Server;

fn main() {
    plugin_sdk::runtime::init_tracing();
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build the tokio runtime")
        .block_on(async move {
            let stdio = plugin_sdk::runtime::stdio();
            let server = Server::new(stdio.submit.clone(), offset);
            plugin_sdk::runtime::serve(plugin_sdk::TaskSourceServer(server), &stdio).await;
        });
}
