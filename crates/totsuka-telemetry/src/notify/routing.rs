use std::collections::HashMap;
use totsuka_core::NotifyKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SinkId {
    Log,
    Slack,
    Github,
}

/// spec §13.5 の写像表
pub fn default_routing() -> HashMap<NotifyKind, Vec<SinkId>> {
    use NotifyKind::*;
    let log_only = vec![SinkId::Log];
    let log_slack = vec![SinkId::Log, SinkId::Slack];
    let mut m = HashMap::new();
    for k in [
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
    ] {
        m.insert(k, log_slack.clone());
    }
    m.insert(QaAnswerTimeout, log_only.clone());
    m.insert(WorktreeGcAlert, log_only);
    m
}

/// 種別ごとの dedup TTL 秒。0 = dedup 無効
pub fn default_dedup_ttl() -> HashMap<NotifyKind, u64> {
    use NotifyKind::*;
    let mut m = HashMap::new();
    m.insert(HumanGate1, 0);
    m.insert(HumanGate2, 0);
    m.insert(TaskFailed, 0);
    m.insert(GivingUp, 0);
    m.insert(ProcessDead, 0);
    m.insert(ArgvSecretViolation, 0);
    m.insert(TaskStuck, 3600);
    m.insert(ProcessUnhealthy, 600);
    m.insert(PgmqDead, 600);
    m.insert(ConfigError, 1800);
    m.insert(WritebackConflict, 3600);
    m.insert(QaSpawnFailed, 300);
    m.insert(QaAnswerTimeout, 600);
    m.insert(WorktreeGcAlert, 3600);
    m.insert(SecretRotationWarn, 86400);
    m
}
