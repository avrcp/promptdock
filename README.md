# PromptDock

Self-hostable notification bridge between Codex desktop sessions and WeChat.

PromptDock captures turn boundaries from an existing Codex desktop install via Codex
Hooks, delivers structured notifications through a Relay server, and hosts read-only
result pages behind your own domain. Nothing in PromptDock executes tasks, approves
permissions, or intercepts traffic.

## What ships in this monorepo

| Area | Path | Role |
|---|---|---|
| Desktop host | `apps/desktop/` | Tauri 2 Windows app: launches alongside Codex desktop, owns the local run inbox, tray, dispatch hold, and diagnostic surface |
| Relay server | `apps/server/` | Axum HTTP service: accepts hook events from desktops, fans them out to WeChat, hosts read-only result pages |
| Admin console | `apps/admin/` | Vue 3 SPA: operator view of devices, scopes, deliveries, retention |
| Contracts | `contracts/` | Frozen HTTP `/v1`, admin `/v2`, and node-link `/v5` wire surfaces with SHA-256 manifests |
| Server crates | `crates/relay-*` | Domain, application, storage (SQLite), transport (HTTP + gateway), WeChat provider |
| Third-party | `crates/wechat-ilink` | Derived from Tencent's `openclaw-weixin` v2.4.6 (MIT) |
| Deploy assets | `deploy/` | systemd unit, Caddyfile example, Docker Compose, config examples |

## Quick start

The monorepo uses pnpm workspaces (Node ≥ 24.18) and a Cargo workspace (Rust ≥ 1.98,
stable). Every command below runs from the repository root.

```
pnpm install
pnpm build          # type-checks and bundles the desktop and admin Vue frontends
pnpm test           # unit + contract tests
cargo build         # builds the server binary (target: promptdock-relay)
```

Three separate surfaces, three commands: `pnpm build` emits web assets only, so
it never produces `PromptDockDesktop.exe`. The Windows desktop executable comes
from the Tauri CLI, which needs the Rust toolchain and WebView2:

```
pnpm tauri:dev      # desktop host with hot reload, Windows
pnpm tauri:build    # PromptDockDesktop.exe, Windows
```

See `docs/getting-started/` for a full walkthrough, and `docs/self-hosting/` for
deployment on a Linux box or via Docker.

## Trust boundary

- Codex Hook events are observed, never intercepted. PromptDock reads what Codex
  already writes to stdin / stdout on the hook boundaries you install.
- Result pages are content-addressed, token-gated, and single-purpose. FullFinal
  output is served one link at a time (`result_pages_v1`); there is no segmented
  fallback.
- Local capture stores are encrypted with per-user Windows DPAPI (desktop) or your
  server-side master key (relay). Reset tooling clears only its fixed allowlist and
  preserves `capture-policy.json`.
- The relay never sees the desktop's private working directory, credentials, or
  hook inputs beyond what the policy explicitly opts in.

Read `SECURITY.md` before deploying to the internet.

## License

Licensed under the Apache License, Version 2.0 (the "License"); you may not use
these files except in compliance with the License. A copy of the License is at
[`LICENSE`](LICENSE). You can find the licenses of bundled third-party code and
other attribution requirements in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## Contributing

[`ARCHITECTURE.md`](ARCHITECTURE.md) states which component owns which decision,
and [`AGENTS.md`](AGENTS.md) lists the invariants a change is expected to
preserve — including the commands to run for the area you touched.
`CONTRIBUTING.md` covers how to run the quality gates locally, how contract drift is
detected, and how to file issues. Read `docs/development/` before opening a PR.

## Status

This monorepo is the result of an extraction from an internal project. Some flows are
still exercised only inside the source environment, so the public test suite covers
unit and contract surfaces but not every production acceptance path. See
`CHANGELOG.md` for what actually ships.
