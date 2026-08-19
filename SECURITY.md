> 🌐 **English** · [日本語](SECURITY.ja.md)

# Security policy

## Supported versions

totsuka is pre-1.0 and ships from a single line. Only the **latest released
version** receives security fixes; there are no maintained backport branches.
See the [releases page](https://github.com/tomoya-k31/totsuka/releases) for the
current version.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.** Use the private
channel below instead.

Report it privately through GitHub's
[private vulnerability reporting](https://github.com/tomoya-k31/totsuka/security/advisories/new).
This is a personal project maintained in spare time, so please allow a few days
for an initial response.

Useful things to include: the version, your platform, the configuration
involved (with secrets redacted), and the steps to reproduce.

## Scope

totsuka runs locally on an operator's machine, holds credentials for the
services it talks to (GitHub, Slack, Notion, an LLM endpoint), and dispatches AI
agents that execute code in git worktrees. Reports that are especially in scope:

- **Credential disclosure** — a secret reaching a log, an error message, a
  terminal, or a subprocess environment that should not receive it.
- **Untrusted input reaching a privileged sink** — task text arrives from
  external sources (Slack messages, GitHub issues), so command injection,
  argument injection, or terminal-escape injection through that path.
- **Sandbox and boundary escapes** — a task escaping its own worktree, or a
  plugin reaching beyond the plugin protocol.
- **Publishing under the operator's identity** without the approval step that
  is supposed to gate it.

## Known non-goals

These are deliberate design decisions, not vulnerabilities. Please do not
report them as such:

- **Read-only execution is explicitly not guaranteed.** The read-only profiles
  (`answer` / `triage` / `design`) and `mode = "plan"` express intent and add
  layered `deny` rules plus after-the-fact detection — they do **not**
  structurally stop an agent from writing files, committing, pushing, or
  opening a pull request via Bash. Sandboxing was evaluated and deliberately
  not implemented; see
  [ADR-0045](ai-docs/decisions/adr-0045-read-only-is-not-guaranteed.md).
- **The agent is trusted with the worktree.** totsuka hands an AI agent a
  checkout and lets it work; a prompt that convinces the agent to do something
  unwanted within that checkout is a property of the agent, not a totsuka
  boundary.
- **Secrets are only as private as the backend holding them.** totsuka stores
  references (Keychain, `op://`, `cmd:`, env) and never writes secret values to
  its own config files, but a secret placed by hand into a config file stays
  where it was put.
