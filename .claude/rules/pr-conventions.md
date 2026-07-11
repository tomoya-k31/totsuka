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

- Patch: auto-merge is fine once CI passes.
- Minor: normal review flow, no auto-merge.
- Major: human review required — confirm breaking changes described in the PR body before merging.

## Releases

- Ship a release by merging the release-please "Release PR" (SemVer tag + CHANGELOG generated from the Conventional Commits history).
