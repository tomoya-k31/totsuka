# Development flow — pre-PR checks, post-PR monitoring, merge

How to take a change from local edits through CI, automated review, and merge.
Complements [git-conventions.md](git-conventions.md) (branches/commits),
[pr-conventions.md](pr-conventions.md) (PR title/description/merge strategy), and
the docs obligation in [CLAUDE.md](../../CLAUDE.md). Commands below are concrete
examples — adjust `<n>` (PR number) and paths as needed.

## Before opening a PR

- Run the CI-required checks **locally first**, so a red CI is caught before push:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`

  (Mirror the flags the CI workflow uses — this is a 9-crate workspace, so a
  missing `--workspace` can pass locally yet fail in CI.)
  - rust-analyzer LSP diagnostics clean — fix type errors / missing imports
    (rustc + clippy backed, per [CLAUDE.md](../../CLAUDE.md)).
- Review your own diff before writing the PR: `git diff main...HEAD` and every
  commit with `git log main..HEAD`.
- Confirm no unrelated files or stray working-tree changes are included;
  `git add <explicit files>` only — never `-A` / `.` (→ git-conventions).
- Docs obligation: if a trigger applies (design decision / new component /
  API·schema·infra change / release), update the relevant `docs/` concept plus
  its `index.md` / `log.md` in the **same** PR, then
  `bash scripts/okf-lint.sh docs` to zero errors (→ CLAUDE.md, docs/CLAUDE.md).

## After opening a PR — monitor, assess, iterate

**Do not merge until BOTH the CI run and the Copilot review have been fetched
and assessed.** CI-green alone is not enough — merging on CI-only once let two
Copilot findings slip past review. Step 5 (`/code-review`) does not depend on CI,
so it may start while CI is still running.

**Write all review output in Japanese** — the `/code-review` comments you post
(step 5), the finding summaries you report (step 3), and the rationale you leave
on findings (Handling findings below). (Copilot's own review language is not
controllable.)

### Polling policy (applies to every wait below)

Poll every **30 seconds**, for at most **10 minutes**. If it has not completed
by 10 minutes, **stop polling, report the current state to the user, and wait** —
do not keep waiting and do not merge.

Run the watch in the background (or wrap it with a `timeout`-style cap) so the
10-minute ceiling is enforced, e.g.:

```
gh pr checks <n> --watch --interval 30   # wrap with a 10-min cap; raw --watch runs until done
```

### Steps

1. **Monitor CI** (the GitHub Actions workflows defined for the repo), on the
   30 s / 10 min policy above:

   ```
   gh pr checks <n> --watch --interval 30
   ```

   All required checks must be green.

2. **Monitor the GitHub Copilot auto-review.** It runs in parallel with CI and
   can take a few minutes to appear; poll on the same 30 s / 10 min cap. The
   review author is `copilot-pull-request-reviewer` (inline comment user
   `Copilot`). Fetch both levels:

   ```
   # inline review comments
   gh api repos/{owner}/{repo}/pulls/<n>/comments \
     --jq '.[] | {user: .user.login, path, line, body}'

   # review-level summary / state
   gh pr view <n> --json reviews \
     --jq '.reviews[] | {author: .author.login, state, body}'
   ```

3. **Grasp both results** — summarize the CI outcome and every Copilot finding.

4. **Vet each Copilot finding for correctness first** — decide valid vs
   mistaken. Do not assume the bot is right; verify against the docs, `--help`,
   or the code itself (e.g. `cargo update --help` can disprove a claim about
   what a flag does).

5. **Run `/code-review:code-review --comment` in a subagent** — launch it via
   the Agent tool so it reviews the diff and posts its findings as a comment on
   the PR (a single review-summary comment via `gh pr comment`), e.g.:

   ```
   Agent(subagent_type: "general-purpose",
         prompt: "Run /code-review:code-review --comment on PR <n> and report the findings")
   ```

6. **Vet each `/code-review` finding the same way** (valid vs mistaken).

7. **Merge only on explicit instruction; otherwise wait.**

### Handling findings (steps 4 & 6)

- **Valid finding** → fix it in a **new** follow-up commit (or a follow-up PR if
  the PR is already merged). Never `git commit --amend`, never `--no-verify`
  (→ git-conventions).
- **Mistaken finding** → do **not** silently ignore it: record the rationale as
  a reply on the PR comment and/or in the fixing commit / PR body.
- After pushing any fix, **re-run the loop (steps 1–6)** — new commits
  re-trigger CI and re-invoke Copilot / `code-review`. Iterate until clean.

### If CI is red

Investigate → fix → push → re-monitor. Never merge on red. If `main` itself
broke, follow pr-conventions "If `main` breaks" (revert first, root-cause after).

## Merging (step 7 — only when instructed)

- Default strategy: **Squash and Merge**; delete the branch
  (→ pr-conventions for when a non-default strategy is allowed):

  ```
  gh pr merge <n> --squash --delete-branch
  ```

- Pre-merge: `mergeStateStatus` clean, required checks green, no unresolved
  review threads:

  ```
  gh pr view <n> --json mergeStateStatus,mergeable,reviewDecision
  ```

- Post-merge: verify local `main` fast-forwarded and the working tree is clean
  (`git status`, `git log --oneline -1`).
- Merging `main` is outward-facing and hard to reverse → only with an explicit
  user go-ahead. High-risk git operations (force push, etc.) follow
  git-conventions' confirm-first rules.
