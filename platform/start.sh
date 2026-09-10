#!/usr/bin/env bash

set -Eeuo pipefail

platform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dashboard_dir="$platform_dir/apps/dashboard"
server_dir="$platform_dir/apps/server"
sites_dir="$platform_dir/apps/sites-service"
worker_dir="$platform_dir/apps/worker"

for command_name in cargo curl pnpm; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Required command not found: $command_name" >&2
    exit 1
  fi
done

if [[ ! -d "$dashboard_dir/node_modules" ]]; then
  echo "Dashboard dependencies are not installed. Run 'pnpm install' in $dashboard_dir first." >&2
  exit 1
fi

start_timeout="${PLATFORM_START_TIMEOUT_SECONDS:-300}"
if [[ ! "$start_timeout" =~ ^[0-9]+$ ]] || (( start_timeout == 0 )); then
  echo "PLATFORM_START_TIMEOUT_SECONDS must be a positive integer." >&2
  exit 1
fi

declare -a service_names=()
declare -a service_pids=()

# Give every background service its own process group so cleanup also reaches
# children such as the Rust binary launched by Cargo and Vite launched by pnpm.
set -m

start_service() {
  local name="$1"
  local directory="$2"
  shift 2

  echo "Starting $name..."
  (
    cd "$directory"
    exec "$@"
  ) &
  service_names+=("$name")
  service_pids+=("$!")
}

stop_services() {
  local index pid deadline any_running

  trap - EXIT INT TERM
  if (( ${#service_pids[@]} == 0 )); then
    return
  fi

  echo
  echo "Stopping Platform services..."
  for pid in "${service_pids[@]}"; do
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
  done

  deadline=$((SECONDS + 10))
  while (( SECONDS < deadline )); do
    any_running=0
    for pid in "${service_pids[@]}"; do
      if kill -0 -- "-$pid" 2>/dev/null; then
        any_running=1
        break
      fi
    done
    (( any_running == 0 )) && break
    sleep 0.1
  done

  for pid in "${service_pids[@]}"; do
    if kill -0 -- "-$pid" 2>/dev/null; then
      kill -KILL -- "-$pid" 2>/dev/null || true
    fi
  done
  for index in "${!service_pids[@]}"; do
    wait "${service_pids[$index]}" 2>/dev/null || true
  done
}

trap stop_services EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

dotenv_value() {
  local file="$1"
  local key="$2"
  local value=""

  if [[ -f "$file" ]]; then
    value="$(sed -n "s/^${key}=//p" "$file" | tail -n 1)"
    value="${value%$'\r'}"
    if [[ "$value" == \"*\" && "$value" == *\" ]]; then
      value="${value:1:${#value}-2}"
    elif [[ "$value" == \'*\' && "$value" == *\' ]]; then
      value="${value:1:${#value}-2}"
    fi
  fi
  printf '%s' "$value"
}

platform_url="${PLATFORM_URL:-$(dotenv_value "$worker_dir/.env" PLATFORM_URL)}"
platform_url="${platform_url:-http://127.0.0.1:3100}"

start_service "Platform API" "$server_dir" cargo run -p platform-server
server_pid="${service_pids[0]}"

echo "Waiting for Platform API at $platform_url..."
deadline=$((SECONDS + start_timeout))
until curl --silent --show-error --output /dev/null \
  --connect-timeout 1 --max-time 2 "${platform_url%/}/api/projects" 2>/dev/null; do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    set +e
    wait "$server_pid"
    exit_code=$?
    set -e
    echo "Platform API exited before it became ready (status $exit_code)." >&2
    (( exit_code == 0 )) && exit_code=1
    exit "$exit_code"
  fi
  if (( SECONDS >= deadline )); then
    echo "Timed out waiting for Platform API after ${start_timeout}s." >&2
    exit 1
  fi
  sleep 0.5
done

start_service "Sites service" "$sites_dir" cargo run -p platform-sites-service
start_service "worker" "$worker_dir" cargo run -p platform-worker
start_service "dashboard" "$dashboard_dir" pnpm dev

echo
echo "Platform is running. Press Ctrl-C to stop all services."

while true; do
  for index in "${!service_pids[@]}"; do
    pid="${service_pids[$index]}"
    if ! kill -0 "$pid" 2>/dev/null; then
      set +e
      wait "$pid"
      exit_code=$?
      set -e
      echo "${service_names[$index]} exited (status $exit_code); stopping the stack." >&2
      (( exit_code == 0 )) && exit_code=1
      exit "$exit_code"
    fi
  done
  sleep 1
done
