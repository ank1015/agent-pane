#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

server_pids=()
server_names=()
started_pid=""
compose_file="$repo_root/apps/agent/compose.yaml"
llm_gateway_env="$repo_root/apps/llm-gateway/.env"
platform_env="$repo_root/apps/platform/.env"
execution_gateway_env="$repo_root/apps/execution-gateway/.env"
agent_env="$repo_root/apps/agent/.env"
pi_harness_env="$repo_root/apps/pi-harness/.env"
dashboard_dir="$repo_root/apps/dashboard"
machine_daemon_config="${MACHINE_DAEMON_CONFIG:-$repo_root/apps/machine-daemon/machine-daemon.local.json}"
use_docker_infrastructure="${DEV_SERVERS_USE_DOCKER_INFRASTRUCTURE:-1}"
keep_infrastructure="${DEV_SERVERS_KEEP_INFRASTRUCTURE:-0}"
start_dashboard="${DEV_SERVERS_START_DASHBOARD:-1}"
postgres_port="${DEV_SERVERS_POSTGRES_PORT:-55432}"
nats_port="${DEV_SERVERS_NATS_PORT:-4222}"
nats_monitor_port="${DEV_SERVERS_NATS_MONITOR_PORT:-8222}"
export DEV_SERVERS_POSTGRES_PORT="$postgres_port"
export DEV_SERVERS_NATS_PORT="$nats_port"
export DEV_SERVERS_NATS_MONITOR_PORT="$nats_monitor_port"
postgres_started_here=0
nats_started_here=0

for required_env in "$llm_gateway_env" "$platform_env" "$execution_gateway_env"; do
  if [[ ! -f "$required_env" ]]; then
    echo "Missing $required_env. Copy its .env.example and configure it." >&2
    exit 1
  fi
done

require_command() {
  local command_name="$1"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Required command '$command_name' was not found." >&2
    exit 1
  fi
}

require_port_free() {
  local name="$1"
  local port="$2"
  if port_is_open "$port"; then
    echo "$name cannot start because TCP port $port is already in use." >&2
    echo "Stop the existing stack before running dev-servers.sh again." >&2
    exit 1
  fi
}

process_alive() {
  local pid="$1"
  local state
  if ! kill -0 "$pid" 2>/dev/null; then
    return 1
  fi
  state="$(ps -o state= -p "$pid" 2>/dev/null | tr -d '[:space:]')"
  [[ -n "$state" && "$state" != "Z" ]]
}

env_value() {
  local env_file="$1"
  local variable_name="$2"
  if [[ ! -f "$env_file" ]]; then
    return 0
  fi
  (
    set -a
    source "$env_file"
    set +a
    printf '%s' "${!variable_name:-}"
  )
}

require_value() {
  local variable_name="$1"
  local value="$2"
  local env_file="$3"
  if [[ -z "$value" ]]; then
    echo "$variable_name is missing from $env_file." >&2
    exit 1
  fi
}

compose() {
  docker compose -f "$compose_file" "$@"
}

compose_service_running() {
  local service="$1"
  local container_id
  container_id="$(compose ps -q "$service")"
  if [[ -z "$container_id" ]]; then
    return 1
  fi
  [[ "$(docker inspect --format '{{.State.Running}}' "$container_id")" == "true" ]]
}

compose_host_port() {
  local service="$1"
  local container_port="$2"
  local address
  address="$(compose port "$service" "$container_port")"
  printf '%s' "${address##*:}"
}

wait_for_postgres() {
  local attempt
  local consecutive=0
  for attempt in {1..120}; do
    if [[ "$(compose exec -T postgres psql -U postgres -d postgres -tAc 'select 1' 2>/dev/null)" == "1" ]]; then
      consecutive="$((consecutive + 1))"
      if [[ "$consecutive" -ge 4 ]]; then
        return 0
      fi
    else
      consecutive=0
    fi
    sleep 0.25
  done
  echo "PostgreSQL did not become ready." >&2
  return 1
}

wait_for_url() {
  local name="$1"
  local url="$2"
  local attempt
  for attempt in {1..120}; do
    if curl --silent --show-error --fail --max-time 1 "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "$name did not become ready at $url." >&2
  return 1
}

port_is_open() {
  local port="$1"
  (echo >/dev/tcp/127.0.0.1/"$port") >/dev/null 2>&1
}

next_available_port() {
  local port="$1"
  while port_is_open "$port"; do
    port="$((port + 1))"
  done
  printf '%s' "$port"
}

ensure_database() {
  local database="$1"
  local exists
  exists="$(compose exec -T postgres psql -U postgres -d postgres -tAc \
    "select 1 from pg_database where datname = '$database'")"
  if [[ "$exists" != "1" ]]; then
    compose exec -T postgres createdb -U postgres "$database"
  fi
}

cleanup() {
  trap - EXIT INT TERM

  local pid
  if [[ "${#server_pids[@]}" -gt 0 ]]; then
    for pid in "${server_pids[@]}"; do
      if process_alive "$pid"; then
        kill -INT "$pid" 2>/dev/null || true
      fi
    done
    local attempt
    local any_running
    for attempt in {1..50}; do
      any_running=0
      for pid in "${server_pids[@]}"; do
        if process_alive "$pid"; then
          any_running=1
        fi
      done
      if [[ "$any_running" == "0" ]]; then
        break
      fi
      sleep 0.1
    done
    for pid in "${server_pids[@]}"; do
      if process_alive "$pid"; then
        kill -TERM "$pid" 2>/dev/null || true
      fi
    done
    for pid in "${server_pids[@]}"; do
      wait "$pid" 2>/dev/null || true
    done
  fi

  if [[ "$use_docker_infrastructure" == "1" && "$keep_infrastructure" != "1" ]]; then
    if [[ "$nats_started_here" == "1" ]]; then
      compose stop nats >/dev/null 2>&1 || true
    fi
    if [[ "$postgres_started_here" == "1" ]]; then
      compose stop postgres >/dev/null 2>&1 || true
    fi
  fi
}

start_with_env() {
  local name="$1"
  local env_file="$2"
  local binary="$3"
  shift 3
  (
    if [[ -f "$env_file" ]]; then
      set -a
      source "$env_file"
      set +a
    fi
    while [[ "$#" -gt 0 ]]; do
      export "$1"
      shift
    done
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
  for attempt in {1..120}; do
    if ! process_alive "$pid"; then
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

require_command cargo
require_command curl

require_port_free llm-gateway 3000
require_port_free platform 3100
require_port_free platform-oauth-callback 1455
require_port_free agent 8780
require_port_free execution-gateway 8790
if [[ "$start_dashboard" == "1" ]]; then
  require_port_free dashboard 5173
fi

if [[ "$use_docker_infrastructure" == "1" ]]; then
  require_command docker
  if ! docker info >/dev/null 2>&1; then
    echo "Docker is not running." >&2
    exit 1
  fi
  if compose_service_running postgres; then
    postgres_port="$(compose_host_port postgres 5432)"
    export DEV_SERVERS_POSTGRES_PORT="$postgres_port"
  else
    postgres_started_here=1
    postgres_port="$(next_available_port "$postgres_port")"
    export DEV_SERVERS_POSTGRES_PORT="$postgres_port"
  fi
  if compose_service_running nats; then
    nats_port="$(compose_host_port nats 4222)"
    nats_monitor_port="$(compose_host_port nats 8222)"
    export DEV_SERVERS_NATS_PORT="$nats_port"
    export DEV_SERVERS_NATS_MONITOR_PORT="$nats_monitor_port"
  else
    nats_started_here=1
    nats_port="$(next_available_port "$nats_port")"
    nats_monitor_port="$(next_available_port "$nats_monitor_port")"
    export DEV_SERVERS_NATS_PORT="$nats_port"
    export DEV_SERVERS_NATS_MONITOR_PORT="$nats_monitor_port"
  fi
  compose up -d postgres nats
  wait_for_postgres
  wait_for_url "NATS" "http://127.0.0.1:$nats_monitor_port/healthz"
  ensure_database agent
  ensure_database llm_gateway
  ensure_database execution_gateway
  agent_database_url="postgres://postgres:postgres@127.0.0.1:$postgres_port/agent"
  llm_gateway_database_url="postgres://postgres:postgres@127.0.0.1:$postgres_port/llm_gateway"
  execution_gateway_database_url="postgres://postgres:postgres@127.0.0.1:$postgres_port/execution_gateway"
  nats_url="nats://127.0.0.1:$nats_port"
else
  agent_database_url="${AGENT_DATABASE_URL:-$(env_value "$agent_env" AGENT_DATABASE_URL)}"
  agent_database_url="${agent_database_url:-postgresql://localhost/agent}"
  llm_gateway_database_url="${DATABASE_URL:-$(env_value "$llm_gateway_env" DATABASE_URL)}"
  execution_gateway_database_url="${EXECUTION_GATEWAY_DATABASE_URL:-$(env_value "$execution_gateway_env" EXECUTION_GATEWAY_DATABASE_URL)}"
  nats_url="${AGENT_NATS_URL:-$(env_value "$agent_env" AGENT_NATS_URL)}"
  nats_url="${nats_url:-nats://127.0.0.1:4222}"
fi

agent_control_token="${AGENT_CONTROL_TOKEN:-$(env_value "$agent_env" AGENT_CONTROL_TOKEN)}"
agent_control_token="${agent_control_token:-agent-dev-control-token-that-is-long-enough}"
agent_harness_token="${AGENT_HARNESS_TOKEN:-$(env_value "$agent_env" AGENT_HARNESS_TOKEN)}"
agent_harness_token="${agent_harness_token:-agent-dev-harness-token-that-is-long-enough}"
llm_gateway_admin_token="${GATEWAY_ADMIN_TOKEN:-$(env_value "$llm_gateway_env" GATEWAY_ADMIN_TOKEN)}"
execution_gateway_api_token="${EXECUTION_GATEWAY_API_TOKEN:-$(env_value "$execution_gateway_env" EXECUTION_GATEWAY_API_TOKEN)}"
execution_gateway_admin_token="${EXECUTION_GATEWAY_ADMIN_TOKEN:-$(env_value "$execution_gateway_env" EXECUTION_GATEWAY_ADMIN_TOKEN)}"
execution_gateway_control_token="${EXECUTION_GATEWAY_CONTROL_TOKEN:-$(env_value "$execution_gateway_env" EXECUTION_GATEWAY_CONTROL_TOKEN)}"

require_value GATEWAY_ADMIN_TOKEN "$llm_gateway_admin_token" "$llm_gateway_env"
require_value EXECUTION_GATEWAY_API_TOKEN "$execution_gateway_api_token" "$execution_gateway_env"
require_value EXECUTION_GATEWAY_ADMIN_TOKEN "$execution_gateway_admin_token" "$execution_gateway_env"
require_value EXECUTION_GATEWAY_CONTROL_TOKEN "$execution_gateway_control_token" "$execution_gateway_env"

echo "Building Rust services..."
cargo build \
  -p llm-gateway \
  -p platform \
  -p execution-gateway \
  -p machine-daemon \
  -p agent \
  -p pi-harness

if [[ "$start_dashboard" == "1" ]]; then
  require_command pnpm
  if [[ ! -x "$dashboard_dir/node_modules/.bin/vite" ]]; then
    pnpm --dir "$dashboard_dir" install --frozen-lockfile
  fi
fi

start_with_env \
  "llm-gateway" \
  "$llm_gateway_env" \
  "$repo_root/target/debug/llm-gateway" \
  "DATABASE_URL=$llm_gateway_database_url"
llm_gateway_pid="$started_pid"

start_with_env \
  "execution-gateway" \
  "$execution_gateway_env" \
  "$repo_root/target/debug/execution-gateway" \
  "EXECUTION_GATEWAY_DATABASE_URL=$execution_gateway_database_url"
execution_gateway_pid="$started_pid"

start_with_env \
  "agent" \
  "$agent_env" \
  "$repo_root/target/debug/agent" \
  "AGENT_DATABASE_URL=$agent_database_url" \
  "AGENT_CONTROL_TOKEN=$agent_control_token" \
  "AGENT_HARNESS_TOKEN=$agent_harness_token" \
  "AGENT_NATS_URL=$nats_url" \
  "AGENT_BIND_ADDRESS=127.0.0.1:8780" \
  "RUST_LOG=agent=info"
agent_pid="$started_pid"

wait_for_service "llm-gateway" "http://127.0.0.1:3000/ready" "$llm_gateway_pid"
wait_for_service "execution-gateway" "http://127.0.0.1:8790/health" "$execution_gateway_pid"
wait_for_service "agent" "http://127.0.0.1:8780/ready" "$agent_pid"

start_with_env \
  "platform" \
  "$platform_env" \
  "$repo_root/target/debug/platform" \
  "PLATFORM_LLM_GATEWAY_ADMIN_TOKEN=$llm_gateway_admin_token" \
  "PLATFORM_EXECUTION_GATEWAY_CONTROL_TOKEN=$execution_gateway_control_token" \
  "PLATFORM_AGENT_CONTROL_TOKEN=$agent_control_token"
platform_pid="$started_pid"
wait_for_service "platform" "http://127.0.0.1:3100/ready" "$platform_pid"

machine_daemon_pid=""
if [[ -f "$machine_daemon_config" ]]; then
  require_command jq
  registration_token="$(
    curl --silent --show-error --fail \
      --request POST \
      --header "Authorization: Bearer $execution_gateway_admin_token" \
      --header "Content-Type: application/json" \
      --data '{"label":"dev-servers local machine"}' \
      http://127.0.0.1:8790/v1/admin/machine-registrations |
      jq --exit-status --raw-output '.registration_token'
  )"
  MACHINE_DAEMON_REGISTRATION_TOKEN="$registration_token" \
    "$repo_root/target/debug/machine-daemon" \
    --config "$machine_daemon_config" \
    register \
    --gateway http://127.0.0.1:8790
  start_server \
    "machine-daemon" \
    "$repo_root/target/debug/machine-daemon" \
    --config "$machine_daemon_config" \
    connect
  machine_daemon_pid="$started_pid"
fi

start_with_env \
  "pi-harness" \
  "$pi_harness_env" \
  "$repo_root/target/debug/pi-harness" \
  "PI_HARNESS_AGENT_URL=http://127.0.0.1:8780" \
  "PI_HARNESS_AGENT_TOKEN=$agent_harness_token" \
  "PI_HARNESS_AGENT_CONTROL_TOKEN=$agent_control_token" \
  "PI_HARNESS_NATS_URL=$nats_url" \
  "PI_HARNESS_LLM_GATEWAY_URL=http://127.0.0.1:3000" \
  "PI_HARNESS_EXECUTION_GATEWAY_URL=http://127.0.0.1:8790" \
  "PI_HARNESS_EXECUTION_GATEWAY_TOKEN=$execution_gateway_api_token" \
  "RUST_LOG=pi_harness=info"
pi_harness_pid="$started_pid"

sleep 0.25
if ! kill -0 "$pi_harness_pid" 2>/dev/null; then
  echo "pi-harness stopped during startup." >&2
  exit 1
fi

dashboard_pid=""
if [[ "$start_dashboard" == "1" ]]; then
  start_server \
    "dashboard" \
    node \
    "$dashboard_dir/node_modules/vite/bin/vite.js" \
    "$dashboard_dir" \
    --host 127.0.0.1 \
    --port 5173 \
    --strictPort
  dashboard_pid="$started_pid"
  wait_for_service "dashboard" "http://127.0.0.1:5173" "$dashboard_pid"
fi

echo
if [[ "$use_docker_infrastructure" == "1" ]]; then
  echo "postgres:          postgresql://127.0.0.1:$postgres_port (Docker)"
  echo "nats:              nats://127.0.0.1:$nats_port (monitor http://127.0.0.1:$nats_monitor_port)"
fi
echo "llm-gateway:       http://127.0.0.1:3000 (pid $llm_gateway_pid)"
echo "platform:          http://127.0.0.1:3100 (pid $platform_pid)"
echo "execution-gateway: http://127.0.0.1:8790 (pid $execution_gateway_pid)"
echo "agent:             http://127.0.0.1:8780 (pid $agent_pid)"
echo "pi-harness:        NATS server, 20 concurrent turns (pid $pi_harness_pid)"
if [[ -n "$machine_daemon_pid" ]]; then
  echo "machine-daemon:    connected (pid $machine_daemon_pid)"
else
  echo "machine-daemon:    skipped (local config missing)"
fi
if [[ -n "$dashboard_pid" ]]; then
  echo "dashboard:         http://127.0.0.1:5173 (pid $dashboard_pid)"
fi
echo "Pi revision:       pi / pi-2026-08-27-nats-server"
echo "oauth callback:    http://localhost:1455/auth/callback"
echo "Press Ctrl-C to stop the stack. Docker data volumes are preserved."

while true; do
  for index in "${!server_pids[@]}"; do
    pid="${server_pids[$index]}"
    if ! process_alive "$pid"; then
      set +e
      wait "$pid"
      exit_code="$?"
      set -e
      echo "${server_names[$index]} stopped with status $exit_code"
      exit "$exit_code"
    fi
  done
  sleep 1
done
