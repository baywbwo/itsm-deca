# syntax=docker/dockerfile:1
# ITSM UAT images. Build context = repository root:
#   docker compose -f deploy/uat/compose.yaml build
# Base images are pinned by digest (resolved from Docker Hub on 2026-11-02; re-verify with
# `docker buildx imagetools inspect <image>` before building).

# --- API build ---------------------------------------------------------------------------
FROM rust:1.99.1-slim-bookworm@sha256:24707e70a4f1e23d9a7b25202ec43126d28a52294b96aad5c670df1fd1f1d400 AS api-build
# Use the image toolchain as-is (rust-toolchain.toml would otherwise fetch rustfmt/clippy).
ENV RUSTUP_TOOLCHAIN=1.99.1 CARGO_INCREMENTAL=0
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY api ./api
RUN cargo build --release --locked -p itsm-api \
 && install -m 0755 target/release/itsm-api /itsm-api

# --- Web build ---------------------------------------------------------------------------
FROM node:24.22.1-bookworm-slim@sha256:be8204d4bd327363357f532187b752ff62759c31bedf98e9e18fc08c9cbd605c AS web-build
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY openapi /src/openapi
COPY web ./
# Contract types must match openapi.json; the bundle gets no secrets (no VITE_* env is set).
RUN npm run gen:api && npm run check:api && npm run build

# --- API runtime -------------------------------------------------------------------------
FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS api
COPY --from=api-build /itsm-api /usr/local/bin/itsm-api
COPY deploy/uat/api-entrypoint.sh /usr/local/bin/api-entrypoint.sh
COPY deploy/uat/api-healthcheck.sh /usr/local/bin/api-healthcheck.sh
RUN install -d -o 10001 -g 10001 -m 0750 /var/lib/itsm/attachments
USER 10001:10001
ENV ITSM_BIND=0.0.0.0:8080 ITSM_ATTACHMENT_DIR=/var/lib/itsm/attachments
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=4 CMD ["/usr/local/bin/api-healthcheck.sh"]
ENTRYPOINT ["/usr/local/bin/api-entrypoint.sh"]
CMD ["serve"]

# --- Ingress (Caddy + static web) --------------------------------------------------------
FROM caddy:2.11.7-alpine@sha256:e70b164da81ef698b3ad19af799ea6086ce6d9e48ed4079eb9fe26f6fbf14ee5 AS ingress
COPY deploy/uat/Caddyfile /etc/caddy/Caddyfile
COPY deploy/uat/ingress-guard.sh /usr/local/bin/ingress-guard.sh
COPY --from=web-build /src/web/dist /srv
HEALTHCHECK --interval=15s --timeout=3s --retries=3 CMD ["wget", "-q", "-O", "/dev/null", "http://127.0.0.1:8099/ingress-health"]
ENTRYPOINT ["/usr/local/bin/ingress-guard.sh"]
CMD ["caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]

# --- Egress proxy (Squid, CP6b) ----------------------------------------------------------
# The only way out of the backend network: HTTP CONNECT to allowlisted hosts on 443 with proxy
# credentials, private addresses refused after DNS (deploy/RUNBOOK-egress.md). The Debian package
# version is the one tested natively (Squid 5.7); a newer Debian security release replaces it in
# the archive, so bump the pin and re-run the E1-E11 harness when the build stops resolving it.
FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS egress
RUN apt-get update \
 && apt-get install -y --no-install-recommends squid=5.7-2+deb12u6 squid-common=5.7-2+deb12u6 squid-langpack \
 && rm -rf /var/lib/apt/lists/* /etc/squid/conf.d \
 && install -d -o proxy -g proxy -m 0700 /run/squid
COPY deploy/egress/squid.conf deploy/egress/allowlist.regex /etc/squid/itsm/
COPY deploy/egress/entrypoint.sh /usr/local/bin/egress-entrypoint.sh
COPY deploy/egress/healthcheck.sh /usr/local/bin/egress-healthcheck.sh
USER proxy
EXPOSE 3128
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 CMD ["/usr/local/bin/egress-healthcheck.sh"]
ENTRYPOINT ["/usr/local/bin/egress-entrypoint.sh"]
