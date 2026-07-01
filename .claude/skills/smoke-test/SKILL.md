---
name: smoke-test
description: Boot the totsuka stack end-to-end (up → status → down) on real hardware and verify clean shutdown. Use when asked to "run a smoke test", "resume the smoke test", or to manually verify a supervisor/lifecycle change actually works outside of `cargo test`.
---

# Smoke Test

Boots the real `totsukactl`-managed stack, confirms all children reach Healthy, then shuts it down and verifies the shutdown was actually clean (not just that the command exited).

## Preconditions

- No supervisor already running: `ps aux | grep totsukactl | grep -v grep` should be empty. If one is running, decide with the user whether to reuse it or tear it down first — don't kill an unknown supervisor without asking.
- `docker ps --format '{{.Names}}\t{{.Status}}'` shows `totsuka-pgmq` healthy. If not, `just pgmq-up` (or ask before starting new infrastructure).
- `~/.config/totsuka/config.toml` and `secrets.toml` exist (from a prior `totsukactl init`).

## Steps

1. **Rebuild from current source** (always — a stale binary silently tests old code):

   ```bash
   cargo build --release --bin totsukactl
   ```

2. **Boot in the background**, detached from this shell (a plain background `&` gets SIGHUP'd when a Bash tool call's shell exits — this does not):

   ```bash
   nohup ./target/release/totsukactl up </dev/null >~/.local/state/totsuka/logs/supervisor.stdout 2>&1 & disown
   sleep 12
   cat ~/.local/state/totsuka/logs/supervisor.stdout
   ```

   Expect 4 `child state transition ... next=Healthy` lines (agent-adapter, orchestrator, github-watcher, qa-service).

3. **Confirm via status**:

   ```bash
   ./target/release/totsukactl status
   ```

   Expect `pgmq running` + all 4 children `healthy`.

4. **Shut down and check the exit code**:

   ```bash
   time ./target/release/totsukactl down; echo "exit: $?"
   ```

   A non-zero exit or an error message ("stack not running", "did not exit in Ns") is the actual bug signal this smoke test exists to catch — don't treat it as an environment flake without investigating.

5. **Verify the shutdown was actually clean** — a zero exit code alone isn't enough:

   ```bash
   ps aux | grep -E "agent-adapter|orchestrator|github-watcher|qa-service" | grep -v grep
   ls -la ~/.local/state/totsuka/supervisor.pid 2>&1   # expect "No such file"
   ls -la ~/.local/state/totsuka/sock/ 2>&1             # expect empty
   ```

6. **Review the shutdown sequence** for the reverse-order stages and any unexpected escalation:

   ```bash
   grep -E "SIGTERM|SIGKILL|shutdown" ~/.local/state/totsuka/logs/supervisor.stdout | tail -20
   ```

   Expect: `github-watcher`+`qa-service` SIGTERM together (ingestion tier), then `orchestrator`, then `agent-adapter` — each roughly `shutdown_grace_secs` apart, with a `SIGTERM (2nd)` line for any child that needed escalation (not itself a failure — it's the designed backstop).

## Reporting

State plainly whether it passed (all 4 healthy → clean `down` → verified empty state) and cite the exact numbers (uptime, shutdown wall-clock, whether escalation fired). If anything failed, don't guess at a root cause from the summary alone — read `~/.local/state/totsuka/logs/supervisor.stdout` and the per-child logs under `~/.local/state/totsuka/logs/` before proposing a fix.
