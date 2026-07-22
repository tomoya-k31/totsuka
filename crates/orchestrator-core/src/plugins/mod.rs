//! Plugin management: the on-disk plugin store (F-52/F-55/F-56).
//!
//! Enable/disable is not here — that is a `config.toml` edit
//! ([`config::edit`](crate::config::edit)) since it is declarative (F-56).

pub mod spec;
pub mod store;

pub use spec::{DEFAULT_PLUGIN_TIMEOUT, SpecError, plugin_init_config, plugin_spec};
pub use store::{InstallPlan, InstalledPlugin, PluginStore, StoreError};
