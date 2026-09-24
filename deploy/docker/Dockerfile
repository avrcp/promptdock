# syntax=docker/dockerfile:1.7

ARG NODE_VERSION=24.18.0
ARG UBUNTU_VERSION=24.04
ARG RUST_VERSION=1.98.0
ARG APP_UID=10001
ARG APP_GID=10001

FROM caddy:2.11.4-alpine AS caddy-toolchain

FROM node:${NODE_VERSION}-bookworm-slim AS admin-deps

ENV PNPM_HOME=/pnpm
ENV PATH=${PNPM_HOME}:${PATH}
WORKDIR /workspace

RUN corepack enable && corepack prepare pnpm@11.10.0 --activate

COPY package.json pnpm-workspace.yaml pnpm-lock.yaml .npmrc ./
COPY apps/admin/package.json apps/admin/package.json
COPY apps/desktop/package.json apps/desktop/package.json
COPY packages/admin-api-contract/package.json packages/admin-api-contract/package.json

RUN --mount=type=cache,target=/pnpm/store,sharing=locked \
    pnpm install --frozen-lockfile

FROM admin-deps AS admin-builder

ARG BUILD_VERSION=0.6.0-rc.1
ARG SOURCE_COMMIT

COPY apps/admin apps/admin
COPY packages packages
COPY contracts contracts
COPY scripts/contracts/check-admin-contract.mjs scripts/contracts/check-admin-contract.mjs
COPY scripts/server/verify-admin-dist.mjs scripts/server/verify-admin-dist.mjs
COPY Cargo.toml Cargo.toml

RUN node scripts/contracts/check-admin-contract.mjs && \
    VITE_BUILD_VERSION="${BUILD_VERSION}" \
    VITE_BUILD_COMMIT="${SOURCE_COMMIT}" \
    VITE_ADMIN_API_MAJOR=2 \
    PROMPTDOCK_BUILD_SOURCE_ARCHIVE=1 \
    pnpm --filter @promptdock/relay-admin build:production && \
    VITE_BUILD_VERSION="${BUILD_VERSION}" \
    VITE_BUILD_COMMIT="${SOURCE_COMMIT}" \
    VITE_ADMIN_API_MAJOR=2 \
    node scripts/server/verify-admin-dist.mjs

FROM node:${NODE_VERSION}-bookworm-slim AS rust-toolchain

ARG RUST_VERSION
ARG APP_UID
ARG APP_GID

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PNPM_HOME=/opt/pnpm \
    PATH=/opt/cargo/bin:/opt/pnpm:${PATH}

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

COPY --from=caddy-toolchain /usr/bin/caddy /usr/local/bin/caddy

# The upstream binary carries cap_net_bind_service. Tooling containers drop all
# capabilities and enable no-new-privileges, so strip that file capability while
# retaining an ordinary executable used only for offline config validation.
RUN cp --no-preserve=xattr /usr/local/bin/caddy /tmp/caddy && \
    mv /tmp/caddy /usr/local/bin/caddy && \
    chmod 0755 /usr/local/bin/caddy

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean && \
    apt-get update && \
    apt-get install --yes --no-install-recommends \
        build-essential ca-certificates curl git libsqlite3-dev libssl-dev \
        pkg-config python3 sqlite3 && \
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
        https://sh.rustup.rs | \
        sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain "${RUST_VERSION}" --component clippy,rustfmt && \
    corepack enable && corepack prepare pnpm@11.10.0 --activate && \
    groupadd --gid "${APP_GID}" promptdock && \
    useradd --uid "${APP_UID}" --gid "${APP_GID}" \
        --create-home --shell /bin/bash promptdock && \
    install --directory --owner="${APP_UID}" --group="${APP_GID}" \
        /home/promptdock/.cargo /workspace /workspace/target /opt/pnpm

ENV CARGO_HOME=/home/promptdock/.cargo
WORKDIR /workspace
USER promptdock

FROM rust-toolchain AS development
COPY --from=admin-deps --chown=promptdock:promptdock /workspace /workspace
COPY --chown=promptdock:promptdock . .
CMD ["bash", "scripts/quality/quality.sh"]

FROM rust-toolchain AS rust-builder

ARG SOURCE_COMMIT
COPY --chown=promptdock:promptdock . .

RUN --mount=type=cache,id=promptdock-cargo-rust-1.98-debian12-amd64,target=/home/promptdock/.cargo,uid=10001,gid=10001,sharing=locked \
    --mount=type=cache,id=promptdock-target-rust-1.98-debian12-amd64,target=/workspace/target,uid=10001,gid=10001,sharing=locked \
    PROMPTDOCK_GIT_COMMIT="${SOURCE_COMMIT}" \
    cargo build --locked --release --bin promptdock-relay && \
    install --mode=0755 target/release/promptdock-relay /tmp/promptdock-relay

FROM ubuntu:${UBUNTU_VERSION} AS relay-runtime

ARG APP_UID
ARG APP_GID
ENV DEBIAN_FRONTEND=noninteractive

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean && \
    apt-get update && \
    apt-get install --yes --no-install-recommends ca-certificates curl && \
    groupadd --gid "${APP_GID}" promptdock && \
    useradd --uid "${APP_UID}" --gid "${APP_GID}" \
        --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin promptdock && \
    install --directory --owner="${APP_UID}" --group="${APP_GID}" \
        /etc/promptdock-relay /var/lib/promptdock-relay

COPY --from=rust-builder /tmp/promptdock-relay /usr/local/bin/promptdock-relay
COPY --chown=promptdock:promptdock deploy/docker/relay.toml /etc/promptdock-relay/config.toml

USER promptdock
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/promptdock-relay"]
CMD ["serve", "--config", "/etc/promptdock-relay/config.toml"]
HEALTHCHECK --interval=5s --timeout=2s --start-period=5s --retries=6 \
    CMD ["curl", "--fail", "--silent", "--show-error", "http://127.0.0.1:8080/health/ready"]

FROM relay-runtime AS runtime

FROM caddy:2.11.4-alpine AS admin-caddy-runtime
COPY deploy/docker/Caddyfile.admin /etc/caddy/Caddyfile
COPY --from=admin-builder /workspace/apps/admin/dist /srv/admin
EXPOSE 8443
