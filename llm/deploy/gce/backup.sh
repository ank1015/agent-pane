#!/bin/sh
set -eu

cd /opt/llm-gateway
backup_file="$(mktemp /tmp/llm-gateway-backup.XXXXXX.sql.gz)"
trap 'rm -f "$backup_file"' EXIT

docker-compose exec -T postgres pg_dump \
  --username llm_gateway \
  --dbname llm_gateway \
  --format plain \
  --no-owner \
  --no-privileges | gzip -9 >"$backup_file"

object_name="llm-gateway-$(date -u +%Y%m%dT%H%M%SZ).sql.gz"
gcloud storage cp "$backup_file" "gs://${BACKUP_BUCKET}/${object_name}" --quiet
