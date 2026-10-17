---
applyTo: "crates/orchestrator-core/src/config/**,crates/orchestrator-core/src/domain/workflow.rs,crates/orchestrator-core/src/plugins/**,crates/orchestrator-core/src/prompts/**,crates/orchestrator-cli/src/**,crates/orchestrator-cli/templates/config.toml,crates/plugin-protocol/src/methods.rs,crates/plugin-sdk/src/config_schema.rs,plugins/**,apps/macos/**"
---

# Keep the `totsuka-config` skill in step

Write every comment in Japanese. `skills/totsuka-config/` (ADR-0116) walks a
user through `config.toml`. Users install it with `npx skills add`, so a
wrong fact in it reaches them directly. Key names come from
`totsuka config schema` at run time, so **a new key alone is not a finding**.
What the skill hard-codes is behaviour that the schema does not show. Flag the
PR when it changes one of the facts below and does not update the file named
next to it:

| Fact the PR changes | Skill file |
|---|---|
| A profile added, removed, or what it produces / when it is done / whether it replies | `references/workflows.md` "Profiles" |
| Which profile a source supports, or the `source` output capability | the support table in `references/workflows.md` and the source's file |
| A trigger key, trigger rule, or `on_*` write-back of a source | the source's "Trigger keys" / "Write-backs" section |
| What `config set` / `get` / `validate` / `schema` accept, refuse, or print (exit codes, `error:` / `note:` lines, JSON Pointer `/-`) | `SKILL.md` "The ground rules" |
| Secret handling: the `secret:` scheme, `--secrets-stdin`, which stores the app refuses, or a new `x-secret` key whose name the read filter regex `(^\|_)token$\|^api_key(_ref)?$` misses | `SKILL.md` "The ground rules" and step 1.4 |
| A cross-entry rule that only fails at Start (project with no repository, Slack with two repositories and no `[llm]`, …) | `SKILL.md` steps 4 / 3.5 and the source's file |
| The minimum `totsuka` version the skill relies on, or the app's install location | `SKILL.md` step 1 |
| A new top-level table (`[llm]`, `[tools.*]`, …) or how notifiers are configured | `SKILL.md` steps 3.5–3.6, "Leave notifier plugins out" |

A new plugin missing from the skill, and a removed config key still named in
it, are already caught before the PR is opened (`scripts/docs-gap.sh`); do not
repeat them.

Look in `skills/totsuka-config/` for text that names the changed key, profile,
flag, or message. Text the PR did not update and that now states the old
behaviour is a finding; quote it with `path:line`. A changed behaviour with no hit is a
finding only if a user following the skill would now hit an error or a silent
no-op.

The skill is English only (ADR-0116) and has no `.ja.md` siblings; do not ask
for one.
