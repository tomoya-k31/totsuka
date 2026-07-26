//! Plugin store: install/uninstall/list plugin binaries on disk (F-52, F-55,
//! F-56).
//!
//! "install = the binary exists" and "enabled = declared in config" are kept
//! separate (F-56): this module only manages the on-disk binaries under
//! `$XDG_DATA_HOME/totsuka/plugins/{name}/`; the `enabled` flag lives in
//! `config.toml` and is edited via [`config::edit`](crate::config::edit).
//!
//! Installing is a two-step, side-effect-free-until-confirmed flow so the CLI
//! can show the source and SHA-256 checksum and require confirmation before
//! anything is written (§5.4): [`PluginStore::prepare_install`] validates and
//! hashes, [`PluginStore::commit_install`] copies.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use plugin_protocol::manifest::{Manifest, PluginKind};
use plugin_protocol::version;
use sha2::{Digest, Sha256};

/// Filename of the manifest inside a plugin directory.
const MANIFEST_FILE: &str = "plugin.toml";

/// Errors from plugin-store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem error.
    #[error("plugin store io error: {0}")]
    Io(#[from] std::io::Error),
    /// The source has no `plugin.toml`.
    #[error("no plugin.toml found in {0} → the source must contain a manifest")]
    NoManifest(PathBuf),
    /// The manifest failed to parse.
    #[error("invalid plugin.toml: {0}")]
    Manifest(#[from] plugin_protocol::manifest::ManifestError),
    /// The declared binary is missing from the source.
    #[error("plugin binary `{binary}` not found in {dir} → expected a file named after the plugin")]
    NoBinary {
        /// Expected binary filename.
        binary: String,
        /// Source directory.
        dir: PathBuf,
    },
    /// The plugin name is not a single safe path component (traversal guard).
    #[error(
        "invalid plugin name `{0}` → names must not be empty or contain path separators or `..`"
    )]
    InvalidName(String),
    /// The plugin's protocol range excludes this Orchestrator (F-54).
    #[error(
        "plugin `{name}` is protocol-incompatible: it supports `{req}` but the orchestrator is {have} → use a compatible plugin build"
    )]
    ProtocolIncompatible {
        /// Plugin name.
        name: String,
        /// Declared protocol requirement.
        req: String,
        /// Orchestrator protocol version.
        have: String,
    },
}

/// A validated, ready-to-commit installation (no side effects yet).
#[derive(Debug, Clone)]
pub struct InstallPlan {
    /// Plugin name (from the manifest).
    pub name: String,
    /// Where the plugin is being installed from (for display).
    pub source: PathBuf,
    /// Canonical (absolute) path of the source binary.
    pub binary: PathBuf,
    /// Hex-encoded SHA-256 of the binary (§5.4).
    pub checksum: String,
    /// The parsed manifest.
    pub manifest: Manifest,
}

/// Summary of an installed plugin (for `plugin list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    /// Plugin name.
    pub name: String,
    /// Plugin kind as a stable snake_case string (`task_source`, ...).
    pub kind: String,
    /// Plugin version.
    pub version: String,
    /// Supported Orchestrator protocol range.
    pub protocol_version: String,
}

/// Manages installed plugin binaries under a root directory.
#[derive(Debug, Clone)]
pub struct PluginStore {
    root: PathBuf,
}

impl PluginStore {
    /// A store rooted at `root` (usually `$XDG_DATA_HOME/totsuka/plugins`).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The install directory for a plugin.
    pub fn plugin_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Whether a plugin is installed (its manifest exists). An unsafe name is
    /// treated as not installed (never probes outside the plugins root).
    pub fn is_installed(&self, name: &str) -> bool {
        validate_plugin_name(name).is_ok() && self.plugin_dir(name).join(MANIFEST_FILE).is_file()
    }

    /// Validate a source directory and compute its checksum, **without** writing
    /// anything. The source must contain `plugin.toml` and a binary named after
    /// the manifest's `name`. Rejects protocol-incompatible plugins (F-54).
    pub fn prepare_install(&self, source_dir: &Path) -> Result<InstallPlan, StoreError> {
        let manifest_path = source_dir.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            return Err(StoreError::NoManifest(source_dir.to_path_buf()));
        }
        let manifest = Manifest::from_toml_str(&fs::read_to_string(&manifest_path)?)?;

        // A crafted manifest name must not escape the plugins root on commit.
        validate_plugin_name(&manifest.name)?;

        // F-54: refuse incompatible plugins at install time.
        let orchestrator = version::protocol_version();
        if !manifest.is_compatible_with(&orchestrator) {
            return Err(StoreError::ProtocolIncompatible {
                name: manifest.name.clone(),
                req: manifest.protocol_version.to_string(),
                have: orchestrator.to_string(),
            });
        }

        let binary = source_dir.join(&manifest.name);
        if !binary.is_file() {
            return Err(StoreError::NoBinary {
                binary: manifest.name.clone(),
                dir: source_dir.to_path_buf(),
            });
        }
        // Canonicalize so `binary` is genuinely absolute (the source may be a
        // relative path).
        let binary = binary.canonicalize()?;
        let checksum = sha256_hex(&binary)?;

        Ok(InstallPlan {
            name: manifest.name.clone(),
            source: source_dir.to_path_buf(),
            binary,
            checksum,
            manifest,
        })
    }

    /// Copy the binary and manifest into the store (the confirmed step).
    ///
    /// The binary is staged next to its destination and moved into place, never
    /// written over the live path. macOS caches a code-signature verdict per
    /// vnode, so rewriting an installed binary in place leaves the cached
    /// signature describing bytes that are no longer there and the *next* exec
    /// dies on `SIGKILL` — with no output, no error, and a `doctor` that can
    /// only report "crashed or exited" (#292). `rename` gives the path a fresh
    /// inode, so no stale verdict can attach to it.
    pub fn commit_install(&self, plan: &InstallPlan) -> Result<(), StoreError> {
        let dir = self.plugin_dir(&plan.name);
        fs::create_dir_all(&dir)?;
        let dest_binary = dir.join(&plan.name);
        // Staged inside `dir` so the rename stays on one filesystem, which is
        // what makes it atomic. The leading dot keeps a leftover out of the way
        // of `list`, which only walks directories.
        let staged = dir.join(format!(".{}.incoming", plan.name));
        // Every failure before the rename must take the staging file with it —
        // including a `fs::copy` that dies partway (a full disk leaves a
        // truncated binary behind), which is exactly when a leftover is most
        // likely and least welcome.
        let staged_result = fs::copy(&plan.binary, &staged)
            .map_err(StoreError::from)
            // Before the rename: the destination must never be observable in a
            // non-executable state.
            .and_then(|_| set_executable(&staged))
            .and_then(|_| fs::rename(&staged, &dest_binary).map_err(StoreError::from));
        if let Err(e) = staged_result {
            let _ = fs::remove_file(&staged);
            return Err(e);
        }
        fs::copy(plan.source.join(MANIFEST_FILE), dir.join(MANIFEST_FILE))?;
        Ok(())
    }

    /// The plugin kind of an installed plugin as a config string, if installed.
    pub fn kind_str_of(&self, name: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .manifest_of(name)?
            .map(|m| kind_string(m.kind).to_string()))
    }

    /// Remove an installed plugin. Returns whether it existed.
    pub fn uninstall(&self, name: &str) -> Result<bool, StoreError> {
        validate_plugin_name(name)?;
        let dir = self.plugin_dir(name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Read one installed plugin's manifest.
    pub fn manifest_of(&self, name: &str) -> Result<Option<Manifest>, StoreError> {
        validate_plugin_name(name)?;
        let path = self.plugin_dir(name).join(MANIFEST_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(Manifest::from_toml_str(&fs::read_to_string(path)?)?))
    }

    /// List installed plugins (directories containing a valid manifest).
    pub fn list(&self) -> Result<Vec<InstalledPlugin>, StoreError> {
        let mut plugins = Vec::new();
        if !self.root.exists() {
            return Ok(plugins);
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest_path = entry.path().join(MANIFEST_FILE);
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = Manifest::from_toml_str(&fs::read_to_string(&manifest_path)?)?;
            plugins.push(InstalledPlugin {
                name: manifest.name,
                kind: kind_string(manifest.kind).to_string(),
                version: manifest.version.to_string(),
                protocol_version: manifest.protocol_version.to_string(),
            });
        }
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(plugins)
    }
}

/// The stable snake_case string for a plugin kind (matches manifest/config).
fn kind_string(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::TaskSource => "task_source",
        PluginKind::AgentIde => "agent_ide",
        PluginKind::Notifier => "notifier",
    }
}

/// Reject plugin names that are not a single safe path component, so a name
/// (from an untrusted manifest or CLI arg) cannot escape the plugins root.
fn validate_plugin_name(name: &str) -> Result<(), StoreError> {
    let safe = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0');
    if safe {
        Ok(())
    } else {
        Err(StoreError::InvalidName(name.to_string()))
    }
}

/// Compute the hex-encoded SHA-256 of a file, streaming it (binaries can be
/// large) rather than reading it all into memory.
fn sha256_hex(path: &Path) -> Result<String, StoreError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Make a file executable (owner rwx, others rx) on Unix; no-op elsewhere.
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create a fake plugin source dir (manifest + binary) and return its path.
    fn fake_source(dir: &Path, name: &str, protocol_req: &str, binary_contents: &[u8]) {
        fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                r#"
name = "{name}"
kind = "task_source"
version = "0.1.0"
protocol_version = "{protocol_req}"
"#
            ),
        )
        .unwrap();
        fs::write(dir.join(name), binary_contents).unwrap();
    }

    #[test]
    fn prepare_then_commit_installs_binary_and_manifest() {
        let base = scratch("store_install");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.1.6, <0.4", b"#!/bin/sh\necho hi\n");

        let store = PluginStore::new(base.join("plugins"));
        assert!(!store.is_installed("github"));

        // prepare has no side effects.
        let plan = store.prepare_install(&src).unwrap();
        assert_eq!(plan.name, "github");
        assert_eq!(plan.checksum.len(), 64); // hex SHA-256
        assert!(!store.is_installed("github"), "prepare must not install");

        // commit places the files.
        store.commit_install(&plan).unwrap();
        assert!(store.is_installed("github"));
        assert!(store.plugin_dir("github").join("github").is_file());

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "github");

        assert!(store.uninstall("github").unwrap());
        assert!(!store.is_installed("github"));
        assert!(
            !store.uninstall("github").unwrap(),
            "second uninstall is a no-op"
        );
    }

    /// #292: upgrading a plugin is a *re*install, and the binary it replaces
    /// may be one the OS has already executed. Replacing it in place is what
    /// made macOS `SIGKILL` the next launch, so the property to hold is that
    /// the destination gets a new inode rather than new bytes.
    #[test]
    fn reinstalling_replaces_the_binary_instead_of_rewriting_it() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let base = scratch("store_reinstall");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.1.6, <0.4", b"#!/bin/sh\nexit 0\n");

        let store = PluginStore::new(base.join("plugins"));
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();

        let installed = store.plugin_dir("github").join("github");
        let first = fs::metadata(&installed).unwrap().ino();

        // A new build of the same plugin, installed over the old one.
        fake_source(&src, "github", ">=0.1.6, <0.4", b"#!/bin/sh\nexit 1\n");
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();

        let second = fs::metadata(&installed).unwrap().ino();
        assert_ne!(
            first, second,
            "the reinstall rewrote the live binary in place instead of replacing it (#292)"
        );
        assert_eq!(
            fs::read(&installed).unwrap(),
            b"#!/bin/sh\nexit 1\n",
            "the new build is what ended up installed"
        );
        assert_eq!(
            fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o755,
            "the replacement is executable"
        );
        assert!(
            !store.plugin_dir("github").join(".github.incoming").exists(),
            "no staging file is left behind"
        );
        // The staging file must not be mistaken for an installed plugin.
        assert_eq!(store.list().unwrap().len(), 1);
    }

    /// A failed install must not leave the staging file behind: it would sit
    /// next to a working plugin forever, and the next attempt would have to
    /// overwrite whatever partial binary it holds.
    #[test]
    fn a_failed_commit_leaves_no_staging_file() {
        let base = scratch("store_failed_commit");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.1.6, <0.4", b"#!/bin/sh\nexit 0\n");

        let store = PluginStore::new(base.join("plugins"));
        let mut plan = store.prepare_install(&src).unwrap();
        // The source binary disappears between `prepare` and `commit` — the
        // copy is then the step that fails.
        plan.binary = src.join("gone");

        assert!(store.commit_install(&plan).is_err());
        assert!(
            !store.plugin_dir("github").join(".github.incoming").exists(),
            "a failed copy left its staging file behind"
        );
    }

    #[test]
    fn incompatible_protocol_is_rejected_on_prepare() {
        let base = scratch("store_incompat");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "future", ">=1.0.0", b"bin");

        let store = PluginStore::new(base.join("plugins"));
        let err = store.prepare_install(&src).unwrap_err();
        assert!(
            matches!(err, StoreError::ProtocolIncompatible { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn traversal_names_are_rejected() {
        let base = scratch("store_traversal");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        // A manifest name that would escape the plugins root on commit.
        fake_source(&src, "x", ">=0.1.6, <0.4", b"bin");
        fs::write(
            src.join(MANIFEST_FILE),
            "name = \"../evil\"\nkind = \"notifier\"\nversion = \"0.1.0\"\nprotocol_version = \">=0.1.0, <0.4\"\n",
        )
        .unwrap();
        let store = PluginStore::new(base.join("plugins"));
        assert!(matches!(
            store.prepare_install(&src).unwrap_err(),
            StoreError::InvalidName(_)
        ));
        // And via the uninstall arg.
        assert!(matches!(
            store.uninstall("../../etc").unwrap_err(),
            StoreError::InvalidName(_)
        ));
        // Read paths are guarded too (no probing outside the root).
        assert!(!store.is_installed("../../etc/passwd"));
        assert!(matches!(
            store.manifest_of("../../etc/passwd").unwrap_err(),
            StoreError::InvalidName(_)
        ));
    }

    #[test]
    fn listed_kind_is_snake_case() {
        let base = scratch("store_kind");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.1.6, <0.4", b"bin");
        let store = PluginStore::new(base.join("plugins"));
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();
        assert_eq!(store.list().unwrap()[0].kind, "task_source");
        assert_eq!(
            store.kind_str_of("github").unwrap().as_deref(),
            Some("task_source")
        );
    }

    #[test]
    fn missing_manifest_or_binary_is_an_error() {
        let base = scratch("store_missing");
        let empty = base.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let store = PluginStore::new(base.join("plugins"));
        assert!(matches!(
            store.prepare_install(&empty).unwrap_err(),
            StoreError::NoManifest(_)
        ));

        // Manifest present but no binary.
        let no_bin = base.join("nobin");
        fs::create_dir_all(&no_bin).unwrap();
        fs::write(
            no_bin.join(MANIFEST_FILE),
            "name = \"x\"\nkind = \"notifier\"\nversion = \"0.1.0\"\nprotocol_version = \">=0.1.0, <0.4\"\n",
        )
        .unwrap();
        assert!(matches!(
            store.prepare_install(&no_bin).unwrap_err(),
            StoreError::NoBinary { .. }
        ));
    }
}
