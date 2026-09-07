#!/usr/bin/env bash
# Creates a dedicated PUBLIC image bucket. No lifecycle expiry: history uses URLs.
set -euo pipefail
project_id="${1:?Usage: provision-codex-image-bucket.sh PROJECT BUCKET [LOCATION]}"
bucket_name="${2:?Provide a globally unique bucket name}"
location="${3:-us-central1}"
gcloud storage buckets create "gs://${bucket_name}" \
  --project="${project_id}" --location="${location}" \
  --uniform-bucket-level-access --quiet
gcloud storage buckets update "gs://${bucket_name}" \
  --project="${project_id}" --no-public-access-prevention --quiet
gcloud storage buckets add-iam-policy-binding "gs://${bucket_name}" \
  --project="${project_id}" --member=allUsers --role=roles/storage.objectViewer --quiet
printf 'Set BASIC_CODEX_IMAGE_BUCKET=%s and BASIC_CODEX_TOOLS_ENABLED=true on the worker.\n' "${bucket_name}"
