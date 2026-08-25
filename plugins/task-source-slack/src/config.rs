//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `[slack]` table of `config.toml` as JSON with secrets already expanded
//! (F-65, #554).
//!
//! Slack tokens involved: the App-Level Token (`xapp-`) opens the Socket
//! Mode WebSocket, and the *user* token (`xoxp-`) calls the Web API so
//! replies are posted under the operator's own name. The optional *bot*
//! token (`xoxb-`) powers only the notification nudge DM (#305) — never a
//! reply.

use std::sync::LazyLock;

use serde::Deserialize;

/// The OpenAI-compatible LLM used for repository classification when channel
/// rules leave more than one candidate. This is the plugin's own LLM call
/// (repo resolution happens entirely inside this plugin; tasks are submitted
/// with a resolved `repo_hint`). An explicit `[llm]` here always wins; when
/// omitted, the orchestrator's `[llm]` (supplied at `initialize` since
/// protocol 0.1.2, #119) is adopted as the default — see `server`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    /// OpenAI-compatible base URL (e.g. `https://openrouter.ai/api/v1`).
    pub base_url: String,
    /// Model identifier.
    pub model: String,
    /// API key (resolved by the orchestrator, F-65).
    pub api_key: String,
    /// Minimum classification confidence; below it the plugin falls back to
    /// asking in-thread via an ephemeral message.
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
}

/// A channel-name prefix rule narrowing repository candidates (first match in
/// declaration order wins).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelGroup {
    /// Channel-name prefix (e.g. `dev-frontend-`).
    pub prefix: String,
    /// Candidate repository names; each must exist in [`SlackConfig::repos`].
    pub repos: Vec<String>,
}

/// A candidate repository the plugin may resolve a mention to. `name` must
/// match a `[[repositories]].name` in the orchestrator's `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoInfo {
    /// Repository name (as known to the orchestrator).
    pub name: String,
    /// One-line description fed to the LLM classifier.
    #[serde(default)]
    pub summary: Option<String>,
    /// Local checkout path; when set, the README head is added as classifier
    /// material.
    #[serde(default)]
    pub path: Option<String>,
}

/// The embedded prompt defaults, parsed once on first use.
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
/// A separate type from [`SlackPrompts`] on purpose, and the duplication is the
/// point. `SlackPrompts` fills omitted keys from `DEFAULTS`; if `DEFAULTS` were
/// also a `SlackPrompts`, then deleting a key from `defaults.toml` would make
/// its `#[serde(default)]` read `DEFAULTS` **while `DEFAULTS` is still
/// initialising** — a re-entrant `LazyLock`, which **deadlocks rather than
/// panicking**. The failure would be a CI job hanging to its timeout instead of
/// a test failing with a readable message.
///
/// With the fields required here, a deleted key is an ordinary "missing field"
/// parse error that `embedded_defaults_parse` reports immediately. (A
/// *misspelled* key was always safe — `deny_unknown_fields` catches it — but
/// genuine absence was not.)
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedPrompts {
    reply_instructions: String,
    implement_instructions: String,
    triage_instructions: String,
    reply_style_suffix: String,
    body_template: String,
    body_thread_header: String,
    body_thread_line: String,
    body_thread_unavailable: String,
    classifier_system: String,
    classifier_user: String,
    classifier_correction: String,
}

/// Prompt text this plugin sends to the model (#318, epic #311).
///
/// Built-in values live in the embedded `defaults.toml`, not in Rust string
/// literals, so rewording is a data edit. Every field is overridden per
/// installation under `[slack.prompts]` in `config.toml`, and the field names
/// here are the config keys.
///
/// Everything here is LLM-facing: a bad override degrades classification or
/// produces a weaker draft, but cannot break completion detection the way the
/// core prompts can.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackPrompts {
    /// Reply-crafting directions carried as `Task.instructions`.
    #[serde(default = "default_reply_instructions")]
    pub reply_instructions: String,
    /// Sent instead of [`reply_instructions`](Self::reply_instructions) when
    /// the matched workflow's profile is `implement` (#397/#398).
    #[serde(default = "default_implement_instructions")]
    pub implement_instructions: String,
    /// Sent instead of [`reply_instructions`](Self::reply_instructions) when
    /// the matched workflow's profile is `triage` (#450) — the `:books:` flow
    /// files an issue instead of answering or implementing.
    #[serde(default = "default_triage_instructions")]
    pub triage_instructions: String,
    /// Appended to [`reply_instructions`](Self::reply_instructions) only when
    /// [`SlackConfig::reply_style`] is set. Placeholder: `{style}`.
    #[serde(default = "default_reply_style_suffix")]
    pub reply_style_suffix: String,
    /// The visible task body. Placeholders: `{sender}` `{channel}` `{text}`.
    #[serde(default = "default_body_template")]
    pub body_template: String,
    /// Thread-context section header. Placeholder: `{count}`.
    #[serde(default = "default_body_thread_header")]
    pub body_thread_header: String,
    /// One thread-context line. Placeholder: `{line}`.
    #[serde(default = "default_body_thread_line")]
    pub body_thread_line: String,
    /// Emitted instead of the thread-context section when the fetch failed.
    #[serde(default = "default_body_thread_unavailable")]
    pub body_thread_unavailable: String,
    /// Classifier system prompt. Placeholder: `{repo_names}`.
    #[serde(default = "default_classifier_system")]
    pub classifier_system: String,
    /// Classifier user message. Placeholders: `{mention_text}`
    /// `{thread_context}` `{catalog}`.
    #[serde(default = "default_classifier_user")]
    pub classifier_user: String,
    /// Retry turn after a malformed answer.
    #[serde(default = "default_classifier_correction")]
    pub classifier_correction: String,
}

impl Default for SlackPrompts {
    fn default() -> Self {
        Self {
            reply_instructions: DEFAULTS.reply_instructions.clone(),
            implement_instructions: DEFAULTS.implement_instructions.clone(),
            triage_instructions: DEFAULTS.triage_instructions.clone(),
            reply_style_suffix: DEFAULTS.reply_style_suffix.clone(),
            body_template: DEFAULTS.body_template.clone(),
            body_thread_header: DEFAULTS.body_thread_header.clone(),
            body_thread_line: DEFAULTS.body_thread_line.clone(),
            body_thread_unavailable: DEFAULTS.body_thread_unavailable.clone(),
            classifier_system: DEFAULTS.classifier_system.clone(),
            classifier_user: DEFAULTS.classifier_user.clone(),
            classifier_correction: DEFAULTS.classifier_correction.clone(),
        }
    }
}

impl SlackPrompts {
    /// Placeholders each key may reference. Used to warn at `initialize` about
    /// tokens an overridden template names but nothing supplies.
    const ALLOWED: &'static [(&'static str, &'static [&'static str])] = &[
        ("reply_instructions", &[]),
        // The two profile-selected variants take no placeholders either — the
        // deliverable and its URL requirement are stated in prose.
        ("implement_instructions", &[]),
        ("triage_instructions", &[]),
        ("reply_style_suffix", &["style"]),
        ("body_template", &["sender", "channel", "text"]),
        ("body_thread_header", &["count"]),
        ("body_thread_line", &["line"]),
        ("body_thread_unavailable", &[]),
        ("classifier_system", &["repo_names"]),
        (
            "classifier_user",
            &["mention_text", "thread_context", "catalog"],
        ),
        ("classifier_correction", &[]),
    ];

    /// `(key, unknown placeholder)` pairs across every template.
    ///
    /// Advisory only — `server` logs it at `initialize`. The plugin *does*
    /// have a `config/validate` hook (`server::config_validate`), so this
    /// could be a hard error; it deliberately is not. Rendering keeps an
    /// unknown key verbatim, so the symptom is a visible `{token}` in a draft
    /// rather than a silent deletion, and every key here is LLM-facing. Core's
    /// `[prompts]` treats the same typo class as an error because there the
    /// deleted text is the completion-marker convention and the only symptom
    /// is tasks escalating on a timeout.
    pub fn unknown_placeholders(&self) -> Vec<(&'static str, String)> {
        let values: &[(&str, &String)] = &[
            ("reply_instructions", &self.reply_instructions),
            ("implement_instructions", &self.implement_instructions),
            ("triage_instructions", &self.triage_instructions),
            ("reply_style_suffix", &self.reply_style_suffix),
            ("body_template", &self.body_template),
            ("body_thread_header", &self.body_thread_header),
            ("body_thread_line", &self.body_thread_line),
            ("body_thread_unavailable", &self.body_thread_unavailable),
            ("classifier_system", &self.classifier_system),
            ("classifier_user", &self.classifier_user),
            ("classifier_correction", &self.classifier_correction),
        ];
        let mut out = Vec::new();
        for (key, value) in values {
            let allowed = Self::ALLOWED
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, a)| *a)
                .unwrap_or(&[]);
            for name in crate::template::scan(value) {
                let entry = (*key, name.to_string());
                // A template that repeats `{bogus}` should log once, not once
                // per occurrence.
                if !allowed.contains(&name) && !out.contains(&entry) {
                    out.push(entry);
                }
            }
        }
        out
    }
}

fn default_triage_instructions() -> String {
    DEFAULTS.triage_instructions.clone()
}

fn default_implement_instructions() -> String {
    DEFAULTS.implement_instructions.clone()
}

fn default_reply_instructions() -> String {
    DEFAULTS.reply_instructions.clone()
}
fn default_reply_style_suffix() -> String {
    DEFAULTS.reply_style_suffix.clone()
}
fn default_body_template() -> String {
    DEFAULTS.body_template.clone()
}
fn default_body_thread_header() -> String {
    DEFAULTS.body_thread_header.clone()
}
fn default_body_thread_line() -> String {
    DEFAULTS.body_thread_line.clone()
}
fn default_body_thread_unavailable() -> String {
    DEFAULTS.body_thread_unavailable.clone()
}
fn default_classifier_system() -> String {
    DEFAULTS.classifier_system.clone()
}
fn default_classifier_user() -> String {
    DEFAULTS.classifier_user.clone()
}
fn default_classifier_correction() -> String {
    DEFAULTS.classifier_correction.clone()
}

/// Slack task-source settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackConfig {
    /// App-Level Token (`xapp-`) for Socket Mode.
    pub app_token: String,
    /// User OAuth Token (`xoxp-`); replies are posted as the operator.
    pub user_token: String,
    /// Bot User OAuth Token (`xoxb-`); when set, the bot DMs the operator a
    /// notification nudge for drafts and pickers — surfaces that generate no
    /// Slack notification of their own. Absent = nudges disabled (#305).
    #[serde(default)]
    pub bot_token: Option<String>,
    /// The operator's own Slack user id (`U…`). Mentions of this user become
    /// tasks, and the TokenGuard refuses a token belonging to anyone else.
    pub target_user_id: String,
    /// How many recent thread messages to include as context.
    #[serde(default = "default_thread_context_limit")]
    pub thread_context_limit: u32,
    /// Optional tone/style instruction injected into the task body.
    #[serde(default)]
    pub reply_style: Option<String>,
    /// The plugin instance name stamped onto each `Task.source`.
    #[serde(default = "default_source_name")]
    pub source_name: String,
    /// Repository-selection LLM. Required when more than one repository
    /// candidate ends up declared (with a single candidate there is nothing
    /// to classify) — but optional since #119: when omitted, the
    /// orchestrator's `[llm]` (supplied at `initialize`) fills in.
    #[serde(default)]
    pub llm: Option<LlmConfig>,
    /// Channel-prefix rules, checked before the LLM (first match wins).
    #[serde(default)]
    pub channel_groups: Vec<ChannelGroup>,
    /// Candidate repositories. Optional since #109: when omitted, the
    /// orchestrator's `[[repositories]]` (supplied at `initialize`) become
    /// the candidates; an explicit list here always wins.
    #[serde(default)]
    pub repos: Vec<RepoInfo>,
    /// Slack Web API base URL (overridable for tests).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// State-directory root for the persisted draft store (#122), replacing
    /// `${XDG_STATE_HOME:-~/.local/state}/totsuka` (overridable for tests).
    #[serde(default)]
    pub state_dir: Option<std::path::PathBuf>,
    /// Max retry attempts for retryable API failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Prompt text overrides (#318). Every key falls back to the embedded
    /// default when omitted.
    #[serde(default)]
    pub prompts: SlackPrompts,
}

impl SlackConfig {
    /// The declared repository names.
    pub fn repo_names(&self) -> Vec<&str> {
        self.repos.iter().map(|r| r.name.as_str()).collect()
    }
}

/// Config keys this plugin has removed, paired with what to do instead.
///
/// [`SlackConfig`] is `deny_unknown_fields`, so simply deleting a field turns a
/// `slack.toml` that worked yesterday into `unknown field 'trigger_reactions',
/// expected one of ...` — loud, but it does not say the key was *removed* or
/// what replaced it. These pairs answer that, and [`removed_keys_in`] reports
/// them. Same shape as the herdr plugin's table (#411).
const REMOVED_KEYS: &[(&str, &str)] = &[(
    "trigger_reactions",
    "reaction triggers are declared as `[[workflows]].trigger = { reaction = \"<emoji>\" }` \
     in the orchestrator config since #396; define each one **above** the mention catch-all \
     (`trigger = {}` matches everything, so anything below it is unreachable)",
)];

/// The removed keys present in a raw plugin-config object, rendered as
/// operator-facing lines. Empty when the config is clean — including when it is
/// not an object at all, which is a different error and reported by serde.
pub fn removed_keys_in(config: &serde_json::Value) -> Vec<String> {
    let Some(map) = config.as_object() else {
        return Vec::new();
    };
    REMOVED_KEYS
        .iter()
        .filter(|(key, _)| map.contains_key(*key))
        .map(|(key, advice)| format!("`{key}` was removed: {advice}. Delete the key."))
        .collect()
}

/// Strip surrounding colons from each emoji name and drop the ones that are
/// left empty. Slack sends `reaction` without colons, but writing `":eyes:"`
/// in TOML is the natural thing to do, so both spellings must land on the
/// same key.
pub fn normalize_reactions(names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| name.trim().trim_matches(':').to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

fn default_thread_context_limit() -> u32 {
    6
}
fn default_source_name() -> String {
    "slack".to_string()
}
fn default_api_url() -> String {
    "https://slack.com/api".to_string()
}
fn default_max_retries() -> u32 {
    3
}
pub(crate) fn default_confidence_threshold() -> f64 {
    0.6
}

/// Static (offline) config problems for `config/validate` (F-63), each in the
/// "cause → next action" form (§7). Live token verification is *not* done
/// here — that is the TokenGuard's job at `initialize` — so `config validate`
/// and `doctor` probes stay deterministic and network-free.
pub fn static_config_errors(config: &SlackConfig) -> Vec<String> {
    let mut errors = Vec::new();

    if !config.app_token.starts_with("xapp-") {
        errors.push(
            "`app_token` is not an App-Level Token (must start with `xapp-`) → generate one \
             under the Slack app's Basic Information > App-Level Tokens (scope \
             `connections:write`) and update `[slack]` in config.toml"
                .into(),
        );
    }
    if !config.user_token.starts_with("xoxp-") {
        errors.push(
            "`user_token` is not a User OAuth Token (must start with `xoxp-`; a bot token \
             `xoxb-` cannot post as you) → copy the User OAuth Token from the Slack app's \
             OAuth & Permissions page and update `[slack]` in config.toml"
                .into(),
        );
    }
    if let Some(bot_token) = &config.bot_token
        && !bot_token.starts_with("xoxb-")
    {
        errors.push(
            "`bot_token` is not a Bot User OAuth Token (must start with `xoxb-`; an `xoxp-` \
             user token cannot send the notification nudge) → copy the Bot User OAuth Token \
             from the Slack app's OAuth & Permissions page and update `[slack]` in config.toml, or \
             remove `bot_token` to disable the nudge"
                .into(),
        );
    }
    if config.target_user_id.is_empty() {
        errors.push(
            "`target_user_id` is empty → set your Slack user id (profile > … > \
             Copy member ID)"
                .into(),
        );
    }
    if config.thread_context_limit == 0 {
        errors.push("`thread_context_limit` is 0 → set it to 1 or more".into());
    }
    // An empty `[[repos]]` is legal here: the orchestrator supplies its
    // `[[repositories]]` at `initialize` (#109), where the merged candidate
    // list is validated (see `server`). Offline validation can only check
    // an explicit list.
    let names = config.repo_names();
    for (i, name) in names.iter().enumerate() {
        if names[..i].contains(name) {
            errors.push(format!(
                "`[[slack.repos]]` declares `{name}` more than once → remove the duplicate entry"
            ));
        }
    }
    // More than one candidate needs an `[llm]` to classify — but offline
    // validation cannot know whether `initialize` will supply the
    // orchestrator's `[llm]` as the default (#119), so that check runs at
    // `initialize` (see `server`), like the empty-`[[repos]]` case above.
    if let Some(llm) = &config.llm {
        if llm.base_url.is_empty() {
            errors.push("`llm.base_url` is empty → set the OpenAI-compatible base URL".into());
        }
        if llm.model.is_empty() {
            errors.push("`llm.model` is empty → set the model identifier".into());
        }
        if llm.api_key.is_empty() {
            errors
                .push("`llm.api_key` is empty → set it (or its ${ENV}/keychain: reference)".into());
        }
        if !(0.0..=1.0).contains(&llm.confidence_threshold) {
            errors.push(format!(
                "`llm.confidence_threshold` is {} → use a value between 0.0 and 1.0",
                llm.confidence_threshold
            ));
        }
    }

    for group in &config.channel_groups {
        if group.prefix.is_empty() {
            errors.push(
                "a `[[slack.channel_groups]]` entry has an empty `prefix` → set the channel-name \
                 prefix it should match"
                    .into(),
            );
        }
        if group.repos.is_empty() {
            errors.push(format!(
                "`[[slack.channel_groups]]` (prefix `{}`) has an empty `repos` list → list the \
                 candidate repositories that prefix should narrow to",
                group.prefix
            ));
        }
        // With no explicit `[[repos]]` the candidates are not known until
        // `initialize` supplies them — the reference check runs there.
        if names.is_empty() {
            continue;
        }
        for repo in &group.repos {
            if !names.contains(&repo.as_str()) {
                errors.push(format!(
                    "`[[slack.channel_groups]]` (prefix `{}`) references repo `{repo}` which is \
                     not declared in `[[slack.repos]]` → add it there or fix the name",
                    group.prefix
                ));
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `reply_instructions` must not ask for a deliverable other than the
    /// reply.
    ///
    /// It is the `answer` default *and* the fallback for any `instructions_kind`
    /// this plugin has no set for (`pipeline`), so it is read by workflows whose
    /// tool boundaries differ: `answer` denies file edits and the shell,
    /// `design` denies neither, and a workflow with no profile gets no deny
    /// rules at all. **It therefore cannot claim what the agent can or cannot
    /// run** — only what this task is for.
    ///
    /// #527 is what happens when it does the opposite: the text asked for a
    /// pull-request URL, `answer` cannot open one, and the agent tried, was
    /// refused, and failed the task — leaving the mention with no reply and no
    /// draft. Silent on the operator's side, invisible to CI (a mock agent
    /// never attempts an implementation).
    ///
    /// Checks are positive where they can be. The negative check is `URL`,
    /// bare: #527 was a demand for the URL of a created artifact, and this key
    /// has no artifact to point at — the reply *is* the deliverable. Anything
    /// asking for a URL here is therefore wrong whatever its wording, which is
    /// a sharper line than a forbidden-fragment list could draw (such a list
    /// cannot tell "create a PR" from "do not create a PR"). The sibling keys
    /// `implement_instructions` and `triage_instructions` do demand a URL and
    /// are right to; this test reads neither.
    #[test]
    fn the_reply_instructions_ask_only_for_a_reply() {
        let text = &DEFAULTS.reply_instructions;
        for required in [
            "the only deliverable",
            "do not open a pull request",
            "do not attempt it",
        ] {
            assert!(
                !text.is_empty() && text.contains(required),
                "`{required}` missing from reply_instructions:\n{text}"
            );
        }
        assert!(
            !text.contains("URL"),
            "a URL demand is back — `answer` has no artifact to point at (#527):\n{text}"
        );
    }

    fn parse(json: serde_json::Value) -> SlackConfig {
        serde_json::from_value(json).unwrap()
    }

    fn minimal() -> serde_json::Value {
        json!({
            "app_token": "xapp-1-A1-token",
            "user_token": "xoxp-user-token",
            "target_user_id": "U012345",
            "repos": [{ "name": "web-app" }]
        })
    }

    #[test]
    fn embedded_defaults_parse() {
        // Force the LazyLock so a malformed `defaults.toml` fails here rather
        // than at `initialize` in a real run.
        let p = SlackPrompts::default();
        assert!(!p.reply_instructions.trim().is_empty());
        assert!(!p.classifier_system.trim().is_empty());
        assert!(
            p.unknown_placeholders().is_empty(),
            "the built-ins must only use placeholders their key supplies: {:?}",
            p.unknown_placeholders()
        );
    }

    /// The behavior-preservation proof for #318: every default must render
    /// byte-identically to the pre-#318 `format!`s.
    ///
    /// Expectations are transcribed from the pre-#318 source, NOT re-derived
    /// from `defaults.toml` — deriving them would make this vacuous.
    ///
    /// **Two keys are exceptions, and are weaker for it.**
    /// `reply_instructions` and `reply_style_suffix` were deliberately
    /// rewritten into English, so their pre-#318 bytes no longer exist to be
    /// transcribed; their expectations below ARE derived from
    /// `defaults.toml` and therefore only catch an *unintended* edit, never a
    /// wrong one. What still checks those two on merit is elsewhere:
    /// [`the_reply_instructions_ask_only_for_a_reply`] pins what
    /// `reply_instructions` must and must not ask for, and
    /// [`SlackPrompts::unknown_placeholders`] pins `{style}`. The remaining
    /// six assertions are untouched and remain real #318 proofs.
    #[test]
    fn defaults_reproduce_todays_prompt_bytes() {
        let p = SlackPrompts::default();

        // Was the `String::from(...)` in `pipeline::build_task`.
        //
        // **The byte-for-byte claim no longer holds for this key, on purpose.**
        // ADR-0026 had appended a PR-URL request here, reasoning that the reply
        // is the only channel a URL can travel on once the orchestrator stopped
        // creating pull requests. That reasoning is right for `implement` and
        // wrong for this key: `answer` is denied file edits *and* the shell, so
        // it cannot open a pull request at all. #527 caught it live — the agent
        // tried, was refused, and failed the task, leaving the mention with no
        // reply. The request is gone; do not restore it.
        //
        // What still has to survive is the instruction's own shape: what to
        // produce, and that the output is the reply alone. The wording is
        // English since the language directive moved out of the text (an
        // instruction that names a language overrides the operator's own), so
        // this is a re-baseline, not a transcription — see the note above.
        let original = "Draft a reply to the Slack mention below. \
             Investigate the target repository and answer with evidence. \
             Write the reply in the same language as the thread. \
             Output the reply text only, with no preamble, no postscript and no commentary.";
        assert!(
            p.reply_instructions.starts_with(original),
            "the instruction's shape must survive: {}",
            p.reply_instructions
        );
        // Was `format!("\n返信スタイル: {style}")`; the label is English now,
        // but `{style}` itself stays whatever the operator wrote.
        assert_eq!(
            crate::template::render(&p.reply_style_suffix, &[("style", "簡潔に")]),
            "\nReply style: 簡潔に"
        );
        // Was the body `format!`.
        assert_eq!(
            crate::template::render(
                &p.body_template,
                &[
                    ("sender", "太郎"),
                    ("channel", "dev"),
                    ("text", "こんにちは")
                ],
            ),
            "## メンション\n\n- 送信者: 太郎\n- チャンネル: #dev\n- 本文:\n\n> こんにちは\n"
        );
        assert_eq!(
            crate::template::render(&p.body_thread_header, &[("count", "3")]),
            "\n## スレッド文脈（直近 3 件・古い順）\n\n"
        );
        assert_eq!(
            crate::template::render(&p.body_thread_line, &[("line", "発言")]),
            "- 発言\n"
        );
        assert_eq!(
            p.body_thread_unavailable,
            "\n## スレッド文脈\n\n（スレッド文脈の取得に失敗したため省略されています）\n"
        );
        // Was the classifier `format!`s in `llm::request_body`.
        assert_eq!(
            crate::template::render(&p.classifier_system, &[("repo_names", "a, b")]),
            "You classify which local repository a Slack mention is about. \
             Answer with ONLY a JSON object of the exact shape \
             {\"repo\": string, \"confidence\": number, \"reason\": string} — \
             no prose, no code fences. `repo` MUST be one of: a, b. `confidence` \
             is 0.0-1.0, your own estimate of how sure you are."
        );
        assert_eq!(
            crate::template::render(
                &p.classifier_user,
                &[
                    ("mention_text", "M"),
                    ("thread_context", "T"),
                    ("catalog", "C"),
                ],
            ),
            "## Mention\nM\n\n## Thread context\nT\n\n## Candidate repositories\nC"
        );
        assert_eq!(
            p.classifier_correction,
            "That response was not a parseable JSON verdict. Answer again with \
             ONLY the JSON object — no prose, no code fences."
        );
    }

    /// The `{"repo": ...}` shape in the system prompt is JSON output contract,
    /// not a placeholder — `"repo"` is not a bare identifier, so the
    /// single-pass renderer emits it verbatim. This was `{{...}}` in Rust to
    /// escape `format!`; in TOML it is literal, so it needs pinning.
    #[test]
    fn classifier_system_json_shape_survives_rendering() {
        let rendered = crate::template::render(
            &SlackPrompts::default().classifier_system,
            &[("repo_names", "x")],
        );
        assert!(
            rendered.contains(r#"{"repo": string, "confidence": number, "reason": string}"#),
            "got {rendered}"
        );
    }

    #[test]
    fn a_repeated_unknown_placeholder_is_reported_once() {
        let p: SlackPrompts = serde_json::from_value(serde_json::json!({
            "body_template": "{bogus} と {bogus} と {sender}",
        }))
        .unwrap();
        assert_eq!(
            p.unknown_placeholders(),
            vec![("body_template", "bogus".to_string())]
        );
    }

    #[test]
    fn unknown_placeholders_are_reported() {
        let p: SlackPrompts = serde_json::from_value(serde_json::json!({
            "body_template": "{sender} と {bogus}",
            "reply_instructions": "{style} はここでは使えない",
        }))
        .unwrap();
        let found = p.unknown_placeholders();
        assert!(
            found.contains(&("body_template", "bogus".to_string())),
            "{found:?}"
        );
        assert!(
            found.contains(&("reply_instructions", "style".to_string())),
            "{found:?}"
        );
        // The keys that were not overridden stay clean.
        assert_eq!(found.len(), 2, "{found:?}");
    }

    /// **Every overridable key is scanned, not just the ones that take a
    /// placeholder.** `implement_instructions` was absent from the scan from
    /// the day it was added (#397) and `triage_instructions` inherited the
    /// same gap on arrival (#450): a typo in either override skipped the
    /// initialize-time warning that every other key gets, and the symptom is
    /// a literal `{token}` reaching the model.
    #[test]
    fn the_profile_selected_instructions_are_scanned_too() {
        let p: SlackPrompts = serde_json::from_value(serde_json::json!({
            "implement_instructions": "実装して {bogus} してください",
            "triage_instructions": "起票して {nonsense} してください",
        }))
        .unwrap();
        let found = p.unknown_placeholders();
        assert!(
            found.contains(&("implement_instructions", "bogus".to_string())),
            "{found:?}"
        );
        assert!(
            found.contains(&("triage_instructions", "nonsense".to_string())),
            "{found:?}"
        );
    }

    /// The shipped defaults must not name a placeholder nothing supplies —
    /// otherwise the warning above fires on a stock install.
    #[test]
    fn the_builtin_prompts_name_no_unknown_placeholder() {
        let p: SlackPrompts = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(p.unknown_placeholders(), vec![]);
    }

    #[test]
    fn prompt_overrides_parse_from_the_plugin_config() {
        let cfg: SlackConfig = serde_json::from_value(serde_json::json!({
            "app_token": "xapp-x",
            "user_token": "xoxp-x",
            "target_user_id": "U1",
            "prompts": { "reply_instructions": "短く返して" },
        }))
        .unwrap();
        assert_eq!(cfg.prompts.reply_instructions, "短く返して");
        // Unset keys keep the built-in.
        assert_eq!(
            cfg.prompts.classifier_correction,
            SlackPrompts::default().classifier_correction
        );
    }

    #[test]
    fn minimal_config_applies_defaults() {
        let cfg = parse(minimal());
        assert_eq!(cfg.thread_context_limit, 6);
        assert_eq!(cfg.source_name, "slack");
        assert_eq!(cfg.api_url, "https://slack.com/api");
        assert_eq!(cfg.max_retries, 3);
        assert!(cfg.reply_style.is_none());
        assert!(cfg.llm.is_none());
        assert!(cfg.channel_groups.is_empty());
        // The nudge is opt-in: no `bot_token` is a valid config (#305).
        assert!(cfg.bot_token.is_none());
        assert!(static_config_errors(&cfg).is_empty());
    }

    #[test]
    fn the_removed_keys_are_reported_by_name_not_as_unknown_fields() {
        // `SlackConfig` is `deny_unknown_fields`, so a slack.toml that still
        // sets this would otherwise fail with serde's `unknown field ...`,
        // which does not say the key was removed or what replaced it.
        let found = removed_keys_in(&json!({ "trigger_reactions": ["eyes"] }));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("trigger_reactions"), "{}", found[0]);
        // The message has to name the replacement, not just the removal.
        assert!(found[0].contains("[[workflows]]"), "{}", found[0]);
        assert!(found[0].contains("reaction ="), "{}", found[0]);
        // …and the ordering rule, which is the part that silently does
        // nothing when it is got wrong.
        assert!(found[0].contains("catch-all"), "{}", found[0]);
        // A clean config, and a non-object, both report nothing.
        assert!(removed_keys_in(&json!({ "target_user_id": "U1" })).is_empty());
        assert!(removed_keys_in(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn bot_token_prefix_is_checked() {
        let mut value = minimal();
        value["bot_token"] = json!("xoxp-not-a-bot-token");
        let errors = static_config_errors(&parse(value));
        assert!(errors.iter().any(|e| e.contains("xoxb-")), "{errors:?}");

        let mut value = minimal();
        value["bot_token"] = json!("xoxb-bot-token");
        assert!(static_config_errors(&parse(value)).is_empty());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let mut value = minimal();
        value["typo_field"] = json!(true);
        let err = serde_json::from_value::<SlackConfig>(value).unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    #[test]
    fn llm_confidence_threshold_defaults() {
        let mut value = minimal();
        value["llm"] = json!({ "base_url": "https://llm", "model": "m", "api_key": "k" });
        let cfg = parse(value);
        assert!((cfg.llm.unwrap().confidence_threshold - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn token_prefixes_are_checked() {
        let mut value = minimal();
        value["app_token"] = json!("xoxb-wrong");
        value["user_token"] = json!("xoxb-bot-token");
        let errors = static_config_errors(&parse(value));
        assert!(errors.iter().any(|e| e.contains("xapp-")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("xoxp-")), "{errors:?}");
    }

    #[test]
    fn multiple_repos_without_llm_defer_to_initialize() {
        // Offline validation cannot know whether initialize will supply the
        // orchestrator's `[llm]` as the default (#119) — the "more than one
        // candidate needs an `[llm]`" check fires at initialize instead.
        let mut value = minimal();
        value["repos"] = json!([{ "name": "a" }, { "name": "b" }]);
        assert!(static_config_errors(&parse(value)).is_empty());
    }

    #[test]
    fn single_repo_needs_no_llm() {
        let cfg = parse(minimal());
        assert!(static_config_errors(&cfg).is_empty());
    }

    #[test]
    fn empty_repos_defer_to_initialize_but_duplicates_are_flagged() {
        // Empty is legal offline: the orchestrator supplies its
        // repositories at initialize (#109), where the merged list is
        // checked — including channel_groups references.
        let mut value = minimal();
        value["repos"] = json!([]);
        value["channel_groups"] = json!([{ "prefix": "dev-", "repos": ["ghost"] }]);
        assert!(static_config_errors(&parse(value)).is_empty());

        let mut value = minimal();
        value["repos"] = json!([{ "name": "web-app" }, { "name": "web-app" }]);
        let errors = static_config_errors(&parse(value));
        assert!(
            errors.iter().any(|e| e.contains("more than once")),
            "{errors:?}"
        );
    }

    #[test]
    fn channel_group_referencing_unknown_repo_is_flagged() {
        let mut value = minimal();
        value["channel_groups"] = json!([{ "prefix": "dev-", "repos": ["web-app", "ghost"] }]);
        let errors = static_config_errors(&parse(value));
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("ghost"), "{errors:?}");
    }

    #[test]
    fn out_of_range_confidence_is_flagged() {
        let mut value = minimal();
        value["llm"] = json!({
            "base_url": "https://llm", "model": "m", "api_key": "k",
            "confidence_threshold": 1.5
        });
        let errors = static_config_errors(&parse(value));
        assert!(
            errors.iter().any(|e| e.contains("confidence_threshold")),
            "{errors:?}"
        );
    }
}
