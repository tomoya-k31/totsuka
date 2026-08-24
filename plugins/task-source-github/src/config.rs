//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `[github]` table of `config.toml` as JSON with secrets already
//! expanded (F-65, #554).

use std::collections::HashMap;
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

/// One ProjectsV2 board this plugin polls, and the repositories it is the
/// tracker for (`[[projects]]`, #542).
///
/// Boards are a list rather than one-plugin-instance-per-board: the instance
/// route would need `name ≠ bin name`, which is the relaxation
/// [ADR-0027](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0027-plugin-artifact-naming.md)
/// refused, and it would fork `[[workflows]].source` per board too.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// Project owner login (user or org).
    pub owner: String,
    /// Whether `owner` is a user or an organization.
    #[serde(default)]
    pub owner_type: OwnerType,
    /// ProjectsV2 number under `owner`.
    pub project_number: i64,
    /// The repositories this board is the tracker for.
    ///
    /// Two jobs in one list, and they are not separable. It is the ingest
    /// filter (an issue from a repository not listed here is skipped) *and*
    /// the forward mapping repository → board that
    /// [`GithubConfig::claimed_repos`] publishes. Required and non-empty for
    /// the second reason: an omitted list used to mean "every repo on the
    /// board", which cannot be turned into claims — the board does not know
    /// its own future repositories.
    pub repos: Vec<String>,
    /// The Status option a triage-filed item should land in (#548 follow-up).
    ///
    /// Absent means the item is added with **no** Status. That leaves a
    /// human-triage gate *when every workflow on this source filters by
    /// status*: a status-less item matches no `project_status` condition —
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

impl ProjectConfig {
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
    /// The boards this plugin polls (`[[projects]]`, #542). Required and
    /// non-empty; `static_config_errors` rejects an empty list.
    ///
    /// This replaced the flat `owner` / `owner_type` / `project_number` /
    /// `repos` keys outright. `deny_unknown_fields` makes a pre-#542 config a
    /// hard `initialize` failure, which is the intended outcome — see
    /// [ADR-0056](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0056-multi-tracker-routing.md).
    pub projects: Vec<ProjectConfig>,
    /// SingleSelect field name holding the status column (F-02).
    #[serde(default = "default_status_field")]
    pub status_field: String,
    /// The operator's own login, used to detect self-assigned tasks (F-08).
    pub github_login: String,
    /// Status names treated as "in progress" and therefore excluded from
    /// ingest (F-08).
    #[serde(default)]
    pub in_progress_statuses: Vec<String>,
    /// Maps an orchestrator-side status name to the project's SingleSelect
    /// option name for `task/update_status` (F-84). Identity when absent.
    #[serde(default)]
    pub status_map: HashMap<String, String>,
    /// The plugin instance name stamped onto each `Task.source`.
    #[serde(default = "default_source_name")]
    pub source_name: String,
    /// GraphQL endpoint (overridable for GitHub Enterprise / tests).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// Max retry attempts for retryable API failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Instruction text overrides (#398). Every key falls back to the embedded
    /// default when omitted.
    #[serde(default)]
    pub prompts: GithubPrompts,
}

impl GithubConfig {
    /// Resolve the project option name for an orchestrator status via
    /// [`status_map`](Self::status_map), falling back to the name itself.
    pub fn map_status<'a>(&'a self, status: &'a str) -> &'a str {
        self.status_map.get(status).map_or(status, String::as_str)
    }

    /// Whether a task with these `assignees` may be ingested by this operator
    /// (F-08): unassigned, or `github_login` is among the assignees. A task
    /// assigned only to other people is skipped. Matching is over the whole
    /// list, so ingest never depends on GitHub's assignee ordering.
    pub fn assignable_to_me(&self, assignees: &[&str]) -> bool {
        assignees.is_empty()
            || assignees
                .iter()
                .any(|login| login.eq_ignore_ascii_case(&self.github_login))
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
    /// A repository listed on two boards appears **twice**, and this does not
    /// deduplicate. `static_config_errors` rejects such a config, but only
    /// `config/validate` calls it — `initialize` does not, so a config the
    /// operator never validated reaches here intact. The Orchestrator's own
    /// cross-source check is what sees the duplicate either way, and it keeps
    /// the first claim; dropping one here would hide the conflict from it.
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
        serde_json::from_value(json).unwrap()
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
            "projects": [{ "owner": "me", "project_number": 1, "repos": ["r"] }],
            "typo_field": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    #[test]
    fn unknown_field_inside_a_project_entry_is_rejected() {
        let err = serde_json::from_value::<GithubConfig>(serde_json::json!({
            "token": "t", "github_login": "me",
            "projects": [{
                "owner": "me", "project_number": 1, "repos": ["r"], "statusField": "Status"
            }]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("statusField"), "got {err}");
    }

    #[test]
    fn ingest_gating_helpers() {
        let cfg = parse(serde_json::json!({
            "token": "t", "github_login": "Me",
            "in_progress_statuses": ["In Progress"],
            "projects": [{ "owner": "me", "project_number": 1, "repos": ["totsuka"] }]
        }));
        // Self-detection is case-insensitive; others are excluded; and I count
        // as assigned even when I am not the first assignee.
        assert!(cfg.assignable_to_me(&[]));
        assert!(cfg.assignable_to_me(&["me"]));
        assert!(cfg.assignable_to_me(&["another-dev", "me"]));
        assert!(!cfg.assignable_to_me(&["someone-else"]));
        assert!(!cfg.assignable_to_me(&["a", "b"]));
        assert!(cfg.is_in_progress("In Progress"));
        assert!(!cfg.is_in_progress("Todo"));
        // The repository filter is per board now (#542), and there is no
        // "empty means any" arm left: `repos` is required and non-empty.
        assert!(cfg.projects[0].repo_allowed("totsuka"));
        assert!(!cfg.projects[0].repo_allowed("other"));
    }

    #[test]
    fn status_map_falls_back_to_identity() {
        let cfg = parse(serde_json::json!({
            "token": "t", "github_login": "me",
            "projects": [{ "owner": "me", "project_number": 1, "repos": ["r"] }],
            "status_map": { "レビュー待ち": "In Review" }
        }));
        assert_eq!(cfg.map_status("レビュー待ち"), "In Review");
        assert_eq!(cfg.map_status("実装待ち"), "実装待ち");
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
