# Notion (`notion`)

Work in this order, with `totsuka config validate --offline` after each write: outside totsuka, `[notion]`, the project, its repositories, its workflows. Source-independent workflow rules (profiles, loops, `timeout_secs`, `initial_prompt`) are in [workflows.md](workflows.md).

## Outside totsuka

- An internal integration (notion.so/my-integrations), whose secret the user enters in the app at Start.
- The database shared with that integration (the database's ••• menu → Connections). Without the share, Notion answers 404.

## `[notion]`

`{"token": "secret:notion.token", "property_map": {...}, "notion_user_id": "..."}`. The skill has no Notion token, so the property and option names come from the user: ask them to open the database and read out the property names, and the options of the status property, exactly as written (emoji included).

- `property_map` (applies to every database in the file):
  - `title`: the title property, when it is not `Name`.
  - `status`: the status property. Needed for a `status` trigger, for any `on_*` write-back, and for `triage_status`.
  - `status_kind`: `"select"` when that property is a Select, not Notion's Status type.
  - `assignee`: a People property. **Without it every page looks unassigned, and the whole database is picked up.** Map it whenever the database divides work by person.
  - `repo_hint`: a property naming the repository, when the database spans several repositories.
- `notion_user_id`: the user's Notion user ID. Without it, `@me` matches nobody.
- `in_progress_statuses`: options no workflow should ever pick a page from.

## Project

`{"name", "source": "notion", "database_id"}`. The ID is the 32-hex-character part of the database's URL. `triage_status` works as for GitHub (an option name, needs `property_map.status`).

## Repositories

At least one linked, as for GitHub. A repository already linked to a GitHub board cannot serve here; add a separate repository entry. With several repositories, set `property_map.repo_hint` or the top-level `[llm]`, or tasks wait for someone to pick the repository.

## Workflows

### Each page runs once

A page runs at most once, ever, across all workflows: totsuka remembers the page, not the column or the workflow. So:

- **Stages do not work on Notion.** With a `design` workflow on one option and an `implement` workflow on another, a page that went through design never starts implement. Give each kind of work its own pages (one database or one option per kind), and tell the user this before they build a GitHub-style board flow.
- Moving a page back to the trigger column does nothing. Retrying is done on the totsuka side (`totsuka task retry`), not in Notion.

### Questions, in order

1. Implement or design?
2. Which status option starts it, and which options on start, success and failure?
3. Whose pages: the user's and unassigned (default, needs `property_map.assignee` and `notion_user_id`), or anyone's (`"@any"`, works without an assignee mapping)?
4. Any other condition (a tag, a sprint)? That goes into `filter`.

### Recipe

```json
{"name": "notion-implement", "projects": ["tasks-db"], "agent": "herdr", "profile": "implement",
 "trigger": {"status": "Todo", "assignee": "@me"},
 "on_start": {"status": "In progress"}, "on_success": {"status": "Review"},
 "on_failure": {"status": "Blocked"}}
```

### Trigger keys

- `status` (one option), `assignee` (`@me`, `@none`, `@any`, user IDs; default `["@me", "@none"]`), `exclude` (`status` / `assignee`), `filter`.
- `filter` is a raw Notion API filter, sent as-is: `{"and": [{"property": "Tags", "multi_select": {"contains": "AI"}}]}`. It is the place for "does not contain" and nested `and` / `or`; it cannot go inside `exclude`.
- A value that changes over time, such as the current sprint's page, is written `@<name>` inside `filter` and defined under `[notion.dynamic.<name>]` (`database_id` and a `filter` matching exactly one page). Offer it only when the user asks for "the current sprint" or similar; a rollup column in Notion is the cheaper fix when they can change the database.

### Write-backs

`status` only. `labels` is rejected for Notion.
