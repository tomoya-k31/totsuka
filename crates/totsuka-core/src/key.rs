use crate::{Phase, TaskId};

/// GitHub webhook delivery 由来 (spec §8.1)
pub fn event_key_gh_delivery(delivery_id: &str) -> String {
    format!("gh:delivery:{}", delivery_id)
}

/// GitHub Project status snapshot diff 由来 (spec §8.3、catchup)
pub fn event_key_gh_status(item_id: &str, to_status_hash: &str) -> String {
    format!("gh:status:{}:{}", item_id, to_status_hash)
}

/// GitHub issue updated (REST since pull) 由来
pub fn event_key_gh_issue(issue_node_id: &str, updated_at_ms: i64) -> String {
    format!("gh:issue:{}:{}", issue_node_id, updated_at_ms)
}

/// Slack event 由来
pub fn event_key_slack(event_id: &str) -> String {
    format!("slack:event:{}", event_id)
}

/// orchestrator 内部派生 (deterministic)
pub fn event_key_derived(key: &str) -> String {
    format!("derived:{}", key)
}

/// agent spawn 副作用キー (spec §11.15: attempt で DiffBack 再 spawn を区別)
pub fn spawn_effect_key(task: &TaskId, phase: Phase, attempt: i32) -> String {
    format!("spawn:{}:{}:{}", task.as_str(), phase.as_snake(), attempt)
}

/// カラム移動副作用 (spec §8.2 型B)
pub fn column_move_effect_key(task: &TaskId, to_status_snake: &str) -> String {
    format!("move:{}:{}", task.as_str(), to_status_snake)
}

/// Slack 投稿副作用
pub fn slack_post_effect_key(channel: &str, event_id: &str) -> String {
    format!("slack:{}:{}", channel, event_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_key_formats_are_stable() {
        assert_eq!(event_key_gh_delivery("abc-123"), "gh:delivery:abc-123");
        assert_eq!(event_key_slack("Ev01"), "slack:event:Ev01");
        assert_eq!(
            event_key_derived("phase_timeout:t1"),
            "derived:phase_timeout:t1"
        );
    }

    #[test]
    fn spawn_effect_key_includes_attempt() {
        let t = TaskId::new("PVTI_x");
        assert_eq!(
            spawn_effect_key(&t, Phase::ImplVerify, 0),
            "spawn:PVTI_x:impl_verify:0"
        );
        assert_eq!(
            spawn_effect_key(&t, Phase::ImplVerify, 1),
            "spawn:PVTI_x:impl_verify:1"
        );
        assert_eq!(
            spawn_effect_key(&t, Phase::Design, 0),
            "spawn:PVTI_x:design:0"
        );
    }

    #[test]
    fn diff_back_produces_different_effect_key() {
        let t = TaskId::new("PVTI_y");
        let k1 = spawn_effect_key(&t, Phase::ImplVerify, 0);
        let k2 = spawn_effect_key(&t, Phase::ImplVerify, 1);
        assert_ne!(k1, k2);
    }
}
