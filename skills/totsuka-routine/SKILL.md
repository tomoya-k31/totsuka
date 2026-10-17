---
name: totsuka-routine
description: "Create, change, list, pause or try out totsuka routines: prompts an AI agent runs in a repository on a cron schedule, like Claude routines (\"every weekday at 9, check the dependencies and open a PR\"). Use when someone wants something to run on a schedule through totsuka, asks what routines exist or when one runs next, wants to edit, disable or delete one, or wants to run one now."
---

# totsuka routines

A routine is one Markdown file: a few `key: value` lines (which repository, when) and a prompt the agent runs there. totsuka's routine plugin reads `<config dir>/routines/<host>/*.md` every 30 seconds and starts a task when one is due, in a fresh worktree like any other workflow. Talk in the user's language.

What the user should know, said once when it matters:

- It runs only while totsuka runs (the menu bar app, or `totsuka run --watch`). A run missed while the machine slept or was off is made up **once** when it comes back, if it is no older than the catch-up window (default 24 hours).
- If the previous run of the same routine is still going (or waiting for an answer), that time is skipped, not queued.
- Routines run unattended: nobody approves the result in the agent's pane. The prompt has to say what the deliverable is (a pull request, a comment, a file) and the agent produces it itself; totsuka posts nothing back.

## 1. Preflight

1. `totsuka routine --help` must work. If not, totsuka is too old or missing: point to `brew upgrade totsuka` / `brew install tomoya-k31/tap/totsuka` and stop.
2. `totsuka config get | jq -r .config_path` is the config file. The **config dir** is the directory holding it; when that path is `<dir>/hosts/<host>.toml`, the config dir is `<dir>`, not `hosts/`.
3. `totsuka routine list` prints `routines in <directory>` — that directory is where this machine's routine files go. If it fails with "no routine plugin is enabled", do **2. One-time setup** first.

## 2. One-time setup

Only when `routine list` says no routine plugin is enabled. Each step ends with `totsuka config validate --offline` (exit 0 ends with `configuration is valid`). Write with `totsuka config set <JSON Pointer> '<json>'`, one whole object per call.

1. `totsuka plugin install --bundled routine --yes`, then `totsuka config set /plugins/routine '{"kind":"task_source","enabled":true}'`.
2. `totsuka config set /routine '{}'`. Add `{"catchup_window": "6h"}` only if the user wants a different catch-up window (`"0"` = only on time; also `90m`, `7d`).
3. `totsuka config set /projects/- '{"name":"routines","source":"routine"}'`.
4. The workflow. Ask which agent IDE runs it — `totsuka plugin list` shows the installed `agent_ide` plugins (herdr or orca). Use the `unattended` profile, made for this: no permission prompts, no approval at the end, nothing posted back. The other profiles stop an unattended run (`design` / `implement` wait for a human to approve in the pane; `answer` / `triage` post back to the source, which this source cannot do), and so does a workflow with no profile (no permission settings, so Claude waits at a permission prompt). Write:

   ```bash
   totsuka config set /workflows/- '{"name":"routine","projects":["routines"],"trigger":{},"agent":"<agent>","profile":"unattended"}'
   ```

   `trigger` stays empty: any key in it fails at start. If the workflow already exists without `"profile": "unattended"`, change it to that (remove any `mode` / `verification` with `totsuka config unset`).
5. The repositories a routine may name are the `[[repositories]]` entries: `totsuka config get | jq -r '.config.repositories[].name'`. If the one the user wants is missing, add `{"name","path"}` with `set /repositories/-` (check the path with `git -C <path> rev-parse --show-toplevel`).
6. The running app does not reload `config.toml`: the user clicks **Stop**, then **Start** in the menu bar app (or restarts `totsuka run --watch`). Then `totsuka routine list` must print the routines directory.

## 3. Create a routine

Ask, one at a time: what should happen (the prompt), in which repository, when, and what it should leave behind. Then:

1. **Name**: short, lowercase, `-` between words (`dep-update`). It is the file name and how the user refers to it later; tasks are named after it.
2. **Schedule**: turn "weekdays at 9" into five cron fields `minute hour day month weekday`, local time — `0 9 * * 1-5`. Each field is `*`, a number, `a-b`, `*/n` or a comma list; weekday `0`–`7` (0 and 7 are Sunday). No `MON` / `JAN` names, no `@daily`. When both day-of-month and weekday are restricted, a day matching **either** runs. Read the schedule back in words before writing.
3. **Prompt**: what the agent does and what it leaves behind, written so it works with nobody watching. The agent's skills can be named in it.
4. Write `<routines directory>/<name>.md` (create the directory if needed; it is the one `routine list` printed):

   ```markdown
   ---
   repo: web-app
   schedule: "0 9 * * 1-5"
   ---
   Check the dependencies for updates. If any moved, update them, run the tests,
   and open a pull request with the changes.
   ```

   Optional lines: `workflow: <name>` (needed only when more than one workflow uses the routine source), `catchup_window: 6h` (this routine only), `enabled: false` (stops the schedule; `routine run` still works).
5. `totsuka routine list`: the new routine shows its next run. A new (or just edited, or re-enabled) routine waits for its next time: times that passed before the file was written are not made up. A line starting with `!` under it says why it cannot run — fix the file and list again. No restart is needed for routine files.
6. Offer a trial run: `totsuka routine run <name>` starts it now and prints the task number (`totsuka task show <n>` follows it). It needs totsuka running, and it does not use up a scheduled run.

The frontmatter is `key: value` lines, not YAML: values may be quoted, `#` after a space starts a comment, and an unknown or repeated key, a missing `repo` / `schedule`, or an empty prompt makes the routine fail (`routine list` shows why).

## 4. Change, pause or remove

- **List**: `totsuka routine list` (`--json` for details) shows each routine's schedule, repository, next run and last run with its state.
- **Change**: edit the file. Changes apply within 30 seconds; renaming the file makes it a new routine. Editing right after a missed run means that run is not made up.
- **Pause**: add `enabled: false` to the frontmatter (remove it to resume).
- **Remove**: delete the file, after confirming with the user.
- **A run that is stuck** blocks the next ones: `totsuka task cancel <n>` (the task number from `routine list`'s last run).

## Routines on several machines

Each machine reads only its own `routines/<host>/` directory (`<host>` is the hostname up to the first `.`, lowercased — the same name `hosts/<host>.toml` uses). A routine meant for another machine goes in that machine's directory; one copied to two directories runs on both.
