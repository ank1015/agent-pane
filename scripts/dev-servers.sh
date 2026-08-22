#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

server_pids=()

cleanup() {
  trap - EXIT INT TERM

  for pid in "${server_pids[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      kill -INT "$pid" 2>/dev/null || true
    fi
  done

  for pid in "${server_pids[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
}

trap cleanup EXIT
trap 'exit 130' INT TERM

cargo build -p llm-gateway -p platform

target/debug/llm-gateway &
gateway_pid=$!
server_pids+=("$gateway_pid")

target/debug/platform &
platform_pid=$!
server_pids+=("$platform_pid")

echo "llm-gateway: http://127.0.0.1:3000 (pid $gateway_pid)"
echo "platform:    http://127.0.0.1:3100 (pid $platform_pid)"
echo "oauth callback: http://localhost:1455/auth/callback"
echo "Press Ctrl-C to stop both servers."

while kill -0 "$gateway_pid" 2>/dev/null && kill -0 "$platform_pid" 2>/dev/null; do
  sleep 1
done

set +e
if ! kill -0 "$gateway_pid" 2>/dev/null; then
  wait "$gateway_pid"
  exit_code=$?
  echo "llm-gateway stopped with status $exit_code"
else
  wait "$platform_pid"
  exit_code=$?
  echo "platform stopped with status $exit_code"
fi
set -e

exit "$exit_code"
