> 🌐 **English** · [日本語](README.ja.md)

# Slack Event Gateway conformance suite

This directory is the **contract** required by decision 7 of
[ADR-0072](../../ai-docs/decisions/adr-0072-slack-event-gateway.md).

With `event_source = "gateway"`, Slack's deliveries reach totsuka through an
Event Gateway that runs under the operator's own account and may be a fork
(decision 9). **totsuka cannot trust that code**, so the only thing the two
sides can agree on is what is in here.

## Why a test suite and not one sample record

Decision 4 narrowed what gets published, which turned the gateway's filter into
a **gate**: a message it drops has no record and is invisible to totsuka
forever. The two error directions are not symmetric.

| Error | Result |
|---|---|
| False positive (published, not really a mention) | One wasted `fetch_message`, which `mention.rs` then discards. Harmless |
| False negative (a mention, not published) | **The mention silently disappears. The only fatal case** |

A single golden record would pin the *shape* and let the *judgement* drift —
"this text should raise this flag" is exactly what one sample cannot express,
and judgement is where the damage is. So every case is a **pair**: a raw
delivery, and the records it must become.

## Where this lives, and how each side reads it

This directory sits at the repository root. The gateway
(`slack-event-gateway/`) is outside the Cargo workspace via `[workspace]
exclude` (decision 9), so it cannot share totsuka's types — these files are all
there is.

Both sides locate it by walking **up** from `CARGO_MANIFEST_DIR` until they
find a directory containing `contracts/slack-event-gateway/cases`. Do not
hardcode the number of `../` steps: if either side is ever moved, a hardcoded
path does not fail — it finds nothing, and **a suite that loads zero cases
passes**. Panic when the walk finds nothing.

## Case format

One file per case under `cases/*.json`.

```json
{
  "name": "message-mention-closed-tag",
  "why": "One sentence naming the property this case defends",
  "delivery": {
    "endpoint": "events",
    "content_type": "application/json",
    "payload": { "…what Slack POSTs…" }
  },
  "expect": [
    {
      "topic": "events",
      "identity": "message:C0LOBBY:1757640000.000100",
      "record": { "…what goes on the Pub/Sub topic…" }
    }
  ]
}
```

| Key | Meaning |
|---|---|
| `delivery.endpoint` | `events` = the Event Subscriptions Request URL, `interactivity` = the Interactivity & Shortcuts Request URL. **These are two separate settings in the Slack app, and pointing only the first at the gateway means no approval button ever arrives** |
| `delivery.content_type` | `application/json` or `application/x-www-form-urlencoded` |
| `delivery.payload` | The **decoded** JSON. For `x-www-form-urlencoded` the real request body is `payload=` followed by this object, `JSON.stringify`-ed and percent-encoded |
| `expect` | The records to publish. **An empty array means the delivery is filtered out** — that is half the suite |
| `expect[].topic` | `events` or `block_actions`. The latter is a separate topic because its retention has to clear `response_url`'s life (decision 5) |
| `expect[].identity` | The delivery-identity key; see below |
| `expect[].record` | The Pub/Sub message body |
| `expect_challenge` | Present only for `url_verification`: the string to echo back |

### Fixed values

| Value | Setting |
|---|---|
| Registered operator's Slack user id | `U_ME` |
| `received_at` | `2026-09-13T00:00:00Z` |

`received_at` is genuinely "when it was received", so the implementation
decides it. A conformance runner must **inject a fixed clock** — build the
gateway with a replaceable time source. totsuka does not use this value to
decide what to file: the ingest window is computed from Slack's `ts`, so a
gateway with a skewed clock cannot change what gets filed.

### Delivery identity

Both Slack's redelivery and Pub/Sub's delivery are at-least-once, so each
`kind` needs a stable key for "the same happening" (decision 7). Without one,
each implementation falls to one side or the other: duplicate work, or a
dropped event.

| `kind` | `identity` |
|---|---|
| `message` | `message:{channel}:{ts}` |
| `reaction` | `reaction:{channel}:{ts}:{user}:{reaction}` |
| `block_actions` | `block_actions:{container_channel}:{action_ts}:{action_id}` |

For `message`, the identity without its prefix is **byte-identical** to
totsuka's `Mention::message_key()`, so the existing dedup covers gateway-sourced
and socket-sourced deliveries alike. A test pins this.

## What this suite does not cover

**HTTP-level rejection — bad signature, stale timestamp, unknown path — is not
here.** That is not a contract about projection but an acceptance criterion for
the gateway itself (ADR decision 11). Passing this suite does not make an
implementation without signature verification conformant.

For the same reason, decoding `x-www-form-urlencoded` belongs to the gateway's
own tests. What is fixed here is **what comes out of the decoded payload**.

---

The design decisions behind this contract are recorded in
`ai-docs/decisions/adr-0072-slack-event-gateway.md`.
