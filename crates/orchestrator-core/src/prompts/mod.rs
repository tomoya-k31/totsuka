//! The prompt text totsuka injects into claude / codex / opencode (#313).
//!
//! The text lives in the embedded [`defaults.toml`], not in Rust string
//! literals, so rewording what an agent is told is an edit to a data file
//! rather than a code change (epic #311). This module parses that file once and
//! resolves each template when a set is built.
//!
//! **The file is embedded with `include_str!`, so editing it needs a rebuild.**
//! #314 added a `[prompts]` config surface to avoid exactly that; #465 removed
//! it again, re-accepting the rebuild. The reason is in the ladder on
//! [`Prompts::resolve_for`]: an override could silently defeat the completion
//! protocol and the verification criteria a workflow's `profile` had already
//! chosen, and it always failed the loose way. What survives is one key,
//! [`WorkflowConfig::rubric`](crate::config::WorkflowConfig::rubric) — the
//! criteria, which is the part an operator ever actually wrote.
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
//! [ADR-0020](https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0020-status-marker-stays.md)
//! the marker is the one completion signal shared by all three tools. Prompt
//! text changes what the model is *told*; it never changes what *runs*.
//!
//! [`defaults.toml`]: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/prompts/defaults.toml

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::config::{Profile, RootConfig, WorkflowConfig};
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
    /// The self-report [`resolve_for`](Self::resolve_for) substitutes for
    /// [`marker_self_report`](Self::marker_self_report) on a profile whose
    /// completion is judged by a human at the pane (#440): the agent asks for
    /// confirmation with NEEDS_INPUT and emits COMPLETED only after an
    /// explicit approval in the conversation.
    ///
    /// Selected by the profile alone. Nothing overrides it — an operator
    /// override of `marker_self_report` used to, which is one of the two silent
    /// failures #465 removed the surface for.
    marker_self_report_confirm: String,
    /// Dispatch-time instruction to create the task's branch. Emitted only
    /// when the worktree is handed over detached.
    branch_convention: String,
    /// Judging criteria of the `prompt`-type Stop hook.
    verification_rubric: String,
    /// The rubric [`resolve_for`](Self::resolve_for) substitutes for
    /// [`verification_rubric`](Self::verification_rubric) on a profile whose
    /// deliverable the agent writes outside the worktree (#398).
    ///
    /// Selected by the profile, and beaten only by the workflow's own
    /// `rubric` — the one spelling of this leaf that a config can write.
    verification_rubric_artifact_url: String,
    /// The rubric [`resolve_for`](Self::resolve_for) substitutes on a profile
    /// whose completion is judged by a human at the pane (#440): the judge —
    /// which runs in-session and can see the conversation — checks that the
    /// human explicitly approved before a COMPLETED passes. The mechanical
    /// backstop for the protocol
    /// [`marker_self_report_confirm`](Self::marker_self_report_confirm)
    /// teaches.
    ///
    /// Selected by the profile, same as
    /// [`verification_rubric_artifact_url`](Self::verification_rubric_artifact_url).
    verification_rubric_human_approval: String,
    /// Intermediate-Stop exemption appended to the rubric.
    verification_background_exemption: String,
    /// Non-claim exemption: a Stop already reporting NEEDS_INPUT/FAILED is not
    /// claiming completion, so the judge must let it through (#389).
    verification_nonclaim_exemption: String,
    /// Marker convention appended to the rubric.
    verification_marker_convention: String,
    /// How the four keys above are assembled.
    verification_prompt: String,
    /// Prose body of the opencode plan-mode agent file. One file on disk backs
    /// every session, which is why it was global-only while it was
    /// configurable at all.
    opencode_plan_agent: String,
    /// [`marker_self_report`](Self::marker_self_report) with its placeholders
    /// already substituted.
    ///
    /// Rendered once by [`finish`](Self::finish) when the set is built, not per
    /// call: this one is on the dispatch path, and the pre-#313 code had it as
    /// a `LazyLock<String>` that every dispatch merely copied from. Deriving it
    /// here keeps that property while the *template* stays per-set: the profile
    /// picks between two of them ([`marker_self_report_confirm`](Self::marker_self_report_confirm)),
    /// so a single global cache would be wrong.
    #[serde(skip)]
    rendered_marker_self_report: String,
}

/// Placeholders that resolve to the wire marker constants.
const MARKER_PLACEHOLDERS: &[&str] = &["marker_completed", "marker_needs_input", "marker_failed"];

/// Which `{placeholder}` each prompt key may reference (#315).
///
/// Two consumers, and they check different things. A unit test checks
/// `defaults.toml` itself against this table, which is where a typo would now
/// come from; [`config::validate`](mod@crate::config::validate) checks the one
/// operator-written value that still fills a leaf, `[[workflows]].rubric`.
/// Either way an unknown name is an **error**, unlike the PR templates which
/// pass one through silently: `{marker_completd}` deletes the completion
/// convention, and the symptom — every Stop parsing as UNKNOWN until the task
/// escalates — gives no hint about its cause.
pub const ALLOWED_PLACEHOLDERS: &[(&str, &[&str])] = &[
    ("marker_self_report", MARKER_PLACEHOLDERS),
    ("marker_self_report_confirm", MARKER_PLACEHOLDERS),
    ("branch_convention", &[]),
    ("verification_rubric", &[]),
    ("verification_rubric_artifact_url", &[]),
    ("verification_rubric_human_approval", &[]),
    ("verification_background_exemption", &[]),
    (
        "verification_nonclaim_exemption",
        &["marker_needs_input", "marker_failed"],
    ),
    ("verification_marker_convention", MARKER_PLACEHOLDERS),
    (
        "verification_prompt",
        &[
            "rubric",
            "background_exemption",
            "nonclaim_exemption",
            "marker_convention",
        ],
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
        let nonclaim =
            template::render(&self.verification_nonclaim_exemption, &Self::marker_vars());
        template::render(
            &self.verification_prompt,
            &[
                ("rubric", self.verification_rubric.as_str()),
                (
                    "background_exemption",
                    self.verification_background_exemption.as_str(),
                ),
                ("nonclaim_exemption", nonclaim.as_str()),
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

    /// Workflow scope. Precedence, strongest first:
    ///
    /// 1. `[[workflows]].rubric` (rubric leaf only)
    /// 2. **the profile's defaults** (rubric leaf #398/#440, self-report leaf
    ///    #440)
    /// 3. the built-in default
    ///
    /// **This ladder used to have five rungs**, with a global `[prompts]` table
    /// and a per-workflow `prompts` table around the two that are left. #465
    /// removed both. The two rungs that went were the ones that could defeat
    /// rung 2 without saying so: setting `[prompts].verification_rubric` cost
    /// every `triage` workflow its artifact-URL check, and setting
    /// `[prompts].marker_self_report` cost every `design` workflow the #440
    /// confirmation protocol. Both failures were silent and both leaned the
    /// unsafe way — verification got *looser*. The surviving rung is
    /// per-workflow, so it cannot reach a workflow the operator was not
    /// looking at.
    pub fn resolve_for(wf: &WorkflowConfig) -> Prompts {
        let mut p = Self::builtin().clone();
        match wf.profile {
            // Completion is judged by the human at the pane (#440): the
            // self-report teaches ask-then-COMPLETED, and the rubric makes the
            // judge check the approval actually happened. This shadows the
            // artifact-URL rubric on purpose — the human saw the artifact, so
            // a URL demand would second-guess an approval already given.
            Some(profile) if Self::profile_confirms_with_a_human(profile) => {
                p.marker_self_report
                    .clone_from(&p.marker_self_report_confirm);
                p.verification_rubric
                    .clone_from(&p.verification_rubric_human_approval);
            }
            Some(profile) if Self::profile_verifies_an_artifact(profile) => {
                p.verification_rubric
                    .clone_from(&p.verification_rubric_artifact_url);
            }
            _ => {}
        }
        if let Some(rubric) = wf.rubric.as_deref() {
            p.verification_rubric = rubric.to_string();
        }
        p.finish()
    }

    /// Whether this profile's completion is judged by a human at the pane
    /// (#440): the pane is attended, the agent asks for confirmation with
    /// NEEDS_INPUT, and COMPLETED means "the human approved".
    ///
    /// Profiles only — a spelled-out `mode = "implement"` workflow keeps the
    /// plain self-report, the same line #420 drew for permissions: a profile
    /// is what buys a behavior bundle, and an existing config must not change
    /// meaning on upgrade.
    fn profile_confirms_with_a_human(profile: Profile) -> bool {
        match profile {
            Profile::Design | Profile::Implement => true,
            Profile::Answer | Profile::Triage => false,
        }
    }

    /// Whether this profile's deliverable is written outside the worktree, so
    /// the only evidence it exists is a URL in the final message (#393 D3).
    ///
    /// `answer` is excluded: its reply goes back through the source plugin's
    /// approval gate, so there is no URL to demand and demanding one would fail
    /// every well-behaved answer. `design` / `implement` still satisfy this
    /// predicate, but [`resolve_for`](Self::resolve_for) checks
    /// [`profile_confirms_with_a_human`](Self::profile_confirms_with_a_human)
    /// first, so since #440 only `triage` actually resolves to the URL rubric.
    fn profile_verifies_an_artifact(profile: Profile) -> bool {
        match profile {
            Profile::Triage | Profile::Design | Profile::Implement => true,
            Profile::Answer => false,
        }
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
    ///
    /// The global set is the built-in one verbatim since #465 — there is no
    /// global override table any more. It remains a distinct field because
    /// [`for_workflow`](Self::for_workflow) needs a fallback for a task whose
    /// workflow has since been deleted from the config.
    pub fn from_config(cfg: &RootConfig) -> Self {
        Self {
            global: Prompts::builtin().clone(),
            by_workflow: cfg
                .workflows
                .iter()
                .map(|wf| (wf.name.clone(), Prompts::resolve_for(wf)))
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

    /// The global set — since #465 the built-in one, with no workflow layer.
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
            ("marker_self_report_confirm", &p.marker_self_report_confirm),
            ("branch_convention", &p.branch_convention),
            ("verification_rubric", &p.verification_rubric),
            (
                "verification_rubric_human_approval",
                &p.verification_rubric_human_approval,
            ),
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
        assert!(
            text.contains("Before your first commit") || text.contains("before your first commit"),
            "must put the branch BEFORE the first commit — commits made while \
             still detached are reachable from nothing once the worktree goes: \
             {text}"
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

        // The verification prompt is deliberately NOT part of this proof any
        // more. #389 rewrote it from a list of imperatives into a single
        // condition, because Claude Code judges a `prompt` hook by asking
        // "is the user-provided condition met?" and blocks on `ok: false` —
        // an instruction to allow is not something it can act on. Its shape is
        // pinned by the tests below, which assert the contract rather than the
        // pre-#313 bytes.
    }

    /// The composed condition, byte-exact.
    ///
    /// Pinned in full rather than by `contains` because every part of it is
    /// load-bearing: the leading sentence is what makes the text a condition,
    /// the branches are what make an exempt stop answer `ok: true`, and the
    /// trailing clause is what keeps the status marker in the block reason.
    #[test]
    fn the_verification_prompt_is_one_condition_with_three_branches() {
        let nonclaim = format!(
            "- 最終メッセージが {MARKER_NEEDS_INPUT} または {MARKER_FAILED} を報告している。エージェント自身が未完了を申告しているので、完了検証の対象ではない"
        );
        let background = "- バックグラウンドタスク（サブエージェント等）が実行中のままターンを終える中間停止である。これはハートビートであって完了申告ではない";
        let completion = "- 完了を申告しており、かつ作業が指示された要件を実際に満たしている。対象リポジトリの現在のコードと状態に基づいて確かめること。表面的な自己申告では足りず、変更が意図どおり機能し破綻や取りこぼしが無いこと";
        let convention = format!(
            "条件が満たされていない場合は、何が不足しているかに加えて「応答の最終行に {MARKER_COMPLETED} / {MARKER_NEEDS_INPUT} / {MARKER_FAILED} のいずれかを付けること」を reason に含めること。"
        );
        assert_eq!(
            Prompts::builtin().verification_prompt(),
            format!(
                "この停止を許可してよい。すなわち次のいずれかが成り立つ:\n\n{nonclaim}\n{background}\n{completion}\n\n{convention}"
            )
        );
        assert_eq!(Prompts::builtin().verification_rubric(), completion);
    }

    /// #389: a Stop already reporting NEEDS_INPUT/FAILED must make the
    /// condition TRUE, so the judge answers `ok: true` and the turn ends.
    ///
    /// **The wording has to stay declarative.** Claude Code's judge is asked
    /// "is the user-provided condition met?" and blocks on `ok: false`; it has
    /// no way to act on an instruction. #389 first shipped an imperative
    /// ("その場合は…停止を許可してください") and measured it live: the judge
    /// applied the clause correctly and quoted it verbatim in all 8 rounds,
    /// then answered `ok: false` every time, because "verification does not
    /// apply" is not "the condition is met".
    ///
    /// Asserted on the **composed** output: an assembly that dropped
    /// `{nonclaim_exemption}` would leave the leaf intact and still ship the
    /// bug.
    #[test]
    fn a_stop_that_claims_no_completion_satisfies_the_condition() {
        let rendered = Prompts::builtin().verification_prompt();
        assert!(
            rendered.starts_with("この停止を許可してよい。すなわち次のいずれかが成り立つ:"),
            "the prompt has to read as a condition, not as orders: {rendered}"
        );
        assert!(
            rendered.contains(&format!(
                "- 最終メッセージが {MARKER_NEEDS_INPUT} または {MARKER_FAILED} を報告している"
            )),
            "the non-claim branch is missing from the composed condition: {rendered}"
        );
        // The imperative that did not work must not creep back in.
        assert!(
            !rendered.contains("停止を許可してください"),
            "an instruction to allow is not something the judge can act on: {rendered}"
        );
    }

    /// The branches must not swallow the thing they sit next to: a Stop that
    /// *does* claim completion is only allowed when the work holds up. A
    /// condition that were true unconditionally would pass the test above while
    /// disabling verification entirely (D-01).
    #[test]
    fn a_completion_claim_still_has_to_earn_the_condition() {
        let rendered = Prompts::builtin().verification_prompt();
        assert!(
            rendered.contains("完了を申告しており、かつ作業が指示された要件を実際に満たしている"),
            "the completion branch is conjunctive — claiming is not enough: {rendered}"
        );
        assert!(
            rendered.contains("表面的な自己申告では足りず"),
            "the self-report escape hatch must stay closed: {rendered}"
        );
        // `ok: false` carries `reason` back to the agent, and its next turn has
        // to be parseable by `on-stop.sh`.
        assert!(
            Prompts::missing_markers(&rendered).is_empty(),
            "the block reason must still teach every marker: {rendered}"
        );
    }

    /// The two exemptions are separate keys in `defaults.toml`, and both reach
    /// the composed condition. They were separate so that rewording one could
    /// not silently drop the other; nothing can reword them any more (#465),
    /// but the composed prompt still has to carry both — #389 is the reason the
    /// non-claim branch exists at all.
    #[test]
    fn both_exemptions_reach_the_composed_condition() {
        let rendered = Prompts::builtin().verification_prompt();
        assert!(
            rendered.contains("バックグラウンドタスク"),
            "the background exemption is a branch of the condition: {rendered}"
        );
        assert!(
            rendered.contains(MARKER_NEEDS_INPUT) && rendered.contains(MARKER_FAILED),
            "the non-claim exemption names the markers it exempts: {rendered}"
        );
    }

    /// ADR-0020, moved here by #465. The composed self-report and the composed
    /// judging prompt must teach every marker, and with no override surface
    /// left this is a property of `defaults.toml` alone — so it belongs in a
    /// unit test rather than in config validation, which used to carry it and
    /// could only ever have found a breakage at startup.
    #[test]
    fn every_builtin_composition_teaches_every_marker() {
        for (what, text) in [
            (
                "the plain self-report",
                Prompts::builtin().marker_self_report().to_string(),
            ),
            (
                "the judging prompt",
                Prompts::builtin().verification_prompt(),
            ),
        ] {
            assert!(
                Prompts::missing_markers(&text).is_empty(),
                "{what} drops {:?}: {text}",
                Prompts::missing_markers(&text)
            );
        }
    }

    /// [`ALLOWED_PLACEHOLDERS`] is the table `defaults.toml` is written
    /// against. It used to be enforced only on operator overrides, which meant
    /// a typo introduced in the asset itself — the one file that ships in every
    /// build — went unchecked. #465 removed the overrides; the table now checks
    /// the asset.
    #[test]
    fn every_builtin_prompt_uses_only_its_allowed_placeholders() {
        let raw: toml::Table = toml::from_str(include_str!("defaults.toml")).unwrap();
        let table = raw["prompts"].as_table().unwrap();
        for (key, allowed) in ALLOWED_PLACEHOLDERS {
            let value = table[*key]
                .as_str()
                .unwrap_or_else(|| panic!("{key} is a string"));
            for name in crate::template::scan(value, crate::template::ScanMode::Rendered) {
                assert!(
                    allowed.contains(&name),
                    "defaults.toml `{key}` uses `{{{name}}}`, which is not in its allowed set \
                     {allowed:?} — an unknown name is emitted verbatim at render time"
                );
            }
        }
        assert_eq!(
            table.len(),
            ALLOWED_PLACEHOLDERS.len(),
            "every key in defaults.toml needs a row in ALLOWED_PLACEHOLDERS"
        );
    }

    #[test]
    fn with_rubric_replaces_only_the_rubric() {
        let custom = Prompts::builtin().with_rubric("独自の観点");
        let rendered = custom.verification_prompt();
        assert!(
            rendered.contains("独自の観点"),
            "the custom rubric reaches the composed condition: {rendered}"
        );
        // No longer `starts_with`: since #389 the prompt opens with the fixed
        // sentence that makes it a condition, and the rubric is one branch
        // underneath it rather than the whole text.
        assert!(
            rendered.starts_with("この停止を許可してよい。"),
            "a custom rubric must not cost the condition framing: {rendered}"
        );
        assert!(
            !rendered.contains(Prompts::builtin().verification_rubric()),
            "the default rubric is replaced, not appended"
        );
        assert!(
            rendered.contains(MARKER_COMPLETED),
            "the marker convention survives a custom rubric"
        );
        // The exempting branches are not the rubric's to take with it — an
        // operator narrowing the completion criteria must not re-break #389.
        assert!(
            rendered.contains(MARKER_NEEDS_INPUT),
            "the non-claim branch survives a custom rubric: {rendered}"
        );
    }

    /// One workflow, with whatever extra keys the caller adds.
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

    /// One profile workflow, with whatever extra keys the caller adds.
    fn profile_cfg(profile: &str, extra: &str) -> RootConfig {
        RootConfig::from_toml_str(&format!(
            r#"
[[workflows]]
name = "w"
source = "github"
profile = "{profile}"
agent = "herdr"
{extra}
"#
        ))
        .unwrap()
    }

    /// Each profile's default rubric, post-#440: `triage` is judged on the
    /// artifact URL (#398), `design` / `implement` on the human's explicit
    /// approval in the conversation (#440), and `answer` on the generic
    /// rubric.
    ///
    /// `answer` matters as much as the others: its reply goes back through
    /// the plugin's approval gate, so there is no URL to produce and demanding
    /// one would fail every well-behaved answer.
    #[test]
    fn each_profile_resolves_to_its_own_rubric_default() {
        let c = profile_cfg("triage", "");
        let rubric = Prompts::resolve_for(&c.workflows[0])
            .verification_rubric()
            .to_string();
        assert!(
            rubric.contains("URL"),
            "triage must be judged on the artifact URL: {rubric}"
        );
        assert_ne!(rubric, Prompts::builtin().verification_rubric());

        for profile in ["design", "implement"] {
            let c = profile_cfg(profile, "");
            let rubric = Prompts::resolve_for(&c.workflows[0])
                .verification_rubric()
                .to_string();
            assert!(
                rubric.contains("人間（ユーザー）が完了を明示的に承認している"),
                "{profile} must be judged on the human's approval: {rubric}"
            );
            assert!(
                !rubric.contains("URL"),
                "the human saw the artifact — a URL demand would second-guess \
                 an approval already given: {rubric}"
            );
        }

        let c = profile_cfg("answer", "");
        assert_eq!(
            Prompts::resolve_for(&c.workflows[0]).verification_rubric(),
            Prompts::builtin().verification_rubric(),
            "answer publishes through the plugin, so there is no URL to require"
        );
    }

    /// #440: design / implement teach the ask-then-COMPLETED protocol; answer /
    /// triage keep the plain self-report. The confirm variant must still teach
    /// every marker — it goes through the same `missing_markers` validation.
    #[test]
    fn design_and_implement_teach_the_confirmation_protocol() {
        for profile in ["design", "implement"] {
            let c = profile_cfg(profile, "");
            let text = Prompts::resolve_for(&c.workflows[0])
                .marker_self_report()
                .to_string();
            assert!(
                text.contains("the human in this conversation is the final judge of completion"),
                "{profile} must name the human as the judge: {text}"
            );
            assert!(
                text.contains("only after the human has explicitly approved"),
                "{profile} must gate COMPLETED on an explicit approval: {text}"
            );
            assert!(
                text.contains("完了確認待ち"),
                "{profile} must teach the confirmation-park reason: {text}"
            );
            assert!(
                Prompts::missing_markers(&text).is_empty(),
                "the confirm variant must still teach every marker: {text}"
            );
        }

        for profile in ["answer", "triage"] {
            let c = profile_cfg(profile, "");
            assert_eq!(
                Prompts::resolve_for(&c.workflows[0]).marker_self_report(),
                Prompts::builtin().marker_self_report(),
                "{profile} keeps the plain self-report"
            );
        }
    }

    /// The approval rubric composes into the same condition frame, and the
    /// branches it sits next to survive — a confirmation-request stop
    /// (NEEDS_INPUT) must still satisfy the non-claim branch.
    #[test]
    fn the_approval_rubric_keeps_the_condition_frame_and_the_exemptions() {
        let c = profile_cfg("design", "");
        let rendered = Prompts::resolve_for(&c.workflows[0]).verification_prompt();
        assert!(
            rendered.starts_with("この停止を許可してよい。すなわち次のいずれかが成り立つ:"),
            "the approval rubric must not cost the condition framing: {rendered}"
        );
        assert!(
            rendered.contains("人間（ユーザー）が完了を明示的に承認している"),
            "the approval branch reaches the composed condition: {rendered}"
        );
        assert!(
            rendered.contains(&format!(
                "最終メッセージが {MARKER_NEEDS_INPUT} または {MARKER_FAILED} を報告している"
            )),
            "the non-claim branch survives — the confirmation request itself \
             stops with NEEDS_INPUT and must pass the judge: {rendered}"
        );
        assert!(
            !rendered.contains("停止を許可してください"),
            "still a condition, not an order (#389): {rendered}"
        );
    }

    /// The whole precedence ladder for the rubric leaf, after #465 cut it from
    /// five rungs to three.
    ///
    /// **The rung that went is the point of the change.** A global
    /// `[prompts].verification_rubric` used to sit *between* these two, above
    /// the profile default, so setting it once cost every `triage` workflow its
    /// artifact-URL check and every `design` workflow the approval check — with
    /// no symptom beyond a task passing on work it never posted. The rung that
    /// stayed is per-workflow, so it cannot reach a workflow the operator was
    /// not looking at.
    #[test]
    fn the_workflow_rubric_beats_the_profile_default_which_beats_the_builtin() {
        let rubric = |c: &RootConfig| {
            Prompts::resolve_for(&c.workflows[0])
                .verification_rubric()
                .to_string()
        };

        // The generic built-in, when no profile supplies one.
        let c = profile_cfg("answer", "");
        assert_eq!(rubric(&c), Prompts::builtin().verification_rubric());

        // The profile default beats it (post-#440 `design` resolves to the
        // approval rubric; the ladder slot is what matters here).
        let c = profile_cfg("design", "");
        assert!(rubric(&c).contains("承認"));

        // The workflow's own `rubric` beats the profile default.
        let c = profile_cfg("design", "rubric = \"ワークフロー\"\n");
        assert_eq!(rubric(&c), "ワークフロー");
    }

    #[test]
    fn the_workflow_rubric_feeds_the_verification_prompt() {
        let c = cfg("rubric = \"レガシー\"\n");
        let p = Prompts::resolve_for(&c.workflows[0]);
        assert_eq!(p.verification_rubric(), "レガシー");
        // Identical to what `with_rubric` produced before #314 — the key is
        // older than the surface #465 removed, and outlived it.
        assert_eq!(
            p.verification_prompt(),
            Prompts::builtin()
                .with_rubric("レガシー")
                .verification_prompt()
        );
    }

    /// A `[prompts]` table changes nothing at all now: resolution ignores it
    /// and validation is what rejects it (see `config::validate`). Pinned here
    /// because "removed" has two possible meanings — refused, or accepted and
    /// ignored — and only the first is safe.
    #[test]
    fn a_removed_prompts_table_does_not_reach_the_resolved_set() {
        let c = cfg("\n[prompts]\nverification_rubric = \"グローバル\"\n");
        assert_eq!(
            Prompts::resolve_for(&c.workflows[0]).verification_rubric(),
            Prompts::builtin().verification_rubric()
        );
        assert_eq!(
            PromptSet::from_config(&c).global().verification_rubric(),
            Prompts::builtin().verification_rubric()
        );
    }

    #[test]
    fn prompt_set_falls_back_to_global_for_an_unknown_workflow() {
        let c = cfg("rubric = \"ワークフロー\"\n");
        let set = PromptSet::from_config(&c);
        assert_eq!(
            set.for_workflow("reply").verification_rubric(),
            "ワークフロー"
        );
        // A task can outlive its workflow; that must not panic or lose the
        // marker convention. The fallback is the built-in set, so the workflow
        // rubric does *not* leak to a workflow that no longer exists.
        assert_eq!(
            set.for_workflow("消えた").verification_rubric(),
            Prompts::builtin().verification_rubric()
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
