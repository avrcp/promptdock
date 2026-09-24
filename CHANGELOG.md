# Changelog

All notable changes to this project are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Nothing pending.

## [0.6.0-rc.1] — 2026-09-18

Initial public extraction. This is the first commit tree that ships outside the
source environment. It adopts the relay server's current release line
(`0.6.0-rc.1`), which the frozen HTTP v1 / Admin v2 contract fixtures declare;
the desktop host is versioned independently at `0.1.0`.

### Added

- pnpm + Cargo monorepo workspace combining the desktop host, relay server,
  admin console, contracts, deploy assets and third-party crates.
- Frozen contract surfaces under `contracts/`:
  - `server-http/v1` (16 fixtures, SHA-256 `6fee4061…`) — desktop-to-relay
    notification API.
  - `admin-api/v2` — admin console API surface.
  - `node-link/v5` — Codex gateway protocol frames.
- `apps/desktop/`: Tauri 2 Windows host with Codex Hook observation,
  capture policy authority, local run inbox, tray, dispatch hold, staged
  health diagnostics, and encrypted DPAPI capture envelope.
- `apps/server/`: axum 0.8 relay with SQLite persistence, WeChat provider,
  read-only result pages (`result_pages_v1`), device scopes (`device_scopes_v1`),
  and retention worker.
- `apps/admin/`: Vue 3 SPA admin console with generated TypeScript client.
- `crates/wechat-ilink/`: WeChat iLink client derived from Tencent's
  `openclaw-weixin` v2.4.6 (MIT).
- `deploy/`: systemd unit, Caddyfile example, Docker Compose, config
  examples for production and notification-only topologies.
- Root quality gate scripts (`scripts/quality/`), contract manifest scripts
  (`scripts/contracts/`), server ops scripts (`scripts/server/`).
- Desktop packaging: `verify-versions.ps1`, `smoke-desktop-startup.ps1`,
  plus a `clear-local-data.cmd` reset entrypoint.
- `SECURITY.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`,
  `THIRD_PARTY_NOTICES.md`, `README.md` / `README.zh-CN.md`, `docs/`.
- `LICENSE`: the project is offered under Apache-2.0, and every Cargo and pnpm
  manifest records `license`. Bundled upstreams keep their own terms and are
  listed in `THIRD_PARTY_NOTICES.md`.

### Changed from the internal baseline

- Server Cargo package renamed from the internal identifier to
  `promptdock-server` so the tracing filter target and crate name no longer
  collide with the private repo path.
- Desktop library crate renamed from the internal donor-derived identifier
  to `promptdock_desktop_lib`.
- Reset entrypoint file renamed to `clear-local-data.cmd` for filesystem
  portability. Runtime error strings updated accordingly.
- Vendored relay contract under the desktop tree replaced by a single
  `contracts/server-http/v1/` at the monorepo root. The desktop validator
  (`apps/desktop/scripts/relay-contract-lib.ts`) reads the root manifest.
- Cross-repo release attestation packaging (`release-contract.ps1`,
  `release-bundle.test.ps1`, `build-windows.ps1`, `smoke-portable.ps1`) not
  carried into the public tree; see `CONTRIBUTING.md`.
- Shipped admin defaults are `read_only`. `deploy/docker/relay.toml`, which the
  image bakes as `/etc/promptdock-relay/config.toml`, and
  `deploy/examples/config.notification-only.example.toml` no longer set
  `[admin].mode = "operator"`; the mode only ever widens the write surface, so
  defaulting it asked every self-hoster for an escalation nothing in the stack
  uses. `SECURITY.md` now states the privilege model that follows from this,
  including the header boundary a `/v2` write must clear even under `operator`.
- Build documentation separates the three surfaces: `pnpm build` produces web
  assets and never `PromptDockDesktop.exe`, the admin production bundle is
  `build:production` plus `scripts/server/verify-admin-dist.mjs`, and the Tauri
  dev/build commands are given from the repository root.

### Not yet shipped

- Cross-platform desktop builds; the desktop host is Windows-only.
- Real-Codex end-to-end acceptance inside CI; only the synthetic hook
  fixture is exercised.
- Real-WeChat delivery inside CI; the provider is exercised against fixtures
  and the Docker verifier, not against the live protocol.

[Unreleased]: https://github.com/avrcp/promptdock/compare/v0.6.0-rc.1...HEAD
[0.6.0-rc.1]: https://github.com/avrcp/promptdock/releases/tag/v0.6.0-rc.1
