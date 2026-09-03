# Crabhop

Single-administrator link shortener in Rust (Axum + SQLite), behind Caddy.

Features:

- Public redirects with correct status codes and cache headers.
- Browser admin UI for creating and managing links.
- Protected JSON API for scripts and future integrations.
- Docker packaging with automatic TLS and edge authentication.
- Documented off-machine backup and restore runbook.

## Quickstart (local)

Requirements: Rust stable, Docker + Compose plugin.

```bash
cp .env.example .env
# The example already uses localhost:8080. Generate two SEPARATE secrets —
# they serve different purposes and must differ:
#   CSRF signing key   -> CSRF_SIGNING_KEY
#   Caddy-to-app proof -> UPSTREAM_AUTH_TOKEN
# Generate each with:
#   openssl rand -base64 48
# Generate Caddy password hash for local test:
#   docker run --rm caddy:2 caddy hash-password --algorithm argon2id --plaintext 'dev-password'
# Put the generated values in .env (escape each $ in the password hash as $$).
# The stack refuses to start with the example placeholders.

cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features

docker build -t go-shortener:latest .
docker compose up --build
# App: http://localhost:8080/{slug} (via Caddy), admin: http://localhost:8080/admin
# Verify the full boundary: Caddy's Basic Auth at the edge AND the app's own
# proxy-token gate (direct requests without the token fail closed with 401):
ADMIN_PASSWORD=dev-password SMOKE_APP_CONTAINER="$(docker compose ps -q shortener)" ./scripts/smoke-caddy-auth.sh http://localhost:8080
```

Direct app run (dev, no Caddy):

```bash
export APP_ENV=development APP_BIND=127.0.0.1:3000 \
  BASE_URL=http://localhost:3000 DATABASE_URL='sqlite://./dev.db?mode=rwc' \
  RUST_LOG=debug CSRF_SIGNING_KEY="$(openssl rand -base64 48)" \
  UPSTREAM_AUTH_ALLOW_DIRECT=true
cargo run
```

`UPSTREAM_AUTH_ALLOW_DIRECT=true` permits direct management access without the
proxy token, but only because all three hold: development env, explicit flag,
loopback bind. Production ignores the flag entirely.

## Configuration

| Var | Required | Example |
|---|---|---|
| `APP_ENV` | no (default `development`) | `production` |
| `APP_BIND` | no (default `0.0.0.0:3000`) | `0.0.0.0:3000` |
| `BASE_URL` | yes | `https://go.example.com` |
| `DATABASE_URL` | yes | `sqlite:///data/go.db` |
| `RUST_LOG` | no | `info` |
| `CSRF_SIGNING_KEY` | yes (≥32 bytes) | `openssl rand -base64 48` |
| `UPSTREAM_AUTH_TOKEN` | yes in production (≥32 chars) | `openssl rand -base64 48` |
| `UPSTREAM_AUTH_ALLOW_DIRECT` | no (default `false`) | `true` enables direct dev access only |
| `SITE_ADDRESS` | yes (Caddy only) | local: `http://localhost`; production: `<your-domain>` |
| `CADDY_HTTP_HOST` / `CADDY_HTTP_PORT` | no | local: `127.0.0.1` / `8080`; production: `0.0.0.0` / `80` |
| `CADDY_HTTPS_HOST` / `CADDY_HTTPS_PORT` | no | local: `127.0.0.1` / `8443`; production: `0.0.0.0` / `443` |
| `ADMIN_PASSWORD_HASH` | yes (Caddy only) | Argon2id hash from `caddy hash-password` |

Rules: `BASE_URL` must be a root HTTP(S) URL without credentials, query, or fragment.
Production refuses non-HTTPS URLs, short/missing CSRF keys, and the public example placeholder.
Never commit `.env` or hashes.

## Routes

```text
GET   /                        landing
GET   /robots.txt              Disallow: /
GET   /health/live             no DB
GET   /health/ready            SELECT 1 (+ migrations ran at startup)
GET   /admin                   list + create (CSRF cookie issued)
POST  /admin/links             create
GET   /admin/links/{slug}      edit form
POST  /admin/links/{slug}      update target/label/expiry
POST  /admin/links/{slug}/disable
POST  /admin/links/{slug}/enable
GET   /api/v1/links            ?q=&page=&per_page= (default 20, max 100)
POST  /api/v1/links            {target_url, custom_slug?, label?, expires_at?}
GET   /api/v1/links/{slug}
PATCH /api/v1/links/{slug}     at least one of target_url/label/expires_at
DELETE /api/v1/links/{slug}    soft-disable → 204
POST  /api/v1/links/{slug}/enable
GET   /{slug} / HEAD /{slug}   302 active / 404 unknown+disabled / 410 expired
```

Redirects carry `Cache-Control: no-store` + `X-Robots-Tag: noindex, nofollow`.
API errors are `{"error":{"message":"…","code":NNN}}`; `deny_unknown_fields` is on.
`expires_at` must be in the future and accepts RFC 3339
(`2026-12-31T23:59:59Z`) or Unix millis. Admin date fields are explicitly UTC
and accept `datetime-local` (`YYYY-MM-DDTHH:MM`).

## Security model

- Management routes (`/admin*`, `/api*`) require **two** proofs: Caddy's
  `basic_auth argon2id` at the edge, and the `X-Crabhop-Proxy-Token` header the
  app checks on every management request. Caddy strips any client-supplied
  value and injects the shared `UPSTREAM_AUTH_TOKEN` when proxying, so even an
  accidentally exposed port 3000 fails closed with 401 and leaks nothing.
  `X-Authenticated-User` is informational only and never trusted by itself;
  upstream never sees `Authorization` (`header_up -Authorization`).
- Admin POSTs: signed double-submit CSRF cookie (`HttpOnly; SameSite=Strict; Path=/;
  Secure` on https) + `Origin`/`Referer` must match `BASE_URL`. Missing both → 403.
- API mutations: require `Content-Type: application/json` + `X-Requested-With`
  header; no CORS headers are ever emitted.
- Headers: `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
  `Referrer-Policy: same-origin`, restrictive CSP on `/admin`.
  Every `/admin` and `/api` response carries `Cache-Control: no-store` so
  private link data is never retained in browser caches.
- Production HTTPS responses carry
  `Strict-Transport-Security: max-age=31536000` (no `includeSubDomains`, no
  preload — add them only after review).
- Body cap 16 KiB, 10 s timeout, Askama auto-escaping, no credential/URL logging,
  non-root read-only container (writable `/data` only).

## Production deployment

1. Provision Ubuntu 24.04, apply updates, install Docker + Compose plugin.
2. Firewall: allow 22/80/443 only (`ufw allow 22,80,443/tcp`), restrict SSH (keys only).
3. DNS: `<your-domain> A → <host IP>`.
4. On server, create restricted `.env` (mode 600): `APP_ENV=production`,
   `BASE_URL=https://<your-domain>`, `SITE_ADDRESS=<your-domain>`,
   `CADDY_HTTP_HOST=0.0.0.0`, `CADDY_HTTP_PORT=80`,
   `CADDY_HTTPS_HOST=0.0.0.0`, `CADDY_HTTPS_PORT=443`,
   `DATABASE_URL=sqlite:///data/go.db`,
   `CSRF_SIGNING_KEY` (48 random bytes),
   `UPSTREAM_AUTH_TOKEN` (`openssl rand -base64 48`; leave
   `UPSTREAM_AUTH_ALLOW_DIRECT` unset — production ignores it),
   `ADMIN_PASSWORD_HASH`
   (`docker run --rm caddy:2 caddy hash-password --algorithm argon2id --plaintext '…'`).
   Because argon2id hashes contain `$`, write each one as `$$` in `.env`
   (Compose interpolates unescaped `$VAR` and would corrupt the hash —
   verified during local stack testing).
5. `docker compose up -d --build`; verify: `https://<your-domain>/health/live`,
   HTTP→HTTPS redirect, `/admin` prompts for password, app port not reachable
   externally (`ss -tlnp` shows only 80/443).
6. Install fail2ban, copy `deploy/fail2ban/filter.d/crabhop-caddy.conf` into
   `/etc/fail2ban/filter.d/`, and copy the jail example into
   `/etc/fail2ban/jail.d/crabhop-caddy.local`. Update its `logpath` with the
   mountpoint reported by `docker volume inspect crabhop_caddy-logs`, then
   restart fail2ban. The shipped policy bans five management-route 401s in ten
   minutes for one hour through Docker's `DOCKER-USER` firewall chain. Verify
   it with `fail2ban-client status crabhop-caddy`.
7. Run `ADMIN_PASSWORD='<plaintext password>' ./scripts/smoke-caddy-auth.sh
   https://<your-domain>` and confirm the fail2ban jail is active.

### Updating an existing VPS

Run the deployment script as the VPS user that owns the checkout and can use
Docker. It finds the repository relative to itself, so it can be invoked from
any working directory:

```bash
/opt/crabhop/scripts/deploy-vps.sh
```

The script refuses tracked local changes, pulls with `git pull --ff-only`,
rebuilds the Rust image, recreates changed services, waits for both internal and
public readiness, and restores the previous application image if verification
fails. After a successful deployment it deletes previous and dangling Crabhop
images by their image ID/label. It does not run a global Docker prune and cannot
remove images or volumes belonging to other applications on the VPS.

The public check uses `BASE_URL`. For a VPS that cannot connect to its own public
address, set `PUBLIC_HEALTHCHECK_URL` to another Caddy-reachable URL, or use
`SKIP_PUBLIC_HEALTHCHECK=true` only when an external monitor verifies Caddy.

If the checkout lives somewhere else and the script was copied outside it, set
the location explicitly:

```bash
CRABHOP_DIR=/srv/crabhop /usr/local/bin/deploy-crabhop
```

## Upgrade / rollback

- Upgrade with `scripts/deploy-vps.sh`; it temporarily preserves the running
  image and restores it automatically if startup or readiness verification fails.
- After a successful deployment the previous image is deleted as part of the
  requested cleanup. To return to an older release later, revert that Git commit,
  push the revert, and run the deployment script again.
- SQLite migrations run at startup and are backwards-compatible (additive only).
- Dependabot proposes weekly Cargo and container-image updates as pull requests.
  Before deploying an image bump, review the new image's vulnerability report,
  update the digest pin, and let CI rebuild and re-verify.

## Credential rotation

- **Admin password:** regenerate hash, update server `.env`, `docker compose up -d caddy`.
- **CSRF key:** set new `CSRF_SIGNING_KEY`, `docker compose up -d shortener`
  (existing sessions/cookies invalidate once; users re-load `/admin`).
- **Proxy token:** generate a new `UPSTREAM_AUTH_TOKEN`, update the server
  `.env` (both services share it), then `docker compose up -d` to recycle app
  and Caddy together. Between the two restarts, management requests may 401 —
  rotate during a maintenance window and verify with the smoke test below.

## Backups & restore

- Recommended: Litestream sidecar using `litestream.yml.example`
  (set `LITESTREAM_BUCKET/ENDPOINT` + `AWS_*` keys, replicate `/data/go.db`).
- Minimal alternative (cron on host):
  `sqlite3 /var/lib/docker/volumes/*app-data*/_data/go.db ".backup '/tmp/go-backup.db'" && rclone copy /tmp/go-backup.db remote:backups/go-$(date +%F).db`
- **Restore test (required before calling backups done):**
  ```bash
  docker compose down
  docker volume create test-restore
  # copy backup file into volume, then:
  docker run --rm -v test-restore:/data -v "$PWD:/b" alpine cp /b/go-backup.db /data/go.db
  # point compose at test volume, up, verify /health/ready + known redirect
  ```
- Monitor: health endpoint + one known redirect and alert on disk usage of
  `app-data`, `caddy-data`, and `caddy-logs`. Compose caps container logs at
  5 × 10 MiB per service; Caddy rolls ten 10 MiB access logs for at most 30 days.

## CI

Every pull request runs `.github/workflows/ci.yml`:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo audit            # honors .cargo/audit.toml (dated, scoped ignores only)
docker build .
compose startup + Caddy authentication smoke test (scripts/smoke-caddy-auth.sh),
an end-to-end link lifecycle through the proxy, and a check that port 3000
is not published
```

Do not auto-deploy production from unprotected branches; deploy tagged images.
