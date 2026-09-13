> 🌐 **English** · [日本語](README.ja.md)

# Event Gateway — OpenTofu module

`tofu apply` stands the whole thing up. This file is the module's own
reference; the Slack app side of the setup is a separate step, and the gateway's
own README covers what the service is and how the two Request URLs fit together.

## What it creates

| | |
|---|---|
| Cloud Run | One service, shared by every operator. `min_instance_count = 0`, `invoker_iam_disabled = true` |
| Pub/Sub | Two topics and two subscriptions **per operator** — one pair for messages and reactions, one for button presses |
| Secret Manager | The registration table, mounted into the service as a file |
| IAM | The service publishes to every topic; each operator reads **only their own two subscriptions** |

No service-account keys are created, and no organisation policy is touched.

## Before you apply

**Check one organisation policy.** The public endpoint works through
`invoker_iam_disabled`, which is Google's documented answer when Domain
Restricted Sharing refuses an `allUsers` grant. An administrator can block that
too:

```bash
gcloud resource-manager org-policies describe \
  constraints/run.managed.requireInvokerIam --organization <ORG_ID>
```

It is not enforced by default. If it *is* enforced in your organisation, this
construction does not work — and an external load balancer does not rescue it,
because a serverless NEG still reaches Cloud Run unauthenticated.

## Decide where state goes, first

**`tofu init` with no backend writes state to a local file**, and that file
holds every operator's Slack signing secret in clear text. The module declares
no backend on purpose — it does not know your organisation's bucket — which
means the default applies unless you choose otherwise.

For anything but a throwaway experiment, put it in a bucket only the deployer
can read. Create `backend.tf` next to this file *before* the first `init`:

```hcl
terraform {
  backend "gcs" {
    bucket = "my-tofu-state"     # versioning on, uniform bucket-level access
    prefix = "slack-event-gateway"
  }
}
```

Moving state later works (`tofu init -migrate-state`), but the local copy has
already existed by then — with the secrets in it.

## Apply

```bash
cd services/slack-event-gateway/tofu
cp terraform.tfvars.example terraform.tfvars   # then fill it in
tofu init
tofu plan
tofu apply
```

Then read the outputs:

```bash
tofu output -json request_urls     # paste into BOTH Slack Request URL settings
tofu output -json totsuka_config   # the [slack.gateway] block for each operator
```

## Adding a person

One entry in `operators`, then `tofu apply`. Topics, subscriptions, IAM and the
registration table all follow. Nothing else has to be edited, and no existing
operator's resources are touched — the resources are keyed by `key`, not
indexed by position.

The apply also rolls a new Cloud Run revision, so the new table is live when it
finishes. That is why the mount pins the exact secret version rather than
`latest`: with `latest`, nothing about the service changes, no revision is
created, and running instances keep serving the old table until they happen to
be recycled.

## Destroy

```bash
tofu destroy
```

Works as-is: `deletion_protection` is off on the Cloud Run service. APIs
enabled by this module are left enabled — another workload in the project may
be using them.

**The registration table goes with it.** Old secret versions are *abandoned*
rather than deleted while the stack is alive — which is what makes a rollback
possible — but `destroy` removes the parent secret, and that takes every
version with it. Keep a copy of `terraform.tfvars` if you intend to rebuild.

## Two things this module will not do

**It does not restrict by source IP, and cannot.** Slack publishes no stable
list of addresses to allow. An allowlist built on guesses starts dropping
deliveries, and enough dropped deliveries is what makes Slack disable the
subscription — the exact failure the gateway exists to remove. The gates on that
leg are the unguessable path, the signature, and the five-minute timestamp
window; all three are inside the container.

**It does not enable VPC Service Controls.** If you want the *pull* side
restricted to company networks, a perimeter around Pub/Sub is the way to do it
— but it is an addition you make deliberately, not a default, because it also
stops the operator draining their queue from home or on a trip. That is
squarely the arrangement totsuka is built for, so turning it on by default
would break the common case to harden the uncommon one.

## Secrets and state

`terraform.tfvars` holds every operator's Slack signing secret, and **so does
the state file** — OpenTofu does not encrypt it. Both are gitignored, but that
only keeps them out of git; see "Decide where state goes" above for the part
that matters.

Treat read access to the state bucket as read access to every operator's Slack
app.
