//! Declarative config editing helpers (F-57).
//!
//! Every helper here takes the raw `config.toml` text and returns the edited
//! text. `toml_edit` preserves comments, ordering, and whitespace, so the
//! result is identical to a hand-edit — `config.toml` stays the single source
//! of truth (F-56) and the file a human wrote stays the file they recognise.
//!
//! Text in, text out (rather than "load, mutate, serialise") is what makes that
//! possible: a round-trip through [`crate::config::RootConfig`] would drop
//! every comment and reorder the file, and would fold in the `TOTSUKA_*`
//! environment layer, writing overrides into the file as if the user had typed
//! them.
//!
//! These helpers touch the filesystem not at all, which keeps them cheap to
//! test — and the property that matters most is tested directly: whatever they
//! produce must load through the real schema and pass `config::validate` with
//! no errors.
//!
//! # What a draft owns
//!
//! **A draft is authoritative for exactly the fields it models, and blind to
//! every other key.** A `None` optional field therefore means *absent* — the
//! key is removed if it is there — while a key the draft has no field for
//! (`max_concurrency` on a repository, say) is never touched.
//!
//! Leaving a stale `verification` or `on_success` behind instead would break
//! the convergence these helpers exist to provide: re-applying a draft has to
//! produce the config the draft describes, not that config merged with
//! whatever an earlier draft happened to write.

use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use crate::domain::{OutputPolicy, Profile, VerificationMode, WorkflowMode};

/// Errors from editing `config.toml`.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The document could not be parsed.
    #[error("failed to parse config.toml for editing: {0}")]
    Parse(#[from] toml_edit::TomlError),
    /// A path expected to be a table was something else.
    #[error("`{0}` is not a table in config.toml → fix it by hand")]
    NotATable(String),
    /// A path expected to be an array of tables was something else.
    #[error("`{0}` is not an array of tables in config.toml → fix it by hand")]
    NotAnArrayOfTables(String),
    /// A key path did not lead anywhere ([`set_path`] / [`unset_path`]).
    #[error("`{path}`: {detail}")]
    Path {
        /// The path as given.
        path: String,
        /// What was wrong with it.
        detail: String,
    },
    /// An inline-table fragment supplied by the caller did not parse.
    #[error("`{field}` is not a valid TOML inline table: {detail} → e.g. `{{ key = \"value\" }}`")]
    BadFragment {
        /// Which draft field held the fragment.
        field: &'static str,
        /// What was wrong with it.
        detail: String,
    },
}

/// Set `[plugins.{name}] enabled = <enabled>`, preserving all other formatting.
///
/// Creates the `[plugins.{name}]` table if absent. When the section is being
/// created (it has no `kind` yet) and `kind_if_new` is provided, `kind` is also
/// written — otherwise a brand-new section would be missing the required `kind`
/// field and make `config.toml` fail to load. Returns the edited document.
pub fn set_plugin_enabled(
    config_toml: &str,
    name: &str,
    enabled: bool,
    kind_if_new: Option<&str>,
) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;

    let plugins = doc
        .as_table_mut()
        .entry("plugins")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| EditError::NotATable("plugins".to_string()))?;
    // Keep `[plugins.foo]` idiomatic (no bare `[plugins]` header when it only
    // holds subtables).
    plugins.set_implicit(true);

    let section = plugins
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| EditError::NotATable(format!("plugins.{name}")))?;

    // Fill in `kind` only when the section lacks it, so a newly-created section
    // is a valid `PluginConfig` and existing sections are left untouched.
    if !section.contains_key("kind")
        && let Some(kind) = kind_if_new
    {
        set_value(section, "kind", kind);
    }
    set_value(section, "enabled", enabled);

    Ok(doc.to_string())
}

/// A `[[repositories]]` entry to write.
#[derive(Debug, Clone)]
pub struct RepositoryDraft<'a> {
    /// Identity within `config.toml` — an existing entry with this name is
    /// updated in place rather than appended.
    pub name: &'a str,
    /// Repository path (`~` and `${ENV}` are resolved at load time, not here).
    pub path: &'a str,
    /// One-line summary used for LLM repository selection. `None` removes it.
    pub summary: Option<&'a str>,
    /// The `[[projects]]` entry this repository files into (#554). `None`
    /// removes the binding, which is the state of a repository with no
    /// project.
    pub project: Option<&'a str>,
}

/// A `[[projects]]` entry to write (#554).
///
/// `options` is a TOML **inline table** fragment for the same reason
/// [`WorkflowDraft`]'s trigger is: the keys belong to the plugin named by
/// `source`, and the schema keeps them as an untyped table rather than
/// inventing a structure the Orchestrator does not have.
#[derive(Debug, Clone)]
pub struct ProjectDraft<'a> {
    /// Identity within `config.toml`, and what `[[repositories]].project`
    /// points at.
    pub name: &'a str,
    /// The task_source plugin that owns this project.
    pub source: &'a str,
    /// The plugin's own keys, as an inline-table fragment.
    pub options: &'a str,
}

/// A `[[workflows]]` entry to write.
///
/// `trigger` / `on_success` / `on_failure` are TOML **inline table** fragments
/// (`{ status = "実装待ち" }`) rather than typed values: their shape is
/// plugin-defined — the schema itself keeps them as an untyped `toml::Table` —
/// so mirroring that in a Rust type would invent a structure the orchestrator
/// does not have. A malformed fragment is rejected as
/// [`EditError::BadFragment`], not written and left for `run` to trip over.
#[derive(Debug, Clone)]
pub struct WorkflowDraft<'a> {
    /// Identity within `config.toml` — an existing entry with this name is
    /// updated in place.
    pub name: &'a str,
    /// The `[[projects]]` entries this workflow draws from (#626). The task
    /// source is their owner, so it is not written here.
    pub projects: &'a [&'a str],
    /// Inline-table fragment, or `None` to match every task from the source.
    pub trigger: Option<&'a str>,
    /// One of the four archetypes (#394). When set, `mode` and `verification`
    /// must be `None` — the profile supplies them, and writing both is a
    /// `ProfileConflict`.
    pub profile: Option<Profile>,
    /// Plan or implement. Required unless `profile` supplies it.
    pub mode: Option<WorkflowMode>,
    /// Agent IDE plugin name.
    pub agent: &'a str,
    /// What to do with the result. Required unless `profile` supplies it, and
    /// the one key a profile may be overridden on.
    pub output: Option<OutputPolicy>,
    /// Omitted when `None`, so the schema default applies.
    pub verification: Option<VerificationMode>,
    /// Inline-table fragment, or `None` for no success hook.
    pub on_success: Option<&'a str>,
    /// Inline-table fragment, or `None` for no failure hook.
    pub on_failure: Option<&'a str>,
}

/// Insert or update `[[repositories]]` matched by `name`.
///
/// Only the fields the draft models are touched: an entry a human has extended
/// with `tool` or `max_concurrency` keeps them.
pub fn upsert_repository(config_toml: &str, draft: &RepositoryDraft) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;
    let entry = array_entry(&mut doc, "repositories", draft.name)?;
    set_value(entry, "name", draft.name);
    set_value(entry, "path", draft.path);
    put_value(entry, "summary", draft.summary);
    put_value(entry, "project", draft.project);
    Ok(doc.to_string())
}

/// Insert or update `[[projects]]` matched by `name` (#554).
pub fn upsert_project(config_toml: &str, draft: &ProjectDraft) -> Result<String, EditError> {
    let options = parse_fragment("options", Some(draft.options))?;
    let mut doc: DocumentMut = config_toml.parse()?;
    let entry = array_entry(&mut doc, "projects", draft.name)?;
    set_value(entry, "name", draft.name);
    set_value(entry, "source", draft.source);
    if let Some(options) = options {
        for (key, value) in options.iter() {
            entry[key] = toml_edit::Item::Value(value.clone());
        }
    }
    Ok(doc.to_string())
}

/// Insert or update `[[workflows]]` matched by `name`.
pub fn upsert_workflow(config_toml: &str, draft: &WorkflowDraft) -> Result<String, EditError> {
    // Parse the caller's fragments before touching the document: a bad one must
    // not leave half a workflow behind.
    let trigger = parse_fragment("trigger", draft.trigger)?;
    let on_success = parse_fragment("on_success", draft.on_success)?;
    let on_failure = parse_fragment("on_failure", draft.on_failure)?;

    let mut doc: DocumentMut = config_toml.parse()?;
    let entry = array_entry(&mut doc, "workflows", draft.name)?;
    set_value(entry, "name", draft.name);
    set_array(entry, "projects", draft.projects);
    set_value(entry, "agent", draft.agent);
    // `put_value` rather than `set_value` for the three profile-owned keys: an
    // entry being rewritten from the spelled-out notation to a profile has to
    // lose the keys it no longer sets, or the result is a `ProfileConflict` the
    // wizard itself wrote.
    put_value(entry, "profile", draft.profile.map(Profile::as_str));
    put_value(entry, "mode", draft.mode.map(WorkflowMode::as_str));
    put_value(entry, "output", draft.output.map(OutputPolicy::as_str));
    put_value(
        entry,
        "verification",
        draft.verification.map(VerificationMode::as_str),
    );
    for (key, table) in [
        ("trigger", trigger),
        ("on_success", on_success),
        ("on_failure", on_failure),
    ] {
        put_value(entry, key, table.map(Value::InlineTable));
    }
    Ok(doc.to_string())
}

/// Write the `[llm]` table in its chat form.
///
/// The schema treats `[llm]` as all-or-nothing (`base_url` + `model` are both
/// required), so this writes the complete table rather than individual keys.
/// `api_key_ref: None` removes the key — a backend that injects the key through
/// the environment needs the stale reference gone, not merely unmentioned.
///
/// Writing chat means **removing `api` and `endpoint`** (#723): left over from
/// a decisions table, `api = "decisions"` next to the new `base_url` would be
/// a config that no longer loads.
pub fn set_llm(
    config_toml: &str,
    base_url: &str,
    model: &str,
    api_key_ref: Option<&str>,
) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;
    let llm = table_at(&mut doc, "llm")?;
    put_value(llm, "api", None::<&str>);
    put_value(llm, "endpoint", None::<&str>);
    set_value(llm, "base_url", base_url);
    set_value(llm, "model", model);
    put_value(llm, "api_key_ref", api_key_ref);
    Ok(doc.to_string())
}

/// Write `[tools.{name}]`.
///
/// `claude` / `codex` / `opencode` are built in, so most configs need no
/// `[tools]` section at all; this exists for overriding a command or adding a
/// second profile of the same kind. `command: None` removes any override,
/// restoring the built-in command for that `kind`.
pub fn set_tool(
    config_toml: &str,
    name: &str,
    kind: &str,
    command: Option<&str>,
) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;
    let tools = table_at(&mut doc, "tools")?;
    tools.set_implicit(true);
    let section = tools
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| EditError::NotATable(format!("tools.{name}")))?;
    set_value(section, "kind", kind);
    put_value(section, "command", command);
    Ok(doc.to_string())
}

/// Set the top-level `default_tool`.
pub fn set_default_tool(config_toml: &str, name: &str) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;
    set_value(doc.as_table_mut(), "default_tool", name);
    Ok(doc.to_string())
}

/// Assign `key = new`, keeping whatever decoration the line already had.
///
/// Plain `table[key] = value(x)` replaces the whole `Item`, and the trailing
/// comment lives on the *value* — so `name = "totsuka"  # ours` silently loses
/// `# ours` the first time a value is rewritten. Preserving comments is the
/// entire reason these helpers edit text instead of re-serialising the struct,
/// so the loss would defeat the module.
///
/// An unchanged value is left completely alone, which is also what makes
/// re-applying the same draft byte-identical.
fn set_value<V: Into<Value>>(table: &mut Table, key: &str, new: V) {
    let new: Value = new.into();
    let existing = table.get(key).and_then(Item::as_value);
    if existing.is_some_and(|v| same_value(v, &new)) {
        return;
    }
    let decor = existing.map(|v| v.decor().clone());
    table[key] = Item::Value(new);
    if let Some(decor) = decor
        && let Some(v) = table.get_mut(key).and_then(Item::as_value_mut)
    {
        *v.decor_mut() = decor;
    }
}

/// Assign `key = [new…]`, preserving the existing decor as [`set_value`] does.
///
/// Separate from `set_value` because `Value` has no `From<&[&str]>`: an array
/// has to be built element by element, and comparing "unchanged" means
/// comparing the elements rather than one scalar.
fn set_array(table: &mut Table, key: &str, new: &[&str]) {
    let mut array = toml_edit::Array::new();
    for item in new {
        array.push(*item);
    }
    let unchanged = table
        .get(key)
        .and_then(Item::as_array)
        .is_some_and(|existing| {
            existing.len() == new.len()
                && existing
                    .iter()
                    .zip(new)
                    .all(|(a, b)| a.as_str() == Some(*b))
        });
    if unchanged {
        return;
    }
    let decor = table
        .get(key)
        .and_then(Item::as_value)
        .map(|v| v.decor().clone());
    table[key] = Item::Value(Value::Array(array));
    if let Some(decor) = decor
        && let Some(v) = table.get_mut(key).and_then(Item::as_value_mut)
    {
        *v.decor_mut() = decor;
    }
}

/// Assign `key = new`, or remove `key` when the draft has no value for it.
///
/// See the module docs: a draft owns the fields it models, so `None` means the
/// key should be absent, not that the existing one should survive.
fn put_value<V: Into<Value>>(table: &mut Table, key: &str, new: Option<V>) {
    match new {
        Some(new) => set_value(table, key, new),
        None => {
            table.remove(key);
        }
    }
}

/// Whether two TOML values are equal ignoring formatting.
///
/// Every variant is compared, not just the ones today's callers happen to
/// write. An incomplete match here does not fail loudly — it silently answers
/// "different", which makes [`set_value`] rewrite an identical value and
/// discard the formatting inside it. `trigger = { labels = ["migration"] }`
/// (an array, from the docs' own example) is exactly that case.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.value() == y.value(),
        (Value::Integer(x), Value::Integer(y)) => x.value() == y.value(),
        (Value::Float(x), Value::Float(y)) => x.value() == y.value(),
        (Value::Boolean(x), Value::Boolean(y)) => x.value() == y.value(),
        (Value::Datetime(x), Value::Datetime(y)) => x.value() == y.value(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(xv, yv)| same_value(xv, yv))
        }
        (Value::InlineTable(x), Value::InlineTable(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, xv)| y.get(k).is_some_and(|yv| same_value(xv, yv)))
        }
        // Genuinely different types. Not a fallback for unhandled variants —
        // every variant above is matched.
        _ => false,
    }
}

/// The top-level table at `key`, created if absent.
fn table_at<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut Table, EditError> {
    doc.as_table_mut()
        .entry(key)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| EditError::NotATable(key.to_string()))
}

/// The entry of array-of-tables `key` whose `name` matches, appending one if
/// there is none.
///
/// Matching on `name` rather than position is what makes re-running an edit a
/// no-op instead of appending a duplicate — and a duplicate here is not
/// harmless: `config::validate` rejects the file outright
/// (`DuplicateRepo` / `DuplicateWorkflow`).
fn array_entry<'a>(
    doc: &'a mut DocumentMut,
    key: &str,
    name: &str,
) -> Result<&'a mut Table, EditError> {
    let item = doc
        .as_table_mut()
        .entry(key)
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
    let array = item
        .as_array_of_tables_mut()
        .ok_or_else(|| EditError::NotAnArrayOfTables(key.to_string()))?;

    let existing = array
        .iter()
        .position(|t| t.get("name").and_then(|n| n.as_str()) == Some(name));
    let index = match existing {
        Some(index) => index,
        None => {
            array.push(Table::new());
            array.len() - 1
        }
    };
    Ok(array.get_mut(index).expect("index just resolved"))
}

/// Parse an inline-table fragment supplied by a caller.
fn parse_fragment(
    field: &'static str,
    fragment: Option<&str>,
) -> Result<Option<InlineTable>, EditError> {
    let Some(text) = fragment else {
        return Ok(None);
    };
    // Wrapped in an assignment so the fragment is parsed as a *value*, which is
    // what makes `{ a = 1 }` and a stray `a = 1` distinguishable.
    let doc: DocumentMut = format!("x = {text}")
        .parse()
        .map_err(|e: toml_edit::TomlError| EditError::BadFragment {
            field,
            detail: e.to_string(),
        })?;
    match doc["x"].as_value() {
        Some(Value::InlineTable(table)) => Ok(Some(table.clone())),
        _ => Err(EditError::BadFragment {
            field,
            detail: "parsed, but is not a table".to_string(),
        }),
    }
}

/// Set the value at a key path, preserving the formatting of everything
/// else — the per-key write behind `totsuka config set` (ADR-0113).
///
/// The path is a JSON Pointer (RFC 6901): `/log/level`,
/// `/repositories/0/tool`, `/tools/my.tool/kind`; `~1` stands for `/` and
/// `~0` for `~` inside a key. A numeric segment is an index only where the
/// container already is an array — on a table it is an ordinary key — and `-`
/// appends to an array. Missing tables on the way are created; a missing
/// container followed by `-` is created as an array. A JSON object becomes a
/// `[table]` (an inline table where the parent or the replaced value is
/// inline), an array of objects becomes `[[array of tables]]`, and `null` is
/// refused — removing a key is [`unset_path`]. Returns the edited document.
pub fn set_path(
    config_toml: &str,
    path: &str,
    value: &serde_json::Value,
) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;
    let (parents, last) = split_path(path)?;
    let mut node = Node::Table(doc.as_table_mut());
    for (depth, segment) in parents.iter().enumerate() {
        // What to create when `segment` is missing: an array only when told
        // so by an append (`/repositories/-`). A number is not enough — it is
        // an ordinary key on a table (`/tools/123/kind`).
        let next = parents.get(depth + 1).map(String::as_str).unwrap_or(&last);
        let create = if next == APPEND {
            Create::Array
        } else {
            Create::Table
        };
        node = node
            .step(segment, create)
            .ok_or_else(|| path_error(path, &pointer(&parents[..=depth]), "not found"))?;
    }
    let last = last.as_str();
    let err = |detail: String| path_error(path, path, &detail);
    match node {
        Node::Table(table) => {
            let inline = table.get(last).is_some_and(Item::is_value);
            match json_item(value, inline).map_err(err)? {
                Item::Value(v) => set_value(table, last, v),
                other => {
                    table.insert(last, other);
                }
            }
        }
        Node::Tables(array) => {
            let index = array_index(last, array.len())
                .ok_or_else(|| err("expected an index or `-`".into()))?;
            let serde_json::Value::Object(map) = value else {
                return Err(err("an array of tables holds only objects".into()));
            };
            let table = json_table(map).map_err(err)?;
            let len = array.len();
            if index < len {
                *array.get_mut(index).expect("index < len") = table;
            } else if index == len {
                array.push(table);
            } else {
                return Err(err("index out of range".into()));
            }
        }
        Node::Inline(table) => {
            table.insert(last, json_value(value).map_err(err)?);
        }
        Node::Array(array) => {
            let index = array_index(last, array.len())
                .ok_or_else(|| err("expected an index or `-`".into()))?;
            let new = json_value(value).map_err(err)?;
            if index < array.len() {
                array.replace(index, new);
            } else if index == array.len() {
                array.push(new);
            } else {
                return Err(err("index out of range".into()));
            }
        }
    }
    Ok(doc.to_string())
}

/// Remove the key or array element at a JSON Pointer, the inverse of
/// [`set_path`] (`totsuka config unset`). Removing what is not there is not an
/// error: the document already says what was asked.
pub fn unset_path(config_toml: &str, path: &str) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;
    let (parents, last) = split_path(path)?;
    let mut node = Node::Table(doc.as_table_mut());
    for segment in &parents {
        match node.step(segment, Create::No) {
            Some(next) => node = next,
            None => return Ok(doc.to_string()),
        }
    }
    let index = parse_index(&last);
    match node {
        Node::Table(table) => {
            table.remove(&last);
        }
        Node::Inline(table) => {
            table.remove(&last);
        }
        Node::Tables(array) => {
            if let Some(i) = index.filter(|i| *i < array.len()) {
                array.remove(i);
            }
        }
        Node::Array(array) => {
            if let Some(i) = index.filter(|i| *i < array.len()) {
                array.remove(i);
            }
        }
    }
    Ok(doc.to_string())
}

/// What [`Node::step`] creates for a missing key.
#[derive(Clone, Copy)]
enum Create {
    No,
    Table,
    Array,
}

/// A container a key path can step through. `toml_edit` stores array-of-table
/// entries as `Table`s and inline containers as `Value`s, so there is no single
/// `&mut Item` to walk down.
enum Node<'a> {
    Table(&'a mut Table),
    Inline(&'a mut InlineTable),
    Tables(&'a mut ArrayOfTables),
    Array(&'a mut toml_edit::Array),
}

impl<'a> Node<'a> {
    fn of_item(item: &'a mut Item) -> Option<Self> {
        match item {
            Item::Table(t) => Some(Node::Table(t)),
            Item::ArrayOfTables(a) => Some(Node::Tables(a)),
            Item::Value(v) => Node::of_value(v),
            Item::None => None,
        }
    }

    fn of_value(value: &'a mut Value) -> Option<Self> {
        match value {
            Value::InlineTable(t) => Some(Node::Inline(t)),
            Value::Array(a) => Some(Node::Array(a)),
            _ => None,
        }
    }

    /// One step down, creating a missing key under a table as `create` says
    /// (inline forms under an inline parent).
    fn step(self, segment: &str, create: Create) -> Option<Self> {
        match self {
            Node::Table(table) => {
                if !table.contains_key(segment) {
                    match create {
                        Create::No => return None,
                        Create::Table => {
                            let mut new = Table::new();
                            new.set_implicit(true);
                            table.insert(segment, Item::Table(new));
                        }
                        Create::Array => {
                            table.insert(segment, Item::ArrayOfTables(ArrayOfTables::new()));
                        }
                    }
                }
                Node::of_item(table.get_mut(segment)?)
            }
            Node::Inline(table) => {
                if !table.contains_key(segment) {
                    match create {
                        Create::No => return None,
                        Create::Table => {
                            table.insert(segment, Value::InlineTable(InlineTable::new()));
                        }
                        Create::Array => {
                            table.insert(segment, Value::Array(toml_edit::Array::new()));
                        }
                    }
                }
                Node::of_value(table.get_mut(segment)?)
            }
            Node::Tables(array) => Some(Node::Table(array.get_mut(parse_index(segment)?)?)),
            Node::Array(array) => Node::of_value(array.get_mut(parse_index(segment)?)?),
        }
    }
}

/// The JSON Pointer segment that appends to an array (RFC 6901).
const APPEND: &str = "-";

fn path_error(path: &str, at: &str, detail: &str) -> EditError {
    EditError::Path {
        path: path.to_string(),
        detail: format!("{detail} at `{at}`"),
    }
}

/// `/a/b~1c/d` → (`[a, b/c]`, `d`): the segments of a JSON Pointer,
/// unescaped. The empty pointer (the whole document) and empty segments are
/// refused — neither names a key that can be set.
fn split_path(path: &str) -> Result<(Vec<String>, String), EditError> {
    let bad = |detail: &str| EditError::Path {
        path: path.to_string(),
        detail: detail.into(),
    };
    let rest = path
        .strip_prefix('/')
        .ok_or_else(|| bad("not a JSON Pointer → start with `/`, e.g. `/log/level`"))?;
    let mut segments: Vec<String> = rest
        .split('/')
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect();
    if segments.iter().any(String::is_empty) {
        return Err(bad("empty key segment"));
    }
    let last = segments.pop().expect("split yields at least one");
    Ok((segments, last))
}

/// `segments` written back as a JSON Pointer, for error messages.
fn pointer(segments: &[String]) -> String {
    segments
        .iter()
        .map(|s| format!("/{}", s.replace('~', "~0").replace('/', "~1")))
        .collect()
}

fn parse_index(segment: &str) -> Option<usize> {
    segment.parse().ok()
}

/// An array position: a number, or `-` for one past the end (append).
fn array_index(segment: &str, len: usize) -> Option<usize> {
    if segment == APPEND {
        Some(len)
    } else {
        parse_index(segment)
    }
}

/// A JSON value as a TOML item under a standard table: objects become
/// `[tables]` and arrays of objects `[[arrays of tables]]`, unless `inline`
/// (the value being replaced was written inline), which keeps them inline.
fn json_item(value: &serde_json::Value, inline: bool) -> Result<Item, String> {
    match value {
        serde_json::Value::Object(map) if !inline => Ok(Item::Table(json_table(map)?)),
        serde_json::Value::Array(items)
            if !inline && !items.is_empty() && items.iter().all(|v| v.is_object()) =>
        {
            let mut array = ArrayOfTables::new();
            for item in items {
                if let serde_json::Value::Object(map) = item {
                    array.push(json_table(map)?);
                }
            }
            Ok(Item::ArrayOfTables(array))
        }
        other => Ok(Item::Value(json_value(other)?)),
    }
}

fn json_table(map: &serde_json::Map<String, serde_json::Value>) -> Result<Table, String> {
    let mut table = Table::new();
    for (key, value) in map {
        table.insert(key, json_item(value, false)?);
    }
    Ok(table)
}

fn json_value(value: &serde_json::Value) -> Result<Value, String> {
    Ok(match value {
        serde_json::Value::Null => {
            return Err("TOML has no null → remove the key with `config unset` instead".into());
        }
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::from(n.as_f64().ok_or("a number out of TOML's range")?),
        },
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(json_value(item)?);
            }
            Value::Array(array)
        }
        serde_json::Value::Object(map) => {
            let mut table = InlineTable::new();
            for (key, item) in map {
                table.insert(key, json_value(item)?);
            }
            Value::InlineTable(table)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flips_enabled_preserving_comments_and_layout() {
        let original = r#"# totsuka config
version = 1

[plugins.herdr]  # our agent
enabled = true
kind = "agent_ide"   # keep this comment
max_concurrency = 3
"#;
        let disabled = set_plugin_enabled(original, "herdr", false, None).unwrap();
        // enabled flipped...
        assert!(disabled.contains("enabled = false"));
        // ...and everything else preserved verbatim.
        assert!(disabled.contains("# totsuka config"));
        assert!(disabled.contains("[plugins.herdr]  # our agent"));
        assert!(disabled.contains("kind = \"agent_ide\"   # keep this comment"));
        assert!(disabled.contains("max_concurrency = 3"));

        // Round-trips back to enabled.
        let reenabled = set_plugin_enabled(&disabled, "herdr", true, None).unwrap();
        assert!(reenabled.contains("enabled = true"));
        assert!(reenabled.contains("# keep this comment"));
    }

    #[test]
    fn creates_section_with_kind_and_stays_schema_valid() {
        let out = set_plugin_enabled("version = 1\n", "notion", true, Some("task_source")).unwrap();
        assert!(out.contains("[plugins.notion]"));
        assert!(out.contains("enabled = true"));
        assert!(out.contains("kind = \"task_source\""));
        // No stray bare `[plugins]` header.
        assert!(!out.contains("[plugins]\n"));
        assert!(out.contains("version = 1"));
        // The result must load through the real schema (kind is required).
        crate::config::RootConfig::from_toml_str(&out).expect("new section must be schema-valid");
    }

    #[test]
    fn existing_section_kind_is_untouched() {
        let out = set_plugin_enabled(
            "[plugins.x]\nenabled = true\nkind = \"notifier\"\n",
            "x",
            false,
            Some("task_source"), // ignored: section already has a kind
        )
        .unwrap();
        crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert!(out.contains("enabled = false"));
        assert!(
            out.contains("kind = \"notifier\""),
            "existing kind must not change"
        );
    }

    // ------------------------------------------------------------------
    // Typed section editors (#347). These back `totsuka setup`, so the
    // property that matters is not "the right bytes came out" but "the
    // result is a config the orchestrator will actually accept".
    // ------------------------------------------------------------------

    /// Build a config the way `setup` will: start from the skeleton `init`
    /// writes (every line a comment) and apply every editor in turn.
    fn fully_edited() -> String {
        let skeleton = "# totsuka configuration\n\
                        # max_concurrency = 4\n\
                        \n\
                        # [[repositories]]\n\
                        # name = \"my-repo\"\n";

        let out = set_plugin_enabled(skeleton, "slack", true, Some("task_source")).unwrap();
        let out = set_plugin_enabled(&out, "herdr", true, Some("agent_ide")).unwrap();
        // The domain the workflow below draws from (#626). A source with one
        // of them still declares it, because a workflow names domains rather
        // than plugins — which is what `setup` writes for every task source
        // it installs.
        let out = upsert_project(
            &out,
            &ProjectDraft {
                name: "slack",
                source: "slack",
                options: "{}",
            },
        )
        .unwrap();
        let out = upsert_repository(
            &out,
            &RepositoryDraft {
                name: "totsuka",
                path: "~/Workspace/totsuka",
                summary: Some("the orchestrator itself"),
                project: None,
            },
        )
        .unwrap();
        let out = upsert_workflow(
            &out,
            &WorkflowDraft {
                name: "slack-reply",
                projects: &["slack"],
                trigger: Some(r#"{ mention = true }"#),
                profile: None,
                mode: Some(WorkflowMode::Plan),
                agent: "herdr",
                output: Some(OutputPolicy::Source),
                verification: Some(VerificationMode::Human),
                on_success: None,
                on_failure: None,
            },
        )
        .unwrap();
        let out = set_llm(
            &out,
            "https://openrouter.ai/api/v1",
            "anthropic/claude-haiku-4-5",
            Some("keychain:totsuka/openrouter"),
        )
        .unwrap();
        let out = set_tool(&out, "claude-plan", "claude", Some("claude")).unwrap();
        set_default_tool(&out, "claude").unwrap()
    }

    #[test]
    fn a_fully_edited_skeleton_loads_and_validates_clean() {
        let out = fully_edited();

        // Loading is the sharp end: every struct is `deny_unknown_fields`, so a
        // single mistyped key fails here rather than at run time.
        let cfg = crate::config::RootConfig::from_toml_str(&out)
            .unwrap_or_else(|e| panic!("edited config does not load: {e}\n---\n{out}"));

        // And it must survive the same static validation `run` performs. The
        // repository path does not exist in the test environment, so that one
        // finding is expected; nothing else may be.
        let no_env = |_: &str| None;
        let unexpected: Vec<String> = crate::config::validate_static(&cfg, &no_env)
            .into_iter()
            .map(|e| e.to_string())
            .filter(|e| !e.contains("path"))
            .collect();
        assert!(
            unexpected.is_empty(),
            "unexpected findings: {unexpected:?}\n---\n{out}"
        );

        // The values actually landed.
        assert_eq!(cfg.repositories.len(), 1);
        assert_eq!(cfg.repositories[0].name, "totsuka");
        assert_eq!(cfg.workflows.len(), 1);
        assert_eq!(cfg.workflows[0].agent, "herdr");
        assert_eq!(
            cfg.llm.as_ref().unwrap().model,
            "anthropic/claude-haiku-4-5"
        );
        assert_eq!(cfg.default_tool.as_deref(), Some("claude"));
        // The user's own comments survived all seven edits.
        assert!(out.contains("# totsuka configuration"), "{out}");
        assert!(out.contains("# max_concurrency = 4"), "{out}");
    }

    #[test]
    fn applying_the_same_edits_twice_changes_nothing() {
        // Re-running `setup` must converge, not append. A duplicated
        // `[[repositories]]` is not cosmetic — `validate` rejects the file.
        let once = fully_edited();
        let twice = {
            let out = upsert_repository(
                &once,
                &RepositoryDraft {
                    name: "totsuka",
                    path: "~/Workspace/totsuka",
                    summary: Some("the orchestrator itself"),
                    project: None,
                },
            )
            .unwrap();
            upsert_workflow(
                &out,
                &WorkflowDraft {
                    name: "slack-reply",
                    projects: &["slack"],
                    trigger: Some(r#"{ mention = true }"#),
                    profile: None,
                    mode: Some(WorkflowMode::Plan),
                    agent: "herdr",
                    output: Some(OutputPolicy::Source),
                    verification: Some(VerificationMode::Human),
                    on_success: None,
                    on_failure: None,
                },
            )
            .unwrap()
        };
        assert_eq!(once, twice, "second application must be a no-op");
    }

    #[test]
    fn upsert_updates_in_place_and_keeps_fields_it_does_not_own() {
        let original = r#"# repos
[[repositories]]
name = "totsuka"          # ours
path = "/old/path"
max_concurrency = 2

[[repositories]]
name = "dotfiles"
path = "/dotfiles"
"#;
        let out = upsert_repository(
            original,
            &RepositoryDraft {
                name: "totsuka",
                path: "/new/path",
                summary: None,
                project: None,
            },
        )
        .unwrap();

        assert!(out.contains("/new/path"), "{out}");
        assert!(!out.contains("/old/path"), "{out}");
        // A field the draft has no opinion about is left alone.
        assert!(out.contains("max_concurrency = 2"), "{out}");
        assert!(out.contains("# ours"), "comment lost: {out}");
        // The other entry is untouched and no duplicate appeared.
        assert!(out.contains("name = \"dotfiles\""), "{out}");
        assert_eq!(out.matches("name = \"totsuka\"").count(), 1, "{out}");
    }

    #[test]
    fn workflow_enums_round_trip_through_the_real_schema() {
        // The editor writes `mode` / `output` / `verification` from `as_str`.
        // If serde's rename and `as_str` ever drift, the config stops loading —
        // so every variant is exercised against the real deserializer.
        for mode in [WorkflowMode::Plan, WorkflowMode::Implement] {
            for output in [OutputPolicy::Source, OutputPolicy::None] {
                for verification in [
                    VerificationMode::Llm,
                    VerificationMode::Human,
                    VerificationMode::None,
                ] {
                    let out = upsert_workflow(
                        "",
                        &WorkflowDraft {
                            name: "w",
                            projects: &["s"],
                            trigger: None,
                            profile: None,
                            mode: Some(mode),
                            agent: "a",
                            output: Some(output),
                            verification: Some(verification),
                            on_success: None,
                            on_failure: None,
                        },
                    )
                    .unwrap();
                    let cfg = crate::config::RootConfig::from_toml_str(&out)
                        .unwrap_or_else(|e| panic!("{mode:?}/{output:?}/{verification:?}: {e}"));
                    assert_eq!(cfg.workflows[0].mode, Some(mode));
                    assert_eq!(cfg.workflows[0].output, Some(output));
                    assert_eq!(cfg.workflows[0].verification, Some(verification));
                }
            }
        }
    }

    #[test]
    fn profile_round_trips_and_omits_the_keys_it_supplies() {
        // Same drift guard for `Profile::as_str` vs serde's rename, plus the
        // half that matters more: a profile draft must not also emit `mode` or
        // `verification`, or the file the wizard just wrote fails validation
        // with `ProfileConflict`.
        for profile in [
            Profile::Answer,
            Profile::Triage,
            Profile::Design,
            Profile::Implement,
        ] {
            let out = upsert_workflow(
                "",
                &WorkflowDraft {
                    name: "w",
                    projects: &["s"],
                    trigger: None,
                    profile: Some(profile),
                    mode: None,
                    agent: "a",
                    output: None,
                    verification: None,
                    on_success: None,
                    on_failure: None,
                },
            )
            .unwrap();
            let cfg = crate::config::RootConfig::from_toml_str(&out)
                .unwrap_or_else(|e| panic!("{profile:?}: {e}\n---\n{out}"));
            assert_eq!(cfg.workflows[0].profile, Some(profile));
            assert_eq!(cfg.workflows[0].mode, None, "{out}");
            assert_eq!(cfg.workflows[0].verification, None, "{out}");
            assert_eq!(cfg.workflows[0].resolved_mode(), profile.mode());
            assert_eq!(cfg.workflows[0].resolved_output(), profile.output());
        }
    }

    #[test]
    fn rewriting_a_workflow_as_a_profile_drops_the_keys_it_replaces() {
        // The upsert path a `setup` re-run takes. `set_value` would have left
        // `mode` behind next to the new `profile`, which validation rejects —
        // the entry has to lose the keys the profile now owns.
        let spelled_out = upsert_workflow(
            "",
            &WorkflowDraft {
                name: "w",
                projects: &["s"],
                trigger: None,
                profile: None,
                mode: Some(WorkflowMode::Implement),
                agent: "a",
                output: Some(OutputPolicy::Source),
                verification: Some(VerificationMode::Human),
                on_success: None,
                on_failure: None,
            },
        )
        .unwrap();
        assert!(spelled_out.contains("mode"), "{spelled_out}");

        let as_profile = upsert_workflow(
            &spelled_out,
            &WorkflowDraft {
                name: "w",
                projects: &["s"],
                trigger: None,
                profile: Some(Profile::Design),
                mode: None,
                agent: "a",
                output: None,
                verification: None,
                on_success: None,
                on_failure: None,
            },
        )
        .unwrap();
        let cfg = crate::config::RootConfig::from_toml_str(&as_profile)
            .unwrap_or_else(|e| panic!("{e}\n---\n{as_profile}"));
        assert_eq!(cfg.workflows[0].profile, Some(Profile::Design));
        assert_eq!(cfg.workflows[0].mode, None, "{as_profile}");
        assert_eq!(cfg.workflows[0].verification, None, "{as_profile}");
        assert_eq!(cfg.workflows[0].output, None, "{as_profile}");
    }

    #[test]
    fn a_malformed_fragment_is_rejected_before_anything_is_written() {
        let err = upsert_workflow(
            "",
            &WorkflowDraft {
                name: "w",
                projects: &["s"],
                trigger: Some("not a table"),
                profile: None,
                mode: Some(WorkflowMode::Plan),
                agent: "a",
                output: Some(OutputPolicy::Source),
                verification: None,
                on_success: None,
                on_failure: None,
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                EditError::BadFragment {
                    field: "trigger",
                    ..
                }
            ),
            "{err}"
        );
        // The message has to say what a good one looks like.
        assert!(err.to_string().contains("key = "), "{err}");
    }

    #[test]
    fn a_conflicting_shape_is_reported_not_overwritten() {
        // Someone wrote `repositories = "oops"`. Silently replacing it would
        // destroy whatever they meant.
        let err = upsert_repository(
            "repositories = \"oops\"\n",
            &RepositoryDraft {
                name: "x",
                path: "/x",
                summary: None,
                project: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NotAnArrayOfTables(_)), "{err}");

        let err = set_llm("llm = 1\n", "u", "m", None).unwrap_err();
        assert!(matches!(err, EditError::NotATable(_)), "{err}");
    }

    #[test]
    fn optional_fields_are_omitted_rather_than_written_empty() {
        let out = upsert_workflow(
            "",
            &WorkflowDraft {
                name: "w",
                projects: &["s"],
                trigger: None,
                profile: None,
                mode: Some(WorkflowMode::Implement),
                agent: "a",
                output: Some(OutputPolicy::None),
                verification: None,
                on_success: None,
                on_failure: None,
            },
        )
        .unwrap();
        assert!(!out.contains("verification"), "{out}");
        assert!(!out.contains("trigger"), "{out}");
        // The schema default applies when the key is absent.
        let cfg = crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert_eq!(cfg.workflows[0].verification, None);
        assert_eq!(
            cfg.workflows[0].resolved_verification(),
            VerificationMode::Llm
        );
    }

    #[test]
    fn a_none_optional_clears_a_key_a_previous_draft_wrote() {
        // Convergence: re-applying a draft has to produce the config the draft
        // describes. Leaving the old `verification` / `on_success` / `summary`
        // behind would merge two drafts into a config neither one describes,
        // and the difference is behavioural — a stale `verification = "human"`
        // parks every task waiting for a sign-off nobody knows to give.
        let with_extras = upsert_workflow(
            "",
            &WorkflowDraft {
                name: "w",
                projects: &["s"],
                trigger: Some(r#"{ status = "実装待ち" }"#),
                profile: None,
                mode: Some(WorkflowMode::Implement),
                agent: "a",
                output: Some(OutputPolicy::Source),
                verification: Some(VerificationMode::Human),
                on_success: Some(r#"{ status = "done" }"#),
                on_failure: None,
            },
        )
        .unwrap();
        assert!(with_extras.contains("verification"), "{with_extras}");

        let cleared = upsert_workflow(
            &with_extras,
            &WorkflowDraft {
                name: "w",
                projects: &["s"],
                trigger: None,
                profile: None,
                mode: Some(WorkflowMode::Implement),
                agent: "a",
                output: Some(OutputPolicy::Source),
                verification: None,
                on_success: None,
                on_failure: None,
            },
        )
        .unwrap();
        for key in ["verification", "trigger", "on_success"] {
            assert!(!cleared.contains(key), "`{key}` survived: {cleared}");
        }
        let cfg = crate::config::RootConfig::from_toml_str(&cleared).unwrap();
        assert_eq!(cfg.workflows.len(), 1, "{cleared}");
        assert_eq!(cfg.workflows[0].verification, None);
        assert_eq!(
            cfg.workflows[0].resolved_verification(),
            VerificationMode::Llm
        );

        // Same rule for the other drafts' optionals.
        let repo = upsert_repository(
            "",
            &RepositoryDraft {
                name: "r",
                path: "/r",
                summary: Some("was here"),
                project: None,
            },
        )
        .unwrap();
        let repo = upsert_repository(
            &repo,
            &RepositoryDraft {
                name: "r",
                path: "/r",
                summary: None,
                project: None,
            },
        )
        .unwrap();
        assert!(!repo.contains("summary"), "{repo}");

        let llm = set_llm("", "https://x/v1", "m", Some("keychain:totsuka/k")).unwrap();
        let llm = set_llm(&llm, "https://x/v1", "m", None).unwrap();
        assert!(!llm.contains("api_key_ref"), "{llm}");

        // A decisions table rewritten as chat must still load (#723).
        let decisions = "[llm]\napi = \"decisions\"\nmodel = \"~typesafe/jev-latest\"\n\
                         endpoint = \"https://gw/decisions\"\n";
        let chat = set_llm(decisions, "https://x/v1", "m", None).unwrap();
        let parsed = crate::config::RootConfig::from_toml_str(&chat)
            .unwrap_or_else(|e| panic!("{e}\n{chat}"));
        assert_eq!(parsed.llm.unwrap().api.name(), "chat");

        let tool = set_tool("", "t", "claude", Some("/bin/claude")).unwrap();
        let tool = set_tool(&tool, "t", "claude", None).unwrap();
        assert!(!tool.contains("command"), "{tool}");
    }

    #[test]
    fn an_unchanged_fragment_containing_an_array_is_left_alone() {
        // `same_value` decides whether a value is rewritten. Any variant it
        // cannot compare answers "different", so an identical value gets
        // replaced and the formatting inside it is lost — silently, because
        // nothing errors. Arrays reach this path from the docs' own example
        // (`trigger = { labels = ["migration", "high-risk"] }`).
        let hand_written = concat!(
            "[[workflows]]\n",
            "name = \"migration\"\n",
            "projects = [\"github\"]\n",
            "mode = \"implement\"\n",
            "agent = \"herdr\"\n",
            "output = \"source\"\n",
            // Spacing a human chose, and a comment attached to the value.
            "trigger = { labels = [ \"migration\",  \"high-risk\" ] }  # mine\n",
        );
        let draft = WorkflowDraft {
            name: "migration",
            projects: &["github"],
            trigger: Some(r#"{ labels = ["migration", "high-risk"] }"#),
            profile: None,
            mode: Some(WorkflowMode::Implement),
            agent: "herdr",
            output: Some(OutputPolicy::Source),
            verification: None,
            on_success: None,
            on_failure: None,
        };
        let out = upsert_workflow(hand_written, &draft).unwrap();
        assert_eq!(
            out, hand_written,
            "an equal value was rewritten, losing the formatting inside it"
        );
    }

    #[test]
    fn tools_section_stays_idiomatic_and_loadable() {
        let out = set_tool("", "my-claude", "claude", Some("/usr/local/bin/claude")).unwrap();
        assert!(out.contains("[tools.my-claude]"), "{out}");
        assert!(!out.contains("[tools]\n"), "stray bare header: {out}");
        let cfg = crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert_eq!(
            cfg.tools["my-claude"].command.as_deref(),
            Some("/usr/local/bin/claude")
        );
    }

    const DOC: &str = r#"# my config
version = 1

[log]
level = "info" # keep me

[[repositories]]
name = "a"
path = "/a"

[[workflows]]
name = "w"
projects = ["p"]
agent = "herdr"
profile = "implement"
trigger = { status = "Todo" }
"#;

    #[test]
    fn set_path_replaces_a_scalar_and_keeps_its_comment() {
        let out = set_path(DOC, "/log/level", &serde_json::json!("debug")).unwrap();
        assert!(out.contains(r#"level = "debug" # keep me"#), "{out}");
        assert!(out.starts_with("# my config"), "{out}");
    }

    #[test]
    fn set_path_creates_missing_tables() {
        let out = set_path(DOC, "/worktree/git_timeout_secs", &serde_json::json!(60)).unwrap();
        let cfg = crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert_eq!(cfg.worktree.git_timeout_secs, Some(60));
        assert!(out.contains("[worktree]"), "{out}");
    }

    #[test]
    fn set_path_indexes_and_appends_arrays_of_tables() {
        let out = set_path(DOC, "/repositories/0/tool", &serde_json::json!("codex")).unwrap();
        assert!(out.contains(r#"tool = "codex""#), "{out}");
        let out = set_path(
            &out,
            "/repositories/-",
            &serde_json::json!({ "name": "b", "path": "/b" }),
        )
        .unwrap();
        let cfg = crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert_eq!(cfg.repositories.len(), 2);
        assert_eq!(cfg.repositories[1].name, "b");
        let err = set_path(&out, "/repositories/5", &serde_json::json!({ "name": "c" }));
        assert!(matches!(err, Err(EditError::Path { .. })), "{err:?}");
    }

    #[test]
    fn set_path_keeps_an_inline_table_inline() {
        let out = set_path(
            DOC,
            "/workflows/0/trigger",
            &serde_json::json!({ "status": "Ready" }),
        )
        .unwrap();
        assert!(out.contains(r#"trigger = { status = "Ready" }"#), "{out}");
        let out = set_path(
            DOC,
            "/workflows/0/trigger/status",
            &serde_json::json!("Doing"),
        )
        .unwrap();
        assert!(out.contains(r#"status = "Doing""#), "{out}");
    }

    #[test]
    fn set_path_creates_a_missing_array_only_for_an_append() {
        let out = set_path(
            "version = 1\n",
            "/repositories/-",
            &serde_json::json!({ "name": "a", "path": "/a" }),
        )
        .unwrap();
        assert!(out.contains("[[repositories]]"), "{out}");
        let cfg = crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert_eq!(cfg.repositories[0].name, "a");
    }

    /// A number is an ordinary key on a table, and `~1` / dots make any key
    /// addressable — tool and plugin names may contain both (Copilot on #845).
    #[test]
    fn set_path_numbers_and_dots_are_keys_on_a_table() {
        let out = set_path(
            "version = 1\n",
            "/tools/123/kind",
            &serde_json::json!("claude"),
        )
        .unwrap();
        let out = set_path(&out, "/tools/my.tool/kind", &serde_json::json!("codex")).unwrap();
        let out = set_path(&out, "/tools/a~1b/kind", &serde_json::json!("opencode")).unwrap();
        let cfg = crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert!(cfg.tools.contains_key("123"), "{out}");
        assert!(cfg.tools.contains_key("my.tool"), "{out}");
        assert!(cfg.tools.contains_key("a/b"), "{out}");
    }

    #[test]
    fn set_path_refuses_null_and_empty_segments() {
        assert!(set_path(DOC, "/log/level", &serde_json::Value::Null).is_err());
        assert!(set_path(DOC, "/log//level", &serde_json::json!("x")).is_err());
        assert!(set_path(DOC, "log/level", &serde_json::json!("x")).is_err());
    }

    #[test]
    fn unset_path_removes_keys_and_elements_and_ignores_absent_ones() {
        let out = unset_path(DOC, "/log/level").unwrap();
        assert!(!out.contains("level"), "{out}");
        let out = unset_path(DOC, "/repositories/0").unwrap();
        let cfg = crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert!(cfg.repositories.is_empty());
        assert_eq!(unset_path(DOC, "/nope/deeper").unwrap(), DOC);
    }
}
