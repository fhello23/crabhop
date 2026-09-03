#!/bin/sh
set -eu

base_url="${1:-http://localhost:8080}"
admin_user="${ADMIN_USERNAME:-admin}"
# Optional: name/ID of the running app container for direct-access checks.
# When set, the script execs into it and verifies the application boundary
# itself rejects management requests without the real proxy token.
app_container="${SMOKE_APP_CONTAINER:-}"

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
expect_status 401 --user "$admin_user:wrong-password" "$base_url/admin"
expect_status 401 --user "$admin_user:wrong-password" "$base_url/api/v1/links"
expect_status 200 --user "$admin_user:$ADMIN_PASSWORD" "$base_url/admin"
expect_status 200 --user "$admin_user:$ADMIN_PASSWORD" "$base_url/api/v1/links"

# A forged proxy-token header must not matter: Caddy's assignment replaces
# any client-supplied value after Basic Auth succeeds.
expect_status 200 \
  -H 'X-Crabhop-Proxy-Token: forged-token-0123456789abcdef' \
  --user "$admin_user:$ADMIN_PASSWORD" "$base_url/admin"
expect_status 200 \
  -H 'X-Crabhop-Proxy-Token: forged-token-0123456789abcdef' \
  --user "$admin_user:$ADMIN_PASSWORD" "$base_url/api/v1/links"

if [ -n "$app_container" ]; then
  # Bypass the proxy and talk to the app directly: without the real token
  # every management route must fail closed with 401.
  direct_status() {
    path="$1"
    shift
    # Extra wget options (e.g. --header) must precede the URL.
    docker exec "$app_container" wget -S -O /dev/null "$@" "http://127.0.0.1:3000$path" 2>&1 \
      | awk '/^  HTTP/{code=$2} END{print code}'
  }

  for path in /admin /api/v1/links; do
    code="$(direct_status "$path")"
    if [ "$code" != "401" ]; then
      echo "expected direct HTTP 401 for $path, received $code" >&2
      exit 1
    fi
    code="$(direct_status "$path" --header='X-Crabhop-Proxy-Token: forged-token-0123456789abcdef')"
    if [ "$code" != "401" ]; then
      echo "expected direct HTTP 401 for forged token on $path, received $code" >&2
      exit 1
    fi
  done
  # Public routes stay reachable directly (the boundary is management-only).
  code="$(direct_status /health/live)"
  if [ "$code" != "200" ]; then
    echo "expected direct HTTP 200 for /health/live, received $code" >&2
    exit 1
  fi
fi

echo "Caddy authentication smoke test passed for $base_url"
