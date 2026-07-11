# Git conventions

## Branch naming

- Format: `<type>/<slug>`, all lowercase, `-` as separator (not `_`).
- `type` ∈ `feat|fix|docs|style|refactor|perf|test|chore|revert` (same taxonomy as commit types).
- Before creating/switching branches: run `git status` (no uncommitted changes) and confirm the target branch name with the user.
- To catch up with `main`: `git rebase main` (never `git merge main` — keeps history linear), then push with `--force-with-lease`. Only on your own feature branch that no one else has pushed to.

## Commits

- Conventional Commits 1.0.0: `type(optional scope): <description>`. Description in Japanese or English; body is optional (motivation/background).
- One commit per completed, revertable unit of work. Related changes across multiple files may be combined into one commit; for multi-step migrations, commit at the completion of each step.
- Do not commit while: there are build errors, the implementation is incomplete, or tests are failing.
- `git add` explicit files only — never `-A` / `.`. Never commit `.env` files or credentials.
- Never `git commit --amend` — create a new commit instead. Never skip hooks with `--no-verify`.

## High-risk operations — confirm with the user first, every time

- `git push --force` / `-f` (`--force-with-lease` on your own feature branch is fine).
- Force-pushing `main`/`master` — warn even if permission is given.
