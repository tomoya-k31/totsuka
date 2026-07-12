//! XDG Base Directory-compliant path resolution (F-60, §5.6).
//!
//! totsuka resolves its config/data/state/cache/runtime directories from the
//! XDG environment variables, falling back to the XDG-specified defaults under
//! `$HOME`. We deliberately do **not** use the `dirs` crate's macOS defaults
//! (`~/Library/...`): the spec requires XDG semantics on macOS too, to keep the
//! future Linux port cheap.
//!
//! Every resolved directory is suffixed with the application name (`totsuka`),
//! e.g. `$XDG_CONFIG_HOME/totsuka`.

use std::path::{Path, PathBuf};

/// Application directory name appended to every base directory.
pub const APP_NAME: &str = "totsuka";

/// Errors that can occur while resolving paths.
#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    /// `HOME` is required to compute XDG fallbacks and was not set.
    #[error("HOME environment variable is not set")]
    NoHome,
}

/// Resolved, application-scoped base directories.
///
/// Construct with [`Paths::from_system`] in production or [`Paths::from_env`]
/// in tests to inject a fake environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
}

impl Paths {
    /// Resolve paths from the real process environment.
    pub fn from_system() -> Result<Self, PathsError> {
        Self::from_env(|key| std::env::var(key).ok())
    }

    /// Resolve paths from an injected environment lookup.
    ///
    /// `env` mirrors [`std::env::var`]: it returns `Some(value)` when the
    /// variable is set to valid UTF-8, and `None` otherwise. This is the seam
    /// used by unit tests to exercise both the "XDG set" and "XDG unset" paths
    /// without touching global process state.
    pub fn from_env<F>(env: F) -> Result<Self, PathsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let home = env("HOME")
            .filter(|h| !h.is_empty())
            .ok_or(PathsError::NoHome)?;
        let home = PathBuf::from(home);

        let base = |var: &str, default: &str| -> PathBuf {
            xdg_base(env(var), &home, default).join(APP_NAME)
        };

        // XDG_RUNTIME_DIR has no standard fallback; when unset we reuse the
        // state directory so socket/lock paths remain deterministic (F-74).
        let state = base("XDG_STATE_HOME", ".local/state");
        let runtime = match xdg_base_opt(env("XDG_RUNTIME_DIR")) {
            Some(dir) => dir.join(APP_NAME),
            None => state.clone(),
        };

        Ok(Self {
            config: base("XDG_CONFIG_HOME", ".config"),
            data: base("XDG_DATA_HOME", ".local/share"),
            state,
            cache: base("XDG_CACHE_HOME", ".cache"),
            runtime,
        })
    }

    /// `$XDG_CONFIG_HOME/totsuka` — user configuration (`config.toml`).
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// `$XDG_DATA_HOME/totsuka` — installed plugin binaries and manifests.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// `$XDG_STATE_HOME/totsuka` — state DB, logs, lock file.
    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    /// `$XDG_CACHE_HOME/totsuka` — regenerable caches (e.g. README summaries).
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    /// `$XDG_RUNTIME_DIR/totsuka`, or the state dir when `XDG_RUNTIME_DIR` is
    /// unset — runtime sockets.
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime
    }
}

/// Resolve one XDG base directory: use the env value when it is an absolute
/// path (per the XDG spec, relative values are ignored), else the `$HOME`
/// default.
fn xdg_base(value: Option<String>, home: &Path, default: &str) -> PathBuf {
    xdg_base_opt(value).unwrap_or_else(|| home.join(default))
}

/// Return the env value as a path only if it is set and absolute.
fn xdg_base_opt(value: Option<String>) -> Option<PathBuf> {
    value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn uses_xdg_values_when_set() {
        let paths = Paths::from_env(env_from(&[
            ("HOME", "/home/alice"),
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_DATA_HOME", "/xdg/data"),
            ("XDG_STATE_HOME", "/xdg/state"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
            ("XDG_RUNTIME_DIR", "/xdg/run"),
        ]))
        .unwrap();

        assert_eq!(paths.config_dir(), Path::new("/xdg/config/totsuka"));
        assert_eq!(paths.data_dir(), Path::new("/xdg/data/totsuka"));
        assert_eq!(paths.state_dir(), Path::new("/xdg/state/totsuka"));
        assert_eq!(paths.cache_dir(), Path::new("/xdg/cache/totsuka"));
        assert_eq!(paths.runtime_dir(), Path::new("/xdg/run/totsuka"));
    }

    #[test]
    fn falls_back_to_home_defaults_when_unset() {
        let paths = Paths::from_env(env_from(&[("HOME", "/home/bob")])).unwrap();

        assert_eq!(paths.config_dir(), Path::new("/home/bob/.config/totsuka"));
        assert_eq!(
            paths.data_dir(),
            Path::new("/home/bob/.local/share/totsuka")
        );
        assert_eq!(
            paths.state_dir(),
            Path::new("/home/bob/.local/state/totsuka")
        );
        assert_eq!(paths.cache_dir(), Path::new("/home/bob/.cache/totsuka"));
        // XDG_RUNTIME_DIR unset -> reuse state dir.
        assert_eq!(paths.runtime_dir(), paths.state_dir());
    }

    #[test]
    fn ignores_relative_xdg_values() {
        // The XDG spec says relative paths must be ignored; we fall back.
        let paths = Paths::from_env(env_from(&[
            ("HOME", "/home/carol"),
            ("XDG_CONFIG_HOME", "relative/config"),
            ("XDG_CACHE_HOME", ""),
        ]))
        .unwrap();

        assert_eq!(paths.config_dir(), Path::new("/home/carol/.config/totsuka"));
        assert_eq!(paths.cache_dir(), Path::new("/home/carol/.cache/totsuka"));
    }

    #[test]
    fn missing_home_is_an_error() {
        let err = Paths::from_env(env_from(&[])).unwrap_err();
        assert!(matches!(err, PathsError::NoHome));
    }
}
