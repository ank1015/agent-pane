#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

server_pids=()
server_names=()
started_pid=""
llm_gateway_env="$repo_root/apps/llm-gateway/.env"
platform_env="$repo_root/apps/platform/.env"
execution_gateway_env="$repo_root/apps/execution-gateway/.env"
agent_env="$repo_root/apps/agent/.env"
machine_daemon_config="${MACHINE_DAEMON_CONFIG:-$repo_root/apps/machine-daemon/machine-daemon.local.json}"
machine_daemon_credential="$repo_root/.machine-daemon/cloud-credential.json"

for required_env in "$llm_gateway_env" "$platform_env" "$execution_gateway_env"; do
  if [[ ! -f "$required_env" ]]; then
    echo "Missing $required_env. Copy its .env.example and configure it." >&2
    exit 1
  fi
done

if [[ -f "$agent_env" ]]; then
  set -a
  source "$agent_env"
  set +a
fi

export AGENT_DATABASE_URL="${AGENT_DATABASE_URL:-postgresql://localhost/agent}"
export AGENT_CONTROL_TOKEN="${AGENT_CONTROL_TOKEN:-agent-dev-control-token-that-is-long-enough}"
export AGENT_WORKER_TOKEN="${AGENT_WORKER_TOKEN:-agent-dev-worker-token-that-is-long-enough}"
export AGENT_BIND_ADDRESS="${AGENT_BIND_ADDRESS:-127.0.0.1:8780}"

execution_gateway_api_token="$({
  set -a
  source "$execution_gateway_env"
  set +a
  printf '%s' "${EXECUTION_GATEWAY_API_TOKEN:-}"
})"
if [[ -z "$execution_gateway_api_token" ]]; then
  echo "EXECUTION_GATEWAY_API_TOKEN is missing from $execution_gateway_env." >&2
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

start_with_env() {
  local name="$1"
  local env_file="$2"
  local binary="$3"
  (
    set -a
    source "$env_file"
    set +a
    exec "$binary"
  ) &
  started_pid="$!"
  server_pids+=("$started_pid")
  server_names+=("$name")
}

start_server() {
  local name="$1"
  shift
  "$@" &
  started_pid="$!"
  server_pids+=("$started_pid")
  server_names+=("$name")
}

wait_for_service() {
  local name="$1"
  local url="$2"
  local pid="$3"
  local attempt
  for attempt in {1..80}; do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "$name stopped before becoming ready." >&2
      return 1
    fi
    if curl --silent --show-error --fail --max-time 1 "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "$name did not become ready at $url." >&2
  return 1
}

trap cleanup EXIT
trap 'exit 130' INT TERM

cargo build \
  -p llm-gateway \
  -p platform \
  -p execution-gateway \
  -p machine-daemon \
  -p agent \
  -p worker-pi

start_with_env "llm-gateway" "$llm_gateway_env" "$repo_root/target/debug/llm-gateway"
gateway_pid="$started_pid"

start_with_env "platform" "$platform_env" "$repo_root/target/debug/platform"
platform_pid="$started_pid"

start_with_env \
  "execution-gateway" \
  "$execution_gateway_env" \
  "$repo_root/target/debug/execution-gateway"
execution_gateway_pid="$started_pid"

start_server "agent" "$repo_root/target/debug/agent"
agent_pid="$started_pid"

wait_for_service "llm-gateway" "http://127.0.0.1:3000/ready" "$gateway_pid"
wait_for_service "platform" "http://127.0.0.1:3100/ready" "$platform_pid"
wait_for_service "execution-gateway" "http://127.0.0.1:8790/health" "$execution_gateway_pid"
wait_for_service "agent" "http://127.0.0.1:8780/ready" "$agent_pid"

machine_daemon_pid=""
if [[ -f "$machine_daemon_config" && -f "$machine_daemon_credential" ]]; then
  start_server \
    "machine-daemon" \
    "$repo_root/target/debug/machine-daemon" \
    --config "$machine_daemon_config" \
    connect
  machine_daemon_pid="$started_pid"
fi

export PI_WORKER_AGENT_URL="http://127.0.0.1:8780"
export PI_WORKER_AGENT_TOKEN="$AGENT_WORKER_TOKEN"
export PI_WORKER_AGENT_CONTROL_TOKEN="$AGENT_CONTROL_TOKEN"
export PI_WORKER_HARNESS_REVISION_IDS="pi-2026-08-26-machine-target"
export PI_WORKER_LLM_GATEWAY_URL="http://127.0.0.1:3000"
export PI_WORKER_EXECUTION_GATEWAY_URL="http://127.0.0.1:8790"
export PI_WORKER_EXECUTION_GATEWAY_TOKEN="$execution_gateway_api_token"
export RUST_LOG="worker_pi=info"
start_server "worker-pi" "$repo_root/target/debug/worker-pi"
worker_pi_pid="$started_pid"

sleep 0.25
if ! kill -0 "$worker_pi_pid" 2>/dev/null; then
  echo "worker-pi stopped during startup." >&2
  exit 1
fi

echo "llm-gateway:       http://127.0.0.1:3000 (pid $gateway_pid)"
echo "platform:          http://127.0.0.1:3100 (pid $platform_pid)"
echo "execution-gateway: http://127.0.0.1:8790 (pid $execution_gateway_pid)"
if [[ -n "$machine_daemon_pid" ]]; then
  echo "machine-daemon:    connected (pid $machine_daemon_pid)"
fi
echo "agent:             http://127.0.0.1:8780 (pid $agent_pid)"
echo "worker-pi:         polling Agent (pid $worker_pi_pid)"
echo "Pi harness:        pi / pi-2026-08-26-machine-target"
echo "oauth callback:    http://localhost:1455/auth/callback"
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
