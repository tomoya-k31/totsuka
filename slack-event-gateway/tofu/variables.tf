variable "project_id" {
  description = "GCP project the gateway and its queues live in. Nothing about this module is specific to any one project, so it is never hardcoded."
  type        = string
}

variable "region" {
  description = <<-EOT
    Cloud Run region. Tokyo (asia-northeast1) is a tier 1 region, which is what
    the cost estimate in ADR-0072 decision 10 assumes. Tier 2 regions
    (asia-east2, asia-northeast3, asia-southeast1, …) cost more per vCPU-second;
    changing this changes the bill, not just the latency.
  EOT
  type        = string
  default     = "asia-northeast1"
}

variable "service_name" {
  description = "Cloud Run service name. One service serves every operator; routing is by path, not by service (ADR-0072 decision 6). It also prefixes the service account id, which GCP caps at 30 characters."
  type        = string
  default     = "slack-event-gateway"

  validation {
    # The service account id is `<service_name>-sa`, and GCP requires 6-30
    # characters of this shape. Caught here rather than partway through an
    # apply that has already created topics.
    condition     = can(regex("^[a-z][a-z0-9-]{2,26}[a-z0-9]$", var.service_name))
    error_message = "service_name must be 4-28 characters of lowercase letters, digits and hyphens, starting with a letter: it prefixes the service account id, which GCP caps at 30."
  }
}

variable "image" {
  description = <<-EOT
    Container image. The default is the official image totsuka publishes on each
    release.

    **Pinned to an exact version on purpose.** There is no `:latest` tag,
    because a floating tag would let `tofu apply` silently change what is
    running — not a property to want for a service that holds Slack signing
    secrets.

    A company deployment usually wants this pointed at its own Artifact
    Registry instead; `slack-event-gateway/README.md` has the build and push
    commands.
  EOT
  type        = string
  default     = "ghcr.io/tomoya-k31/totsuka/slack-event-gateway:v0.7.5"
}

variable "max_instances" {
  description = <<-EOT
    Ceiling on concurrent Cloud Run instances.

    **This is a cost control, not a capacity one.** All three gates on the
    public leg — the opaque path, the signature, the timestamp window — are
    evaluated *inside* the container (ADR-0072 decision 11), so a request
    rejected for a bad signature is still a request that started an instance
    and is still billed, cold start included. With `invoker_iam_disabled` there
    is no IAM layer in front to absorb that.

    The default is sized for the design's own premise: a handful of operators,
    each in the channels one person can follow. A busy weekday for five
    operators is well under one sustained request per second, and Cloud Run's
    default concurrency is 80 requests per instance. Four leaves a wide margin
    for a burst and still bounds what an unauthenticated flood can cost.

    Raise it if deliveries are actually being shed — Cloud Run reports that —
    not pre-emptively.
  EOT
  type        = number
  default     = 4

  validation {
    condition     = var.max_instances >= 1 && var.max_instances <= 100
    error_message = "max_instances must be between 1 and 100. Leaving it at the platform default of 100 is what this variable exists to avoid."
  }
}

variable "events_retention" {
  description = <<-EOT
    How long the messages/reactions subscription keeps an unacknowledged
    record. Pub/Sub allows 10 minutes to 31 days.

    Seven days against a default ingest window of 24 hours (`drain_max_age_hours`)
    is deliberate slack for a long absence. Longer would only hold records
    totsuka has already decided not to file.
  EOT
  type        = string
  default     = "604800s" # 7 days
}

variable "block_actions_retention" {
  description = <<-EOT
    How long the button-press subscription keeps an unacknowledged record.

    **Must exceed the ~30 minutes a `response_url` stays usable.** At five
    minutes, coming back from a ten-minute outage would discard presses that
    are still answerable. Deciding a press has expired is the consumer's job;
    retention's only job is not losing one that has not.
  EOT
  type        = string
  default     = "2100s" # 35 minutes

  validation {
    condition     = tonumber(trimsuffix(var.block_actions_retention, "s")) >= 1800
    error_message = "block_actions_retention must be at least 1800s: a shorter window throws away presses whose response_url is still valid."
  }
}

variable "operators" {
  description = <<-EOT
    One entry per person using the gateway. **Adding a user is one entry here**
    — topics, subscriptions, IAM and the registration table all follow.

    - `key`               — short identifier used in resource names (`[a-z0-9-]`)
    - `slack_user_id`     — their Slack user id (`U…`)
    - `path_token`        — the unguessable path segment their Slack app posts
                            to. Generate it, do not invent it:
                            `openssl rand -hex 24`
    - `signing_secret`    — their Slack app's signing secret (Basic Information)
    - `google_principal`  — the identity that may pull *their* subscriptions,
                            e.g. `user:someone@example.com`. Each operator is
                            granted their own two subscriptions and nothing else

    **These values land in the OpenTofu state file, and this module offers no
    way around that** — it builds the registration table from them, which is
    exactly what makes "add a person, apply" true. State is not encrypted by
    the tooling, so keep it in a bucket only the deployer can read, and treat
    access to that bucket as access to every operator's Slack app.
  EOT
  type = list(object({
    key              = string
    slack_user_id    = string
    path_token       = string
    signing_secret   = string
    google_principal = string
  }))
  sensitive = true

  validation {
    condition     = length(var.operators) > 0
    error_message = "At least one operator is required: with none, every delivery is refused as an unknown path."
  }

  validation {
    condition     = length(distinct([for o in var.operators : o.key])) == length(var.operators)
    error_message = "Operator keys must be unique; they name the resources."
  }

  validation {
    condition     = length(distinct([for o in var.operators : o.path_token])) == length(var.operators)
    error_message = "Operator path_tokens must be unique: a duplicate means one of them never receives anything."
  }

  validation {
    condition     = alltrue([for o in var.operators : length(o.path_token) >= 32])
    error_message = "A path_token must be at least 32 characters. It is the routing credential on an endpoint with no IAM in front of it; generate one with `openssl rand -hex 24`."
  }
}

variable "enable_apis" {
  description = "Enable the Cloud Run, Pub/Sub and Secret Manager APIs. Turn this off if the project's APIs are managed elsewhere — enabling an already-enabled API is harmless, but some organisations manage this centrally and do not want it in an application module."
  type        = bool
  default     = true
}
