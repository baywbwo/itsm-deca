# syntax=docker/dockerfile:1
# ITSM production images (CP8). Build context = repository root:
#   docker compose -f deploy/production/compose.yaml build
# Same pinned bases as deploy/uat/Dockerfile (re-verify digests with
# `docker buildx imagetools inspect <image>` before building). The egress proxy image is the
# reviewed deploy/uat/Dockerfile target `egress` (compose.yaml builds it from there).

# --- API build ---------------------------------------------------------------------------
FROM rust:1.99.1-slim-bookworm@sha256:24707e70a4f1e23d9a7b25202ec43126d28a52294b96aad5c670df1fd1f1d400 AS api-build
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

# --- API runtime (api-a, api-b, worker, one-off migrate / init-password) -----------------
FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS api
COPY --from=api-build /itsm-api /usr/local/bin/itsm-api
COPY deploy/production/api-entrypoint.sh /usr/local/bin/api-entrypoint.sh
COPY deploy/uat/api-healthcheck.sh /usr/local/bin/api-healthcheck.sh
COPY deploy/uat/worker-healthcheck.sh /usr/local/bin/worker-healthcheck.sh
RUN install -d -o 10001 -g 10001 -m 0750 /var/lib/itsm/attachments
USER 10001:10001
ENV ITSM_ENV=production ITSM_BIND=0.0.0.0:8080 ITSM_ATTACHMENT_DIR=/var/lib/itsm/attachments
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=4 CMD ["/usr/local/bin/api-healthcheck.sh"]
ENTRYPOINT ["/usr/local/bin/api-entrypoint.sh"]
CMD ["serve"]

# --- Ingress (Caddy + static web) --------------------------------------------------------
FROM caddy:2.11.7-alpine@sha256:e70b164da81ef698b3ad19af799ea6086ce6d9e48ed4079eb9fe26f6fbf14ee5 AS ingress
COPY deploy/production/Caddyfile /etc/caddy/Caddyfile
COPY deploy/production/ingress-guard.sh /usr/local/bin/ingress-guard.sh
COPY --from=web-build /src/web/dist /srv
HEALTHCHECK --interval=15s --timeout=3s --retries=3 CMD ["wget", "-q", "-O", "/dev/null", "http://127.0.0.1:8099/ingress-health"]
ENTRYPOINT ["/usr/local/bin/ingress-guard.sh"]
CMD ["caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]

# --- Postgres (database + backup service) ------------------------------------------------
# age (public-key file encryption, Debian package) encrypts WAL and base backups with the
# operator's public key; the server never holds the private key. Version as tested natively
# (age 1.1.1-1+b3, evidence/i3/cp8/backup-drill.sh); bump the pin if the archive drops it.
FROM postgres:18.4-bookworm@sha256:882236b897e39051d2368c5ccc6cda944904723506b2dfc97f2a8f5bc9afa382 AS postgres
RUN apt-get update \
 && apt-get install -y --no-install-recommends age=1.1.1-1+b3 \
 && rm -rf /var/lib/apt/lists/* \
 && install -d -o postgres -g postgres -m 0700 /backup
COPY deploy/production/postgres/postgresql.conf deploy/production/postgres/pg_hba.conf /etc/itsm/postgres/
COPY deploy/production/db/roles.sql deploy/production/db/database.sql /etc/itsm/db/
COPY deploy/production/postgres/init-db.sh deploy/production/postgres/set-passwords.sh /usr/local/lib/itsm/
COPY deploy/production/backup/ /usr/local/lib/itsm/backup/
RUN ln -s /usr/local/lib/itsm/init-db.sh /docker-entrypoint-initdb.d/10-itsm-init.sh \
 && chmod 0755 /usr/local/lib/itsm/*.sh /usr/local/lib/itsm/backup/*.sh
CMD ["postgres", "-c", "config_file=/etc/itsm/postgres/postgresql.conf"]
