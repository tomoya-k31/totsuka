//! Filled progressively by Tasks 14-17.

use crate::error::OrchestratorError;
use crate::sm::{Engine, HandleOutcome};
use totsuka_core::DomainEvent;

pub async fn on_pr_merged_ready(
    _e: &Engine,
    _ev: &DomainEvent,
) -> Result<HandleOutcome, OrchestratorError> {
    Ok(HandleOutcome::Skipped {
        reason: "not yet implemented".into(),
    })
}

pub async fn on_verification(
    _e: &Engine,
    _ev: &DomainEvent,
    _passed: bool,
) -> Result<HandleOutcome, OrchestratorError> {
    Ok(HandleOutcome::Skipped {
        reason: "not yet implemented".into(),
    })
}

use std::collections::HashMap;
use totsuka_core::{key::spawn_effect_key, Phase, TaskId};

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::branch::branch_name;
use crate::effect::ClaimOutcome;
use crate::repository::Task;

pub async fn on_enter(e: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError> {
    let permit = match e.wip.try_acquire() {
        Some(p) => p,
        None => return Ok(HandleOutcome::WipFull),
    };
    let id: TaskId = task.id.clone();
    let attempt = task.impl_verify_attempt;
    let key = spawn_effect_key(&id, Phase::ImplVerify, attempt);
    let outcome = e
        .effects
        .claim(
            &key,
            &format!("derived:iv:{}", id.as_str()),
            "spawn",
            &e.owner_id,
        )
        .await?;
    if let ClaimOutcome::Skipped { reason } = outcome {
        drop(permit);
        return Ok(HandleOutcome::Skipped { reason });
    }

    let argv = merge_argv(
        &e.config.orchestrator.claude_argv,
        &task.repo,
        &Phase::ImplVerify,
    );
    let req = SpawnReq {
        task_id: id.as_str().into(),
        phase: Phase::ImplVerify.as_snake().into(),
        attempt,
        repo: task.repo.clone(),
        branch: branch_name(&id, Phase::ImplVerify),
        argv,
        env: HashMap::new(),
    };

    let res = match e.adapter.spawn(req).await {
        Ok(r) => r,
        Err(err) => {
            e.effects.fail(&key, &err.to_string()).await?;
            drop(permit);
            return Err(err);
        }
    };

    let now = e.clock.now();
    let mut updated = task.clone();
    updated.current_phase = Some(Phase::ImplVerify.as_snake().into());
    updated.spawned_at = Some(now);
    updated.updated_at = now;
    e.repo.upsert(&updated).await?;

    e.effects
        .complete(
            &key,
            serde_json::json!({
                "agent_id": res.agent_id,
                "terminal_id": res.terminal_id,
                "worktree_path": res.worktree_path,
                "role": "implementer",
            }),
        )
        .await?;

    drop(permit);
    Ok(HandleOutcome::Applied)
}
