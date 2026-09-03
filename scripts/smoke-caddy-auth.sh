#!/bin/sh
set -eu

base_url="${1:-http://localhost:8080}"
admin_user="${ADMIN_USERNAME:-admin}"

if [ -z "${ADMIN_PASSWORD:-}" ]; then
  echo "ADMIN_PASSWORD is required for the authenticated checks" >&2
  exit 2
fi

expect_status() {
  expected="$1"
  shift
  actual="$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' "$@")"
  if [ "$actual" != "$expected" ]; then
    echo "expected HTTP $expected, received $actual" >&2
    exit 1
  fi
}

expect_status 200 "$base_url/health/live"
expect_status 401 "$base_url/admin"
expect_status 401 "$base_url/api/v1/links"
expect_status 200 --user "$admin_user:$ADMIN_PASSWORD" "$base_url/admin"
expect_status 200 --user "$admin_user:$ADMIN_PASSWORD" "$base_url/api/v1/links"

echo "Caddy authentication smoke test passed for $base_url"
