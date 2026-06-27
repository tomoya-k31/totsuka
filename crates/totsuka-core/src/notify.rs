use serde::{Deserialize, Serialize};

/// spec §13.1: 通知種別。NotifyPayload は totsuka-telemetry 側で持つ
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyKind {
    HumanGate1,
    HumanGate2,
    TaskFailed,
    TaskStuck,
    GivingUp,
    ProcessDead,
    ProcessUnhealthy,
    PgmqDead,
    ConfigError,
    SecretRotationWarn,
    WritebackConflict,
    ArgvSecretViolation,
    QaSpawnFailed,
    QaAnswerTimeout,
    WorktreeGcAlert,
}

impl NotifyKind {
    pub fn as_snake(&self) -> &'static str {
        // serde 表現と同じ形式
        match self {
            NotifyKind::HumanGate1 => "human_gate1",
            NotifyKind::HumanGate2 => "human_gate2",
            NotifyKind::TaskFailed => "task_failed",
            NotifyKind::TaskStuck => "task_stuck",
            NotifyKind::GivingUp => "giving_up",
            NotifyKind::ProcessDead => "process_dead",
            NotifyKind::ProcessUnhealthy => "process_unhealthy",
            NotifyKind::PgmqDead => "pgmq_dead",
            NotifyKind::ConfigError => "config_error",
            NotifyKind::SecretRotationWarn => "secret_rotation_warn",
            NotifyKind::WritebackConflict => "writeback_conflict",
            NotifyKind::ArgvSecretViolation => "argv_secret_violation",
            NotifyKind::QaSpawnFailed => "qa_spawn_failed",
            NotifyKind::QaAnswerTimeout => "qa_answer_timeout",
            NotifyKind::WorktreeGcAlert => "worktree_gc_alert",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_variants_have_unique_snake() {
        let all = [
            NotifyKind::HumanGate1,
            NotifyKind::HumanGate2,
            NotifyKind::TaskFailed,
            NotifyKind::TaskStuck,
            NotifyKind::GivingUp,
            NotifyKind::ProcessDead,
            NotifyKind::ProcessUnhealthy,
            NotifyKind::PgmqDead,
            NotifyKind::ConfigError,
            NotifyKind::SecretRotationWarn,
            NotifyKind::WritebackConflict,
            NotifyKind::ArgvSecretViolation,
            NotifyKind::QaSpawnFailed,
            NotifyKind::QaAnswerTimeout,
            NotifyKind::WorktreeGcAlert,
        ];
        let s: std::collections::HashSet<_> = all.iter().map(|k| k.as_snake()).collect();
        assert_eq!(s.len(), all.len(), "all snake forms must be unique");
    }

    #[test]
    fn snake_form_matches_serde() {
        let k = NotifyKind::TaskStuck;
        let j = serde_json::to_string(&k).unwrap();
        assert_eq!(j, format!("\"{}\"", k.as_snake()));
    }
}
