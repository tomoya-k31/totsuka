# GitHub Projects (`github`)

Work in this order, with `totsuka config validate --offline` after each write: outside totsuka, `[github]`, the project, its repositories, its workflows. Source-independent workflow rules (profiles, loops, `timeout_secs`, `initial_prompt`) are in [workflows.md](workflows.md).

## Outside totsuka

- A GitHub Projects (v2) board with a single-select status field (named `Status` unless the user says otherwise).
- `gh auth login` for the account totsuka acts as. The app's **Use gh auth token** reads that account's token at every Start, and the token needs the `project` scope, which `gh auth login` does not grant by default. With the account active (`gh auth switch --hostname github.com --user <login>`), run `gh auth refresh --hostname github.com -s project`. A fine-grained PAT cannot reach a user-owned board.

## `[github]`

`{"token": "secret:github.token", "github_login": "<login>"}`. The login is also the account `gh auth token` is asked for, so it must be the user's real login. Add `"status_field": "<name>"` when the board's status field is not called `Status` (it applies to every board).

`in_progress_statuses` (every board) lists columns that no workflow should ever pick a card from, for example a column a person works in. Offer it when several people share the board.

## Project

`{"name", "source": "github", "owner", "project_number"}`, plus `"owner_type": "organization"` for an organization's board. `gh project list --owner <owner>` lists the boards with their numbers; the number is also the last segment of the board's URL.

`triage_status` is the column an item filed by a `triage` workflow (from Slack, say) lands in. Unset, it lands with no status and waits for a person. Set it to a column a workflow triggers on only when the user wants filed items to start work unattended.

## Repositories

Every GitHub project needs at least one repository whose `"project"` names it, or totsuka refuses to start. Only issues and PRs from linked repositories are picked up.

## Workflows

### Read the board first

Take column names from the board, never from the user's typing; a name that is not on the board matches nothing and nothing reports it until the app's Start:

```bash
gh project field-list <number> --owner <owner> --format json \
  | jq -r '.fields[] | select(.name == "Status") | .options[].name'   # or the status_field name
```

Show the columns to the user and build every workflow from that list.

### Questions, in order

1. What should happen to a card: implement it, design it first, or both in stages? This picks the recipe below.
2. Which column starts it? Which columns should the card move to on start, on success and on failure?
3. Whose cards: the user's and unassigned (the default), only the user's (`"assignee": "@me"`), or anyone's (`"@any"`)?
4. Any labels to require (`"label"`) or to skip (`"exclude": {"label": ...}`)?

### Recipes

**Implement on a column.** The common start.

```json
{"name": "implement", "projects": ["board"], "agent": "herdr", "profile": "implement",
 "trigger": {"status": "Todo"},
 "on_start": {"status": "In Progress"}, "on_success": {"status": "In Review"},
 "on_failure": {"status": "Blocked"}}
```

**Design, then implement (a pipeline).** When a finished card enters another workflow's trigger column, that workflow continues the same conversation in the same worktree, so the implement agent already has the design in context.

```json
{"name": "design", "projects": ["board"], "agent": "herdr", "profile": "design",
 "trigger": {"status": "Design"}, "on_start": {"status": "Designing"},
 "on_success": {"status": "Design Review"}, "on_failure": {"status": "Blocked"}}
{"name": "implement", "projects": ["board"], "agent": "herdr", "profile": "implement",
 "trigger": {"status": "Ready"}, "on_start": {"status": "In Progress"},
 "on_success": {"status": "In Review"}, "on_failure": {"status": "Blocked"}}
```

A person moves the card from `Design Review` to `Ready`. Writing `on_success` straight into `Ready` would skip that review; offer it only if the user wants no review between the stages. Only a finished conversation is handed over; a card moved while the first stage still runs waits for the next poll.

**Hand to a person and never pick up again.** Labels are written to the issue or PR, not the card:

```json
"trigger": {"status": "Todo", "exclude": {"label": "ai:needs-human"}},
"on_start": {"labels": ["+ai:running"]},
"on_success": {"status": "In Review", "labels": ["+ai:needs-human", "-ai:running"]},
"on_failure": {"labels": ["-ai:running"]}
```

### Trigger keys

- `status` (one column), `label` (any of them, case-insensitive), `assignee` (logins, `@me`, `@none`, `@any`; default `["@me", "@none"]`), `exclude` (any of `status` / `label` / `assignee`). Keys combine with AND.
- A trigger without `status` runs each issue once, ever; startup warns about it.
- `exclude` is checked when a card is picked up. Adding the label later does not stop a running task.

### Write-backs

`status` and `labels` (`"+name"` / `"-name"`), both allowed in `on_start`, `on_success` and `on_failure`. Online validation checks that every column and label exists; the labels are looked up in each linked repository.

### Pull requests on the board

`design` and `implement` workflows also pick up open, non-fork PRs in their trigger column. The agent then works on the PR's own branch and reports with a comment on it. Mention this when the board holds PRs (bot bumps, a colleague's PR), so the user is not surprised. For a PR that totsuka opened from an issue, the user moves the issue's card back instead.

### Re-runs

Moving a card back into a trigger column runs that workflow again, including after it finished. This is how the user retries a card that went to `Blocked`.
