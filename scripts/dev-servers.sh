#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

server_pids=()
server_names=()
execution_gateway_env="$repo_root/apps/execution-gateway/.env"

if [[ ! -f "$execution_gateway_env" ]]; then
  echo "Missing $execution_gateway_env. Copy apps/execution-gateway/.env.example and configure it." >&2
  exit 1
fi

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

cargo build -p llm-gateway -p platform -p execution-gateway

target/debug/llm-gateway &
gateway_pid=$!
server_pids+=("$gateway_pid")
server_names+=("llm-gateway")

target/debug/platform &
platform_pid=$!
server_pids+=("$platform_pid")
server_names+=("platform")

(
  set -a
  source "$execution_gateway_env"
  set +a
  exec target/debug/execution-gateway
) &
execution_gateway_pid=$!
server_pids+=("$execution_gateway_pid")
server_names+=("execution-gateway")

echo "llm-gateway: http://127.0.0.1:3000 (pid $gateway_pid)"
echo "platform:    http://127.0.0.1:3100 (pid $platform_pid)"
echo "execution:   http://127.0.0.1:8790 (pid $execution_gateway_pid)"
echo "oauth callback: http://localhost:1455/auth/callback"
echo "Press Ctrl-C to stop all servers."

while true; do
  for index in "${!server_pids[@]}"; do
    pid="${server_pids[$index]}"
    if ! kill -0 "$pid" 2>/dev/null; then
      set +e
      wait "$pid"
      exit_code=$?
      set -e
      echo "${server_names[$index]} stopped with status $exit_code"
      exit "$exit_code"
    fi
  done
  sleep 1
done
