> 🌐 **English** · [日本語](README.ja.md)

# `contracts/`

Machine-readable agreements between totsuka and code totsuka does **not**
control. Each subdirectory is one contract, and each is read by at least two
independent implementations that cannot share types.

| Contract | Read by |
|---|---|
| [`slack-event-gateway/`](slack-event-gateway/README.md) | `plugins/task-source-slack` and `services/slack-event-gateway` — two separate Cargo workspaces, and the gateway may be a fork ([ADR-0072](../ai-docs/decisions/adr-0072-slack-event-gateway.md) decision 9) |

## What belongs here — and what does not

Most fixtures in this repository are **not** contracts in this sense, and the
default is to keep them with the code that owns them:

- `plugins/agent-ide-herdr/schemas/` — the slice of herdr's Socket API that
  this plugin depends on. herdr is the authority; the plugin follows it.
  `scripts/herdr-schema-check.sh` reads it from that path.
- `crates/plugin-protocol/tests/fixtures/` — wire-format samples for the
  plugin protocol. `plugin-protocol` is the authority, and every plugin gets it
  as a dependency rather than by reading files.

A directory earns a place here only when **both** of these hold:

1. **At least one reader is outside this repository's build.** Not another
   workspace member, and not a dependency — something built separately, and
   possibly by someone else.
2. **The authoritative side cannot hand that authority over as a dependency.**
   There *is* an authority here — decision 7 makes totsuka's `project()` the
   reference projection, and `gateway_contract.rs` says so in as many words.
   But the gateway is built in a separate workspace and may be a fork, so it
   can never `use` that function. Fixtures are the only form the authority can
   take for that reader, and a form written *for* someone outside does not
   belong inside the half they cannot see.

If only the first holds, keep the fixtures with their owner and point at them.
If the outside reader can simply depend on the authoritative crate, it needs no
copy at all. If neither holds, they are ordinary test data.

## Rules for anything added here

- **Language-neutral.** JSON, not a Rust module, not a generated binding. A
  reimplementation in another language must be able to read it as-is.
- **Self-contained.** No reference a reader outside this repository cannot
  resolve: no crate paths, no `#[test]` names, no internal identifiers.
- **Located by walking up**, never by counting `../`. A hardcoded path does not
  fail when a directory moves — it finds nothing, and a suite that loads zero
  cases passes green. Both readers of `slack-event-gateway/` walk up from
  `CARGO_MANIFEST_DIR` and panic if the walk finds nothing.
- **One `README.md` / `README.ja.md` per contract**, stating what it pins down
  and why it is a contract rather than a sample.
