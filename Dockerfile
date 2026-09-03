# ---- Build ----
# Base images are pinned by digest (reviewed 2026-09-03). Dependabot proposes
# updates; review the new image's vulnerability report, then update the pin.
FROM rust:1-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
# Build release binary; templates are compiled in.
RUN cargo build --release --locked

# ---- Runtime ----
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
LABEL org.opencontainers.image.title="crabhop" \
      org.opencontainers.image.description="Private Rust link shortener"
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates wget sqlite3 \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --uid 10001 --create-home --shell /usr/sbin/nologin app
WORKDIR /srv/app
COPY --from=builder /app/target/release/shortener /usr/local/bin/shortener
COPY static ./static
# Healthcheck helper (no shell dependency on app port publishing).
RUN printf '#!/bin/sh\nset -e\nwget -qO- http://127.0.0.1:3000/health/live | grep -q live\n' \
  > /usr/local/bin/shortener-healthcheck \
  && chmod +x /usr/local/bin/shortener-healthcheck
RUN mkdir -p /data && chown app:app /data
USER 10001
EXPOSE 3000
ENV APP_BIND=0.0.0.0:3000
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD ["/usr/local/bin/shortener-healthcheck"]
CMD ["/usr/local/bin/shortener"]
