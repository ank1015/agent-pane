#!/bin/sh
set -eu

cd /opt/execution-gateway
backup_file="$(mktemp /tmp/execution-gateway-backup.XXXXXX.sql.gz)"
trap 'rm -f "$backup_file"' EXIT

docker-compose exec -T postgres pg_dump \
  --username execution_gateway \
  --dbname execution_gateway \
  --format plain \
  --no-owner \
  --no-privileges | gzip -9 >"$backup_file"

object_name="execution-gateway-$(date -u +%Y%m%dT%H%M%SZ).sql.gz"
gcloud storage cp "$backup_file" "gs://${BACKUP_BUCKET}/${object_name}" --quiet
