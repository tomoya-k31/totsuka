# Unattended commits — disable signing per invocation

When Claude creates a commit on its own (unattended / background execution),
always pass the signing-off flags on the command itself:

```bash
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "..."
```

- **Why**: `~/.gitconfig` signs commits via the 1Password integration, and
  signing requires a human interaction (Touch ID / approval prompt). A
  background run blocks forever waiting for it.
- **Do NOT change any git config** — neither global nor repo-local. Signing
  must stay enabled for human-made commits.
- Manual commits by the user are unaffected: a plain `git commit` (signed) is
  correct there.
