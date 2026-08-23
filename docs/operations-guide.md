> 🌐 **English** · [日本語](operations-guide.ja.md)

<!-- generated-from: ai-docs/operations/operations-guide.md sha256:87d22a3038a829368b14497b55eb88102cabc64e817d4b710eaa4bcb381ddbe2 -->

# Operations guide

Day-to-day operation: reading `doctor`, cleaning up worktrees and panes, stopping and recovering, working with tasks, and triaging common problems.

## Reading doctor

`totsuka doctor` diagnoses your environment. Add `--json` for machine-readable output. Every failing check prints a cause and a next action.

| Check | What ok means | If it fails |
|---|---|---|
| `git` | git is on `PATH` | Install git |
| `config` | `config.toml` passes validation | Run `totsuka config validate` to see every error |
| `state-db` | The state database opens. Shows the schema version and which totsuka version applied it | If it does not exist yet, run `totsuka run` once. If the database is too new (you downgraded), update totsuka to at least the version named in the message. If it is too old (you just upgraded), run `totsuka run` once — only `run` applies schema changes; `status`, `task`, `focus`, and `doctor` do not |
| `worktree-location` | The configured worktree location expands | Export the missing `${ENV}` variable, or drop the key to fall back to the default |
| `plugin:{name}` | The plugin starts and answers a config validation request | Check that it is installed, and fix `plugins/{name}.toml` |
| `llm` | `api_key_ref` resolves (it does not check whether the key works) | Create a 1Password item, export the environment variable, or add it to your keychain |
| `llm-online` | The provider accepted the API key (only with `--online`) | On 401 or 403, reissue the key with your provider and update `[llm].api_key_ref`. Unreachable hosts and 5xx responses stay warnings |
| `worktrees` | No orphaned worktrees | Offers to clean them up interactively |
| `panes` | No orphaned panes | Offers to release them interactively |
| `trackers` | Each repository files into exactly one tracker | Remove the repository from all but one plugin's `repos`. **No plugin can see this on its own** — each one validates only its own list, so the conflict exists only in the union. Absent when no source claims anything |

A failing `worktree-location` is the nastiest of these: worktrees are created **at the moment a task is dispatched**, so `run` starts up perfectly normally and then every task fails.

Attach the `--json` output when reporting a problem.

### `--online` — check the key actually works

The `llm` check only confirms that the **reference resolves**. It does not confirm that the provider accepts the key. These are independent, and you can end up with a reference that resolves perfectly while the provider rejects every request with a 401.

```bash
totsuka doctor --online
```

sends one minimal request to `[llm]` (no retries, response body discarded) and reports it as the `llm-online` check. It is off by default, because turning it on costs two things:

- It goes to the network, which costs a small amount of money. This is the only check in `doctor` that touches the network
- It genuinely resolves your secret references, so 1Password may prompt for biometric approval

For those reasons, **do not use it from CI or cron**.

The biometric prompt is not unique to `--online`. If any plugin is enabled, the `plugin:{name}` check has to resolve that plugin's secrets in order to start it, so a plain `doctor` can prompt too.

**What happens when a key expires.** When more than one repository is a candidate, totsuka uses the LLM to decide which repository a request belongs to. With an invalid key it cannot decide, so it falls back to asking you to pick, every time. Falling back is the safe behaviour by design — which is exactly what makes this hard to spot, because **a misconfiguration looks like slightly inconvenient normal operation**. If this is happening, `run` logs:

```text
WARN the LLM provider rejected the API key; repository selection falls back to
     the operator picker for every new conversation until it is fixed
```

Only new conversations are affected; later messages in an existing conversation do not redo the decision.

### `--no-repair` — inspect without changing anything

**`doctor` is not read-only by default.** While inspecting, it writes the same setup that `run` does.

| Destination | What |
|---|---|
| `$XDG_DATA_HOME/totsuka/hooks` | Hook scripts and per-workflow settings |
| `$CODEX_HOME/hooks.json` | totsuka's managed entry |
| The opencode config directory | Plugin and plan agent assets |
| `$XDG_STATE_HOME/totsuka/hooks/spool` | Directory creation and a write probe |

That is deliberate: it lets setup finish without a full `run`. But it leaves no way to audit purely — inspecting someone else's machine, or running read-only in CI, would write to `$CODEX_HOME`.

```bash
totsuka doctor --no-repair
```

suppresses those four writes.

- **Every check still runs.** You get the state as found, not the state after repair
- **No cleanup offers** for orphaned worktrees or panes. A read-only audit has no business proposing deletions
- **The trade-off**: it cannot verify that the spool directory is writable, so a missing directory becomes a warning rather than a failure
- **The set of checks and the exit codes do not change.** It only stops writes; it does not check less

## Cleaning up worktrees

Each task gets its own worktree, and a cleanup policy decides what happens afterwards.

- Set `[worktree].cleanup` (default `manual` for implement) and `plan_cleanup` (default `immediate` for plan) to `immediate`, `manual`, or `{ retention_days = N }`
- **A worktree with uncommitted changes is never deleted automatically.** This is the safety valve against losing work
- `retention_days` deletes N days after completion, re-evaluated on each `run` cycle
- **Orphaned worktrees** — ones that belong to no task — are detected by `doctor`, which offers to run `git worktree remove` interactively. Ones with uncommitted changes are skipped

To remove one by hand, use `git worktree remove <path>`; be careful with `--force` if there are unpushed commits. **Removing by hand does not release the associated pane**, so pick the leftover up with the next `doctor` run.

### Branch cleanup

When a worktree is deleted, its `agent/*` branch goes with it. The test is a single question: **does the branch have commits that are not on origin?**

- Every commit is reachable from origin → delete the branch, since nothing is lost
- Even one commit is not → **keep the branch**, because unpushed work exists only there. `run` logs `branch kept: it has commits that are not on origin`

A squash-merged branch has commit hashes that no longer exist on origin. Once `origin/{branch}` is pruned, those commits count as unpushed and the branch is kept from then on. The failure direction is "keep", so nothing is lost; clean the accumulation up by hand with the same test:

```bash
# List the ones that are safe to delete (zero commits missing from origin)
for b in $(git branch --format='%(refname:short)' --list 'agent/*'); do
  [ "$(git rev-list --count "$b" --not --remotes=origin)" = 0 ] && echo "$b"
done

# Delete them once you have looked at the list
for b in $(git branch --format='%(refname:short)' --list 'agent/*'); do
  [ "$(git rev-list --count "$b" --not --remotes=origin)" = 0 ] && git branch -D "$b"
done
```

## Cleaning up orphaned panes

When the link between a worktree and its pane breaks — you removed a worktree by hand, a release was refused, something crashed — the pane is left behind. `doctor` is where you pick those up.

- Agent plugins that can drive panes are asked to list their own panes, and the result is compared against the state database. A pane is a candidate if its task is not in the database, or the task finished and its worktree is already gone. Panes for running tasks, or for tasks whose worktree is deliberately retained, are never candidates
- On a terminal, each one is confirmed individually before release. With `--json` or without a terminal, they are only reported
- **Nothing is released automatically without a human.** Same policy as orphaned worktrees
- If the agent is down and cannot list its panes, this becomes a warning and the other checks continue

## Stopping and recovering

- `run --watch` stops gracefully on Ctrl-C. Running tasks stay in the state database and the lock is released
- After an abnormal exit, restarting restores sessions from the state database and tries to reattach. Tasks that cannot be reattached are **not failed automatically** — they wait for you to choose `totsuka task retry <id>` or `totsuka task cancel <id>`
- A lock file plus a PID check prevents running `run` twice. While `run` is stopped, `totsuka status` says explicitly that its information is stale

## Working with tasks

| Command | What it does |
|---|---|
| `totsuka status [--json]` | Running and waiting tasks plus worktrees. If a task is not starting and there is a known reason, it shows that too |
| `totsuka task show <id>` | State, session history, worktree, and the full event history |
| `totsuka task cancel <id>` | Cancel a task |
| `totsuka task retry <id>` | Restart a failed or cancelled task, reusing its worktree and session |
| `totsuka logs [-f] [--task <id>]` | Formatted logs. Secrets are masked unconditionally |

`retry` only accepts failed or cancelled tasks — a completed task cannot be re-run.

## Common problems

| Symptom | What to do |
|---|---|
| `config not found` | Run `totsuka init` to generate a template, then edit it |
| `state database not found` | Run `totsuka run` once and it is created |
| A plugin is `enabled but not installed` | `totsuka plugin install <dir>` |
| Tasks are not picked up | Use `totsuka run --dry-run` to check trigger matching, repository selection, and agent assignment with no side effects. A workflow's `source` must match the plugin instance name |
| Repository selection stays `pending` | `[llm]` is unset, or the decision was low-confidence. With a single repository it is chosen automatically; with several, configure `[llm]` or add a `repo_hint` to the request |
| `task show` shows no branch | The agent did not create one — worktrees are handed over on a detached HEAD. If there are commits, the worktree is kept, so you can pick the work up there. In plan mode this is always the normal state |
| No notifications arrive | Check that the notifier plugin is enabled and reachable with `doctor`. A failed delivery does not stop the task |

---

This page is generated from the internal document `ai-docs/operations/operations-guide.md`, which carries the design decisions and measurements behind it.
