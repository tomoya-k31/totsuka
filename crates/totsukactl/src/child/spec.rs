use crate::paths::Paths;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ChildSpec {
    pub name: String,
    pub bin_path: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub log_path: PathBuf,
}

pub const RUST_BINS_IN_ORDER: &[&str] = &[
    "agent-adapter",
    "orchestrator",
    "github-watcher",
    "qa-service",
];

pub fn specs_from_config(
    cfg: &totsuka_config::Config,
    paths: &Paths,
    exe_dir: &Path,
    config_path: &str,
) -> Vec<ChildSpec> {
    RUST_BINS_IN_ORDER
        .iter()
        .map(|name| ChildSpec {
            name: (*name).into(),
            bin_path: exe_dir.join(name),
            args: vec!["--config".into(), config_path.into()],
            env: vec![
                ("TOTSUKA_CONFIG".into(), config_path.into()),
                ("RUST_LOG".into(), cfg.totsuka.log_level.clone()),
            ],
            log_path: paths.child_log(name),
        })
        .collect()
}
