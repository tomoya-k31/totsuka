//! Renders the per-phase prompt template sent to a freshly spawned agent.
//! Templates live in `[orchestrator.prompts]`; placeholders are `{repo}`,
//! `{issue_number}`, `{branch}`, `{task_id}`.

use crate::repository::Task;

pub fn render(template: &str, task: &Task, branch: &str) -> String {
    let issue_number = task
        .issue_number
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    template
        .replace("{repo}", &task.repo)
        .replace("{issue_number}", &issue_number)
        .replace("{branch}", branch)
        .replace("{task_id}", task.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use totsuka_core::TaskId;

    fn task(issue_number: Option<i64>) -> Task {
        let t0 = Utc.with_ymd_and_hms(2026, 7, 3, 0, 0, 0).unwrap();
        Task {
            id: TaskId::new("PVTI_prompt_test".to_string()),
            task_id_short: "VTI_prompt_t".to_string(),
            repo: "acme/rocket".to_string(),
            issue_number,
            pr_node_id: None,
            current_column: "design".to_string(),
            current_phase: None,
            impl_verify_attempt: 0,
            suppress_writeback_until_human_move: false,
            spawned_at: None,
            created_at: t0,
            updated_at: t0,
        }
    }

    #[test]
    fn substitutes_all_placeholders() {
        let out = render(
            "repo={repo} issue=#{issue_number} branch={branch} id={task_id}",
            &task(Some(42)),
            "totsuka/x/design",
        );
        assert_eq!(
            out,
            "repo=acme/rocket issue=#42 branch=totsuka/x/design id=PVTI_prompt_test"
        );
    }

    #[test]
    fn missing_issue_number_renders_unknown() {
        let out = render("issue #{issue_number}", &task(None), "b");
        assert_eq!(out, "issue #unknown");
    }
}
