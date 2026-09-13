> 🌐 **English** · [日本語](event-gateway-setup.ja.md)

<!-- generated-from: ai-docs/operations/event-gateway-setup.md sha256:09e88bec4f404d4a75abf1bfbbcec928803048abb071aa28193ef104ba45b7f5 -->

# Event Gateway setup

**Only for `event_source = "gateway"`.** If you run Slack over the socket
connection, none of this applies — [Slack setup](slack-setup.md) is the whole
story.

The gateway exists because with the socket connection **`totsuka run` holds it
itself**: mentions that arrive while totsuka is stopped are lost, and if it stays
stopped long enough Slack switches the app's event subscription off entirely.
The gateway receives on totsuka's behalf and queues, so nothing has to be
running for Slack to deliver. The price is one GCP project and about $1/month.

What this removes is **totsuka being stopped as a cause of failed delivery** —
not failed delivery. A gateway that is down, a wrong Request URL, or a failed
publish all still produce one.

## 0. Check one organization policy first

**If this one does not pass, the design has to change**, so check it before
anything else.

Slack cannot be an IAM principal, so Cloud Run has to accept unauthenticated
callers. The usual way is granting `roles/run.invoker` to `allUsers`, and
**Domain Restricted Sharing refuses that** — it is on by default for
organizations created since May 2024.

The supported alternative is `--no-invoker-iam-check` (`invoker_iam_disabled` in
OpenTofu), which Google documents for exactly this situation. **It requires no
change to any organization policy.**

An administrator can, however, block that too:

```bash
gcloud resource-manager org-policies describe \
  constraints/run.managed.requireInvokerIam --organization <ORG_ID>
```

It is **not enforced by default**, so most organizations pass straight through.
If it *is* enforced, this construction does not work — and an external load
balancer does not rescue it, because a serverless NEG still reaches Cloud Run
unauthenticated, so `allUsers` would be needed anyway. Fall back to the socket
connection, or talk to whoever administers the organization.

## 1. A GCP project

One project with billing enabled. Sharing an existing one is fine, but this
**enables APIs and creates a service account and IAM bindings**, so a dedicated
project is easier to reason about.

```bash
gcloud auth login
gcloud config set project <PROJECT_ID>
```

## 2. Four values per person

**Create the Slack app first.** The ordering is awkward because the hostname
that goes into the Request URL is a result of `tofu apply`, and the signing
secret that goes into `tofu apply` is a result of the Slack app. Split it like
this so it does not go in a circle:

1. **Create the app only** (steps 1–3 of [Slack setup](slack-setup.md)). Leave
   the `<gateway-host>` placeholder in `manifest.gateway.yml` alone — Slack
   verifies a Request URL when you *save* it, and creating from a manifest does
   not verify it yet.
2. The signing secret exists from that moment, so the four values below are
   available.
3. `tofu apply` (step 3) produces the hostname.
4. **Go back to the app and fill in the Request URL in both places** (step 4).
   That is when verification actually runs.

| Value | Where it comes from |
|---|---|
| Slack user id (`U…`) | Slack profile → **⋯** → Copy member ID |
| Signing secret | Slack app → Basic Information → App Credentials → Signing Secret |
| Path token | **Generate it. Do not invent one** — `openssl rand -hex 24` |
| Google principal | The identity allowed to read that person's queues — the account they will run `gcloud auth application-default login` as in step 5, written as `user:alice@example.com` |

**The path token is a credential.** There is no IAM and no IP allowlist in front
of the public endpoint, so the only things standing in the way are the
unguessable path, the signature, and a five-minute timestamp window. It has to
be a random string, not a chosen word.

**Each person gets their own Slack app.** Slack does support one app installed by
several people, but then a message in a channel two of them are in arrives as a
single event, and working out who it was for costs an extra API call on every
message.

## 3. Apply

```bash
cd slack-event-gateway/tofu
cp terraform.tfvars.example terraform.tfvars
# fill in the values from step 2
tofu init
tofu plan
tofu apply
```

That creates one Cloud Run service shared by everyone (routing is by path), two
Pub/Sub topics and two subscriptions per person, the registration table in
Secret Manager, and IAM that lets the service publish everywhere while **each
person can read only their own queues**.

**No service-account keys are created.** The service gets a token from the
metadata server; each person uses their own Google account.

### Secrets and state

**Both `terraform.tfvars` and the state file hold every person's Slack signing
secret.** OpenTofu does not encrypt state, so keep it in a bucket only the
deployer can read, and **treat access to that bucket as access to everyone's
Slack app**. Being gitignored only keeps them out of git.

## 4. Put the Request URL into Slack

```bash
tofu output -json request_urls
```

Paste each person's URL into **two** settings of their Slack app:

| Slack app setting | Carries |
|---|---|
| Event Subscriptions → Request URL | mentions, reactions |
| Interactivity & Shortcuts → Request URL | approval and repository-picker buttons |

**With only the first, mentions keep working and no button ever arrives** — a
symptom that does not point at its cause. Slack verifies the URL when you save
it, so **saving successfully is the connectivity check**. If it will not save,
either the gateway is not running or the URL is wrong.

## 5. Configure totsuka

```bash
tofu output -json totsuka_config
```

**These are keys to add to the `[slack]` table, not a table to paste.**

Run `totsuka setup` first (step 3 of [Slack setup](slack-setup.md)) and let it
write `[slack]` with `user_token` and `target_user_id`. **`setup` leaves an
existing `[slack]` table alone**, so pasting only this block first means those
required keys never get added.

Then add to what `setup` wrote:

```toml
[slack]
# …the user_token / target_user_id setup wrote, unchanged…
event_source = "gateway"

[slack.gateway]
project                    = "<PROJECT_ID>"
subscription               = "slack-event-gateway-<key>-events"
block_actions_subscription = "slack-event-gateway-<key>-block-actions"
```

The `app_token` line `setup` wrote can be deleted — nothing opens a WebSocket.
The `doctor` run at the end of `setup` fails asking for an app-level token
because it happens *before* this edit; re-run `totsuka doctor` afterwards and it
goes green.

Then, once per person, on their own machine:

```bash
gcloud auth application-default login
```

totsuka reads its queues with **that** identity. At startup it reads each queue
once, so a wrong identity, a missing permission or a mistyped name fails
immediately — left unchecked, all three produce the same thing: a green
`doctor` and not one event.

## Adding a person

Add one entry to `operators` in `terraform.tfvars` and `tofu apply`. Topics,
subscriptions, IAM and the registration table all follow, and **no existing
person's resources are touched**. The apply also rolls a new revision, so the
new table is live when it finishes.

Then do steps 2, 4 and 5 for that person: their own Slack app, the Request URL
in both settings, and `gcloud auth application-default login` on their machine.

## Destroy

```bash
tofu destroy
```

Works as-is. APIs this enabled are **left enabled** — another workload in the
project may be using them.

## Two things the cost estimate assumes

About a dollar a month holds **only while both of these do**.

**`min_instance_count = 0`.** At 1 the service is billed for sitting idle and the
figure moves by an order of magnitude. This is not a tuning knob; it is why the
setup is cheap. The cost of a cold start is about a second, against Slack's
three-second acknowledgement window.

**A cap on `max_instances` (4 by default).** Every gate on the public side is
evaluated *inside* the container, so **a request rejected for a bad signature is
still billed, cold start included**. With no IAM layer in front, scaling to the
platform default of 100 does not match the cost premise. Raise it if deliveries
are actually being shed — not pre-emptively.

Tokyo (`asia-northeast1`) is a tier 1 Cloud Run region, which the estimate
assumes. Tier 2 regions (`asia-east2`, `asia-northeast3`, `asia-southeast1`, …)
cost more per vCPU-second.

## What cannot be locked down, and what is not by default

**Source IP cannot be restricted.** Slack publishes no stable list of addresses
to allow. An allowlist built on guesses starts dropping deliveries, and **enough
dropped deliveries is what makes Slack disable the subscription** — the exact
failure this exists to remove. The gates are the unguessable path, the
signature, and the five-minute window, all inside the container.

**VPC Service Controls are not enabled by default.** If you want the *reading*
side confined to company networks, a perimeter around Pub/Sub is the way — but
add it deliberately: it also stops people draining their queue from home or on a
trip, which is squarely what totsuka is built for.

**Domain Restricted Sharing stays on.** This is built to work without relaxing
any organization policy.

---

This page is generated from `ai-docs/operations/event-gateway-setup.md`.
