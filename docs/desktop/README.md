# Desktop host

The desktop host is a Tauri 2 Windows application. It lives at
`apps/desktop/`. It is the only component of PromptDock that runs on the
user's workstation.

## What it does

- Installs and removes Codex desktop Hooks.
- Runs as a hook handler for `UserPromptSubmit` and `Stop` (and optionally
  `PermissionRequest`), writing to a local inbox.
- Synchronises durable inbox records to the configured relay.
- Presents a five-view UI: Overview, Integration, Notifications, Deliveries,
  Diagnostics.
- Offers a system-tray entry with dispatch hold and quick health check.

## What it does not do

- Execute tasks.
- Approve permission requests.
- Read files from the user's working directory.
- Intercept or decrypt network traffic.
- Migrate a legacy database. Refuses incompatible stores on start.

## Views

| Route | Purpose |
|---|---|
| `#/overview` | Recent activity, sync status, top-line health |
| `#/integration` | Codex Hook installation state and per-event health |
| `#/notifications` | Notification drafts and quiet-hours policy |
| `#/deliveries` | Historical per-notification delivery state and result link |
| `#/diagnostics` | Staged health snapshot, runtime access guard status |

Each view has an `*.test.ts` alongside it in `apps/desktop/src/views/`.

## Runtime access guard

Before opening any file inside the local data directory, the desktop host
takes a lock and probes writability. If another process holds the lock — e.g.
an antivirus during a scan, or a second desktop instance — the guard surfaces
a specific error code (`RUNTIME_ACCESS_CONFLICT`) rather than crashing.

## Reset

`clear-local-data.cmd` clears a fixed allowlist of files inside the local
data directory while preserving `capture-policy.json`. See
[local-data.md](local-data.md) for the exact list and rationale.

## Design tokens

The five views share a small design system in
`apps/desktop/src/design-system/tokens.css`. The gate is
`check-design-tokens.mjs` and the rules are documented in
[design-tokens.md](design-tokens.md).

## Local development

From the repository root:

```
pnpm tauri:dev                                     # Tauri dev workflow, WebView2 on Windows
pnpm --filter @promptdock/desktop dev              # Vite SPA in a browser, no Rust
pnpm --filter @promptdock/desktop test             # Vitest unit tests
pnpm --filter @promptdock/desktop contracts:check   # validate HTTP /v1 fixture SHAs
```

`tauri:dev` and `tauri:build` are defined only in the root `package.json`; the
other four are desktop package scripts, so from the root they need `--filter`.
Inside `apps/desktop` the names differ: that package exposes `tauri`, so it is
`pnpm tauri dev` there and `pnpm tauri:dev` at the root, never the other way
round.

On non-Windows hosts the Tauri dev workflow falls back to a browser window.
That is useful for UI iteration but not a substitute for a Windows smoke
test.

## Building the portable exe

The ordinary route is `pnpm tauri:build` from the repository root.

The public tree ships `packaging/verify-versions.ps1` and
`packaging/smoke-desktop-startup.ps1`, which are the version-consistency
gate and the native startup smoke test. Full release attestation packaging
(checksummed bundle with a manifest) is not in the public tree; the advanced
invocation is `pnpm tauri build --target x86_64-pc-windows-msvc --no-bundle -- --locked`,
run from `apps/desktop` (or from the root as
`pnpm --filter @promptdock/desktop tauri build …`), and the resulting
`PromptDockDesktop.exe` is an unsigned artifact.
