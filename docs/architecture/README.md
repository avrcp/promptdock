# Architecture

PromptDock is three cooperating applications connected by three frozen wire
contracts. Nothing runs inside anything else; each piece can be swapped for an
alternative implementation as long as it satisfies the same contract.

## Modules

```
+----------------+   hooks   +----------------+  HTTP v1  +------------------+
| Codex desktop  |---------->| PromptDock     |---------->| PromptDock Relay |
| (unmodified)   |           | Desktop Host   |           | Server           |
+----------------+           | (Tauri 2)      |           | (axum 0.8)       |
                             +----------------+           +---------+--------+
                                    |                               |
                                    | result_pages_v1               | provider API
                                    |                               |
                             +----------------+           +---------+--------+
                             | Operator       |<----------| WeChat iLink     |
                             | (browser)      | admin/v2  | (crates/         |
                             | Admin Console  |-----------|  wechat-ilink)   |
                             +----------------+           +------------------+
```

The desktop host and the relay server also speak a `node-link/v5` protocol
that this extraction retains from the internal project's history. It exists
in `contracts/node-link/v5/` as frozen fixtures and is exercised by tests in
`apps/server/tests/gateway_*.rs`, but the current desktop host does not open
a `/v5/gateway/ws` connection on its own. Treat v5 as a compatibility surface
for future work.

## Directory map

| Path | Crate / package | Role |
|---|---|---|
| `apps/desktop/src/` | `@promptdock/desktop` (Vue) | SPA front-end for the Tauri host |
| `apps/desktop/src-tauri/` | crate `promptdock-desktop`, lib `promptdock_desktop_lib` | Windows-only host: hook installation, inbox, DPAPI envelope, tray, dispatch hold |
| `apps/server/` | crate `promptdock-server`, binary `promptdock-relay` | HTTP relay, retention worker, notification fanout |
| `apps/admin/` | Vue SPA | Operator console — devices, scopes, deliveries, diagnostics |
| `crates/admin-api-contract/` | Rust lib | Admin API v2 constants, contract identity |
| `crates/relay-domain/` | Rust lib | Domain entities (Device, Run, Notification, Result) |
| `crates/relay-application/` | Rust lib | Use cases orchestrating the domain |
| `crates/relay-storage-sqlite/` | Rust lib | SQLite persistence (WAL mode, migrations) |
| `crates/relay-transport-http/` | Rust lib | HTTP server transport: axum routers, middleware |
| `crates/relay-transport-gateway/` | Rust lib | WebSocket gateway transport |
| `crates/relay-provider-wechat/` | Rust lib | WeChat delivery provider |
| `crates/wechat-ilink/` | Rust lib | iLink protocol client (derived from Tencent MIT, see `THIRD_PARTY_NOTICES.md`) |
| `contracts/server-http/v1/` | JSON fixtures + `manifest.json` | Desktop↔relay HTTP API |
| `contracts/admin-api/v2/` | JSON fixtures + `manifest.json` | Browser↔server admin HTTP API |
| `contracts/node-link/v5/` | JSON fixtures + `manifest.json` | WebSocket gateway protocol |
| `deploy/` | systemd unit + Caddyfile + Docker compose | Self-hosting assets |

## Data flow, happy path

1. Codex desktop fires a `UserPromptSubmit` Hook, which calls the installed
   PromptDock command with a JSON envelope on stdin.
2. The desktop host records a `run_started` event to its local inbox (a
   DPAPI-encrypted JSONL file) subject to `capture-policy.json`.
3. Later Codex fires `Stop`; the desktop host records `output_produced`
   (FullFinal) and `run_settling`.
4. The desktop host's sync worker POSTs the durable events to the relay's
   HTTP `/v1` API.
5. The relay persists to SQLite, fans out via the WeChat provider, and
   publishes a `result_pages_v1` link.
6. The user taps the link on their phone. The relay serves a read-only HTML
   page gated by a single-use token.
7. If they open the admin console, they authenticate against
   `admin-api/v2` (operator or read-only modes) to inspect history.

## Data flow, reset

`clear-local-data.cmd` (Windows) invokes `apps/desktop/scripts/clear-local-data.ps1`. That
script clears a fixed allowlist inside `%LOCALAPPDATA%/promptdock-desktop/`
and preserves `capture-policy.json` so a user cannot silently re-enable
capture by resetting. The reset writes a `.reset-pending` marker file before
acting; if the app observes that marker on next start, it refuses to run
(`RUNTIME_RESET_INCOMPLETE`) until reset completes. See
[../desktop/local-data.md](../desktop/local-data.md).

## What lives where, and why

- Capture policy is on the desktop, not on the server. Users should be able
  to see and edit a JSON file locally; the policy is not derived from server
  rollout state.
- Delivery state is on the server. Retention, per-device scopes, and read-only
  result pages require persistence the desktop cannot offer reliably.
- Admin view is on the server. The desktop host has no admin surface; the
  tray entry is diagnostic, not a control plane.

## Frozen contracts

Read [contracts.md](contracts.md) before touching a wire surface. Every
contract has a `manifest.json` with a SHA-256 over each fixture; CI fails on
drift.
