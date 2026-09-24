# Development workflow

This is the day-to-day flow for working on the monorepo.

## Bootstrap

```
corepack enable pnpm
pnpm install
```

Both the JS and Rust sides resolve from the root workspace. There is no
per-package `pnpm install` any more; the two original repos used to keep
separate lockfiles and that pattern is retired.

## Common commands (root)

| Command | What it does |
|---|---|
| `pnpm build` | `vue-tsc` type-check + Vite build of the desktop and admin frontends. Web assets only — it never produces `PromptDockDesktop.exe` |
| `pnpm test` | Vitest across every JS workspace |
| `pnpm lint` | ESLint across every JS workspace |
| `pnpm typecheck` | `vue-tsc --noEmit` per workspace |
| `pnpm design:check` | Desktop design-token SSOT gate |
| `pnpm contracts:check` | Regenerate + diff every contract manifest |
| `pnpm contracts:generate-admin` | Regenerate the admin TypeScript client from OpenAPI |
| `pnpm tauri:dev` | Desktop host in a WebView2 window with hot reload (Windows) |
| `pnpm tauri:build` | `PromptDockDesktop.exe` (Windows; needs the Rust toolchain) |
| `cargo build` | Debug-builds the root workspace: server + `crates/*`. `apps/desktop/src-tauri` is excluded from that workspace and is built by `pnpm tauri:*` |
| `cargo build --release --bin promptdock-relay` | The server binary you actually deploy |
| `cargo test --locked --workspace --all-targets` | Full Rust test suite |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Lint |
| `cargo fmt --all -- --check` | Formatting gate |
| `bash scripts/quality/quality.sh` | Aggregate: every JS + Rust gate above |
| `pwsh scripts/quality/quality.ps1` | Same, on Windows PowerShell |

The admin bundle from `pnpm build` is the mock-capable development artifact. A
production admin bundle is a separate, explicit step, and `quality.sh` runs it
this way:

```
pnpm --filter @promptdock/relay-admin build:production
node scripts/server/verify-admin-dist.mjs
```

`verify-admin-dist.mjs` only accepts that production output — no source maps, no
mock data source — so the two commands together are the admin artifact gate.

## Filtering to a subset

- `cargo test -p promptdock-server` — the server binary crate only.
- `cargo test -p relay-storage-sqlite` — the SQLite crate only.
- `pnpm --filter @promptdock/desktop test` — one JS workspace.
- `pnpm vitest run apps/desktop/src/views/DeliveriesView.test.ts` — one file.

## Adding a new crate

1. `cargo new crates/<name> --lib` at the root.
2. Add `crates/<name>` to `[workspace] members` in the root `Cargo.toml`.
3. Add path dependencies from consumers.
4. Add the crate name to the internal-package set inside
   `scripts/server/check-crate-boundaries.py`.
5. `cargo test --locked -p <name>` from your working copy.

## Adding a new workspace package (JS)

1. Create `apps/<name>/` with a `package.json` whose `name` starts with
   `@promptdock/`.
2. Add `apps/<name>` to `pnpm-workspace.yaml`'s `packages:` list.
3. `pnpm install` regenerates the root lockfile with the new workspace
   member.
4. Add the new package's build / test scripts to
   `scripts/quality/quality.{sh,ps1}`.

## Debugging contract drift

If `pnpm contracts:check` fails:

1. Look at the "expected" SHA and the "actual" SHA in the error output.
2. Open `contracts/<surface>/manifest.json`.
3. Run `git log -p contracts/<surface>/` to find the offending commit.
4. Fix the source (OpenAPI or Rust) or accept the drift by regenerating the
   manifest. Never edit `manifest.json` by hand.

See [contract-drift.md](contract-drift.md) for the regeneration workflow.

## Where to look when something is odd

- **Server startup fails.** `promptdock-relay doctor` prints a secret-free
  diagnostic JSON. Look at `--config` path, key files, DB path, port conflict.
- **Desktop does not launch.** `packaging/smoke-desktop-startup.ps1` proves
  the exe at least reaches `--version`. Beyond that, check `%LOCALAPPDATA%`
  writability and the `.reset-pending` marker.
- **Hooks are not firing.** The desktop's `IntegrationView` reports
  installation state per event. See [../integrations/codex-hooks.md](../integrations/codex-hooks.md).

## What "no compatibility" means for this project

The v0.6.0-rc.1 baseline is a fresh extraction. There is no upgrade path from the
internal project's earlier database or config shapes. If you happen to have
a pre-public v0 install, run `clear-local-data.cmd` (desktop) or drop the
relay's SQLite file and start over. The project refuses to migrate
incompatible stores deliberately.
