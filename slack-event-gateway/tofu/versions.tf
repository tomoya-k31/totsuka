terraform {
  required_version = ">= 1.6"

  required_providers {
    # The GA provider. `invoker_iam_disabled` — the one field that made this
    # look beta-only — is listed in the GA argument reference with no Beta
    # note, and the upstream schema carries no `min_version: beta` for it. The
    # `provider = google-beta` in the registry's *example* for that field is
    # copied into the GA page unchanged; the argument list is the authority.
    #
    # Upper bound as well as lower: without one, `tofu init` can resolve a
    # different major version than CI validated against, and the first sign of
    # that is someone else's `tofu plan`.
    google = {
      source  = "hashicorp/google"
      version = "~> 6.0"
    }
  }
}

provider "google" {
  project = var.project_id
  region  = var.region
}
