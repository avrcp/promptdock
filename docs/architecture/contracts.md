# Contracts

Three wire surfaces are frozen under `contracts/`. Each has:

- A `manifest.json` at its root containing a SHA-256 for every fixture.
- One or more JSON fixtures defining the wire shape.
- A per-language generated client or Rust type where applicable.
- A drift gate that runs in CI.

| Contract | Path | Fixtures | Manifest SHA (first 8) |
|---|---|---:|---|
| HTTP `/v1` (desktop↔relay) | `contracts/server-http/v1/` | 16 | `6fee4061` |
| Admin `/v2` (browser↔relay) | `contracts/admin-api/v2/` | variable | `manifest.json` at that path |
| Node-link `/v5` (gateway WS) | `contracts/node-link/v5/` | 24 frames | `manifest.json` at that path |

## HTTP `/v1` — the desktop↔relay notification API

Defined by OpenAPI in the relay crate and generated to a JSON contract on
every build. The manifest at `contracts/server-http/v1/manifest.json` fixes
the SHA-256 of every fixture. The desktop validator
(`apps/desktop/scripts/relay-contract-lib.ts`) reads that manifest at build
time; the server-side test `api_contract_manifest.rs` regenerates and
compares.

Endpoints covered (see the fixtures for exact shape):

- Device registration and scope provisioning
- Event submission (`run_started`, `output_produced`, `run_settling`)
- Notification delivery acknowledgement
- Result page publication and single-use token issuance
- Health probes

## Admin `/v2` — the operator console API

Generated via utoipa + `@hey-api/openapi-ts`. The Rust source of truth is
`crates/admin-api-contract/src/lib.rs`. The TypeScript client is regenerated
by `pnpm contracts:generate-admin` at the repo root.

Modes (`[admin].mode`, default `read_only`):

- `read_only` advertises `admin_read_v2` and `admin_wechat_status_v2`. Every
  `/v2` read endpoint is served; `require_mutation_boundary` rejects each
  non-`GET`/`HEAD` with `403 ADMIN_READ_ONLY`.
- `operator` additionally advertises `admin_device_manage_v2`,
  `admin_maintenance_v2` and `admin_results_manage_v2`, plus
  `admin_wechat_manage_v2` and `admin_wechat_login_v2` when the WeChat provider
  is enabled. That is the whole difference: more write capability, not a
  different or narrower read surface.

See `SECURITY.md` for why `operator` is the elevated setting and should stay off
externally reachable deployments.

## Node-link `/v5` — the gateway WebSocket

A compatibility surface from the internal project. Currently used only by
`crates/relay-transport-gateway` and its tests. The desktop host does not
open a `/v5/gateway/ws` connection. The contract exists so a future runner
transport can plug in without a coordinated server-side schema change.

Frames: `hello-v5`, `ping-v5`, `pong-v5`, `error-v5`, `superseded-v5`,
`request-ack-v5`, plus request/response pairs for `start-run`, `cancel-run`,
`get-run-detail`, `get-run-tree`, `list-runs`, `list-runtimes`,
`list-workspaces`, `list-harness-profiles`, `list-task-presets`,
`get-device-info`. See `contracts/node-link/v5/manifest.json` for the full
list.

## Versioning policy

Contracts are additive-only within a major. A major bump means a wire
incompatibility, which the manifest SHA makes mechanically obvious.

Schema identities written to the database (`promptdock-relay-v4`) are a
separate axis from contract versions and are documented in the migration
SQL at `crates/relay-storage-sqlite/migrations/001_init.sql`.

## Drift gate

- `pnpm contracts:check` — validates HTTP `/v1`.
- `cargo test --locked --test api_contract_manifest` — regenerates the OpenAPI
  from the Rust source and asserts the SHA-256 matches `manifest.json`.
- `cargo test --locked --test admin_api_contract_manifest` — same for admin.
- `cargo test --locked --test gateway_contract_manifest` — same for node-link.

Every one fails on any byte-level fixture change. See
[../development/contract-drift.md](../development/contract-drift.md).
