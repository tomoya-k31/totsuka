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

/// Marks a plugin directory as a **bundled** install: the store holds this
/// file and nothing else, and the files come from the running binary's tree
/// (#611). Its presence is the whole record — deliberately not a path, see
/// [`Origin::Bundled`].
const BUNDLED_MARKER: &str = "bundled";

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
    /// A plugin is declared as a bundled install, but this build ships no
    /// bundled tree (#611) — the usual cause is a `cargo install` build, which
    /// has no `libexec` beside it.
    ///
    /// Deliberately distinct from "not installed": the declaration is intact
    /// and it is the tree that is absent, so the repair is to install from a
    /// directory, not to re-run the same `--bundled` command.
    #[error(
        "plugin `{name}` was installed from the bundled tree, but this build has none → install it from a directory (`totsuka plugin install <dir>`) or use a build that ships plugins"
    )]
    NoBundledTree {
        /// Plugin name.
        name: String,
    },
}

/// A validated, ready-to-commit installation (no side effects yet).
#[derive(Debug, Clone)]
pub struct InstallPlan {
    /// Plugin name (from the manifest).
    pub name: String,
    /// Path of the `plugin.toml` this plan was built from. [`PluginStore::commit_install`]
    /// copies exactly this file rather than re-deriving it from a source
    /// directory, so the manifest and the binary may live in different trees —
    /// which is what installing straight out of a Cargo checkout needs
    /// (manifest under `plugins/<pkg>/`, binary under `target/<profile>/`).
    pub manifest_path: PathBuf,
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

/// Where an installed plugin's files actually are (#611).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Copied into the store: a directory install or `--from-source`.
    ///
    /// A copy is a snapshot, so it is the operator's to keep — an upgrade of
    /// the CLI must never silently replace a build someone chose.
    Copied,
    /// Provided by the running binary's bundled tree, resolved fresh on every
    /// use (#611, [ADR-0067]).
    ///
    /// **Nothing is copied and no path is stored**, because the bundled root
    /// is *computed* from `current_exe` rather than remembered. That is what
    /// makes the pointer survive an upgrade: Homebrew deletes the old Cellar
    /// directory, so any path recorded at install time would dangle, while a
    /// path derived from the binary that is running now always names that
    /// binary's own tree.
    ///
    /// [ADR-0067]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0067-bundled-plugin-resolution.md
    Bundled,
}

/// Manages installed plugin binaries under a root directory.
#[derive(Debug, Clone)]
pub struct PluginStore {
    root: PathBuf,
    /// The running binary's bundled plugins tree, when it has one.
    ///
    /// Passed in rather than discovered here: locating it means reasoning
    /// about the executable's own layout, which belongs to the CLI. `None` is
    /// the normal state for a `cargo install` build, and for every test that
    /// does not exercise bundled plugins.
    bundled_root: Option<PathBuf>,
}

impl PluginStore {
    /// A store rooted at `root` (usually `$XDG_DATA_HOME/totsuka/plugins`),
    /// with no bundled tree.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            bundled_root: None,
        }
    }

    /// The same store, told where the running binary's bundled plugins live.
    #[must_use]
    pub fn with_bundled_root(mut self, bundled_root: Option<PathBuf>) -> Self {
        self.bundled_root = bundled_root;
        self
    }

    /// The store's own directory for a plugin: where a copy lives, and where
    /// the marker of a bundled install lives.
    ///
    /// **This is not always where the plugin's files are** — use
    /// [`resolved_dir`](Self::resolved_dir) for that.
    pub fn plugin_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// How `name` was installed. An uninstalled plugin reads as `Copied`,
    /// which is what every path below already handles as "no files".
    pub fn origin_of(&self, name: &str) -> Origin {
        if self.plugin_dir(name).join(BUNDLED_MARKER).is_file() {
            Origin::Bundled
        } else {
            Origin::Copied
        }
    }

    /// The directory holding `name`'s manifest and binary right now.
    ///
    /// For a bundled install this is recomputed on every call, so a CLI
    /// upgrade takes effect with nothing to re-run.
    pub fn resolved_dir(&self, name: &str) -> Result<PathBuf, StoreError> {
        validate_plugin_name(name)?;
        match self.origin_of(name) {
            Origin::Copied => Ok(self.plugin_dir(name)),
            Origin::Bundled => match &self.bundled_root {
                Some(root) => Ok(root.join(name)),
                None => Err(StoreError::NoBundledTree {
                    name: name.to_string(),
                }),
            },
        }
    }

    /// Whether a plugin is installed. An unsafe name is treated as not
    /// installed (never probes outside the plugins root).
    ///
    /// A bundled install counts as installed on the strength of its marker
    /// alone. Requiring the bundled tree to resolve as well would report a
    /// `cargo install` build as "not installed" and send the operator to
    /// `plugin install`, which is the wrong repair — the declaration is
    /// intact and it is the tree that is missing. That case is named by
    /// [`resolved_dir`](Self::resolved_dir) instead, where the message can say
    /// so.
    pub fn is_installed(&self, name: &str) -> bool {
        validate_plugin_name(name).is_ok()
            && (self.plugin_dir(name).join(MANIFEST_FILE).is_file()
                || self.plugin_dir(name).join(BUNDLED_MARKER).is_file())
    }

    /// Validate a source directory and compute its checksum, **without** writing
    /// anything. The source must contain `plugin.toml` and a binary named after
    /// the manifest's `name`. Rejects protocol-incompatible plugins (F-54).
    pub fn prepare_install(&self, source_dir: &Path) -> Result<InstallPlan, StoreError> {
        self.prepare_install_from(&source_dir.join(MANIFEST_FILE), source_dir)
    }

    /// Same validation as [`PluginStore::prepare_install`], but the manifest and
    /// the binary are located separately.
    ///
    /// The binary is always `binary_dir/<manifest name>` — the naming
    /// invariant is the same one `plugin install` and the store itself rely on
    /// (see `ai-docs/decisions/adr-0027-plugin-artifact-naming.md`), so splitting
    /// the two paths does not weaken it. This exists so a plugin can be
    /// installed out of a Cargo checkout without first assembling a staging
    /// directory: the manifest stays in `plugins/<pkg>/` and the binary is read
    /// straight from `target/<profile>/`.
    pub fn prepare_install_from(
        &self,
        manifest_path: &Path,
        binary_dir: &Path,
    ) -> Result<InstallPlan, StoreError> {
        if !manifest_path.is_file() {
            return Err(StoreError::NoManifest(
                manifest_path
                    .parent()
                    .unwrap_or(manifest_path)
                    .to_path_buf(),
            ));
        }
        let manifest = Manifest::from_toml_str(&fs::read_to_string(manifest_path)?)?;

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

        let binary = binary_dir.join(&manifest.name);
        if !binary.is_file() {
            return Err(StoreError::NoBinary {
                binary: manifest.name.clone(),
                dir: binary_dir.to_path_buf(),
            });
        }
        // Canonicalize so `binary` is genuinely absolute (the source may be a
        // relative path).
        let binary = binary.canonicalize()?;
        let checksum = sha256_hex(&binary)?;

        Ok(InstallPlan {
            name: manifest.name.clone(),
            manifest_path: manifest_path.to_path_buf(),
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
        // what makes it atomic. (`list` never sees it either way — it walks the
        // plugins root, not each plugin's own directory — so the leading dot is
        // only there to mark it as not-a-plugin to a human reading the dir.)
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
        fs::copy(&plan.manifest_path, dir.join(MANIFEST_FILE))?;
        Ok(())
    }

    /// Record `name` as a bundled install: write the marker and copy nothing
    /// (#611).
    ///
    /// Any previous copy is removed first, so switching a `--from-source`
    /// build back to bundled leaves no stale binary that a later change could
    /// accidentally resolve to.
    pub fn commit_link_bundled(&self, name: &str) -> Result<(), StoreError> {
        validate_plugin_name(name)?;
        let dir = self.plugin_dir(name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(BUNDLED_MARKER), b"")?;
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
        if !self.is_installed(name) {
            return Ok(None);
        }
        let path = self.resolved_dir(name)?.join(MANIFEST_FILE);
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
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // A bundled entry holds only the marker, so its manifest is read
            // from the running binary's tree — which is what makes `list`
            // report the version that would actually launch.
            let manifest_path = match self.resolved_dir(&name) {
                Ok(dir) => dir.join(MANIFEST_FILE),
                // A bundled entry with no tree to resolve: skip it here rather
                // than fail the whole listing. `plugin list` is a diagnostic,
                // and the actionable error belongs where the plugin is used.
                Err(_) => continue,
            };
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
    use std::os::unix::fs::MetadataExt;
    use std::process::Command;

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
        fake_source(&src, "github", ">=0.6.0, <0.7", b"#!/bin/sh\necho hi\n");

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
        use std::os::unix::fs::PermissionsExt;

        let base = scratch("store_reinstall");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.6.0, <0.7", b"#!/bin/sh\nexit 0\n");

        let store = PluginStore::new(base.join("plugins"));
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();

        let installed = store.plugin_dir("github").join("github");
        let first = fs::metadata(&installed).unwrap().ino();
        assert_eq!(
            Command::new(&installed).status().unwrap().code(),
            Some(0),
            "the freshly installed binary runs"
        );

        // A new build of the same plugin, installed over the old one.
        fake_source(&src, "github", ">=0.6.0, <0.7", b"#!/bin/sh\nexit 1\n");
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();

        // The property #292 is really about: the binary that replaced a
        // previously *executed* one still launches. (A shell script carries no
        // code signature, so this cannot reproduce the macOS SIGKILL itself —
        // it pins the replacement strategy, and the inode assertion below is
        // what actually rules the in-place rewrite out.)
        assert_eq!(
            Command::new(&installed).status().unwrap().code(),
            Some(1),
            "the reinstalled binary runs, and it is the new build"
        );

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

    /// Staging exists so a failed *upgrade* is a no-op rather than a
    /// half-replaced binary: the plugin that was working before the attempt
    /// must still be the one installed, still runnable, and no staging file
    /// may be left for the next attempt to trip over.
    #[test]
    fn a_failed_upgrade_leaves_the_working_plugin_intact() {
        let base = scratch("store_failed_commit");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.6.0, <0.7", b"#!/bin/sh\nexit 7\n");

        let store = PluginStore::new(base.join("plugins"));
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();
        let installed = store.plugin_dir("github").join("github");
        let before = fs::metadata(&installed).unwrap().ino();

        // A second install that cannot complete: the source binary disappears
        // between `prepare` and `commit`, so the copy is the step that fails.
        fake_source(&src, "github", ">=0.6.0, <0.7", b"#!/bin/sh\nexit 8\n");
        let mut plan = store.prepare_install(&src).unwrap();
        plan.binary = src.join("gone");
        assert!(store.commit_install(&plan).is_err());

        assert!(
            !store.plugin_dir("github").join(".github.incoming").exists(),
            "a failed copy left its staging file behind"
        );
        assert_eq!(
            fs::metadata(&installed).unwrap().ino(),
            before,
            "the failed upgrade replaced the installed binary anyway"
        );
        assert_eq!(
            Command::new(&installed).status().unwrap().code(),
            Some(7),
            "the previously working plugin still runs, and is still the old build"
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
        fake_source(&src, "x", ">=0.6.0, <0.7", b"bin");
        fs::write(
            src.join(MANIFEST_FILE),
            "name = \"../evil\"\nkind = \"notifier\"\nversion = \"0.1.0\"\nprotocol_version = \">=0.6.0, <0.7\"\n",
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
        fake_source(&src, "github", ">=0.6.0, <0.7", b"bin");
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
            "name = \"x\"\nkind = \"notifier\"\nversion = \"0.1.0\"\nprotocol_version = \">=0.6.0, <0.7\"\n",
        )
        .unwrap();
        assert!(matches!(
            store.prepare_install(&no_bin).unwrap_err(),
            StoreError::NoBinary { .. }
        ));
    }

    // ---- bundled installs resolve at runtime (#611) --------------------

    /// A bundled install stores **only the marker**, and both the manifest and
    /// the binary resolve into whatever tree the store is currently told about.
    #[test]
    fn a_bundled_install_copies_nothing_and_resolves_into_the_bundled_tree() {
        let base = scratch("bundled-link");
        let bundled = base.join("libexec/plugins/github");
        fs::create_dir_all(&bundled).unwrap();
        fake_source(&bundled, "github", ">=0.1.0", b"v1");

        let store = PluginStore::new(base.join("plugins"))
            .with_bundled_root(Some(base.join("libexec/plugins")));
        store.commit_link_bundled("github").unwrap();

        assert!(store.is_installed("github"));
        assert_eq!(store.origin_of("github"), Origin::Bundled);
        // Nothing was copied: the store's own directory holds the marker alone.
        let own = store.plugin_dir("github");
        assert!(own.join(BUNDLED_MARKER).is_file());
        assert!(
            !own.join("github").exists(),
            "the binary must not be copied"
        );
        assert!(
            !own.join(MANIFEST_FILE).exists(),
            "the manifest must not be copied"
        );
        // ...and resolution points into the bundled tree.
        assert_eq!(store.resolved_dir("github").unwrap(), bundled);
        assert!(store.manifest_of("github").unwrap().is_some());
    }

    /// The point of the whole design: replacing the bundled tree's contents —
    /// what a CLI upgrade does — changes what launches, with nothing re-run.
    #[test]
    fn an_upgraded_bundled_tree_is_picked_up_with_no_reinstall() {
        let base = scratch("bundled-upgrade");
        let old_tree = base.join("Cellar/0.1.0/libexec/plugins");
        fs::create_dir_all(old_tree.join("github")).unwrap();
        fake_source(&old_tree.join("github"), "github", ">=0.1.0", b"old");

        let store = PluginStore::new(base.join("plugins"));
        let at_old = store.clone().with_bundled_root(Some(old_tree.clone()));
        at_old.commit_link_bundled("github").unwrap();
        assert_eq!(
            at_old.resolved_dir("github").unwrap(),
            old_tree.join("github")
        );

        // The upgrade: a *new* tree at a different path, and the old one gone —
        // exactly what Homebrew does when it prunes the previous Cellar.
        let new_tree = base.join("Cellar/0.2.0/libexec/plugins");
        fs::create_dir_all(new_tree.join("github")).unwrap();
        fake_source(&new_tree.join("github"), "github", ">=0.1.0", b"new");
        fs::remove_dir_all(base.join("Cellar/0.1.0")).unwrap();

        // The same store record, a newer binary: the marker held no path, so
        // there is nothing stale to correct.
        let at_new = store.with_bundled_root(Some(new_tree.clone()));
        assert_eq!(
            at_new.resolved_dir("github").unwrap(),
            new_tree.join("github")
        );
        assert_eq!(
            fs::read(new_tree.join("github/github")).unwrap(),
            b"new".to_vec()
        );
        assert!(at_new.is_installed("github"));
    }

    /// A build with no bundled tree (`cargo install`) must say *that*, not
    /// "not installed" — the declaration is intact and the tree is what is
    /// missing, so the repair is different.
    #[test]
    fn a_bundled_install_without_a_tree_is_a_named_error_not_a_missing_plugin() {
        let base = scratch("bundled-no-tree");
        let store = PluginStore::new(base.join("plugins"));
        store.commit_link_bundled("github").unwrap();

        assert!(
            store.is_installed("github"),
            "the declaration is still there"
        );
        let err = store.resolved_dir("github").unwrap_err();
        assert!(
            matches!(err, StoreError::NoBundledTree { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("install it from a directory"));
        // `list` is a diagnostic and must not fail wholesale over one entry.
        assert!(store.list().unwrap().is_empty());
    }

    /// A copied install is a snapshot the operator chose. Nothing about a
    /// bundled tree may change where it resolves.
    #[test]
    fn a_copied_install_is_unaffected_by_the_bundled_tree() {
        let base = scratch("copied-unaffected");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.1.0", b"mine");
        let bundled = base.join("libexec/plugins/github");
        fs::create_dir_all(&bundled).unwrap();
        fake_source(&bundled, "github", ">=0.1.0", b"theirs");

        let store = PluginStore::new(base.join("plugins"))
            .with_bundled_root(Some(base.join("libexec/plugins")));
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();

        assert_eq!(store.origin_of("github"), Origin::Copied);
        assert_eq!(
            store.resolved_dir("github").unwrap(),
            store.plugin_dir("github")
        );
        assert_eq!(
            fs::read(store.plugin_dir("github").join("github")).unwrap(),
            b"mine".to_vec()
        );
    }

    /// Switching a copy over to bundled must not leave the old binary behind.
    #[test]
    fn linking_over_a_copy_removes_the_copy() {
        let base = scratch("relink");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        fake_source(&src, "github", ">=0.1.0", b"copied");
        let bundled = base.join("libexec/plugins/github");
        fs::create_dir_all(&bundled).unwrap();
        fake_source(&bundled, "github", ">=0.1.0", b"bundled");

        let store = PluginStore::new(base.join("plugins"))
            .with_bundled_root(Some(base.join("libexec/plugins")));
        let plan = store.prepare_install(&src).unwrap();
        store.commit_install(&plan).unwrap();
        assert!(store.plugin_dir("github").join("github").is_file());

        store.commit_link_bundled("github").unwrap();
        assert_eq!(store.origin_of("github"), Origin::Bundled);
        assert!(
            !store.plugin_dir("github").join("github").exists(),
            "the previous copy must be gone, not shadowed"
        );
        assert!(!store.plugin_dir("github").join(MANIFEST_FILE).exists());
    }

    /// `uninstall` clears a bundled record and must never touch the tree it
    /// pointed at — that tree belongs to the installed CLI.
    #[test]
    fn uninstalling_a_bundled_plugin_leaves_the_bundled_tree_alone() {
        let base = scratch("bundled-uninstall");
        let bundled = base.join("libexec/plugins/github");
        fs::create_dir_all(&bundled).unwrap();
        fake_source(&bundled, "github", ">=0.1.0", b"v1");

        let store = PluginStore::new(base.join("plugins"))
            .with_bundled_root(Some(base.join("libexec/plugins")));
        store.commit_link_bundled("github").unwrap();
        assert!(store.uninstall("github").unwrap());

        assert!(!store.is_installed("github"));
        assert_eq!(store.origin_of("github"), Origin::Copied, "no marker left");
        assert!(
            bundled.join("github").is_file(),
            "the CLI's own tree survives"
        );
    }
}
