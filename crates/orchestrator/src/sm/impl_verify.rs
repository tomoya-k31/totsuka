//! ImplVerify sub-state machine. spec §9.3 + §4.2 + §11.15.

use serde::Deserialize;
use std::collections::HashMap;
use totsuka_core::{key::spawn_effect_key, DomainEvent, Phase, TaskId};

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::branch::branch_name;
use crate::conversation::spawn_verifier;
use crate::effect::ClaimOutcome;
use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackResult;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

#[derive(Deserialize)]
struct PrMergedReady {
    pub item_id: String,
    #[serde(default)]
    pub pr_diff: String,
}

pub async fn on_pr_merged_ready(
    e: &Engine,
    ev: &DomainEvent,
) -> Result<HandleOutcome, OrchestratorError> {
    let p: PrMergedReady = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload: {err}")))?;
    let id = TaskId::new(p.item_id.clone());
    let task = match e.repo.get(&id).await? {
        Some(t) => t,
        None => {
            return Ok(HandleOutcome::Skipped {
                reason: "no such task".into(),
            })
        }
    };
    let key = spawn_effect_key(&id, Phase::ImplVerify, task.impl_verify_attempt);
    let result = e.effects.result_for(&key).await?;
    let agent_id = result
        .as_ref()
        .and_then(|v| v.get("agent_id"))
        .and_then(|x| x.as_str())
        .map(String::from);
    let Some(agent_id) = agent_id else {
        return Ok(HandleOutcome::Skipped {
            reason: "no implementer agent recorded".into(),
        });
    };
    spawn_verifier(e, &task, &agent_id, &p.pr_diff).await
}

#[derive(Deserialize)]
struct Verification {
    pub item_id: String,
}

pub async fn on_verification(
    e: &Engine,
    ev: &DomainEvent,
    passed: bool,
) -> Result<HandleOutcome, OrchestratorError> {
    let p: Verification = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload: {err}")))?;
    let id = TaskId::new(p.item_id);
    let task = match e.repo.get(&id).await? {
        Some(t) => t,
        None => {
            return Ok(HandleOutcome::Skipped {
                reason: "no such task".into(),
            })
        }
    };
    if passed {
        if task.suppress_writeback_until_human_move {
            return Ok(HandleOutcome::Skipped {
                reason: "suppressed".into(),
            });
        }
        match e
            .writeback
            .move_column(task.id.as_str(), "final_review", None)
            .await?
        {
            WritebackResult::Ok => Ok(HandleOutcome::Applied),
            WritebackResult::VersionMismatch => {
                e.repo.set_suppress(&task.id, true).await?;
                Ok(HandleOutcome::Skipped {
                    reason: "OCC".into(),
                })
            }
            WritebackResult::Failed(m) => Err(OrchestratorError::Writeback(m)),
        }
    } else {
        // DiffBack: bump attempt, restart implementer.
        let new_attempt = e.repo.bump_attempt(&task.id).await?;
        tracing::info!(task=%task.id.as_str(), new_attempt, "DiffBack: re-entering ImplVerify");
        let updated = e.repo.get(&task.id).await?.unwrap();
        on_enter(e, &updated).await
    }
}

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
