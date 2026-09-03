#!/bin/sh
set -eu

APP_IMAGE="${APP_IMAGE:-go-shortener:latest}"
ROLLBACK_IMAGE="${ROLLBACK_IMAGE:-go-shortener:deploy-rollback}"
HEALTH_ATTEMPTS="${HEALTH_ATTEMPTS:-30}"
HEALTH_INTERVAL_SECONDS="${HEALTH_INTERVAL_SECONDS:-2}"

log() {
  printf '%s\n' "[crabhop-deploy] $*"
}

die() {
  log "ERROR: $*" >&2
  exit 1
}

script_dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
repo_dir="${CRABHOP_DIR:-$(CDPATH='' cd -- "$script_dir/.." && pwd)}"

for command_name in git docker curl flock; do
  command -v "$command_name" >/dev/null 2>&1 \
    || die "required command not found: $command_name"
done

cd "$repo_dir"
[ -f compose.yml ] || die "compose.yml not found in $repo_dir"
[ -f .env ] || die ".env not found in $repo_dir"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || die "$repo_dir is not a Git working tree"
docker compose version >/dev/null 2>&1 \
  || die "the Docker Compose plugin is not available"

# Prevent two deployments from rebuilding or replacing the same containers.
lock_name="$(printf '%s' "$repo_dir" | tr '/ ' '__')"
lock_file="${TMPDIR:-/tmp}/${lock_name}.deploy.lock"
exec 9>"$lock_file"
flock -n 9 || die "another deployment is already running"

# A VPS checkout should be an exact copy of GitHub. Ignored files such as .env
# are allowed, but every other local change requires manual review.
if [ -n "$(git status --porcelain --untracked-files=all)" ]; then
  die "the Git checkout contains local changes or untracked files"
fi

old_commit="$(git rev-parse --short HEAD)"
running_container_id="$(docker compose ps -q shortener 2>/dev/null || true)"
if [ -n "$running_container_id" ]; then
  old_image_id="$(docker inspect "$running_container_id" --format '{{.Image}}' 2>/dev/null || true)"
else
  old_image_id="$(docker image inspect "$APP_IMAGE" --format '{{.Id}}' 2>/dev/null || true)"
fi

# Preserve the currently deployed application image until health checks pass.
if [ -n "$old_image_id" ]; then
  docker image rm "$ROLLBACK_IMAGE" >/dev/null 2>&1 || true
  docker image tag "$old_image_id" "$ROLLBACK_IMAGE"
fi

log "pulling new commits from the configured upstream"
if ! git pull --ff-only; then
  [ -z "$old_image_id" ] || docker image rm "$ROLLBACK_IMAGE" >/dev/null 2>&1 || true
  die "Git pull failed; the running containers were not changed"
fi
new_commit="$(git rev-parse --short HEAD)"
log "source updated: $old_commit -> $new_commit"

log "validating Compose and Caddy configuration"
if ! docker compose config --quiet || \
   ! docker compose run --rm --no-deps --entrypoint caddy caddy \
     validate --config /etc/caddy/Caddyfile; then
  [ -z "$old_image_id" ] || docker image rm "$ROLLBACK_IMAGE" >/dev/null 2>&1 || true
  die "deployment configuration is invalid; the running containers were not changed"
fi

log "building the Rust application image"
if ! docker compose build --pull shortener; then
  [ -z "$old_image_id" ] || docker image rm "$ROLLBACK_IMAGE" >/dev/null 2>&1 || true
  die "image build failed; the running containers were not changed"
fi

rollback() {
  if [ -z "$old_image_id" ] || ! docker image inspect "$ROLLBACK_IMAGE" >/dev/null 2>&1; then
    log "no previous application image is available for rollback"
    return 1
  fi

  log "restoring the previous application image"
  docker image tag "$ROLLBACK_IMAGE" "$APP_IMAGE"
  docker compose up -d --no-deps --force-recreate shortener
}

log "recreating changed services"
if ! docker compose up -d --remove-orphans; then
  rollback || true
  die "Compose update failed"
fi

log "waiting for the application and database to become ready"
attempt=1
ready=false
while [ "$attempt" -le "$HEALTH_ATTEMPTS" ]; do
  if docker compose exec -T shortener \
    wget -qO- http://127.0.0.1:3000/health/ready >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep "$HEALTH_INTERVAL_SECONDS"
  attempt=$((attempt + 1))
done

if [ "$ready" != true ]; then
  docker compose logs --tail=100 shortener >&2 || true
  rollback || true
  die "the new application did not become ready"
fi

if ! public_url="$(docker compose exec -T shortener sh -c 'printf %s "$BASE_URL"')"; then
  rollback || true
  die "could not read BASE_URL from the running application"
fi
if [ -z "$public_url" ]; then
  rollback || true
  die "BASE_URL is empty in the running application"
fi

if [ "${SKIP_PUBLIC_HEALTHCHECK:-false}" = true ]; then
  log "skipping the public Caddy check by request"
else
  public_health_url="${PUBLIC_HEALTHCHECK_URL:-${public_url%/}/health/ready}"
  log "waiting for the public Caddy endpoint: $public_health_url"
  attempt=1
  public_ready=false
  while [ "$attempt" -le "$HEALTH_ATTEMPTS" ]; do
    if curl --fail --silent --show-error "$public_health_url" >/dev/null 2>&1; then
      public_ready=true
      break
    fi
    sleep "$HEALTH_INTERVAL_SECONDS"
    attempt=$((attempt + 1))
  done

  if [ "$public_ready" != true ]; then
    docker compose logs --tail=100 caddy >&2 || true
    rollback || true
    die "public readiness check failed"
  fi
fi

new_image_id="$(docker image inspect "$APP_IMAGE" --format '{{.Id}}')"

# Remove the rollback tag and only old images belonging to this application.
# Do not use docker system prune: the VPS may host unrelated applications.
if [ -n "$old_image_id" ]; then
  docker image rm "$ROLLBACK_IMAGE" >/dev/null 2>&1 || true
  if [ "$old_image_id" != "$new_image_id" ]; then
    docker image rm "$old_image_id" >/dev/null 2>&1 || true
  fi
fi
docker image prune --force \
  --filter "label=org.opencontainers.image.title=crabhop" >/dev/null

log "deployment complete at commit $new_commit"
docker compose ps
