# Single-VM GCE deployment

This deployment runs Caddy, `llm-gateway`, and PostgreSQL on one standalone
`e2-small` VM. Only Caddy publishes ports. The gateway and database remain on
the private Docker network.

The production defaults are sized for this VM:

- 32 active provider requests and 64 FIFO waiters.
- A 120-second maximum queue wait.
- Five gateway database connections.
- A 64 MiB runtime request-body limit, matching the gateway's local default.
- A 20 GB standard persistent boot disk.

Runtime and admin API tokens, the credential vault key, and the PostgreSQL
password live in Secret Manager. The VM service account can access only those
four secrets, pull from the LLM Artifact Registry repository, and write
database backups to the dedicated bucket.

The VPC admits public traffic only on TCP 80, TCP 443, and UDP 443. SSH is
available only through Identity-Aware Proxy. Every gateway route, including
health checks and unmatched routes, still requires the appropriate bearer
token. Container logs rotate at 10 MiB with three files retained per service.

PostgreSQL is backed up daily to a private regional bucket. Backups expire
after 30 days.

`configure.sh` is executed on the VM after the image is published. It retrieves
secret values through the VM service account, writes a root-only environment
file, starts the Compose stack, and enables the backup timer.
