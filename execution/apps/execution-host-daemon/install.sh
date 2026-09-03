#!/bin/sh
set -eu

version="${EXECUTION_HOST_VERSION:-0.1.0-dev-96a40a15}"
base_url="${EXECUTION_HOST_ARTIFACT_BASE_URL:-https://downloads.acentric.dev}"
install_directory="${EXECUTION_HOST_INSTALL_DIRECTORY:-/usr/local/bin}"

case "$(uname -s):$(uname -m)" in
  Linux:x86_64 | Linux:amd64) target=x86_64-unknown-linux-musl ;;
  Linux:aarch64 | Linux:arm64) target=aarch64-unknown-linux-musl ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  Darwin:arm64 | Darwin:aarch64) target=aarch64-apple-darwin ;;
  *)
    echo "unsupported operating system or architecture: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

temporary_directory="$(mktemp -d)"
trap 'rm -rf "$temporary_directory"' EXIT HUP INT TERM
artifact_url="${base_url}/execution-host/${version}/${target}/execution-host"

curl --fail --location --silent --show-error \
  "$artifact_url" --output "$temporary_directory/execution-host"
curl --fail --location --silent --show-error \
  "${artifact_url}.sha256" --output "$temporary_directory/execution-host.sha256"

expected="$(awk '{print $1}' "$temporary_directory/execution-host.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$temporary_directory/execution-host" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$temporary_directory/execution-host" | awk '{print $1}')"
fi
if [ "$actual" != "$expected" ]; then
  echo "execution-host checksum verification failed" >&2
  exit 1
fi

if [ -d "$install_directory" ] && [ -w "$install_directory" ]; then
  install -m 0755 "$temporary_directory/execution-host" "$install_directory/execution-host"
else
  sudo install -d -m 0755 "$install_directory"
  sudo install -m 0755 "$temporary_directory/execution-host" "$install_directory/execution-host"
fi

"$install_directory/execution-host" --version
