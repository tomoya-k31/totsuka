use std::collections::HashMap;
use totsuka_core::{key::spawn_effect_key, Phase, TaskId};

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::branch::branch_name;
use crate::effect::ClaimOutcome;
use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn try_spawn(e: &Engine, task: &Task) -> Result<HandleOutcome, OrchestratorError> {
    let permit = match e.wip.try_acquire() {
        Some(p) => p,
        None => return Ok(HandleOutcome::WipFull),
    };
    let id: TaskId = task.id.clone();
    let key = spawn_effect_key(&id, Phase::Design, 0);

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

    let argv = merge_argv(
        &e.config.orchestrator.claude_argv,
        &task.repo,
        &Phase::Design,
    );
    let req = SpawnReq {
        task_id: id.as_str().into(),
        phase: Phase::Design.as_snake().into(),
        attempt: 0,
        repo: task.repo.clone(),
        branch: branch_name(&id, Phase::Design),
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

    // Hand the agent its task right away — an idle Claude with no
    // instructions is useless (spec: [orchestrator.prompts]).
    let prompt = crate::prompt::render(
        &e.config.orchestrator.prompts.design,
        task,
        &branch_name(&id, Phase::Design),
    );
    // Trailing CR = Enter: the agent's TUI submits on \r; without it the
    // prompt sits in the input box forever (verified on a live pane).
    let prompt = format!("{prompt}\r");
    if let Err(err) = e.adapter.send(&res.agent_id, &prompt).await {
        e.effects.fail(&key, &err.to_string()).await?;
        drop(permit);
        return Err(err);
    }

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
