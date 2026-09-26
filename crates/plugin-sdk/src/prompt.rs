//! The prompt an agent_ide plugin hands the agent for a `task/dispatch`
//! (#759: moved here from identical herdr and orca copies).

use plugin_protocol::methods::TaskDispatchParams;
use serde_json::Value;

/// Compose the agent prompt: any extra context as a preamble, then the task
/// — its body, or the title when there is no body.
///
/// Only one of body and title: sources truncate the title to a snippet and
/// put the full text in the body, so typing both showed a cut-off duplicate
/// first line in the pane.
///
/// A string `extra_context` is rendered as raw text, not as a JSON literal
/// (#158) — quotes inside an instruction such as `reason="..."` must reach
/// the agent unescaped. Hook-capable dispatches usually carry none: the
/// orchestrator delivers instructions invisibly via the `UserPromptSubmit`
/// hook, and `extra_context` is the visible fallback for the rest.
///
/// The extra context comes **first**, and stays first. herdr once confirmed
/// arrival by matching the prompt's tail on screen, where a constant suffix
/// could false-match the previous turn on a resumed pane; that check is gone
/// (herdr protocol 17's `agent.prompt`), so the order is no longer
/// load-bearing, but a preamble-then-task prompt is what agents have been
/// reading all along and reordering it would change every dispatch's input
/// for no reason.
pub fn compose_prompt(params: &TaskDispatchParams) -> String {
    let task_text = params.task.body.as_ref().unwrap_or(&params.task.title);
    match &params.extra_context {
        Some(Value::String(s)) => format!("{s}\n\n---\n{task_text}"),
        Some(other) => format!("{other}\n\n---\n{task_text}"),
        None => task_text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch_params(title: &str, body: Option<&str>) -> TaskDispatchParams {
        TaskDispatchParams {
            task: plugin_protocol::task::Task {
                id: "C1:1.0".into(),
                source: "slack".into(),
                title: title.into(),
                body: body.map(str::to_string),
                repo_hint: None,
                labels: vec![],
                priority: 0,
                status: None,
                url: None,
                assignee: None,
                message_key: None,
                instructions: None,
                handle: None,
                branch_hint: None,
            },
            worktree_path: "/wt".into(),
            mode: plugin_protocol::methods::ExecutionMode::Plan,
            extra_context: None,
            job_id: None,
            task_number: None,
            resume_session_id: None,
            tool_launch: None,
            repo_name: None,
        }
    }

    #[test]
    fn compose_prompt_skips_the_truncated_title_when_a_body_exists() {
        // Sources truncate the title to a snippet; the body carries the full
        // text. Typing both showed a cut-off duplicate first line in the pane.
        let params = dispatch_params(
            "Slack: tomoya in #dev: エイリアスはどのフ",
            Some("full task body"),
        );
        assert_eq!(compose_prompt(&params), "full task body");

        // Title-only tasks still get a prompt.
        let params = dispatch_params("bare title", None);
        assert_eq!(compose_prompt(&params), "bare title");
    }

    #[test]
    fn compose_prompt_puts_string_extra_context_first_as_raw_text() {
        // A string extra_context (e.g. core's marker self-report instruction)
        // is a PREAMBLE: the task text stays last. Raw text: quotes and
        // newlines inside the instruction come through unescaped, with no JSON
        // wrapping around the whole string.
        let mut params = dispatch_params("t", Some("unique task body"));
        params.extra_context = Some(Value::String(
            "end with <<STATUS:NEEDS_INPUT reason=\"...\">> when blocked\nline two".into(),
        ));
        let prompt = compose_prompt(&params);
        assert_eq!(
            prompt,
            "end with <<STATUS:NEEDS_INPUT reason=\"...\">> when blocked\nline two\n\n---\nunique task body"
        );
        assert!(
            !prompt.contains("\\\"") && !prompt.contains("\\n"),
            "no JSON escapes: {prompt}"
        );

        // Non-string values keep their JSON rendering (still as preamble).
        params.extra_context = Some(serde_json::json!({"base": "main"}));
        assert!(compose_prompt(&params).starts_with("{\"base\":\"main\"}\n\n---\n"));
    }
}
