# Routines (`routine`)

A routine runs a prompt in one repository on a cron schedule, like a Claude routine. Work in this order, with `totsuka config validate --offline` after each config write: `[routine]`, the project, the workflow, then the routine files. Source-independent workflow rules are in [workflows.md](workflows.md).

## `[routine]`

Nothing is required: `{}`. Offer `catchup_window` only if the user asks how missed runs are handled: a run missed while the machine was asleep or off is made up **once**, when it comes back, if it is younger than `catchup_window` (default `"24h"`; `"0"` = only on time).

## Project

`{"name": "routines", "source": "routine"}`, one entry. No repository links to it: each routine names its own repository.

## Workflow

Routines run unattended, so do not use a profile: `design` / `implement` wait for the user's approval in the pane when done, and `answer` / `triage` default to output `source`, which this source rejects (it posts nothing back). Write `mode` and `output` instead:

```json
{"name": "routine", "projects": ["routines"], "trigger": {}, "agent": "herdr",
 "mode": "implement", "output": "none"}
```

- `trigger` stays empty: any key in it fails at Start.
- One workflow is enough. With two or more, every routine file must name its `workflow`.
- Leave out `on_start` / `on_success` / `on_failure` (accepted and ignored).

## Routine files

Routines are files, not config: write each one directly (no `config set`). The directory is `<config dir>/routines/<host>/`: `<config dir>` is the directory holding `config.toml` (when `config_path` is `hosts/<host>.toml`, it is the parent of `hosts/`), and `<host>` is the machine's host key, the same name `hosts/` uses: `hostname | cut -d. -f1 | tr '[:upper:]' '[:lower:]'`. A machine without its own directory runs nothing.

One file per routine, `<name>.md`; the name identifies it:

```markdown
---
repo: web-app
schedule: "0 9 * * 1-5"
---
Check the dependencies for updates and open a PR if any are needed.
```

Questions: what should run, in which repository (`repo` must be exactly a `[[repositories]]` name), and when. Turn "weekdays at 9" into the five cron fields `minute hour day month weekday`, local time, and read the schedule back in words before writing. Optional keys: `workflow`, `catchup_window` (overrides `[routine]`), `enabled: false` (stops the schedule).

- The frontmatter is `key: value` lines, not YAML. An unknown key, a repeated key, a missing `repo` / `schedule`, or an empty body makes the routine fail (logged once; the others keep running).
- No `MON` / `JAN` names, no `@daily`.
- If a run is still going at the next time, that time is skipped.

After writing a file, run `totsuka routine list`: it reads the files the way the plugin does and shows each routine's next run, or why it cannot run (a `!` line). To try one right away while totsuka is running: `totsuka routine run <name>`.
