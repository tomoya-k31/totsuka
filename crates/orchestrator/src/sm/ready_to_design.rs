use std::collections::HashMap;
use totsuka_core::{key::spawn_effect_key, Phase, TaskId};

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::branch::branch_name;
use crate::effect::ClaimOutcome;
use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

/// `seq` is the status-transition generation: it fills the attempt slot of
/// the effect key (and the herdr label), so a card sent back to design by
/// review spawns a fresh designer while redeliveries of the same event
/// stay absorbed.
pub async fn try_spawn(
    e: &Engine,
    task: &Task,
    seq: i64,
) -> Result<HandleOutcome, OrchestratorError> {
    let permit = match e.wip.try_acquire() {
        Some(p) => p,
        None => return Ok(HandleOutcome::WipFull),
    };
    let id: TaskId = task.id.clone();
    let generation = i32::try_from(seq).unwrap_or(i32::MAX);
    let key = spawn_effect_key(&id, Phase::Design, generation);

    let outcome = e
        .effects
        .claim(
            &key,
            &format!("derived:ready:{}", id.as_str()),
            "spawn",
            &e.owner_id,
        )
        .await?;
    if let ClaimOutcome::Skipped { reason } = outcome {
        drop(permit);
        return Ok(HandleOutcome::Skipped { reason });
    }

    let mut argv = merge_argv(
        &e.config.orchestrator.claude_argv,
        &task.repo,
        &Phase::Design,
    );
    // The task prompt is the final argv element (`claude "<prompt>"`):
    // typing it into the pane after spawn races TUI startup / paste
    // detection and the submitting CR gets swallowed — argv delivery has
    // no race by construction.
    argv.push(crate::prompt::render(
        &e.config.orchestrator.prompts.design,
        task,
        &branch_name(&id, Phase::Design),
        &e.config.github.project_owner,
        e.config.github.project_number,
    ));
    let req = SpawnReq {
        task_id: id.as_str().into(),
        phase: Phase::Design.as_snake().into(),
        attempt: generation,
        repo: task.repo.clone(),
        branch: branch_name(&id, Phase::Design),
        argv,
        env: HashMap::new(),
        // Design produces an issue comment, not commits — no branch.
        detached: true,
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
    updated.current_phase = Some(Phase::Design.as_snake().into());
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
            }),
        )
        .await?;

    drop(permit);
    Ok(HandleOutcome::Applied)
}
