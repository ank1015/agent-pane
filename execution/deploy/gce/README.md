# Single-VM GCE deployment

This deployment runs three containers on one private Docker network:

- Caddy exposes only TCP 80 and TCP/UDP 443 and obtains a Let's Encrypt
  certificate for the gateway's public DNS hostname.
- `execution-gateway` accepts traffic only from Caddy.
- PostgreSQL accepts traffic only from the gateway and persists to a Docker
  volume on the VM disk.

The VM environment file is root-readable (`0600`). Its API token, credential
vault key, and PostgreSQL password originate in Google Secret Manager. The VM
service account receives read access only to those individual secrets and read
access to the gateway Artifact Registry repository.

The production gateway URL is `https://<gateway-hostname>/`. The hostname must
resolve directly to the VM's static IPv4 address while Caddy obtains its first
certificate. Health and readiness endpoints require the same bearer token as
the rest of the control API.

The initial deployment intentionally uses one gateway replica because active
Registered Host WebSockets live in gateway memory. PostgreSQL should be backed
up before upgrades. The included systemd timer creates a daily compressed
logical backup in a private regional Cloud Storage bucket. Moving PostgreSQL to
managed Cloud SQL can be done independently later if recovery requirements
grow.
