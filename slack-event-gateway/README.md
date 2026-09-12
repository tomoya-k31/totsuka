> 🌐 **English** · [日本語](README.ja.md)

# Slack Event Gateway

Receives Slack deliveries over HTTPS, projects each one to **coordinates**, and
publishes those to the operator's Pub/Sub topic. totsuka drains the topic while
it is running.

That indirection is the whole point: with Socket Mode, `totsuka run` holds the
WebSocket itself, so every mention that arrives while it is stopped is lost —
and enough failed deliveries make Slack disable the app's event subscription
until a human re-enables it by hand. Nothing here has to stay running for Slack
to deliver.

The design is recorded in `ai-docs/decisions/adr-0072-slack-event-gateway.md`.

## What this process must never do

**It does not store, forward, or log the message body.** The only thing it does
with the text is compare it against constant strings. It calls no external API
to interpret a message — no LLM, nothing. The one outbound call is the Pub/Sub
publish, which is the point of the process.

The record schema has no field that could carry a body, but *that is not the
guarantee*: a process can keep a string in memory, print it, or post it
somewhere else. The guarantee is the code plus the tests that pin it
(`tests/http.rs`).

## Why it lives here but outside the workspace

The root `Cargo.toml` excludes this directory. It cannot go under `plugins/` —
the architecture lint requires every member there to be a totsuka plugin — and
putting it under `crates/` would land it in every contributor's
`cargo build --workspace` for a service most of them will never deploy.

The cost is that it cannot share types with the plugin. That is why
`contracts/slack-event-gateway/` exists: the conformance cases under it are the
entire agreement between the two sides, and both prove they satisfy them
independently. **A fork of this gateway that passes the suite is conformant.**

## The gate

Slack cannot be an IAM principal, and publishes no stable list of source IPs to
allow — an allowlist built on guesses would start dropping deliveries, which is
the very failure this exists to fix. So there is no layer in front of this
container, and exactly three things stand between it and the open internet:

1. **An unguessable path.** `/slack/e/<opaque-token>`, one per operator. The
   path selects whose signing secret verifies the request; reading the identity
   out of the body instead would mean choosing the key by trusting unverified
   input.
2. **The signature.** HMAC-SHA256 over the raw body, compared in constant time.
3. **The timestamp window.** Five minutes. The signature proves a body came
   from Slack; only the window stops a captured request from working forever.

An unregistered path and a bad signature get the same answer, so the path is
not worth guessing at.

## Configuration

| Variable | Meaning |
|---|---|
| `REGISTRATIONS_PATH` | File holding the registration table — a mounted Secret Manager secret. **Preferred** |
| `REGISTRATIONS` | The table inline. Simpler, but it puts every operator's signing secret in the revision's configuration |
| `PORT` | Listen port. Cloud Run sets this; defaults to 8080 |
| `PUBSUB_URL` | Pub/Sub base URL. For tests |

The registration table, one row per operator:

```json
{
  "users": [
    {
      "path_token": "<unguessable>",
      "slack_user_id": "U0123456",
      "signing_secret": "<from the Slack app's Basic Information page>",
      "topic": "projects/<project>/topics/<operator>-events",
      "block_actions_topic": "projects/<project>/topics/<operator>-presses"
    }
  ]
}
```

The two topics are separate because their retentions differ: presses have to
outlive `response_url`'s roughly 30-minute life but not much more, while
messages are kept for days. A table naming one topic for both is refused at
startup.

## Two Request URLs, not one

Slack has **two** settings, and you need both pointed here:

| Slack app setting | Carries |
|---|---|
| Event Subscriptions → Request URL | mentions, reactions |
| Interactivity & Shortcuts → Request URL | button presses (`block_actions`) |

Socket Mode delivered both down one WebSocket, so it is easy to wire up only
the first — and the symptom is that approval and repository-picker buttons
never arrive at all, while mentions keep working.

## Running the tests

```bash
cd slack-event-gateway
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`tests/conformance.rs` reads `contracts/slack-event-gateway/` from the
repository root, so run it from inside a full checkout.
