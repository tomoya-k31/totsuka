# Development flow — pre-PR checks, post-PR monitoring, merge

How to take a change from local edits through CI, automated review, and merge.
Complements [git-conventions.md](git-conventions.md) (branches/commits),
[pr-conventions.md](pr-conventions.md) (PR title/description/merge strategy), and
the docs obligation in [CLAUDE.md](../../CLAUDE.md). Commands below are concrete
examples — adjust `<n>` (PR number) and paths as needed.

## Before opening a PR

**Scope the checks to what the diff can affect.** Get the changed paths first
with `git diff --name-only main...HEAD`, then run only the groups whose trigger
matches. Skipping a group is safe when the diff cannot affect it — e.g. a
docs-only change cannot fail `cargo clippy`, so the Rust set is pointless there.

| Changed paths (`git diff --name-only main...HEAD`) | Run |
|---|---|
| `**/*.rs`, `**/Cargo.toml`, `Cargo.lock`, `deny.toml`, `rustfmt.toml`, `clippy.toml` | **Rust set** (below) |
| a dependency change in `Cargo.toml` / `Cargo.lock` | Rust set **plus** `cargo audit` and `cargo deny check`. If the tools are missing, install them once (`cargo install cargo-audit cargo-deny`); if that is not possible, do NOT silently skip — state it in the PR body and treat the `cargo-audit / cargo-deny` check (`audit.yml`, which fires on these paths) as the gate in post-PR monitoring |
| `docs/**` | **Docs checks** (below) + the docs obligation |
| a prose `*.md` outside the OKF/vendored exclusions and outside `.claude/**` | update its `.ja.md` sibling (→ [documentation-i18n.md](documentation-i18n.md)) |
| `.github/workflows/**` | read the SHA-pin + `ubuntu-slim` rules, validate YAML (`yq . <file>`); if you changed `ci.yml`'s commands, also run the affected Rust set |
| `.claude/**` (settings / hooks / rules) | validate JSON (`python3 -m json.tool .claude/settings.json`); no Rust, no `.ja.md` |
| none of the above touch Rust/Cargo (docs-only, `.claude`-only, …) | **skip the Rust set entirely** |

Note: on every PR, CI runs `clippy / rustfmt` + `test` + `machete (unused
deps)` (`ci.yml`, no path filter) and the `lint` check (`okf-lint.yml`)
regardless of what changed. If `machete` fails, remove the unused dependency
or suppress a false positive per
[dependency-hygiene](../../docs/development/dependency-hygiene.md).
`audit` (`audit.yml`) additionally runs on PRs touching `**/Cargo.toml` /
`Cargo.lock` / `deny.toml` (plus a daily cron); `coverage` runs only on push
to `main`. Scoping only changes what you run **locally** before pushing —
post-PR you still monitor all checks that report on your PR.

**Rust set** — when Rust/Cargo files changed (mirror CI's flags; this is an
11-member workspace, so a missing `--workspace` can pass locally yet fail CI):

- **Toolchain parity first**: CI installs the **latest stable** (the
  SHA-pinned `dtolnay/rust-toolchain` action with `toolchain: stable`), so
  before trusting any local result run
  `rustup check` and, if an update is available, `rustup update stable`.
  Clippy's lint set grows between releases — an outdated local stable passed
  clean while CI failed on `clippy::type_complexity` (PR #197).
- `cargo fmt --all --check`
- `bash scripts/arch-lint.sh` — workspace dependency-boundary fitness function
  (plugins → protocol/sdk only, protocol is a leaf, no cycles). Cheap
  (`cargo metadata --no-deps`, seconds); especially relevant when a
  `Cargo.toml` changed. CI runs it as a step inside the `clippy / rustfmt` job.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `RUSTFLAGS="-D warnings" cargo test --workspace --all-features` — CI
  (`ci.yml`) exports `RUSTFLAGS: -D warnings` job-wide, so a plain
  `cargo test` can pass locally on a warning that fails CI's build/test.
- rust-analyzer LSP diagnostics clean — fix type errors / missing imports
  (rustc + clippy backed, per [CLAUDE.md](../../CLAUDE.md)).

**Docs checks** — when `docs/**` changed:

- `bash scripts/okf-lint.sh docs` to zero errors.
- Docs obligation: if a trigger applies (design decision / new component /
  API·schema·infra change / release), update the relevant `docs/` concept plus
  its `index.md` / `log.md` in the **same** PR (→ CLAUDE.md, docs/CLAUDE.md).

**Always** (any PR, regardless of what changed):

- Review your own diff before writing the PR: `git diff main...HEAD` and every
  commit with `git log main..HEAD`.
- No unrelated files or stray working-tree changes are included;
  `git add <explicit files>` only — never `-A` / `.` (→ git-conventions).

**Pre-PR review** (optional, best-effort — must NEVER block the PR):

- Run it **inline in the main session** via the `/code-review` skill at
  **low or medium** effort. Do NOT wrap it in a subagent that itself spawns
  finder/fan-out agents — the middle agent parks waiting on its children and
  the chain stalls in intermediate states ("waiting for finders", "running
  clippy"), which has repeatedly frozen progress.
- **Hard cap: 10 minutes** (same ceiling as the post-PR polling policy). If it
  has not produced a final report by then, **stop it, note that it was cut
  short, and proceed to opening the PR**. The post-PR gates — CI, the Copilot
  review, and `/code-review --comment` (step 5) — are the safety net; a stalled
  pre-PR review must not be the only thing blocking an otherwise-finished
  change.

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
   `Copilot`). **This repo triggers the Copilot review only on PR creation** —
   later pushes do NOT re-trigger it, so only poll for it once, right after
   opening the PR (see Handling findings for what to do after fix commits).
   Fetch both levels:

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

5. **Run `/code-review:code-review --comment` in a subagent** — once per PR
   (it is expensive; see the re-run policy under Handling findings). Launch it
   via the Agent tool so it reviews the diff and posts its findings as a comment
   on the PR (a single review-summary comment via `gh pr comment`), e.g.:

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
- After pushing any fix commit, **always re-monitor CI for it** (steps 1 & 3):
  a fix commit re-triggers the full CI run. **Copilot does NOT re-review on
  later pushes** (it only reviews on PR creation, per step 2) — do not poll for
  a re-review that will never arrive; the fix commits are covered by CI and the
  human reviewer instead. Iterate until CI is clean.
- **Do not re-run `/code-review` (step 5) for every fix.** Run it **once per
  PR**; re-run it only when a later commit adds **substantive new code / logic**
  (not doc, wording, or trivial diff fixes). CI + Copilot + the human reviewer
  cover the small fix commits, and a full `/code-review` pass is expensive.

### If CI is red

Investigate → fix → push → re-monitor. Never merge on red. If `main` itself
broke, follow pr-conventions "If `main` breaks" (revert first, root-cause after).

### Conflict check & resolution

**When to check**: right after opening the PR, whenever `main` advances while
the PR is open (another PR merged first), and always immediately before merge
(step 7). GitHub computes mergeability asynchronously, so an `UNKNOWN` result
just means "not computed yet" — re-run after a few seconds.

```
gh pr view <n> --json mergeStateStatus,mergeable
# mergeable "MERGEABLE"  / mergeStateStatus "CLEAN" → no conflicts (other
#   pre-merge gates — CI, reviews — still apply, → Merging below)
# mergeable "CONFLICTING" / mergeStateStatus "DIRTY" → resolve below
# mergeable "UNKNOWN" → still computing; wait a few seconds and re-run
```

**Resolution** — rebase onto `main`, never merge `main` into the branch
(→ git-conventions):

```
git worktree list                    # the branch may live in another worktree —
                                     # run the rebase where it is checked out
git fetch origin main
git -c commit.gpgsign=false rebase origin/main
                                     # unattended runs must disable signing —
                                     # rebase re-signs every replayed commit
                                     # (→ unattended-commit-signing.md).
                                     # on conflict: resolve each file, then
git add <resolved files>
GIT_EDITOR=true git -c commit.gpgsign=false rebase --continue
```

- After the rebase completes, re-run the **scoped local checks** for the union
  of the branch's diff and the conflicted files (Rust set if Rust files are
  involved — the branch's code has never been built against the new `main`),
  plus `bash scripts/okf-lint.sh docs` if `docs/**` was conflicted.
- Only when those checks pass, push:
  `git push --force-with-lease` (own feature branch only → git-conventions).
- A force-push re-triggers CI — re-monitor it (steps 1 & 3). Copilot does not
  re-review (step 2). A mechanical rebase (conflicts resolved by keeping both
  sides / taking one side verbatim) needs no `/code-review` re-run; if the
  conflict resolution itself changed behavior, treat it like any substantive
  commit (step 5's re-run policy applies).

## Merging (step 7 — only when instructed)

- Default strategy: **Squash and Merge**; delete the branch
  (→ pr-conventions for when a non-default strategy is allowed):

  ```
  gh pr merge <n> --squash --delete-branch
  ```

- Pre-merge: `mergeStateStatus` clean, required checks green, no unresolved
  review threads. If it reports `mergeStateStatus: DIRTY` /
  `mergeable: CONFLICTING`, go through "Conflict check & resolution" above
  first:

  ```
  gh pr view <n> --json mergeStateStatus,mergeable,reviewDecision
  ```

- Post-merge: verify local `main` fast-forwarded and the working tree is clean
  (`git status`, `git log --oneline -1`).
- Merging `main` is outward-facing and hard to reverse → only with an explicit
  user go-ahead. High-risk git operations (force push, etc.) follow
  git-conventions' confirm-first rules.
