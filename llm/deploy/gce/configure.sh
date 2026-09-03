#!/bin/sh
set -eu

: "${GCP_PROJECT_ID:?GCP_PROJECT_ID is required}"
: "${IMAGE_TAG:?IMAGE_TAG is required}"
: "${GATEWAY_PUBLIC_HOST:?GATEWAY_PUBLIC_HOST is required}"
: "${ACME_EMAIL:?ACME_EMAIL is required}"
: "${BACKUP_BUCKET:?BACKUP_BUCKET is required}"

deploy_source="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
install -d -m 0700 /opt/llm-gateway
install -m 0600 "$deploy_source/compose.yaml" /opt/llm-gateway/compose.yaml
install -m 0600 "$deploy_source/Caddyfile" /opt/llm-gateway/Caddyfile
install -m 0700 "$deploy_source/backup.sh" /opt/llm-gateway/backup.sh
install -m 0644 "$deploy_source/llm-gateway-backup.service" /etc/systemd/system/llm-gateway-backup.service
install -m 0644 "$deploy_source/llm-gateway-backup.timer" /etc/systemd/system/llm-gateway-backup.timer

postgres_password="$(gcloud secrets versions access latest --project="$GCP_PROJECT_ID" --secret=llm-gateway-postgres-password)"
vault_key="$(gcloud secrets versions access latest --project="$GCP_PROJECT_ID" --secret=llm-gateway-vault-key)"
api_token="$(gcloud secrets versions access latest --project="$GCP_PROJECT_ID" --secret=llm-gateway-api-token)"
admin_token="$(gcloud secrets versions access latest --project="$GCP_PROJECT_ID" --secret=llm-gateway-admin-token)"

umask 077
{
  printf 'POSTGRES_PASSWORD=%s\n' "$postgres_password"
  printf 'LLM_GATEWAY_VAULT_KEY=%s\n' "$vault_key"
  printf 'LLM_GATEWAY_API_TOKEN=%s\n' "$api_token"
  printf 'LLM_GATEWAY_ADMIN_TOKEN=%s\n' "$admin_token"
  printf 'LLM_GATEWAY_IMAGE=asia-south1-docker.pkg.dev/%s/llm/llm-gateway:%s\n' "$GCP_PROJECT_ID" "$IMAGE_TAG"
  printf 'GATEWAY_PUBLIC_HOST=%s\n' "$GATEWAY_PUBLIC_HOST"
  printf 'ACME_EMAIL=%s\n' "$ACME_EMAIL"
} >/opt/llm-gateway/.env
printf 'BACKUP_BUCKET=%s\n' "$BACKUP_BUCKET" >/opt/llm-gateway/backup.env

gcloud auth configure-docker asia-south1-docker.pkg.dev --quiet
cd /opt/llm-gateway
docker-compose pull
docker-compose up -d --remove-orphans

systemctl daemon-reload
systemctl enable --now llm-gateway-backup.timer
