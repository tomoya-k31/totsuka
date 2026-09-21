# PR & release conventions

## PR title

- Format: `type(optional scope): <description>` — same type taxonomy as commits.
- A PR may bundle multiple commits but has one primary purpose: choose `type` by the single most valuable change it delivers. If the purpose is too mixed to pick one, split the PR instead.

## PR description

- Use `.github/PULL_REQUEST_TEMPLATE.md`. `# Overview` = a numbered list in Japanese describing background and what was done.
- Issue links: `fixes #xxx` when the PR is evidence of a bug fix; `closes #xxx` / `resolves #xxx` when it completes a feature or task.
- Aim for ≤400 changed lines; if exceeding, state why in the template's "Why This PR Wasn't Split" section (bulk-generated/vendored code, dependency bumps, and rename-only diffs are exempt).

## Before opening a PR

- Review every commit on the branch (`git log`, `git diff main...HEAD`) before writing the title and description.
- Confirm the change works locally and compiles without errors.

## Merge strategy

- Default: Squash and Merge. Merge Commit / Rebase and Merge require a stated reason in the PR description.

## If `main` breaks

- Revert first, investigate the root cause after. Revert commits use `type: revert`, with the original commit hash and the reason for the revert in the body. Fix the root cause through a normal follow-up PR.

## Dependency update PRs (Renovate)

Config: `.github/renovate.json5`; decisions in
[ADR-0088](../../ai-docs/decisions/adr-0088-renovate.md).

- **Automerged by Renovate itself** (`platformAutomerge: false`), only after
  every status on the branch is green: patch (all managers), GitHub Actions
  minor / patch / digest, Docker digests, and the weekly lock file
  maintenance. **Never use GitHub's Auto-merge button or `gh pr merge --auto`
  on them** — the ruleset requires only `lint`, so native auto-merge would
  land a red `clippy / rustfmt`, `test` or `msrv` (→ dev-flow).
- **Human review**: every major; Cargo minor; the four actions that run only
  in `release-please.yml` (`googleapis/release-please-action`,
  `docker/setup-buildx-action`, `docker/login-action`,
  `docker/build-push-action`) — PR CI never executes them, so green CI proves
  nothing about them. For a major, confirm the breaking changes in the PR body
  before merging.
- Commit type: `chore(deps)` (hidden from the CHANGELOG, no release);
  security updates are `fix(deps)` and cut a patch release.
- Labels: `renovate` on every PR, plus `renovate:<major|minor|patch|digest|lockfile|security>`
  and `renovate:<cargo|github-actions|dockerfile|terraform>`.
- Bot PRs (Renovate, release-please) are exempt from the PR description
  template above.

## Releases

- Ship a release by merging the release-please "Release PR" (SemVer tag + CHANGELOG generated from the Conventional Commits history).
