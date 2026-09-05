#!/bin/sh
set -eu

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends ca-certificates docker.io docker-compose
systemctl enable --now docker
install -d -m 0700 /opt/execution-gateway
