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
| any `*.md` anywhere | `rumdl check .` — Markdown lint (below) |
| `ai-docs/**` | **Docs checks** (below) + the docs obligation |
| one of the 5 generation sources under `ai-docs/` (see the `human-docs` skill) | regenerate the matching `docs/` pages with the **`human-docs` skill** in the same PR, then `bash scripts/docs-freshness.sh` |
| `docs/**` | `bash scripts/docs-freshness.sh` to zero errors + `rumdl check .`. Do **not** run `okf-lint` on it — it is not an OKF bundle |
| a prose `*.md` outside the OKF/vendored exclusions and outside `.claude/**` | update its `.ja.md` sibling (→ [documentation-i18n.md](documentation-i18n.md)) |
| `.github/workflows/**` | read the SHA-pin + `ubuntu-slim` rules, validate YAML (`yq . <file>`); if you changed `ci.yml`'s commands, also run the affected Rust set |
| `.claude/**` (settings / hooks / rules) | validate JSON (`python3 -m json.tool .claude/settings.json`); no Rust, no `.ja.md` |
| none of the above touch Rust/Cargo (docs-only, `.claude`-only, …) | **skip the Rust set entirely** |

Note: on every PR, CI runs `clippy / rustfmt` + `test` + `machete (unused
deps)` (`ci.yml`, no path filter) and the `lint` check (`okf-lint.yml`)
regardless of what changed. If `machete` fails, remove the unused dependency
or suppress a false positive per
[dependency-hygiene](../../ai-docs/development/dependency-hygiene.md).
`audit` (`audit.yml`) additionally runs on PRs touching `**/Cargo.toml` /
`Cargo.lock` / `deny.toml` (plus a daily cron); `coverage` runs only on push
to `main`. Scoping only changes what you run **locally** before pushing —
post-PR you still monitor all checks that report on your PR.

**Rust set** — when Rust/Cargo files changed. Keep `--workspace` on every
command: this is an 11-member workspace, so a missing `--workspace` can pass
locally yet fail CI. Two of CI's flags are deliberately **not** mirrored
locally (`RUSTFLAGS="-D warnings"` and `--all-features`) — see the test bullet
for why. CI's own flags stay exactly as they are:

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
- `cargo clippy --workspace --all-targets -- -D warnings` — CI passes
  `--all-features` here; locally it is dropped for the same reason as in the
  test bullet below (zero `[features]` in the workspace, so it selects nothing).
- **Tests — `bash scripts/dev-test.sh`**, not `cargo test` by hand
  (→ [ADR-0049](../../ai-docs/decisions/adr-0049-local-test-loop.md)). It runs
  `cargo build --workspace --all-targets`, then `cargo nextest run --workspace`,
  then `cargo test --doc --workspace`. Measured warm on this workspace: **10s
  against 20s** for the `cargo test` one-liner it replaces, running the same
  1,201 tests. Extra arguments are forwarded to `cargo nextest run` verbatim, so
  narrowing is nextest's filterset and nothing of ours:
  `bash scripts/dev-test.sh -E 'package(=agent-ide-herdr)'`.
  - it covers **tests only**. `fmt` / `arch-lint` / `clippy` / `cargo doc` are
    the other bullets in this list and stay manual
  - **Do not add `RUSTFLAGS="-D warnings"` or `--all-features` to local
    commands.** `--all-features` is a no-op in this workspace (zero `[features]`
    declarations, → ADR-0029), and the deny is already permanent: root
    `Cargo.toml` has `[workspace.lints.rust] warnings = "deny"` and all 11
    members opt in with `[lints] workspace = true`. That covers `tests/` targets
    — measured: an unused variable in an integration test fails a plain
    `cargo build -p <crate> --tests` with `error`, exit 101. Setting `RUSTFLAGS`
    changes the fingerprint without changing the outcome, so it builds
    everything a second time into an artifact space that clippy, `cargo doc` and
    rust-analyzer (none of which set it) do not share.
  - **CI's `env: RUSTFLAGS` is a different question — never touch it.** It is
    part of the rust-cache key (`warm-cache.yml` requires it byte-for-byte), so
    changing it invalidates every cache entry.
  - the script exports `TEST_SUPPORT_PREBUILT_BINS=1` itself, immediately after
    the workspace build, so the "everything has just been built" precondition
    holds by construction — and under a process-per-test runner that matters
    more than it did before, since without it the nested `cargo build` would run
    once *per test*. **If you run `cargo test` / `cargo nextest run` by hand, do
    not set it** unless you have just built the workspace, or the E2Es test
    stale binaries. Never rename it into the `TOTSUKA_` namespace: unrecognised
    `TOTSUKA_*` variables print a warning to the child's stderr (ADR-0009) and
    break the tests that parse stderr as JSON
    (→ [ADR-0018](../../ai-docs/decisions/adr-0018-ci-test-time.md)).
  - **`target/` needs an occasional `cargo clean`.** Nothing garbage-collects
    it: it had reached 1,131,673 files / 80.2 GiB, accumulated one artifact
    space per flag set. #459 reconstructed a ~20-minute local test run from
    artifact mtimes at that size; measured immediately after the clean, the same
    command took 32s. The bloated run was never timed directly, so treat the
    size as the leading suspect rather than a proven cause — but the clean took
    222s, which is cheap against either number.
- `cargo doc --workspace --no-deps` — rustdoc link integrity. `[workspace.lints.rust]
  warnings = "deny"` already makes a broken intra-doc link a hard error
  (exit 101), but **CI never runs `cargo doc`**, so nothing fires it: 18
  broken links accumulated on `main` undetected until PR #240 cleaned them
  up. This is the only check here with no CI counterpart — if you skip it,
  nothing else will catch it. The usual causes and their fixes:
  - link to a **private** item from public docs (`[`CONST`]` where `CONST`
    is not `pub`) → drop the link, use a plain code span `` `CONST` ``
  - item **not in scope** in that file (`[`ToolLaunchSpec`]`) → fully
    qualify it: ``[`ToolLaunchSpec`](plugin_protocol::methods::ToolLaunchSpec)``
  - **redundant** explicit target (the shorthand already resolves) → drop
    the target: ``[`Engine`](orchestrator_core::run::Engine)`` → ``[`Engine`]``
  - a **file path** or a citation marker that is not an item at all
    (``[`ai-docs/references/foo.md`]``, `[V3]`) → code span, or escape the
    brackets (`\[V3\]`)
- rust-analyzer LSP diagnostics clean — fix type errors / missing imports
  (rustc + clippy backed, per [CLAUDE.md](../../CLAUDE.md)).

**Markdown lint** — when any `*.md` changed, anywhere in the repo (not just
`ai-docs/**` — this covers `README*.md`, `.claude/**`, and prompt files under
`crates/`):

- `rumdl check .` to zero issues. Configuration is `.rumdl.toml` at the repo
  root; every disabled rule and per-file ignore carries its rationale inline, so
  read it before adding an ignore of your own.
- `rumdl check --fix .` auto-fixes most formatting findings. **Read the diff
  before accepting it** — the autofix is not always the right answer:
  - `MD040` (missing code-fence language) is filled in as `text` regardless of
    content. Downgrade-proof it by hand: `bash` for command-only blocks,
    `console` for `$`-prompt-plus-output blocks, `text` only for genuine output,
    logs, diagrams, and usage synopses.
  - `MD038` (spaces inside code spans) is disabled precisely because the
    autofix would silently invert meaning where the space *is* the point
    (`` ` #` ``, `` `: ` ``, the pane-label prefix `` `totsuka ` ``).
- It is a **local check only** — there is no CI job for it, so nothing else will
  catch a Markdown regression if you skip it.
- Run it **after resolving a conflict in any `*.md`**, not just after authoring
  one. `rumdl` is what catches a leftover conflict marker; `okf-lint` does not
  look for markers at all, so a docs-only conflict can be committed with one
  still in it and pass every other gate. Which rule fires depends on what
  surrounds the marker — `MD032` when it sits directly before a list, but
  `MD003`/`MD022` when it sits between plain paragraphs (a lone `=======` parses
  as a setext `H2` underline). Do not pattern-match on the rule id; treat *any*
  unexpected finding in a file you just resolved as "look for a marker". A
  `git grep` for all three forms (`<<<<<<<`, `=======`, `>>>>>>>`) across the
  worktree is the cheap belt-and-braces check.

**Docs checks** — when `ai-docs/**` changed:

- `bash scripts/okf-lint.sh ai-docs` to zero errors.
- If you touched one of the 5 sources that `docs/` is generated from, use the
  **`human-docs` skill** to regenerate the matching pages in the same PR and get
  `bash scripts/docs-freshness.sh` to zero errors. CI runs that check inside the
  `lint` job, so skipping it fails the PR — and **the check only proves the
  pages are not stale, never that their content is right** (→ ADR-0047).
- Docs obligation: if a trigger applies (design decision / new component /
  API·schema·infra change / release), update the relevant `ai-docs/` concept plus
  its `index.md` / `log.md` in the **same** PR (→ CLAUDE.md, ai-docs/CLAUDE.md).
- **`ai-docs/log.md` and the `index.md` concept lists are generated** (#360,
  [ADR-0031](../../ai-docs/decisions/adr-0031-docs-ledger-conflicts.md)). Write a
  **new** fragment `ai-docs/log.d/YYYY-MM-DD-<slug>.md` instead of editing
  `log.md`, then regenerate. `okf-lint`'s `log-sync` / `index-sync` fail if you
  forget:

  ```bash
  bash scripts/okf-log-build.sh    # ai-docs/log.md
  bash scripts/okf-index-build.sh  # index.md のマーカー区間
  ```

  Pick a `<slug>` unique to your change (issue number, topic) — that filename is
  the entire mechanism that keeps two same-day PRs from colliding.
- **Keep every ledger edit in ONE commit.** `git rebase` replays commit by
  commit, so a branch that touches `ai-docs/log.md` in three commits stops for the
  same conflict three times. One commit → at most one stop.

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

```bash
gh pr checks <n> --watch --interval 30   # wrap with a 10-min cap; raw --watch runs until done
```

### Steps

1. **Monitor CI** (the GitHub Actions workflows defined for the repo), on the
   30 s / 10 min policy above:

   ```bash
   gh pr checks <n> --watch --interval 30
   ```

   All required checks must be green.

2. **Monitor the GitHub Copilot auto-review.** It runs in parallel with CI and
   can take a few minutes to appear; poll on the same 30 s / 10 min cap.
   **This repo triggers the Copilot review only on PR creation** — later pushes
   do NOT re-trigger it, so only poll for it once, right after opening the PR
   (see Handling findings for what to do after fix commits).

   **Copilot has three different logins depending on where you look. Matching
   the wrong one silently reports "no review" forever.** Verified on PRs
   #354/#355/#357:

   | Where | Field | Value |
   |---|---|---|
   | `gh api .../pulls/<n>/reviews` (REST) | `.user.login` | `copilot-pull-request-reviewer[bot]` |
   | `gh pr view <n> --json reviews` (GraphQL) | `.author.login` | `copilot-pull-request-reviewer` |
   | `gh api .../pulls/<n>/comments` (inline) | `.user.login` | `Copilot` |

   An exact-match `select(.user.login == "copilot-pull-request-reviewer")`
   against the REST endpoint therefore **never matches** — the `[bot]` suffix is
   only present there. That exact bug made three PRs in a row report "no
   Copilot review" while the review had been sitting there the whole time, and
   the reviews were only found later by hand via the GraphQL path. **Match on a
   case-insensitive `copilot` prefix**, which is stable across all three.

   Copy-pasteable watcher (30 s interval, 10 min cap, prints both levels):

   ```bash
   pr=<n>
   deadline=$(( $(date +%s) + 600 ))
   while [ "$(date +%s)" -lt "${deadline}" ]; do
     found=$(gh api "repos/{owner}/{repo}/pulls/${pr}/reviews" \
       --jq '[.[] | select(.user.login | ascii_downcase | startswith("copilot"))] | length')
     if [ "${found}" != "0" ]; then
       echo "### review-level"
       gh api "repos/{owner}/{repo}/pulls/${pr}/reviews" \
         --jq '.[] | select(.user.login | ascii_downcase | startswith("copilot"))
               | "state=\(.state)\n\(.body)"'
       echo "### inline comments"
       gh api "repos/{owner}/{repo}/pulls/${pr}/comments" \
         --jq '.[] | select(.user.login | ascii_downcase | startswith("copilot"))
               | "--- \(.path):\(.line)  id=\(.id)\n\(.body)"'
       exit 0
     fi
     sleep 30
   done
   echo "no Copilot review after 10 min — report to the user and wait"
   ```

   **Fetch both levels.** The review-level record carries the verdict and the
   summary; the findings themselves are the inline comments and are absent from
   it. A PR can have a review with zero inline comments (no findings) — that is
   a real result, not a fetch failure.

   The `id` printed per inline comment is what you reply to when recording a
   rationale (→ Handling findings):

   ```bash
   gh api repos/{owner}/{repo}/pulls/<n>/comments/<id>/replies -f body='…'
   ```

   **Sanity-check a "no review" verdict before believing it.** If the watcher
   reports nothing, run the review-level query once with no `select` at all —
   if rows come back, the filter is wrong, not the review missing:

   ```bash
   gh api repos/{owner}/{repo}/pulls/<n>/reviews --jq '.[] | "\(.user.login)\t\(.state)"'
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

   ```text
   Agent(subagent_type: "general-purpose",
         prompt: "Run /code-review:code-review --comment on PR <n> and report the findings")
   ```

   **Why this runs at all when CI is green and Copilot has reviewed.** The
   three gates fail differently, and the gap `/code-review` covers is the
   repo's own written obligations — nothing in CI encodes them. The concrete
   case: PR #320 commit `5a8c465` added a new `pub mod` to
   `crates/orchestrator-core/src/lib.rs` with no row added to
   `ai-docs/components/orchestrator-core.md`. CI was green on it and Copilot
   reviewed it and reported no comments; `/code-review` is what found the docs
   obligation, and `6c4f8d7` added the row. **Check the commits, not
   `gh pr diff 320`** — the merged diff contains the fix, so the violation is
   only visible in the interim state. `okf-lint` cannot catch this by
   construction: it validates the docs that exist and has no way to know a
   module was added. Do not reason "CI is green, so the diff is fine"; CI
   answers "does it build and pass tests", which is a strictly smaller
   question.

6. **Vet each `/code-review` finding the same way** (valid vs mistaken).

7. **Park the PR — do NOT chase `main`.** Once steps 1-6 are done the PR has a
   *banked green*, and that green already proves what a rebase would re-prove
   (see Parking below). Leave it alone until there is an instruction to merge.

8. **Merge only on explicit instruction; otherwise wait.** Rebase once, at that
   point, if `main` has moved (→ Conflict check & resolution, then Merging).

### Parking a ready PR (step 7)

**Chasing `main` buys nothing, and it is not free.** Three measured facts:

1. **CI runs on the merge ref.** `ci.yml` triggers on `pull_request` with no
   `types`, so GitHub checks out `refs/pull/<n>/merge` — a green run has
   *already* tested "branch ⊕ `main` as of that moment". Rebasing to catch up
   re-proves the same statement about a slightly newer `main`.
2. **The ruleset does not require the branch to be up to date.** Verified on
   `main-required-checks`: `strict_required_status_checks_policy: false`, and
   the required contexts are **`lint` only**. A behind-but-clean branch merges.
3. **A conflicted PR runs no CI at all** — GitHub cannot build
   `refs/pull/<n>/merge`, so every `pull_request` workflow is skipped with no
   report (PR #169: `gh pr checks` says "no checks reported"). So a rebase you
   did not need can *cost* you a full round trip if it lands you in a conflict.

PR #288 paid rebase + a full CI cycle **three times** while open, for a result
that was already proven each time.

**Park by default. Rebase once, when told to merge.** Break the park only for:

| 例外 | なぜ |
|---|---|
| **banked green が無い** | CI が緑になったことが一度も無いなら park する対象がない。まず緑にする |
| **自分か `main` の進みが `Cargo.*` を触る** | lockfile はマージ時に機械的に解決できない。早く突き合わせるほど安い |
| **`main` の進みが自分の変更ファイルと重なる**（元帳は除く） | 論理的な衝突は merge ref の green では捕まらない。同じ関数を両側が触ったら早く見る |
| **bot PR**（release-please） | Release PR は release-please が force-push し、`sync-lockfile` ジョブが `Cargo.lock` を書き戻す。人間の park の前提（ブランチが動かない）が成り立たない |
| **park が 3 回 / 1 日を超えた** | 「証明済みの green」が古くなりすぎると、2 の "up to date でなくてよい" が形式的にしか正しくなくなる |

**Parking makes "no checks reported" the normal state**, because the PR's last
CI run belongs to an older head. Do not read that as pass — see below.

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
  Explicitly **not** re-run triggers: a fix commit answering a finding, a
  force-push that only replays existing work, and **a merge conflict plus a
  mechanical rebase resolving it** (conflicts resolved by keeping both sides,
  taking one side verbatim, or regenerating a ledger from its source) — a
  conflict says `main` moved, not that the diff became riskier. Parking (step 7)
  makes this the common case: a parked PR is rebased once, right before merge.
  The **carve-out is load-bearing**: if the conflict resolution
  itself changed behavior, or the force-push carries substantive new logic, it
  is a substantive commit and the rule above applies. (This restates Conflict
  check & resolution below, because that is not where anyone looks when
  deciding whether to re-run. Keep the carve-out in both copies — an
  unconditional version of this rule was already shipped once and corrected
  after review, in `29161af`.)

### If CI is red — or absent, or stale

Investigate → fix → push → re-monitor. If `main` itself broke, follow
pr-conventions "If `main` breaks" (revert first, root-cause after).

**Never merge on red — and "not red" is not the same as green.** Parking (step
7) makes the other two states normal, so name them explicitly:

```bash
gh pr view <n> --json headRefOid,mergeStateStatus,statusCheckRollup \
  --jq '{head: .headRefOid, merge: .mergeStateStatus,
         checks: [.statusCheckRollup[] | "\(.name)=\(.conclusion)"]}'
```

| 状態 | 見え方 | 意味 |
|---|---|---|
| **red** | `checks` に `FAILURE` がある | 直す |
| **absent（衝突）** | `checks` が空 かつ `merge: "DIRTY"` | **緑ではない。** merge ref を作れないので全ワークフローが無報告でスキップされる（PR #169）。まず衝突を解消する |
| **absent（未着手）** | `checks` が空 かつ `merge` はそれ以外 | まだ走り出していないだけ。待つ |
| **stale** | force-push 直後で `checks` が空／前の run のまま | 新しい head の run を待つ。push した瞬間に前の green は自分のものではなくなる |

`statusCheckRollup` は PR の**最新コミット**に紐づくので、返ってきた結果は
`headRefOid` のものである。**危ないのは「空」の解釈**で、上の 2 行が示すとおり
「衝突している」と「まだ走っていない」は見分けがつかない — `mergeStateStatus` で
割ること。どちらも pass ではない。

### Conflict check & resolution

**When to check**: right after opening the PR, and **once immediately before
merge** (step 8). *Not* every time `main` advances — that is the loop parking
(step 7) exists to break; a `DIRTY` PR you are not about to merge costs nothing
until you merge it. GitHub computes mergeability asynchronously, so an `UNKNOWN`
result just means "not computed yet" — re-run after a few seconds.

```bash
gh pr view <n> --json mergeStateStatus,mergeable
# mergeable "MERGEABLE"  / mergeStateStatus "CLEAN" → no conflicts (other
#   pre-merge gates — CI, reviews — still apply, → Merging below)
# mergeable "CONFLICTING" / mergeStateStatus "DIRTY" → resolve below
# mergeable "UNKNOWN" → still computing; wait a few seconds and re-run
```

**Resolution** — rebase onto `main`, never merge `main` into the branch
(→ git-conventions):

```bash
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

**During a rebase `--ours` and `--theirs` are inverted** relative to intuition:
the rebase replays *your* commits onto `main`, so `--ours` is **`main`** and
`--theirs` is **your branch**. Getting this backwards silently discards one
side. `git checkout --ours <file>` during a rebase keeps `main`'s version.

**The ledger files have a fixed, judgment-free resolution** (#360) — never
hand-merge their markers:

```bash
bash scripts/okf-log-build.sh && git add ai-docs/log.md
```

Both sides' `ai-docs/log.d/` fragments are **new files**, so they merge cleanly on
their own and regenerating picks up both. `ai-docs/**/index.md` is `merge=union`
(`.gitattributes`) and does not conflict at all; run
`bash scripts/okf-index-build.sh` afterwards to collapse any duplicate the union
kept.

**"Keep both sides" is a resolution for list-structured files only** — the
ledgers above, and files that are genuinely a flat list of independent lines.
It is **not** a general strategy: in source, concatenating both sides cuts a
function in half. `cli_commands.rs` was resolved that way once and failed to
compile with `unclosed delimiter`. For anything else, read the hunk.

**Check for leftover conflict markers with `--check`, not `grep`:**

```bash
git diff main...HEAD --check      # 第一手。空なら残骸なし
```

`git grep '======='` also matches a setext `H2` underline, which is a real
construct in Markdown — it false-positives on prose. `--check` knows what a
conflict marker is. (`rumdl` catches markers too, but only for `*.md`, and
which rule fires depends on what surrounds the marker — see Markdown lint.)

#### After the rebase — staged re-checks

**A force-push re-runs the entire CI suite, so local checks here buy fail-fast,
not correctness — with two exceptions.** `rumdl` and `cargo doc --workspace
--no-deps` have **no CI counterpart at all**; if you skip those, nothing else
catches the regression. Everything else is a duplicate of what CI is about to
run anyway.

Pick the tier from **what conflicted × what `main`'s advance contains**:

| Tier | 条件 | ローカルで回すもの |
|---|---|---|
| **T0** | 衝突ゼロで rebase が通り、`main` の進みが自分の変更ファイルと 1 つも重ならない | 何も回さない。push して CI に任せる |
| **T1** | 衝突が**元帳だけ**（`ai-docs/log.md` / `ai-docs/**/index.md`）で、`main` の進みが Rust/Cargo を触っていない | `git diff main...HEAD --check`／`bash scripts/okf-lint.sh ai-docs`／`rumdl check .` |
| **T2** | 衝突が `*.md`・`.claude/**`・`ai-docs/**`（元帳以外）に及ぶ | T1 と同じ（対象が広がるだけ） |
| **T3** | 衝突が Rust/Cargo に及ぶ、**または** `main` の進みが `**/*.rs` / `Cargo.*` を触る | T2 ＋ **Rust セット一式**（`cargo doc` を含む） |

**なぜ T1/T2 で Rust を回さないのか。** `main` の進みが Rust/Cargo を 1 バイトも
触っていないなら、rebase 後のブランチの Rust ソースと依存グラフは、**CI が既に
green にした merge ref のそれとバイト同一**である。clippy と test はその同じ入力に
対する同じ計算なので、証明可能に冗長になる。docs だけの衝突で 11 crate 分の
clippy + test を回していたのはこれである（PR #288）。

**逆に T3 は必ず回す。** `main` が Rust を触ったなら、ブランチのコードが**新しい
`main` に対してビルドされたことは一度も無い**。ここは fail-fast ではなく、
CI を 1 周無駄にしないための実質的な検査になる。

- Only when the tier's checks pass, push:
  `git push --force-with-lease` (own feature branch only → git-conventions).
- A force-push re-triggers CI — re-monitor it (steps 1 & 3). Copilot does not
  re-review (step 2). A mechanical rebase needs no `/code-review` re-run —
  conflicts resolved by keeping both sides, taking one side verbatim, or
  **regenerating a ledger from its source** are all mechanical. **The carve-out
  is load-bearing**: if the conflict resolution itself changed behavior, treat
  it like any substantive commit (step 5's re-run policy applies). Keep this
  wording in step with the copy under Handling findings — an unconditional
  version of this rule shipped once and was corrected after review (`29161af`).

## Merging (step 8 — only when instructed)

- Default strategy: **Squash and Merge**; delete the branch
  (→ pr-conventions for when a non-default strategy is allowed). Close the
  window between "green" and "merged" so another merge cannot invalidate it:

  ```bash
  gh pr checks <n> --watch --fail-fast && gh pr merge <n> --squash --delete-branch
  ```

- **Merge ready PRs one at a time, serially.** Each squash moves `main`, which
  makes every other open PR behind (and possibly `DIRTY`). Merging in parallel
  means resolving the same conflict repeatedly against a `main` that keeps
  moving; merging serially means each PR is rebased at most once, right before
  its own merge.
- Pre-merge: `mergeStateStatus` clean, required checks green **for the current
  head** (→ "red, absent, or stale"), no unresolved review threads. If it
  reports `mergeStateStatus: DIRTY` / `mergeable: CONFLICTING`, go through
  "Conflict check & resolution" above first:

  ```bash
  gh pr view <n> --json mergeStateStatus,mergeable,reviewDecision
  ```

- Post-merge: verify local `main` fast-forwarded and the working tree is clean
  (`git status`, `git log --oneline -1`).
- Merging `main` is outward-facing and hard to reverse → only with an explicit
  user go-ahead. High-risk git operations (force push, etc.) follow
  git-conventions' confirm-first rules.

### Two merge shortcuts that are wrong here

- **`gh pr merge --auto` is unusable in this repo.** `--auto` fires as soon as
  the *required* checks pass, and the ruleset requires **`lint` only** —
  `okf-lint.yml` has no path filter, so it is green on nearly every PR. Auto
  merge would therefore land a PR with `clippy / rustfmt` and `test` still red.
  Using it safely would first need those two contexts added to the ruleset's
  required checks (a repo settings change, not a flag).
- **Never use `gh pr update-branch` or GitHub's "Update branch" button.** Its
  default merges `main` **into** the branch, creating a merge commit — which
  git-conventions forbids ("never `git merge main`", keep history linear).
  `allow_update_branch: true` is set on the repo, so the button is always
  visible in the UI; that is not permission to press it. Rebase instead.
