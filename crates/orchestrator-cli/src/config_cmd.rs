//! `totsuka config ...` — validation, display and editing (F-59/63, §5.1).
//!
//! `validate` runs the offline checks (schema, static references, workflow
//! semantics) and — unless `--offline` — briefly launches each enabled plugin
//! to delegate `config/validate` (F-59). `show` prints the effective files,
//! masking secret-looking values with `--redacted`.
//!
//! `schema` / `get` / `set` / `unset` are the menu bar app's settings window's
//! view of the file (ADR-0109): JSON out, one key per write, comments kept.

use std::collections::{BTreeMap, HashMap};
use std::io;

use clap::Subcommand;
use orchestrator_core::adapters::plugin_host;
use orchestrator_core::config::{self, FindingSeverity};
use orchestrator_core::platform::supplied;
use orchestrator_core::ports::SecretRef;

use orchestrator_core::plugins::{check_workflow_options, plugin_spec};

use crate::common::{CliError, Cx};

/// Config subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Validate config.toml, the Orchestrator's keys and every plugin's.
    Validate {
        /// Skip the online part (launching plugins for `config/validate`).
        #[arg(long)]
        offline: bool,
        /// Read secret values from stdin, as `totsuka run --secrets-stdin`
        /// does (one JSON object on one line; no secret store is opened).
        /// Without it, a plugin whose settings use a `secret:` reference is
        /// not validated online.
        #[arg(long)]
        secrets_stdin: bool,
    },
    /// Print the effective configuration files.
    Show {
        /// Mask values of secret-looking keys (token/key/secret/password).
        #[arg(long)]
        redacted: bool,
    },
    /// Print the JSON Schema of config.toml: the core keys plus each installed
    /// plugin's table (asked through `config/schema`, without `initialize`).
    Schema,
    /// Print the config file's path and contents as JSON.
    Get,
    /// Set one key, keeping the file's comments and layout.
    Set {
        /// Key path as a JSON Pointer: `/log/level`, `/repositories/0/tool`;
        /// `-` appends to an array, `~1` is `/` inside a key.
        path: String,
        /// The value as JSON (`'"debug"'`, `4`, `true`, `{"name":"a"}`).
        value: String,
    },
    /// Remove one key or array element.
    Unset {
        /// Key path as a JSON Pointer, as for `set`.
        path: String,
    },
}

impl ConfigCommand {
    /// Whether errors go out as the JSON envelope: the app-facing commands
    /// always (their output is JSON or nothing), `validate` / `show` never.
    pub fn wants_json(&self) -> bool {
        matches!(
            self,
            Self::Schema | Self::Get | Self::Set { .. } | Self::Unset { .. }
        )
    }
}

/// Dispatch a config subcommand.
pub fn run(cx: &Cx, command: ConfigCommand) -> Result<(), CliError> {
    match command {
        ConfigCommand::Validate {
            offline,
            secrets_stdin,
        } => {
            if secrets_stdin {
                crate::common::install_supplied_secrets()?;
            }
            validate(cx, offline)
        }
        ConfigCommand::Show { redacted } => show(cx, redacted),
        ConfigCommand::Schema => schema(cx),
        ConfigCommand::Get => get(cx),
        ConfigCommand::Set { path, value } => {
            let value: serde_json::Value = serde_json::from_str(&value).map_err(|e| {
                format!("the value is not JSON ({e}) → quote a string as JSON, e.g. '\"debug\"'")
            })?;
            write_edit(cx, |text| config::set_path(text, &path, &value))
        }
        ConfigCommand::Unset { path } => write_edit(cx, |text| config::unset_path(text, &path)),
    }
}

/// `config schema`: the core schema with each installed plugin's table merged
/// in under its name. A plugin that does not declare `config_schema`, or whose
/// answer is unusable, gets `x-raw: true` (edit as TOML) instead of failing the
/// command — one broken plugin must not take the whole settings window down.
///
/// A plugin named like a core key (`log`, `workflows`, …) is left out rather
/// than allowed to replace that key's schema: config validation refuses the
/// name anyway, and the core settings must stay editable meanwhile.
///
/// The keys plugins read on `[[projects]]` / `[[workflows]]` entries go beside
/// the core's, keyed by plugin name, since which apply depends on the entry
/// (ADR-0109 §5): `x-by-source` on a project (its `source`) and on a workflow
/// (the source its `projects` resolve to), `x-by-agent` on a workflow (its
/// `agent`). An unusable one (`$ref`, not an object) is left out, and the
/// reason is reported nowhere — the entry just shows the core's keys. The
/// bundled plugins' own tests are what keep their answers usable.
fn schema(cx: &Cx) -> Result<(), CliError> {
    use orchestrator_core::plugins::plugin_schemas;
    use serde_json::json;

    let mut schema = config::json_schema::core_schema();
    let answers = tokio::runtime::Runtime::new()?.block_on(plugin_schemas(&cx.store()))?;
    let mut entries = EntrySchemas::default();
    let properties = schema["properties"]
        .as_object_mut()
        .expect("the core schema is an object schema");
    for (name, answer) in answers {
        if properties.contains_key(&name) {
            continue;
        }
        entries.collect(&name, &answer);
        let entry = plugin_property(&name, answer);
        properties.insert(name, entry);
    }
    entries.attach(&mut schema);
    crate::common::print_json(&json!({ "config_path": cx.config_path, "schema": schema }))
}

/// Why `schema` cannot be embedded in the merged document, if it cannot.
fn unusable(schema: &serde_json::Value) -> Option<String> {
    // Embedded in a larger document, a local `$ref` would resolve against the
    // wrong root; the protocol asks for inline subschemas.
    if schema.to_string().contains("\"$ref\"") {
        return Some("the schema uses $ref → answer with every subschema inline".into());
    }
    (schema["type"] != "object").then(|| "the answer is not an object schema".into())
}

/// The schema property for one plugin's table: its answer, or `x-raw` (with
/// the reason in `x-schema-error`) when there is no usable answer.
fn plugin_property(
    name: &str,
    answer: orchestrator_core::plugins::PluginSchema,
) -> serde_json::Value {
    use orchestrator_core::plugins::PluginSchema;
    use serde_json::json;

    let category = json!(name);
    let raw = |error: Option<String>| {
        let mut entry = json!({ "type": "object", "x-category": category, "x-raw": true });
        if let Some(error) = error {
            entry["x-schema-error"] = json!(error);
        }
        entry
    };
    match answer {
        PluginSchema::Schema { answer, .. } => match unusable(&answer.schema) {
            Some(error) => raw(Some(error)),
            None => {
                let mut s = answer.schema;
                if s.get("x-category").is_none() {
                    s["x-category"] = category.clone();
                }
                s
            }
        },
        PluginSchema::Undeclared => raw(None),
        PluginSchema::Failed(e) => raw(Some(e)),
    }
}

/// The per-plugin schemas of `[[projects]]` / `[[workflows]]` entries' keys,
/// gathered from the answers and attached to those arrays' item schemas.
#[derive(Default)]
struct EntrySchemas {
    project_by_source: serde_json::Map<String, serde_json::Value>,
    workflow_by_source: serde_json::Map<String, serde_json::Value>,
    workflow_by_agent: serde_json::Map<String, serde_json::Value>,
}

impl EntrySchemas {
    fn collect(&mut self, name: &str, answer: &orchestrator_core::plugins::PluginSchema) {
        use orchestrator_core::plugins::PluginSchema;
        use plugin_protocol::manifest::PluginKind;

        let PluginSchema::Schema { answer, kind } = answer else {
            return;
        };
        let usable = |s: &Option<serde_json::Value>| s.clone().filter(|s| unusable(s).is_none());
        match kind {
            PluginKind::TaskSource => {
                if let Some(s) = usable(&answer.project) {
                    self.project_by_source.insert(name.to_string(), s);
                }
                if let Some(s) = usable(&answer.workflow) {
                    self.workflow_by_source.insert(name.to_string(), s);
                }
            }
            PluginKind::AgentIde => {
                if let Some(s) = usable(&answer.workflow) {
                    self.workflow_by_agent.insert(name.to_string(), s);
                }
            }
            PluginKind::Notifier => {}
        }
    }

    fn attach(self, schema: &mut serde_json::Value) {
        for (array, keyword, map) in [
            ("projects", "x-by-source", self.project_by_source),
            ("workflows", "x-by-source", self.workflow_by_source),
            ("workflows", "x-by-agent", self.workflow_by_agent),
        ] {
            if !map.is_empty() {
                schema["properties"][array]["items"][keyword] = serde_json::Value::Object(map);
            }
        }
    }
}

/// `config get`: the file as written (no `TOTSUKA_*` overrides folded in — the
/// settings window edits the file), or `exists: false` before there is one.
fn get(cx: &Cx) -> Result<(), CliError> {
    let text = read_config_text(&cx.config_path)?;
    let config = match &text {
        Some(text) => serde_json::to_value(
            text.parse::<toml::Table>()
                .map_err(|e| format!("failed to parse TOML: {e}"))?,
        )?,
        None => serde_json::json!({}),
    };
    crate::common::print_json(&serde_json::json!({
        "config_path": cx.config_path,
        "exists": text.is_some(),
        "config": config,
    }))
}

/// The file's text, or `None` when there is none yet.
fn read_config_text(path: &std::path::Path) -> Result<Option<String>, CliError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Apply one edit to the config file and write it back atomically.
///
/// Refused when it turns a file that loaded into one that does not: one key
/// at a time cannot always keep a config *valid* (a new workflow is missing
/// keys until the next write), but it must never be the step that leaves
/// `run` unable to read the file. A file that already failed to load can
/// still be edited — refusing would lock the user out of fixing it.
///
/// Written through a symlink rather than over it (dotfiles are often Stow
/// links) — a dangling one included, so the link is never replaced by a plain
/// file — via a temporary file in the target's directory and a rename. An edit
/// that changes nothing writes nothing (`unset` on a missing file creates no
/// file).
fn write_edit(
    cx: &Cx,
    edit: impl FnOnce(&str) -> Result<String, config::EditError>,
) -> Result<(), CliError> {
    let path = &cx.config_path;
    let before = read_config_text(path)?;
    let after = edit(before.as_deref().unwrap_or(""))?;
    if after == before.as_deref().unwrap_or("") {
        return Ok(());
    }
    let loaded = |text: &str| config::RootConfig::from_toml_str(text);
    if before.as_deref().is_none_or(|t| loaded(t).is_ok())
        && let Err(e) = loaded(&after)
    {
        return Err(format!("{e} → the file was left unchanged").into());
    }
    let target = write_target(path)?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let tmp = target.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let written = (|| {
        std::fs::write(&tmp, &after)?;
        if let Ok(meta) = std::fs::metadata(&target) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

/// The file a write to `path` must land in: the end of its symlink chain,
/// followed even when the last link dangles.
fn write_target(path: &std::path::Path) -> io::Result<std::path::PathBuf> {
    let mut target = path.to_path_buf();
    // A bound, not a policy: a cycle would otherwise loop forever.
    for _ in 0..40 {
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&target)?;
                target = match target.parent() {
                    Some(dir) => dir.join(link),
                    None => link,
                };
            }
            Ok(_) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(target),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!(
        "too many symlinks at {}",
        path.display()
    )))
}

/// Whether validating plugin `name` online would resolve a `secret:`
/// reference: its own table, or — for a task source — the `[llm]` key that
/// `plugin_spec` hands it (the same two places `doctor`'s plugin gate looks).
///
/// Only for an installed plugin: a missing or broken one is left to
/// `plugin_spec`, whose error is the report that matters (Copilot review on
/// #787 — skipping it here turned "not installed" into a passing note).
fn needs_supplied(cx: &Cx, cfg: &config::RootConfig, name: &str) -> bool {
    let Ok(Some(manifest)) = cx.store().manifest_of(name) else {
        return false;
    };
    if cfg.plugin_settings(name).is_some_and(uses_supplied) {
        return true;
    }
    let task_source = manifest.kind == plugin_protocol::manifest::PluginKind::TaskSource
        || cfg
            .plugin(name)
            .is_some_and(|p| p.kind == config::PluginKind::TaskSource);
    task_source
        && cfg
            .llm
            .as_ref()
            .and_then(|llm| llm.api_key_ref.as_deref())
            .is_some_and(|r| uses_supplied(&toml::Value::String(r.to_string())))
}

/// Whether any string leaf is a `secret:` reference.
fn uses_supplied(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(s) => matches!(s.parse(), Ok(SecretRef::Supplied { .. })),
        toml::Value::Array(items) => items.iter().any(uses_supplied),
        toml::Value::Table(table) => table.values().any(uses_supplied),
        _ => false,
    }
}

fn validate(cx: &Cx, offline: bool) -> Result<(), CliError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let cfg = cx.load_config(&env)?;

    let findings = cx.validate_config(&cfg, &env);
    let mut errors = config::has_errors(&findings);
    for finding in &findings {
        let label = match finding.severity {
            FindingSeverity::Error => "error",
            FindingSeverity::Warning => "warning",
        };
        println!("{label}: {}", finding.message);
    }

    // Online part (F-59): each enabled plugin validates its own config
    // (its kind is irrelevant — every kind implements `config/validate`).
    if !offline && !errors {
        let mut specs = Vec::new();
        for (name, _) in cfg.plugins.iter().filter(|(_, p)| p.enabled) {
            // Its values exist only where a launcher hands them over (#754):
            // resolving here would fail a correct config. Skipped plugins stay
            // out of the claim map, which reads as "no answer", not "claims
            // nothing".
            if supplied::installed().is_none() && needs_supplied(cx, &cfg, name) {
                println!(
                    "note: plugin `{name}` not validated online: its settings use a secret: \
                     reference → rerun as `totsuka config validate --secrets-stdin` from \
                     the launcher that holds the values"
                );
                continue;
            }
            // `plugin_spec` already took and secret-resolved the plugin's
            // `[<name>]` table into `init_config`; reuse it rather than
            // resolving secrets twice
            // (a second Keychain access could trigger a second Touch prompt).
            let spec = plugin_spec(&cx.store(), &cfg, name, &env)?;
            let init_config = spec.init_config.clone();
            specs.push((spec, init_config));
        }
        let runtime = tokio::runtime::Runtime::new()?;
        // Only plugins that answered go into the claim map: a name missing
        // from it means "no answer", which `check_workflow_options` treats as
        // unjudgeable rather than as "claims nothing" (#554).
        let mut claims: BTreeMap<String, Vec<plugin_protocol::methods::WorkflowOption>> =
            BTreeMap::new();
        for plugin_host::ValidatedPlugin {
            name,
            result,
            claimed_options,
            ..
        } in runtime.block_on(plugin_host::validate_all(specs))
        {
            match result {
                Ok(v) if v.valid => {
                    claims.insert(name.clone(), claimed_options);
                    println!("ok: plugin `{name}` accepted its config");
                    // Same `ConfigValidateResult` `doctor` reads, so the same
                    // warnings must appear here (protocol 0.7.3, #662). Two
                    // commands answering one question differently is worse
                    // than either answer alone — and the one that stayed
                    // quiet is the one people run *before* they suspect a
                    // problem.
                    print_warnings(&name, &v.warnings);
                }
                Ok(v) => {
                    errors = true;
                    claims.insert(name.clone(), claimed_options);
                    for problem in v.errors {
                        println!("error: plugin `{name}`: {problem}");
                    }
                    print_warnings(&name, &v.warnings);
                }
                Err(e) => {
                    errors = true;
                    println!("error: plugin `{name}` could not be probed: {e}");
                }
            }
        }
        for issue in check_workflow_options(&cfg, &claims) {
            errors = true;
            println!("error: {issue}");
        }
    } else if offline {
        println!("note: --offline skipped plugin config/validate probes (F-63)");
        // Naming the degradation: the plugin-defined keys on `[[workflows]]`
        // are checked by asking the plugins, so `--offline` cannot check them
        // at all — a typo there passes here and fails at `run` (#554).
        println!(
            "note: --offline cannot check plugin-defined `[[workflows]]` keys; \
             `totsuka run` still refuses to start on an unclaimed one"
        );
    }

    if errors {
        return Err("configuration is invalid → fix the errors above".into());
    }
    println!("configuration is valid");
    Ok(())
}

/// Advisory lines from a plugin's `config/validate` (protocol 0.7.3, #662).
///
/// **Never sets the error flag.** A warning is by definition something that
/// does not make the config invalid, so printing it must not change the exit
/// code — `doctor` draws the same distinction with `Check::warn`.
fn print_warnings(name: &str, warnings: &[String]) {
    for warning in warnings {
        println!("warning: plugin `{name}`: {warning}");
    }
}

fn show(cx: &Cx, redacted: bool) -> Result<(), CliError> {
    let contents = std::fs::read_to_string(&cx.config_path).map_err(|e| -> CliError {
        if e.kind() == io::ErrorKind::NotFound {
            format!(
                "config not found at {} → run `totsuka setup` to create it",
                cx.config_path.display()
            )
            .into()
        } else {
            e.into()
        }
    })?;
    println!("# {}", cx.config_path.display());
    print_toml(&contents, redacted)?;

    // `show` prints the *files*, so the env layer is not folded into the TOML
    // above — but leaving it out entirely would misrepresent what the daemon
    // will actually use, which is the very silence this command should break.
    let env: HashMap<String, String> = std::env::vars().collect();
    print_active_env_overrides(&env, redacted);
    Ok(())
}

/// List the `TOTSUKA_*` overrides that are actually in effect (F-66 layer 2),
/// so `show` cannot imply the files are the whole story.
///
/// "In effect" must match `apply_env_overrides` exactly, so an empty value is
/// skipped here too: it is warned about and treated as unset there, and
/// listing it as active would misreport the effective config — the same
/// silence this section exists to break.
fn print_active_env_overrides(env: &HashMap<String, String>, redacted: bool) {
    let active: Vec<(&str, &String)> = config::override_keys()
        .filter_map(|key| env.get_key_value(key).map(|(k, v)| (k.as_str(), v)))
        .filter(|(_, value)| !value.is_empty())
        .collect();
    if active.is_empty() {
        return;
    }
    println!("\n# active env overrides (TOTSUKA_*)");
    for (key, value) in active {
        // Same masking rule as the TOML bodies above, applied to the variable
        // name (`..._API_KEY_REF`).
        let shown = if redacted && is_secret_key(key) {
            "***redacted***"
        } else {
            value.as_str()
        };
        println!("# {key}={shown}");
    }
}

/// Print a TOML document, optionally masking secret-looking keys.
fn print_toml(contents: &str, redacted: bool) -> Result<(), CliError> {
    if !redacted {
        print!("{contents}");
        if !contents.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    let mut table: toml::Table = contents
        .parse()
        .map_err(|e| format!("failed to parse TOML: {e}"))?;
    redact_table(&mut table);
    print!("{}", toml::to_string_pretty(&table)?);
    Ok(())
}

/// Whether a key looks secret-bearing (§5.2 masking convention). Conservative:
/// `key` matches `api_key` / `access_key` / `private_key` / `apikey` too, so
/// `--redacted` masks anything the help text promises (token/key/secret/
/// password) — over-masking is safe, under-masking leaks.
fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["token", "key", "secret", "password", "credential"]
        .iter()
        .any(|pat| k.contains(pat))
}

/// Recursively mask values under secret-looking keys. A secret key's value is
/// masked **whole**, whatever its shape — a bare string, an array of strings
/// (`api_keys = ["…", "…"]`), or an inline table — so no secret survives via a
/// non-string container. Non-secret keys are descended into so nested secrets
/// are still caught.
fn redact_table(table: &mut toml::Table) {
    for (key, value) in table.iter_mut() {
        if is_secret_key(key) {
            *value = toml::Value::String("***redacted***".to_string());
            continue;
        }
        match value {
            toml::Value::Table(inner) => redact_table(inner),
            toml::Value::Array(items) => {
                for item in items {
                    if let toml::Value::Table(inner) = item {
                        redact_table(inner);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_protocol::manifest::PluginKind;

    /// A plugin's local `$ref` would point into the wrong document once
    /// embedded, so such an answer is shown raw (Copilot on #845).
    #[test]
    fn a_plugin_schema_with_ref_is_shown_raw() {
        let with_ref = serde_json::json!({
            "type": "object",
            "properties": { "a": { "$ref": "#/$defs/A" } },
            "$defs": { "A": { "type": "string" } },
        });
        let entry = plugin_property("p", answer(with_ref, None, None, PluginKind::Notifier));
        assert_eq!(entry["x-raw"], true);
        assert!(entry["x-schema-error"].as_str().unwrap().contains("$ref"));
        let inline = serde_json::json!({ "type": "object", "properties": {} });
        let entry = plugin_property("p", answer(inline, None, None, PluginKind::Notifier));
        assert!(entry.get("x-raw").is_none());
        assert_eq!(entry["x-category"], "p");
    }

    fn answer(
        schema: serde_json::Value,
        project: Option<serde_json::Value>,
        workflow: Option<serde_json::Value>,
        kind: PluginKind,
    ) -> orchestrator_core::plugins::PluginSchema {
        orchestrator_core::plugins::PluginSchema::Schema {
            answer: plugin_protocol::methods::ConfigSchemaResult {
                schema,
                project,
                workflow,
            },
            kind,
        }
    }

    /// A source's project / workflow keys land under its name on the entry
    /// schemas, an agent's workflow keys under `x-by-agent`, and an unusable
    /// one nowhere (ADR-0109 §5).
    #[test]
    fn entry_schemas_are_keyed_by_plugin_and_role() {
        use serde_json::json;
        let object =
            |key: &str| json!({ "type": "object", "properties": { key: { "type": "string" } } });
        let mut entries = EntrySchemas::default();
        entries.collect(
            "gh",
            &answer(
                object("token"),
                Some(object("owner")),
                Some(object("trigger")),
                PluginKind::TaskSource,
            ),
        );
        entries.collect(
            "ide",
            &answer(
                object("socket"),
                Some(object("ignored")),
                Some(object("layout")),
                PluginKind::AgentIde,
            ),
        );
        entries.collect(
            "bad",
            &answer(
                object("x"),
                Some(json!({ "$ref": "#/$defs/A" })),
                Some(json!("no")),
                PluginKind::TaskSource,
            ),
        );
        let mut schema = config::json_schema::core_schema();
        entries.attach(&mut schema);
        let projects = &schema["properties"]["projects"]["items"];
        let workflows = &schema["properties"]["workflows"]["items"];
        assert_eq!(projects["x-by-source"], json!({ "gh": object("owner") }));
        assert_eq!(workflows["x-by-source"], json!({ "gh": object("trigger") }));
        assert_eq!(workflows["x-by-agent"], json!({ "ide": object("layout") }));
        assert!(
            projects.get("x-by-agent").is_none(),
            "an agent has no project keys"
        );
    }

    #[test]
    fn redacts_secret_keys_recursively() {
        let mut table: toml::Table = r#"
api_key_ref = "keychain:totsuka/x"
private_key = "-----BEGIN-----"
api_keys = ["ghp_one", "ghp_two"]
name = "visible"

[nested]
github_token = "ghp_plain"

[[creds]]
password = "hunter2"
label = "shown"
"#
        .parse()
        .unwrap();
        redact_table(&mut table);
        assert_eq!(
            table["api_key_ref"].as_str().unwrap(),
            "***redacted***",
            "key containing api_key is masked"
        );
        // A bare `key`-bearing name is masked (help promises it).
        assert_eq!(table["private_key"].as_str().unwrap(), "***redacted***");
        // A secret-looking key whose value is an array of strings is masked
        // whole, not leaked element-by-element.
        assert_eq!(table["api_keys"].as_str().unwrap(), "***redacted***");
        assert_eq!(table["name"].as_str().unwrap(), "visible");
        assert_eq!(
            table["nested"]["github_token"].as_str().unwrap(),
            "***redacted***"
        );
        // Array-of-tables: secret keys inside each table are still caught.
        assert_eq!(
            table["creds"][0]["password"].as_str().unwrap(),
            "***redacted***"
        );
        assert_eq!(table["creds"][0]["label"].as_str().unwrap(), "shown");
    }
}
