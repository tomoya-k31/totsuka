# The Event Gateway's infrastructure (#661, ADR-0072 decisions 6, 9, 10, 11).
#
# `tofu apply` stands up: one Cloud Run service, two Pub/Sub topics and two
# subscriptions per operator, the registration table in Secret Manager, and the
# IAM that keeps each operator to their own queues.
#
# What this module deliberately does NOT do:
#
#   - **It does not touch organisation policy.** Domain Restricted Sharing
#     stays on. The public leg works through `invoker_iam_disabled`, which is
#     Google's documented answer for exactly this case, rather than through an
#     `allUsers` grant that DRS would refuse anyway.
#   - **It creates no service-account keys.** The gateway runs as a service
#     account whose token comes from the metadata server; each operator pulls
#     with their own Google identity.
#   - **It hardcodes no project, no Slack app and no registration content.**
#     All of it arrives as variables.

locals {
  # Keyed map, so adding an operator to `var.operators` adds resources rather
  # than renumbering (and therefore recreating) everyone else's.
  operators = { for o in var.operators : o.key => o }

  # `var.operators` is `sensitive`, and OpenTofu refuses a sensitive value as a
  # `for_each` argument — a resource instance key ends up in plan output and in
  # state addresses. The *keys* are not the secret part (they are names the
  # deployer chose), so they are unwrapped explicitly and everything iterates
  # over them, reading the sensitive object by key inside the resource where
  # sensitivity is handled properly.
  operator_keys = nonsensitive(toset([for o in var.operators : o.key]))

  # The registration table the gateway reads. Built here so that adding an
  # operator is one entry in one variable — the alternative is editing this
  # JSON by hand in Secret Manager and keeping it in step with the topics,
  # which is two places to be wrong.
  registrations = jsonencode({
    users = [
      for key in local.operator_keys : {
        path_token          = local.operators[key].path_token
        slack_user_id       = local.operators[key].slack_user_id
        signing_secret      = local.operators[key].signing_secret
        topic               = google_pubsub_topic.events[key].id
        block_actions_topic = google_pubsub_topic.block_actions[key].id
      }
    ]
  })

  services = var.enable_apis ? toset([
    "run.googleapis.com",
    "pubsub.googleapis.com",
    "secretmanager.googleapis.com",
  ]) : toset([])
}

resource "google_project_service" "required" {
  provider = google-beta
  for_each = local.services

  project = var.project_id
  service = each.value

  # Leaving an API enabled on destroy is the safer default: another workload in
  # the project may be using it, and disabling Pub/Sub under someone else is a
  # much worse outcome than an API left on.
  disable_on_destroy = false
}

# ---- queues ---------------------------------------------------------------
# Per operator, not per workspace. Pub/Sub charges for neither topics nor
# subscriptions, so separating them costs nothing and buys the thing that
# matters: IAM can then say "this person may read their own queue and no
# other" (decision 6). One topic plus subscription filters would be worse on
# both counts — filtered-out messages are still billed as deliveries.

resource "google_pubsub_topic" "events" {
  provider = google-beta
  for_each = local.operator_keys

  project = var.project_id
  name    = "${var.service_name}-${each.key}-events"

  depends_on = [google_project_service.required]
}

resource "google_pubsub_topic" "block_actions" {
  provider = google-beta
  for_each = local.operator_keys

  project = var.project_id
  name    = "${var.service_name}-${each.key}-block-actions"

  depends_on = [google_project_service.required]
}

resource "google_pubsub_subscription" "events" {
  provider = google-beta
  for_each = local.operator_keys

  project = var.project_id
  name    = "${var.service_name}-${each.key}-events"
  topic   = google_pubsub_topic.events[each.key].id

  message_retention_duration = var.events_retention
  # Redeliver an unacknowledged record rather than dropping it. totsuka
  # de-duplicates on the delivery identity the contract defines, so a repeat is
  # cheap; a loss is not.
  retain_acked_messages = false
  ack_deadline_seconds  = 60

  # No expiration. The default expires a subscription after 31 days without a
  # pull, which would quietly delete the queue of anyone on a long leave — and
  # the symptom on their return is "totsuka receives nothing", with no error to
  # read anywhere.
  expiration_policy {
    ttl = ""
  }
}

resource "google_pubsub_subscription" "block_actions" {
  provider = google-beta
  for_each = local.operator_keys

  project = var.project_id
  name    = "${var.service_name}-${each.key}-block-actions"
  topic   = google_pubsub_topic.block_actions[each.key].id

  message_retention_duration = var.block_actions_retention
  retain_acked_messages      = false
  ack_deadline_seconds       = 60

  expiration_policy {
    ttl = ""
  }
}

# ---- the registration table ----------------------------------------------

resource "google_secret_manager_secret" "registrations" {
  provider = google-beta

  project   = var.project_id
  secret_id = "${var.service_name}-registrations"

  replication {
    auto {}
  }

  depends_on = [google_project_service.required]
}

resource "google_secret_manager_secret_version" "registrations" {
  provider = google-beta

  secret      = google_secret_manager_secret.registrations.id
  secret_data = local.registrations

  # Keep the previous version around: a bad table takes the gateway down for
  # everyone, and rolling back by pointing the service at version N-1 is faster
  # than reconstructing it. Secret Manager bills per *version* beyond six free,
  # so this is not free forever — prune old ones when they pile up.
  deletion_policy = "DISABLE"
}

# ---- the service ----------------------------------------------------------

resource "google_service_account" "gateway" {
  provider = google-beta

  project      = var.project_id
  account_id   = "${var.service_name}-sa"
  display_name = "Slack Event Gateway"
  description  = "Publishes Slack event coordinates. Never reads a subscription; never holds a key."
}

# Publish to every operator's topics, read the registration table. Nothing
# else — in particular, no subscriber role: this service never pulls.
resource "google_pubsub_topic_iam_member" "gateway_publishes_events" {
  provider = google-beta
  for_each = local.operator_keys

  project = var.project_id
  topic   = google_pubsub_topic.events[each.key].name
  role    = "roles/pubsub.publisher"
  member  = "serviceAccount:${google_service_account.gateway.email}"
}

resource "google_pubsub_topic_iam_member" "gateway_publishes_block_actions" {
  provider = google-beta
  for_each = local.operator_keys

  project = var.project_id
  topic   = google_pubsub_topic.block_actions[each.key].name
  role    = "roles/pubsub.publisher"
  member  = "serviceAccount:${google_service_account.gateway.email}"
}

resource "google_secret_manager_secret_iam_member" "gateway_reads_registrations" {
  provider = google-beta

  project   = var.project_id
  secret_id = google_secret_manager_secret.registrations.secret_id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.gateway.email}"
}

resource "google_cloud_run_v2_service" "gateway" {
  provider = google-beta

  project  = var.project_id
  name     = var.service_name
  location = var.region

  # `tofu destroy` has to work — it is one of this module's acceptance
  # criteria, and an operator trying the design out must be able to take it
  # down again. The provider defaults this to true, which makes destroy fail
  # with a message about a setting the operator never chose.
  deletion_protection = false

  # Slack cannot be an IAM principal, so the endpoint has to answer
  # unauthenticated callers. `allUsers` + `roles/run.invoker` is the usual way
  # and **Domain Restricted Sharing refuses it** — so this uses Google's
  # documented alternative instead, which leaves the organisation policy alone.
  #
  # An administrator can still block this with
  # `constraints/run.managed.requireInvokerIam`. It is not enforced by default,
  # but check before deploying into an organisation you do not administer:
  #
  #   gcloud resource-manager org-policies describe \
  #     constraints/run.managed.requireInvokerIam --organization <ORG_ID>
  #
  # If it is enforced, this construction does not work and an external load
  # balancer does not help — a serverless NEG still arrives unauthenticated.
  invoker_iam_disabled = true

  ingress = "INGRESS_TRAFFIC_ALL"

  template {
    service_account = google_service_account.gateway.email

    scaling {
      # **The cost premise.** At 1 the service is billed for sitting idle and
      # the monthly figure moves by an order of magnitude (ADR-0072 decision
      # 10). Scale-to-zero is not a tuning choice here; it is why this design
      # costs about a dollar a month.
      #
      # It is also not a latency problem worth trading away: a cold start
      # delays a delivery by a second or so, and Slack's acknowledgement window
      # is three.
      min_instance_count = 0
      max_instance_count = var.max_instances
    }

    containers {
      image = var.image

      resources {
        limits = {
          cpu    = "1"
          memory = "512Mi"
        }
        # CPU only while a request is in flight. With `min_instance_count = 0`
        # there is nothing to do between requests, and paying for an idle vCPU
        # is the same mistake as pinning an instance.
        cpu_idle = true
      }

      # The table is mounted rather than passed as an environment variable: an
      # environment variable is visible in the revision's configuration to
      # anyone who can read it in the console, and this one holds every
      # operator's signing secret.
      env {
        name  = "REGISTRATIONS_PATH"
        value = "/etc/gateway/registrations.json"
      }

      volume_mounts {
        name       = "registrations"
        mount_path = "/etc/gateway"
      }
    }

    volumes {
      name = "registrations"
      secret {
        secret = google_secret_manager_secret.registrations.secret_id
        items {
          # `latest` so a table change takes effect on the next revision
          # without editing this file. Pin a number here to roll back.
          version = "latest"
          path    = "registrations.json"
        }
      }
    }
  }

  depends_on = [
    google_secret_manager_secret_iam_member.gateway_reads_registrations,
    google_project_service.required,
  ]
}

# ---- who may read what ----------------------------------------------------
# The separation that makes one shared service acceptable: each operator is
# granted `roles/pubsub.subscriber` on **their own two subscriptions** and
# nothing else. Granting at the project level would let any of them read
# everyone's.
#
# No service-account keys are created. Each operator pulls with their own
# Google identity, through Application Default Credentials.

resource "google_pubsub_subscription_iam_member" "operator_reads_events" {
  provider = google-beta
  for_each = local.operator_keys

  project      = var.project_id
  subscription = google_pubsub_subscription.events[each.key].name
  role         = "roles/pubsub.subscriber"
  member       = local.operators[each.key].google_principal
}

resource "google_pubsub_subscription_iam_member" "operator_reads_block_actions" {
  provider = google-beta
  for_each = local.operator_keys

  project      = var.project_id
  subscription = google_pubsub_subscription.block_actions[each.key].name
  role         = "roles/pubsub.subscriber"
  member       = local.operators[each.key].google_principal
}
