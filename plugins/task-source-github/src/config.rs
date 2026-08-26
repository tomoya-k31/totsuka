//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `[github]` table of `config.toml` as JSON with secrets already
//! expanded (F-65, #554).

use std::sync::LazyLock;

use plugin_protocol::methods::ClaimedRepo;
use serde::Deserialize;

/// The embedded instruction defaults, parsed once on first use.
///
/// A malformed `defaults.toml` is an authoring error in a file that ships
/// inside the binary — no input can change it — so this panics rather than
/// degrading. `embedded_defaults_parse` forces it in CI instead of at
/// `initialize`.
static DEFAULTS: LazyLock<EmbeddedPrompts> = LazyLock::new(|| {
    toml::from_str::<Defaults>(include_str!("defaults.toml"))
        .expect("embedded defaults.toml must parse")
        .prompts
});

/// Top level of `defaults.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    prompts: EmbeddedPrompts,
}

/// The embedded defaults, with **every field required**.
///
/// A separate type from [`GithubPrompts`], and the duplication is the point —
/// the same trap the Slack plugin documents. `GithubPrompts` fills omitted keys
/// from `DEFAULTS`; if `DEFAULTS` were also a `GithubPrompts`, deleting a key
/// from `defaults.toml` would make its `#[serde(default)]` read `DEFAULTS`
/// **while `DEFAULTS` is still initialising** — a re-entrant `LazyLock`, which
/// **deadlocks rather than panicking**. The symptom would be a CI job hanging
/// to its timeout instead of a test failing with a readable message.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedPrompts {
    triage_instructions: String,
    design_instructions: String,
    implement_instructions: String,
}

/// Instruction text this plugin sends with each task (#398, epic #311).
///
/// Built-in values live in the embedded `defaults.toml`, not in Rust string
/// literals, so rewording is a data edit. Field names are the config keys under
/// `[github.prompts]` in config.toml.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubPrompts {
    /// Sent when the workflow's profile is `triage`.
    #[serde(default = "default_triage_instructions")]
    pub triage_instructions: String,
    /// Sent when the workflow's profile is `design`.
    #[serde(default = "default_design_instructions")]
    pub design_instructions: String,
    /// Sent when the workflow's profile is `implement`.
    #[serde(default = "default_implement_instructions")]
    pub implement_instructions: String,
}

fn default_triage_instructions() -> String {
    DEFAULTS.triage_instructions.clone()
}
fn default_design_instructions() -> String {
    DEFAULTS.design_instructions.clone()
}
fn default_implement_instructions() -> String {
    DEFAULTS.implement_instructions.clone()
}

impl Default for GithubPrompts {
    fn default() -> Self {
        Self {
            triage_instructions: default_triage_instructions(),
            design_instructions: default_design_instructions(),
            implement_instructions: default_implement_instructions(),
        }
    }
}

impl GithubPrompts {
    /// The template for `kind`, or `None` when the Orchestrator sent no
    /// `instructions_kind` — or sent one this plugin has no text for.
    ///
    /// An unknown kind returns `None` rather than falling back to a default:
    /// guessing which instruction a future profile wants would put the agent to
    /// work on the wrong deliverable, which is worse than dispatching it with
    /// the instructions it had before (#398 predates any such profile, so this
    /// arm is reachable only from a newer core).
    pub fn for_kind(&self, kind: &str) -> Option<&str> {
        match kind {
            "triage" => Some(&self.triage_instructions),
            "design" => Some(&self.design_instructions),
            "implement" => Some(&self.implement_instructions),
            _ => None,
        }
    }
}

/// Whether the project owner is a user or an organization (GraphQL requires
/// choosing the right root field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OwnerType {
    /// A user account (`user(login:)`).
    #[default]
    User,
    /// An organization (`organization(login:)`).
    Organization,
}

impl OwnerType {
    /// The GraphQL root field selecting this owner's `projectV2`.
    pub fn graphql_root(self) -> &'static str {
        match self {
            OwnerType::User => "user",
            OwnerType::Organization => "organization",
        }
    }
}

/// The options on one `[[projects]]` entry, as this plugin reads them.
///
/// `name` and `source` are the Orchestrator's keys and never reach here;
/// `deny_unknown_fields` is what turns a typo in the rest into an
/// `initialize` failure instead of a setting that quietly does nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOptions {
    /// Project owner login (user or org).
    pub owner: String,
    /// Whether `owner` is a user or an organization.
    #[serde(default)]
    pub owner_type: OwnerType,
    /// ProjectsV2 number under `owner`.
    pub project_number: i64,
    /// The Status option a triage-filed item should land in (#548 follow-up).
    ///
    /// Absent means the item is added with **no** Status. That leaves a
    /// human-triage gate *when every workflow on this source filters by
    /// status*: a status-less item matches no `status` condition —
    /// but a trigger **without** one matches everything, status-less items
    /// included, so the gate is only as real as the operator's triggers.
    /// **Setting this to a value some trigger polls (e.g. `Todo`) removes
    /// the gate outright** — filing then flows straight into an unattended
    /// run. That can be exactly what the operator wants; it just should not
    /// happen by accident, which is why the default is "no status".
    ///
    /// Per board, not top-level: option names belong to a board.
    #[serde(default)]
    pub triage_status: Option<String>,
}

/// One board this plugin polls: its `[[projects]]` entry plus the
/// repositories bound to it.
///
/// The repositories are **derived** (#554): they are the `[[repositories]]`
/// entries whose `project` names this board, which the Orchestrator supplies
/// on `RepoInfo`. Until then the list lived here, written by hand, doing two
/// jobs at once — the ingest filter and the repository → board mapping
/// ([ADR-0056](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0056-multi-tracker-routing.md)).
/// Deriving it separates them without duplicating anything: one binding, read
/// two ways.
#[derive(Debug, Clone)]
pub struct ProjectConfig {
    /// The entry's `name` — what `[[repositories]].project` points at.
    pub name: String,
    /// Project owner login (user or org).
    pub owner: String,
    /// Whether `owner` is a user or an organization.
    pub owner_type: OwnerType,
    /// ProjectsV2 number under `owner`.
    pub project_number: i64,
    /// The Status option a triage-filed item lands in.
    pub triage_status: Option<String>,
    /// The repositories bound to this board, in `[[repositories]]` order.
    pub repos: Vec<String>,
}

impl ProjectConfig {
    /// A board built directly, for tests and for callers that already have the
    /// pieces. Production builds these through [`resolve`](Self::resolve).
    pub fn new(name: &str, owner: &str, project_number: i64, repos: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            owner: owner.to_string(),
            owner_type: OwnerType::default(),
            project_number,
            triage_status: None,
            repos: repos.iter().map(|r| (*r).to_string()).collect(),
        }
    }

    /// Build the boards from what `initialize` supplied.
    ///
    /// A board with no repositories bound to it is kept, not dropped: it
    /// polls nothing and claims nothing, which is what the config says, and
    /// `static_config_errors` is where that is reported as a mistake.
    pub fn resolve(
        projects: &[plugin_protocol::methods::ProjectInfo],
        repositories: &[plugin_protocol::methods::RepoInfo],
    ) -> Result<Vec<Self>, Vec<String>> {
        let mut out = Vec::new();
        let mut errors = Vec::new();
        for info in projects {
            let options: ProjectOptions =
                match serde_json::from_value(serde_json::Value::Object(info.options.clone())) {
                    Ok(o) => o,
                    Err(e) => {
                        errors.push(format!("project `{}`: {e}", info.name));
                        continue;
                    }
                };
            out.push(Self {
                name: info.name.clone(),
                owner: options.owner,
                owner_type: options.owner_type,
                project_number: options.project_number,
                triage_status: options.triage_status,
                repos: repositories
                    .iter()
                    .filter(|r| r.project.as_deref() == Some(info.name.as_str()))
                    .map(|r| r.name.clone())
                    .collect(),
            });
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    /// Whether `repo` is one this board tracks.
    pub fn repo_allowed(&self, repo: &str) -> bool {
        self.repos.iter().any(|r| r == repo)
    }

    /// Prose telling an agent where an item for this board goes (#542).
    ///
    /// Read by nothing in this plugin: it travels in `claimed_repos` and ends
    /// up in a triage agent's prompt, so it is written as an instruction, not
    /// as a description.
    ///
    /// `status_field` is the operator's name for the status column
    /// ([`GithubConfig::status_field`], shared across boards) — passed in
    /// rather than hard-coding `Status`, because an instruction naming a
    /// column the board does not have sends the agent editing the wrong
    /// field, or reporting that it could not find it.
    pub fn destination(&self, status_field: &str) -> String {
        let mut destination = format!(
            "GitHub Project #{} owned by the {} `{}`. \
             File the issue in the repository itself, then add it to that board with \
             `gh project item-add {} --owner {} --url <issue-url>`.",
            self.project_number,
            match self.owner_type {
                OwnerType::User => "user",
                OwnerType::Organization => "organization",
            },
            self.owner,
            self.project_number,
            self.owner,
        );
        if let Some(status) = &self.triage_status {
            // The concrete command sequence, because there is no one-shot CLI
            // for this: `gh project item-edit` wants raw ids. Naming the
            // steps is the difference between the agent doing it and the
            // agent reporting that it could not find out how.
            destination.push_str(&format!(
                " Then set the new item's `{status_field}` to `{status}`: resolve the ids with \
                 `gh project item-list {} --owner {} --format json` and \
                 `gh project field-list {} --owner {} --format json`, then apply with \
                 `gh project item-edit --project-id <project-id> --id <item-id> \
                 --field-id <status-field-id> --single-select-option-id <option-id>`.",
                self.project_number, self.owner, self.project_number, self.owner,
            ));
        }
        destination
    }
}

/// GitHub task-source settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    /// API token (resolved by the orchestrator, F-65). Never touched by us
    /// beyond sending it as a bearer token.
    pub token: String,
    /// The boards this plugin polls.
    ///
    /// **Not read from `[github]`** (#554): the entries live in the
    /// Orchestrator's `[[projects]]`, and their repositories in
    /// `[[repositories]].project`. Filled in at `initialize` by
    /// [`ProjectConfig::resolve`]; `deny_unknown_fields` on the surrounding
    /// struct is what rejects a `projects = [...]` left over in `[github]`.
    #[serde(skip)]
    pub projects: Vec<ProjectConfig>,
    /// SingleSelect field name holding the status column (F-02).
    #[serde(default = "default_status_field")]
    pub status_field: String,
    /// The operator's own login: detects self-assigned tasks (F-08) and is
    /// the login the claim self-assigns (#556). One login = one totsuka
    /// instance — assignees carry only the login, so two instances sharing
    /// one are indistinguishable to the adjudication (unsupported).
    pub github_login: String,
    /// Status names treated as "in progress" and therefore excluded from
    /// ingest (F-08).
    #[serde(default)]
    pub in_progress_statuses: Vec<String>,
    /// The plugin instance name stamped onto each `Task.source`.
    #[serde(default = "default_source_name")]
    pub source_name: String,
    /// GraphQL endpoint (overridable for GitHub Enterprise / tests).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// Max retry attempts for retryable API failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Internal fetch cadence of the poll loop, in seconds (F-06). Moved
    /// here from `[plugins.github]` in 0.6.0 (#554): the Orchestrator only
    /// ever forwarded it, so it is this plugin's own key. `0` is treated as
    /// unset (busy-spin guard, applied in the server).
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
    /// Milliseconds to wait between writing the exclusion claim and reading
    /// it back (#556). The read-back is what detects both the race and the
    /// silently-ignored write, so it must not run before the API shows the
    /// mutation. Default 750 — measured p95 ≈ 700ms / max 983ms (#556
    /// Phase 0). `0` is honoured (no wait): useful for tests, harmless in
    /// production because a too-early read only costs one extra retry.
    #[serde(default)]
    pub claim_verify_delay_ms: Option<u64>,
    /// Instruction text overrides (#398). Every key falls back to the embedded
    /// default when omitted.
    #[serde(default)]
    pub prompts: GithubPrompts,
}

impl GithubConfig {
    /// The claim read-back delay (#556): configured, or the measured default.
    pub fn claim_verify_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.claim_verify_delay_ms.unwrap_or(750))
    }

    /// Whether `status` is an "in progress" column excluded from ingest (F-08).
    pub fn is_in_progress(&self, status: &str) -> bool {
        self.in_progress_statuses.iter().any(|s| s == status)
    }

    /// The repositories this plugin is the tracker for, and where an item for
    /// each goes (`InitializeResult.claimed_repos`, protocol 0.5.1, #542).
    ///
    /// One entry per (board, repo) pair, in config order.
    ///
    /// A repository can no longer appear twice: it names **one** board
    /// (#554), so the duplicate this used to have to reason about is
    /// unrepresentable rather than detected.
    pub fn claimed_repos(&self) -> Vec<ClaimedRepo> {
        self.projects
            .iter()
            .flat_map(|project| {
                let destination = project.destination(&self.status_field);
                project.repos.iter().map(move |repo| ClaimedRepo {
                    repo: repo.clone(),
                    destination: destination.clone(),
                })
            })
            .collect()
    }
}

/// Build a config the way `initialize` does, for tests: the `[github]` table
/// minus the boards, plus the boards resolved from `[[projects]]` /
/// `[[repositories]]` (#554).
///
/// Tests write the board inline as `"projects": [{ name, owner,
/// project_number, repos }]` — the same information, in one literal — and this
/// splits it into the two lists the Orchestrator actually sends, so the test
/// exercises [`ProjectConfig::resolve`] rather than bypassing it.
#[cfg(test)]
pub(crate) fn config_from_json(mut value: serde_json::Value) -> GithubConfig {
    use plugin_protocol::methods::{ProjectInfo, RepoInfo};

    let boards = value
        .as_object_mut()
        .and_then(|o| o.remove("projects"))
        .and_then(|p| p.as_array().cloned())
        .unwrap_or_default();
    let mut projects = Vec::new();
    let mut repositories = Vec::new();
    for (i, board) in boards.iter().enumerate() {
        let mut options = board.as_object().cloned().unwrap_or_default();
        let repos = options.remove("repos").unwrap_or(serde_json::json!([]));
        let name = options
            .remove("name")
            .and_then(|n| n.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("board-{i}"));
        for repo in repos.as_array().cloned().unwrap_or_default() {
            repositories.push(RepoInfo {
                name: repo.as_str().unwrap_or_default().to_string(),
                summary: None,
                path: None,
                project: Some(name.clone()),
            });
        }
        projects.push(ProjectInfo { name, options });
    }
    let mut config: GithubConfig = serde_json::from_value(value).expect("github config");
    config.projects = ProjectConfig::resolve(&projects, &repositories).expect("boards resolve");
    config
}

fn default_status_field() -> String {
    "Status".to_string()
}
fn default_source_name() -> String {
    "github".to_string()
}
fn default_api_url() -> String {
    "https://api.github.com/graphql".to_string()
}
fn default_max_retries() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> GithubConfig {
        config_from_json(json)
    }

    #[test]
    fn minimal_config_applies_defaults() {
        let cfg = parse(serde_json::json!({
            "token": "t", "github_login": "me",
            "projects": [{ "owner": "me", "project_number": 1, "repos": ["totsuka"] }]
        }));
        assert_eq!(cfg.status_field, "Status");
        assert_eq!(cfg.source_name, "github");
        assert_eq!(cfg.api_url, "https://api.github.com/graphql");
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.projects[0].owner_type, OwnerType::User);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = serde_json::from_value::<GithubConfig>(serde_json::json!({
            "token": "t", "github_login": "me",
            "typo_field": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    /// A `projects = [...]` left in `[github]` is now an unknown field: the
    /// boards moved to the Orchestrator's top-level `[[projects]]` (#554), and
    /// accepting the old spelling would leave a table that reads as configured
    /// and is never consulted.
    #[test]
    fn the_old_projects_key_in_the_plugin_table_is_rejected() {
        let err = serde_json::from_value::<GithubConfig>(serde_json::json!({
            "token": "t", "github_login": "me",
            "projects": [{ "owner": "me", "project_number": 1, "repos": ["r"] }]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("projects"), "got {err}");
    }

    #[test]
    fn unknown_field_inside_a_project_entry_is_rejected() {
        let errors = ProjectConfig::resolve(
            &[plugin_protocol::methods::ProjectInfo {
                name: "board".into(),
                options: serde_json::json!({
                    "owner": "me", "project_number": 1, "statusField": "Status"
                })
                .as_object()
                .unwrap()
                .clone(),
            }],
            &[],
        )
        .unwrap_err();
        assert!(errors[0].contains("statusField"), "got {errors:?}");
        assert!(errors[0].contains("board"), "got {errors:?}");
    }

    #[test]
    fn ingest_gating_helpers() {
        let cfg = parse(serde_json::json!({
            "token": "t", "github_login": "Me",
            "in_progress_statuses": ["In Progress"],
            "projects": [{ "owner": "me", "project_number": 1, "repos": ["totsuka"] }]
        }));
        // The assignee gate moved into the trigger (#572); what is left here
        // is the gating a workflow does not state.
        assert!(cfg.is_in_progress("In Progress"));
        assert!(!cfg.is_in_progress("Todo"));
        // The repository filter is per board now (#542), and there is no
        // "empty means any" arm left: `repos` is required and non-empty.
        assert!(cfg.projects[0].repo_allowed("totsuka"));
        assert!(!cfg.projects[0].repo_allowed("other"));
    }

    /// The destination lands in an agent's prompt, so it must read as prose:
    /// no run of spaces from a line continuation written without its
    /// backslash, and no newline. Checked on the rendered value, which is what
    /// the agent actually sees — reading the literal is how the notion plugin's
    /// copy of this string shipped with a 14-space gap in it.
    #[test]
    fn the_destination_reads_as_one_paragraph() {
        let cfg = parse(serde_json::json!({
            "token": "t", "github_login": "me",
            "projects": [{ "owner": "me", "project_number": 7, "repos": ["totsuka"] }]
        }));
        let destination = &cfg.claimed_repos()[0].destination;
        assert!(
            !destination.contains("  ") && !destination.contains('\n'),
            "{destination:?}"
        );
    }

    /// `triage_status` puts the concrete follow-up commands into the
    /// destination; absent leaves the item status-less (the human-triage
    /// gate), so the instruction must not appear at all.
    #[test]
    fn triage_status_lands_in_the_destination_only_when_set() {
        // A non-default `status_field`, so the test fails if the prose ever
        // hard-codes `Status` again: an instruction naming a column the board
        // does not have sends the agent editing the wrong field.
        let with = parse(serde_json::json!({
            "token": "t", "github_login": "me", "status_field": "状態",
            "projects": [{ "owner": "me", "project_number": 7,
                           "repos": ["totsuka"], "triage_status": "📥 Inbox" }]
        }));
        let destination = &with.claimed_repos()[0].destination;
        assert!(destination.contains("`📥 Inbox`"), "{destination}");
        assert!(destination.contains("`状態`"), "{destination}");
        assert!(!destination.contains("`Status`"), "{destination}");
        assert!(destination.contains("item-edit"), "{destination}");
        // Still one readable paragraph (the prose lands in a prompt).
        assert!(
            !destination.contains("  ") && !destination.contains('\n'),
            "{destination:?}"
        );

        let without = parse(serde_json::json!({
            "token": "t", "github_login": "me",
            "projects": [{ "owner": "me", "project_number": 7, "repos": ["totsuka"] }]
        }));
        let destination = &without.claimed_repos()[0].destination;
        assert!(!destination.contains("Status"), "{destination}");
    }

    #[test]
    fn claims_one_entry_per_board_and_repo_pair() {
        let cfg = parse(serde_json::json!({
            "token": "t", "github_login": "me",
            "projects": [
                { "owner": "me", "project_number": 7, "repos": ["totsuka", "dotfiles"] },
                { "owner": "acme", "owner_type": "organization",
                  "project_number": 3, "repos": ["web-app"] }
            ]
        }));
        let claims = cfg.claimed_repos();
        let repos: Vec<&str> = claims.iter().map(|c| c.repo.as_str()).collect();
        assert_eq!(repos, ["totsuka", "dotfiles", "web-app"]);
        // Repos on the same board share one destination; different boards must
        // not — a claim naming the wrong board sends the agent to the wrong
        // place, and nothing downstream can tell.
        assert_eq!(claims[0].destination, claims[1].destination);
        assert_ne!(claims[0].destination, claims[2].destination);
        assert!(
            claims[0].destination.contains("#7") && claims[0].destination.contains("user `me`"),
            "got {}",
            claims[0].destination
        );
        assert!(
            claims[2].destination.contains("#3")
                && claims[2].destination.contains("organization `acme`"),
            "got {}",
            claims[2].destination
        );
    }
}
