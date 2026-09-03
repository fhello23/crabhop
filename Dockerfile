# ---- Build ----
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
# Build release binary; templates are compiled in.
RUN cargo build --release --locked

# ---- Runtime ----
FROM debian:bookworm-slim
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
