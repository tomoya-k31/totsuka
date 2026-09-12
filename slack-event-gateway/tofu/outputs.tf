output "service_url" {
  description = "The gateway's base URL. Each operator's Request URL is this plus `/slack/e/<their path_token>` — and BOTH Slack settings need it (Event Subscriptions and Interactivity & Shortcuts)."
  value       = google_cloud_run_v2_service.gateway.uri
}

output "request_urls" {
  description = <<-EOT
    Each operator's full Request URL, keyed by operator. Paste the value into
    **both** Slack app settings:

      Event Subscriptions      → Request URL
      Interactivity & Shortcuts → Request URL

    Setting only the first leaves mentions working and every approval button
    dead, which is a hard failure to diagnose from the symptom.
  EOT
  sensitive   = true # the path is a credential
  value = {
    for key in local.operator_keys :
    key => "${google_cloud_run_v2_service.gateway.uri}/slack/e/${local.operators[key].path_token}"
  }
}

output "totsuka_config" {
  description = <<-EOT
    The `[slack.gateway]` block each operator puts in their
    `~/.config/totsuka/config.toml`, alongside `event_source = "gateway"`.
  EOT
  value = {
    for key in local.operator_keys :
    key => {
      project                    = var.project_id
      subscription               = google_pubsub_subscription.events[key].name
      block_actions_subscription = google_pubsub_subscription.block_actions[key].name
    }
  }
}

output "service_account" {
  description = "The gateway's service account. It may publish to every topic and read the registration table; it can read no subscription, and it has no key."
  value       = google_service_account.gateway.email
}
