terraform {
  required_version = ">= 1.6"

  required_providers {
    # `google-beta`, for one field: `invoker_iam_disabled`. The provider's own
    # documented example for it uses google-beta, so this module follows the
    # example rather than a guess about whether the GA provider accepts it.
    #
    # Every other resource here (Pub/Sub, Secret Manager, IAM) behaves
    # identically on either provider, so using one provider for the whole
    # module is simpler than aliasing two.
    google-beta = {
      source  = "hashicorp/google-beta"
      version = ">= 6.0"
    }
  }
}

provider "google-beta" {
  project = var.project_id
  region  = var.region
}
