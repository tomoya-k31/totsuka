# Routines (`routine`)

A routine runs a prompt in one repository on a cron schedule, like a Claude routine. Work in this order, with `totsuka config validate --offline` after each write: `[routine]`, the project, the workflow. The routine files come after, from the totsuka-routine skill. Source-independent workflow rules are in [workflows.md](workflows.md).

## `[routine]`

Nothing is required: `{}`. Offer `catchup_window` only if the user asks how missed runs are handled: a run missed while the machine was asleep or off is made up **once**, when it comes back, if it is younger than `catchup_window` (default `"24h"`; `"0"` = only on time).

## Project

`{"name": "routines", "source": "routine"}`, one entry. No repository links to it: each routine names its own repository.

## Workflow

Routines run unattended, so use the `unattended` profile: `design` / `implement` wait for the user's approval in the pane when done, `answer` / `triage` default to output `source`, which this source rejects (it posts nothing back), and a workflow without a profile gets no permission settings, so Claude stops at a permission prompt with nobody to answer it.

```json
{"name": "routine", "projects": ["routines"], "trigger": {}, "agent": "herdr",
 "profile": "unattended"}
```

- `trigger` stays empty: any key in it fails at Start.
- One workflow is enough. With two or more, every routine file must name its `workflow`.
- Leave out `on_start` / `on_success` / `on_failure` (accepted and ignored).

## Routine files

The routines themselves are files, not config, and have their own skill: **totsuka-routine** writes them with the user, checks them with `totsuka routine list`, and can try one with `totsuka routine run`. If it is not installed, offer `npx skills add tomoya-k31/totsuka --skill totsuka-routine` once the workflow validates, and hand over to it. The running app does not reload `config.toml`, so the user needs Stop then Start before the first routine can run.
