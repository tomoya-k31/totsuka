//! Renders the per-phase prompt template sent to a freshly spawned agent.
//! Templates live in `[orchestrator.prompts]`; placeholders are `{repo}`,
//! `{issue_number}`, `{branch}`, `{task_id}`.

use crate::repository::Task;

pub fn render(
    template: &str,
    task: &Task,
    branch: &str,
    project_owner: &str,
    project_number: u64,
) -> String {
    let issue_number = task
        .issue_number
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let rendered = template
        .replace("{repo}", &task.repo)
        .replace("{issue_number}", &issue_number)
        .replace("{branch}", branch)
        .replace("{task_id}", task.id.as_str())
        .replace("{project_owner}", project_owner)
        .replace("{project_number}", &project_number.to_string());
    if task.issue_number.is_some() {
        return rendered;
    }
    // Draft project items have no linked issue: instructions like
    // `gh issue view unknown` are not executable, so tell the agent
    // explicitly what to fall back on instead of failing silently.
    format!(
        "{rendered}\n\n注意: このカード ({}) には紐づく issue がありません。\
         上記の issue 参照の指示は無視し、プロジェクトカードのタイトルと\
         リポジトリの状況から作業内容を判断してください。",
        task.id.as_str()
    )
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
            "repo={repo} issue=#{issue_number} branch={branch} id={task_id} prj={project_owner}/{project_number}",
            &task(Some(42)),
            "totsuka/x/design",
            "acme",
            9,
        );
        assert_eq!(
            out,
            "repo=acme/rocket issue=#42 branch=totsuka/x/design id=PVTI_prompt_test prj=acme/9"
        );
    }

    #[test]
    fn missing_issue_number_appends_explicit_caveat() {
        // "unknown" alone would leave instructions like `gh issue view
        // unknown` silently non-executable — the agent must be told the
        // issue reference is void and what to fall back on.
        let out = render("issue #{issue_number}", &task(None), "b", "acme", 9);
        assert!(out.starts_with("issue #unknown"), "got: {out}");
        assert!(
            out.contains("紐づく issue がありません"),
            "must warn that no issue is linked: {out}"
        );
        assert!(
            out.contains("PVTI_prompt_test"),
            "fallback must point at the task id: {out}"
        );
    }

    #[test]
    fn present_issue_number_has_no_caveat() {
        let out = render("issue #{issue_number}", &task(Some(5)), "b", "acme", 9);
        assert_eq!(out, "issue #5");
    }
}
