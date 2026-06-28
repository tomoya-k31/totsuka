use std::collections::HashMap;

use totsuka_core::Phase;

use crate::adapter_client::SpawnReq;
use crate::argv::merge_argv;
use crate::effect::ClaimOutcome;
use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn spawn_verifier(
    engine: &Engine,
    task: &Task,
    implementer_agent_id: &str,
    pr_diff: &str,
) -> Result<HandleOutcome, OrchestratorError> {
    let snap = engine.adapter.read(implementer_agent_id, 0).await?;
    let id = task.id.clone();
    // Effect key for verifier — string-built so we don't pollute Phase enum;
    // attempt = task.impl_verify_attempt so DiffBack restart bumps it the same way.
    let key = format!("spawn:{}:verify:{}", id.as_str(), task.impl_verify_attempt);
    let outcome = engine
        .effects
        .claim(
            &key,
            &format!(
                "derived:verify:{}:{}",
                id.as_str(),
                task.impl_verify_attempt
            ),
            "spawn",
            &engine.owner_id,
        )
        .await?;
    if let ClaimOutcome::Skipped { reason } = outcome {
        return Ok(HandleOutcome::Skipped { reason });
    }
    let argv = merge_argv(
        &engine.config.orchestrator.claude_argv,
        &task.repo,
        &Phase::ImplVerify,
    );
    let branch = format!("totsuka/{}/verify", id.short());
    let req = SpawnReq {
        task_id: id.as_str().into(),
        phase: "verify".into(),
        attempt: task.impl_verify_attempt,
        repo: task.repo.clone(),
        branch,
        argv,
        env: HashMap::new(),
    };
    let res = match engine.adapter.spawn(req).await {
        Ok(r) => r,
        Err(err) => {
            engine.effects.fail(&key, &err.to_string()).await?;
            return Err(err);
        }
    };

    let input = format!("{}\n\n--- PR DIFF ---\n{}", snap.text, pr_diff);
    if let Err(err) = engine.adapter.send(&res.agent_id, &input).await {
        engine.effects.fail(&key, &err.to_string()).await?;
        return Err(err);
    }

    engine
        .effects
        .complete(
            &key,
            serde_json::json!({
                "agent_id": res.agent_id,
                "terminal_id": res.terminal_id,
                "worktree_path": res.worktree_path,
                "role": "verifier",
            }),
        )
        .await?;
    Ok(HandleOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The full integration of spawn_verifier into the SM lives in Task 17;
    // this stub asserts the helper symbol exists where Task 17 expects it.
    #[test]
    fn function_exists() {
        let _ = spawn_verifier;
    }
}
