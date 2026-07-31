//! The prompt text totsuka injects into claude / codex / opencode (#313).
//!
//! Built-in defaults live in the embedded [`defaults.toml`], not in Rust string
//! literals, so rewording what an agent is told is an edit to a data file
//! rather than a code change (epic #311). This module parses that file once and
//! resolves each template when a set is built.
//!
//! It sits alongside [`tool`](crate::tool) and [`worktree`](crate::worktree)
//! rather than under [`config`](crate::config) because it is an
//! *interpretation* layer: `config` owns parse / secret-resolve / validate /
//! edit, and the modules that turn a parsed config into runtime values live at
//! the top level. `tool::registry_from_config` is the same shape.
//!
//! # What is configurable and what is not
//!
//! The prose that *teaches* the marker convention is data. The convention
//! itself is not: [`MARKER_COMPLETED`] and friends are the wire format that
//! `on-stop.sh` (bash) and `totsuka-opencode.js` parse, and per
//! [ADR-0020](https://github.com/tomoya-k31/totsuka/blob/main/docs/decisions/adr-0020-status-marker-stays.md)
//! the marker is the one completion signal shared by all three tools. Prompt
//! text changes what the model is *told*; it never changes what *runs*.
//!
//! [`defaults.toml`]: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/prompts/defaults.toml

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::config::{PromptsConfig, RootConfig, WorkflowConfig, WorkflowPromptsConfig};
use crate::domain::signal::{MARKER_COMPLETED, MARKER_FAILED, MARKER_NEEDS_INPUT};
use crate::template;

/// The embedded defaults, parsed once on first use.
///
/// A malformed `defaults.toml` is an authoring error in a file that ships
/// inside the binary — no input can change it — so this panics rather than
/// degrading. Without a test the panic would first surface on a dispatch;
/// `embedded_defaults_toml_parses` forces it in CI instead.
static DEFAULTS: LazyLock<Prompts> = LazyLock::new(|| {
    toml::from_str::<Embedded>(include_str!("defaults.toml"))
        .expect("embedded defaults.toml must parse")
        .prompts
        .finish()
});

/// Top level of `defaults.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Embedded {
    prompts: Prompts,
}

/// One resolved set of prompt templates.
///
/// Fields hold the templates *unrendered*; the accessors substitute
/// placeholders, so the marker constants are never baked into stored state.
///
/// `deny_unknown_fields`: a key that no longer backs anything is dead prompt
/// text that still reads as live, so a rename must fail the build rather than
/// leave a stale copy sitting in the file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prompts {
    /// Dispatch-time completion self-report instruction.
    marker_self_report: String,
    /// Dispatch-time instruction to create the task's branch. Emitted only
    /// when the worktree is handed over detached.
    branch_convention: String,
    /// Judging criteria of the `prompt`-type Stop hook.
    verification_rubric: String,
    /// Intermediate-Stop exemption appended to the rubric.
    verification_background_exemption: String,
    /// Marker convention appended to the rubric.
    verification_marker_convention: String,
    /// How the three keys above are assembled.
    verification_prompt: String,
    /// Prose body of the opencode plan-mode agent file. Global-only: one file
    /// on disk backs every session.
    opencode_plan_agent: String,
    /// [`marker_self_report`](Self::marker_self_report) with its placeholders
    /// already substituted.
    ///
    /// Rendered once by [`finish`](Self::finish) when the set is built, not per
    /// call: this one is on the dispatch path, and the pre-#313 code had it as
    /// a `LazyLock<String>` that every dispatch merely copied from. Deriving it
    /// here keeps that property while the *template* stays editable for the
    /// config overrides landing in #314 — which resolve per workflow at
    /// startup, so a global cache would be wrong.
    #[serde(skip)]
    rendered_marker_self_report: String,
}

/// Placeholders that resolve to the wire marker constants.
const MARKER_PLACEHOLDERS: &[&str] = &["marker_completed", "marker_needs_input", "marker_failed"];

/// Which `{placeholder}` each prompt key may reference (#315).
///
/// [`config::validate`](mod@crate::config::validate) rejects anything else as
/// an **error**, unlike the PR templates which pass an unknown key through
/// silently. A typo like `{marker_completd}` deletes the completion convention,
/// and the symptom — every Stop parsing as UNKNOWN until the task escalates —
/// gives no hint about its cause.
pub const ALLOWED_PLACEHOLDERS: &[(&str, &[&str])] = &[
    ("marker_self_report", MARKER_PLACEHOLDERS),
    ("branch_convention", &[]),
    ("verification_rubric", &[]),
    ("verification_background_exemption", &[]),
    ("verification_marker_convention", MARKER_PLACEHOLDERS),
    (
        "verification_prompt",
        &["rubric", "background_exemption", "marker_convention"],
    ),
    ("opencode_plan_agent", &[]),
];

/// The placeholders `key` may use, or `None` when the key is unknown.
pub fn allowed_placeholders(key: &str) -> Option<&'static [&'static str]> {
    ALLOWED_PLACEHOLDERS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
}

impl Prompts {
    /// The built-in set, with no configuration applied.
    pub fn builtin() -> &'static Prompts {
        &DEFAULTS
    }

    /// The status markers `rendered` fails to teach, if any.
    ///
    /// The guard behind the ADR-0020 validation checks: the marker is the only
    /// completion signal shared by claude, codex, and opencode, so an override
    /// that drops one silently disables that outcome. Checked on the
    /// **composed** output so it catches both a leaf that lost its
    /// `{marker_*}` and an assembly that dropped `{marker_convention}`.
    ///
    /// **Every** marker is required, not merely one of them (#328). An agent
    /// taught only COMPLETED and FAILED has no way to say it needs input, so
    /// that turn parses as UNKNOWN and the task escalates on a timeout. The
    /// finding's message always named all three; only the check was weaker.
    /// A partial loss is also the shape a typo produces — `{marker-needs-input}`
    /// is not a placeholder name, so it is neither substituted nor reported,
    /// and the other two markers would have satisfied an "any" test.
    pub fn missing_markers(rendered: &str) -> Vec<&'static str> {
        [MARKER_COMPLETED, MARKER_NEEDS_INPUT, MARKER_FAILED]
            .into_iter()
            .filter(|m| !rendered.contains(m))
            .collect()
    }

    /// Compute the derived fields. **Every** constructor must end with this —
    /// a set that skips it renders an empty self-report instruction, which
    /// would silently disable completion detection.
    fn finish(mut self) -> Self {
        self.rendered_marker_self_report =
            template::render(&self.marker_self_report, &Self::marker_vars());
        self
    }

    /// The wire marker constants, as template variables.
    fn marker_vars() -> [(&'static str, &'static str); 3] {
        [
            ("marker_completed", MARKER_COMPLETED),
            ("marker_needs_input", MARKER_NEEDS_INPUT),
            ("marker_failed", MARKER_FAILED),
        ]
    }

    /// The completion self-report instruction injected into every hook-capable
    /// dispatch — invisibly via `TOTSUKA_PROMPT_CONTEXT` where the tool
    /// supports it, as visible `extra_context` otherwise.
    ///
    /// Teaching the convention up front is what makes the first Stop carry a
    /// marker, so `on-stop.sh` rarely has to `block` and force a regeneration.
    /// The rationale for each clause is recorded above the key in
    /// `defaults.toml`, where anyone overriding the text will read it.
    ///
    /// Borrowed, not rendered: the substitution happened once when this set was
    /// built, so the dispatch path does no work per task.
    pub fn marker_self_report(&self) -> &str {
        self.rendered_marker_self_report.as_str()
    }

    /// The `prompt`-type Stop hook body for a `verification = "llm"` workflow.
    ///
    /// Rendered as two staged single passes: the leaves first (markers
    /// substituted), then the assembly over those results. Each pass is single,
    /// so a literal `{marker_convention}` inside the rubric is inserted rather
    /// than expanded.
    pub fn verification_prompt(&self) -> String {
        let convention =
            template::render(&self.verification_marker_convention, &Self::marker_vars());
        template::render(
            &self.verification_prompt,
            &[
                ("rubric", self.verification_rubric.as_str()),
                (
                    "background_exemption",
                    self.verification_background_exemption.as_str(),
                ),
                ("marker_convention", convention.as_str()),
            ],
        )
    }

    /// The prose body of the opencode plan-mode agent file (#316).
    ///
    /// Body only — the YAML frontmatter that carries the `permission` deny map
    /// is fixed in [`hooks::opencode`](crate::hooks::opencode) and is not part
    /// of this value. Prompt text changes what the model is told; it never
    /// changes what it is allowed to do.
    pub fn opencode_plan_agent(&self) -> &str {
        &self.opencode_plan_agent
    }

    /// The instruction telling the agent to create the task's branch.
    ///
    /// The caller decides *whether* to send it — it is meaningless in plan
    /// mode (no git) and on a resume that already has a branch. This accessor
    /// only supplies the text.
    pub fn branch_convention(&self) -> &str {
        &self.branch_convention
    }

    /// The rubric leaf, unassembled. Lets a caller distinguish "this set uses
    /// the built-in rubric" from "this set was given one".
    pub fn verification_rubric(&self) -> &str {
        &self.verification_rubric
    }

    /// Build a set whose rubric is replaced, leaving every other template at
    /// this set's value.
    ///
    /// This was the whole of the `[[workflows]].rubric` path before #314;
    /// [`resolve_for`](Self::resolve_for) now assigns the field directly,
    /// because the workflow's `prompts` table has to be able to override it
    /// afterwards. Kept as the reference the back-compat test compares
    /// against — it is the pre-#314 shape, so an equality assertion against it
    /// proves the legacy path still renders identically.
    pub fn with_rubric(&self, rubric: &str) -> Prompts {
        Prompts {
            verification_rubric: rubric.to_string(),
            ..self.clone()
        }
        .finish()
    }

    /// Apply the global `[prompts]` table. Every `None` leaves the current
    /// value, so this composes with the workflow layer below.
    fn overlay_global(mut self, o: &PromptsConfig) -> Self {
        set(&mut self.marker_self_report, &o.marker_self_report);
        set(&mut self.branch_convention, &o.branch_convention);
        set(&mut self.verification_rubric, &o.verification_rubric);
        set(
            &mut self.verification_background_exemption,
            &o.verification_background_exemption,
        );
        set(
            &mut self.verification_marker_convention,
            &o.verification_marker_convention,
        );
        set(&mut self.verification_prompt, &o.verification_prompt);
        set(&mut self.opencode_plan_agent, &o.opencode_plan_agent);
        self
    }

    /// Apply a `[[workflows]].prompts` table — the strongest layer.
    ///
    /// `opencode_plan_agent` is absent here on purpose: it describes a single
    /// shared on-disk file, so it has no per-workflow meaning.
    fn overlay_workflow(mut self, o: &WorkflowPromptsConfig) -> Self {
        set(&mut self.marker_self_report, &o.marker_self_report);
        set(&mut self.branch_convention, &o.branch_convention);
        set(&mut self.verification_rubric, &o.verification_rubric);
        set(
            &mut self.verification_background_exemption,
            &o.verification_background_exemption,
        );
        set(
            &mut self.verification_marker_convention,
            &o.verification_marker_convention,
        );
        set(&mut self.verification_prompt, &o.verification_prompt);
        self
    }

    /// Global scope: built-ins overlaid with `[prompts]`.
    pub fn resolve(cfg: &RootConfig) -> Prompts {
        Self::builtin()
            .clone()
            .overlay_global(&cfg.prompts)
            .finish()
    }

    /// Workflow scope. Precedence, strongest first:
    ///
    /// 1. `[[workflows]].prompts.*`
    /// 2. `[[workflows]].rubric` (legacy, rubric leaf only)
    /// 3. `[prompts].*`
    /// 4. the built-in default
    ///
    /// 2 beating 3 is deliberate — both are about this workflow, so ordering it
    /// the other way would mean adding a global `verification_rubric` silently
    /// overrides every existing per-workflow `rubric`.
    pub fn resolve_for(cfg: &RootConfig, wf: &WorkflowConfig) -> Prompts {
        let mut p = Self::builtin().clone().overlay_global(&cfg.prompts);
        if let Some(rubric) = wf.rubric.as_deref() {
            p.verification_rubric = rubric.to_string();
        }
        p.overlay_workflow(&wf.prompts).finish()
    }
}

/// Overwrite `dst` when the override is present, leave it otherwise.
fn set(dst: &mut String, src: &Option<String>) {
    if let Some(v) = src {
        dst.clone_from(v);
    }
}

/// Every workflow's resolved prompt set, plus the global one (#314).
///
/// Built once at startup and carried on
/// [`EngineSettings`](crate::run::EngineSettings), mirroring the resolved
/// `tools` registry: dispatch looks its workflow up here instead of
/// re-resolving config per task.
#[derive(Debug, Clone)]
pub struct PromptSet {
    global: Prompts,
    by_workflow: HashMap<String, Prompts>,
}

impl Default for PromptSet {
    fn default() -> Self {
        Self {
            global: Prompts::builtin().clone(),
            by_workflow: HashMap::new(),
        }
    }
}

impl PromptSet {
    /// Resolve the global set and one set per configured workflow.
    pub fn from_config(cfg: &RootConfig) -> Self {
        Self {
            global: Prompts::resolve(cfg),
            by_workflow: cfg
                .workflows
                .iter()
                .map(|wf| (wf.name.clone(), Prompts::resolve_for(cfg, wf)))
                .collect(),
        }
    }

    /// The set for `name`, falling back to the global set when the workflow is
    /// unknown. Never panics: a task can outlive the workflow that created it
    /// (dispatch already tolerates that), and losing the marker convention
    /// there would strand the task instead of merely mis-wording a prompt.
    pub fn for_workflow(&self, name: &str) -> &Prompts {
        self.by_workflow.get(name).unwrap_or(&self.global)
    }

    /// The global set (`[prompts]` applied, no workflow layer).
    pub fn global(&self) -> &Prompts {
        &self.global
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_defaults_toml_parses() {
        // Force the LazyLock so a malformed asset fails here rather than on a
        // dispatch, and prove no key is empty.
        let p = Prompts::builtin();
        for (name, value) in [
            ("marker_self_report", &p.marker_self_report),
            ("branch_convention", &p.branch_convention),
            ("verification_rubric", &p.verification_rubric),
            (
                "verification_background_exemption",
                &p.verification_background_exemption,
            ),
            (
                "verification_marker_convention",
                &p.verification_marker_convention,
            ),
            ("verification_prompt", &p.verification_prompt),
        ] {
            assert!(!value.trim().is_empty(), "`{name}` is empty");
        }
    }

    /// The branch instruction is prose, so it is overridable — but the three
    /// things it has to say are what make it work, and a reword that drops one
    /// of them fails silently (the agent commits somewhere the orchestrator
    /// cannot see). Assert the built-in still says them.
    #[test]
    fn the_builtin_branch_convention_states_what_it_has_to() {
        let text = Prompts::builtin().branch_convention();
        assert!(
            text.contains("DETACHED"),
            "must say the worktree is detached — an agent that just commits \
             leaves the work reachable from nothing: {text}"
        );
        assert!(
            text.contains("git switch -c"),
            "must name the command, so the branch is created where HEAD \
             already is: {text}"
        );
        assert!(
            text.contains("NO start-point") || text.contains("no start-point"),
            "must forbid a start-point argument — passing one re-points HEAD \
             and discards work already in the worktree: {text}"
        );
    }

    #[test]
    fn branch_convention_is_overridable_at_both_scopes() {
        let cfg = RootConfig::from_toml_str(
            r#"
version = 1

[prompts]
branch_convention = "global text"

[[workflows]]
name = "wf"
source = "s"
agent = "a"
mode = "implement"
output = "none"
trigger = {}
prompts = { branch_convention = "workflow text" }
"#,
        )
        .unwrap();
        assert_eq!(Prompts::resolve(&cfg).branch_convention(), "global text");
        assert_eq!(
            Prompts::resolve_for(&cfg, &cfg.workflows[0]).branch_convention(),
            "workflow text"
        );
    }

    /// The behavior-preservation proof for #313: the text moved out of Rust
    /// must render byte-identically to what the Rust constants produced.
    ///
    /// The expectations below are transcribed from the pre-#313 source, NOT
    /// re-derived from `defaults.toml` — deriving them would make this
    /// assertion vacuous and let a mangled move through.
    #[test]
    fn defaults_reproduce_todays_prompt_bytes() {
        // Was `run::hooks::MARKER_SELF_REPORT_INSTRUCTION`.
        let expected_self_report = format!(
            "[orchestrator] Completion self-report: EVERY time you end your turn, end \
             your response with exactly one of the following status markers on its own \
             final line — with one exception: while background tasks or subagents are \
             still running, do NOT emit a marker (that stop is an intermediate \
             heartbeat; you will be re-invoked when they finish — restate the full \
             final answer with the marker then). The marker line is stripped \
             automatically before the result is delivered, so include it even when \
             instructed to output nothing but the answer body: \
             {MARKER_COMPLETED} (done) / \
             {MARKER_NEEDS_INPUT} (human input required) / \
             {MARKER_FAILED} (cannot proceed). \
             Delivery contract: ONLY the message carrying the marker is delivered to \
             the requester — earlier messages in this session are NEVER delivered. The \
             marker-bearing message must therefore contain the complete, \
             self-contained answer; never refer to a previous message (no \"as stated \
             above\" / \"already answered earlier\")."
        );
        assert_eq!(
            Prompts::builtin().marker_self_report(),
            expected_self_report
        );

        // Was `hooks::DEFAULT_RUBRIC`.
        let expected_rubric = "作業が指示された要件を実際に満たしているかを、対象リポジトリの現在のコードと状態に基づいて検証してください。表面的な自己申告ではなく、変更が意図どおり機能し破綻や取りこぼしがないことを確認してください。";
        // Was `hooks::BACKGROUND_EXEMPTION`.
        let expected_exemption = "ただし、バックグラウンドタスク（サブエージェント等）が実行中のままターンを終える中間停止は完了申告ではありません。その場合は検証もブロックも行わず停止を許可してください。完了判定はバックグラウンドタスクが残っていない停止に対してのみ行います。";
        // Was `hooks::marker_convention()`.
        let expected_convention = format!(
            "検証結果を踏まえ、応答の最終行に必ず次のいずれかのマーカーを付けてください: {MARKER_COMPLETED} / {MARKER_NEEDS_INPUT} / {MARKER_FAILED}"
        );
        // Was the `format!` at `hooks::render_settings`.
        assert_eq!(
            Prompts::builtin().verification_prompt(),
            format!("{expected_rubric}\n\n{expected_exemption}\n\n{expected_convention}")
        );
        assert_eq!(Prompts::builtin().verification_rubric(), expected_rubric);
    }

    #[test]
    fn with_rubric_replaces_only_the_rubric() {
        let custom = Prompts::builtin().with_rubric("独自の観点");
        let rendered = custom.verification_prompt();
        assert!(rendered.starts_with("独自の観点"));
        assert!(
            !rendered.contains(Prompts::builtin().verification_rubric()),
            "the default rubric is replaced, not appended"
        );
        assert!(
            rendered.contains(MARKER_COMPLETED),
            "the marker convention survives a custom rubric"
        );
    }

    /// One workflow, with whatever `[prompts]` / workflow keys the caller adds.
    fn cfg(extra: &str) -> RootConfig {
        RootConfig::from_toml_str(&format!(
            r#"
[[workflows]]
name = "reply"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"
{extra}
"#
        ))
        .unwrap()
    }

    #[test]
    fn workflow_prompts_override_global_override_defaults() {
        // Layer 4 only.
        let c = cfg("");
        assert_eq!(
            Prompts::resolve_for(&c, &c.workflows[0]).verification_rubric(),
            Prompts::builtin().verification_rubric()
        );

        // Layer 3: `[prompts]` beats the built-in.
        let c = cfg("\n[prompts]\nverification_rubric = \"グローバル\"\n");
        assert_eq!(
            Prompts::resolve_for(&c, &c.workflows[0]).verification_rubric(),
            "グローバル"
        );
        assert_eq!(Prompts::resolve(&c).verification_rubric(), "グローバル");

        // Layer 1: the workflow table beats `[prompts]`.
        let c = cfg(
            "  [workflows.prompts]\n  verification_rubric = \"ワークフロー\"\n\n[prompts]\nverification_rubric = \"グローバル\"\n",
        );
        assert_eq!(
            Prompts::resolve_for(&c, &c.workflows[0]).verification_rubric(),
            "ワークフロー"
        );
        // …and the global set is untouched by the workflow layer.
        assert_eq!(Prompts::resolve(&c).verification_rubric(), "グローバル");
    }

    #[test]
    fn legacy_rubric_still_feeds_the_verification_prompt() {
        let c = cfg("rubric = \"レガシー\"\n");
        let p = Prompts::resolve_for(&c, &c.workflows[0]);
        assert_eq!(p.verification_rubric(), "レガシー");
        // Identical to what `with_rubric` produced before #314.
        assert_eq!(
            p.verification_prompt(),
            Prompts::builtin()
                .with_rubric("レガシー")
                .verification_prompt()
        );
    }

    #[test]
    fn legacy_rubric_beats_the_global_key_but_loses_to_the_workflow_key() {
        // Both are workflow-scoped, so a newly-added global key must not
        // silently override an existing per-workflow `rubric`.
        let c = cfg("rubric = \"レガシー\"\n\n[prompts]\nverification_rubric = \"グローバル\"\n");
        assert_eq!(
            Prompts::resolve_for(&c, &c.workflows[0]).verification_rubric(),
            "レガシー"
        );

        // The new workflow key is the strongest layer.
        let c = cfg(
            "rubric = \"レガシー\"\n  [workflows.prompts]\n  verification_rubric = \"ワークフロー\"\n",
        );
        assert_eq!(
            Prompts::resolve_for(&c, &c.workflows[0]).verification_rubric(),
            "ワークフロー"
        );
    }

    #[test]
    fn every_key_is_overridable_at_both_scopes() {
        let keys = "\
marker_self_report = \"A {marker_completed}\"
verification_rubric = \"B\"
verification_background_exemption = \"C\"
verification_marker_convention = \"D {marker_failed}\"
verification_prompt = \"{rubric}|{background_exemption}|{marker_convention}\"
";
        for (scope, extra) in [
            ("global", format!("\n[prompts]\n{keys}")),
            (
                "workflow",
                format!("  [workflows.prompts]\n{}", keys.replace('\n', "\n  ")),
            ),
        ] {
            let c = cfg(&extra);
            let p = Prompts::resolve_for(&c, &c.workflows[0]);
            assert_eq!(
                p.marker_self_report(),
                format!("A {MARKER_COMPLETED}"),
                "marker_self_report at {scope} scope"
            );
            assert_eq!(
                p.verification_prompt(),
                format!("B|C|D {MARKER_FAILED}"),
                "verification_prompt at {scope} scope"
            );
        }
    }

    #[test]
    fn prompt_set_falls_back_to_global_for_an_unknown_workflow() {
        let c = cfg("\n[prompts]\nverification_rubric = \"グローバル\"\n");
        let set = PromptSet::from_config(&c);
        assert_eq!(
            set.for_workflow("reply").verification_rubric(),
            "グローバル"
        );
        // A task can outlive its workflow; that must not panic or lose the
        // marker convention.
        assert_eq!(
            set.for_workflow("消えた").verification_rubric(),
            "グローバル"
        );
        assert!(
            set.for_workflow("消えた")
                .marker_self_report()
                .contains(MARKER_COMPLETED)
        );
    }

    #[test]
    fn a_marker_token_inside_the_rubric_is_not_expanded() {
        // Staged single passes: text that looks like a placeholder but arrives
        // as *data* must be inserted literally.
        let custom = Prompts::builtin().with_rubric("{marker_completed} と書いた場合");
        assert!(
            custom
                .verification_prompt()
                .contains("{marker_completed} と書いた場合"),
            "a marker token in the rubric is data, not a directive"
        );
    }
}
