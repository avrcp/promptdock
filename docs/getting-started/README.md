# Getting started

This walkthrough gets you from a clean checkout to a working local install.
Every command below is safe to run in an isolated environment; none of them
talk to production services or install real Codex Hooks.

## Prerequisites

- Windows 11 (desktop) or Linux (server / admin)
- Node 24.18.0
- pnpm 11.10.0 (`corepack enable pnpm`)
- Rust stable 1.98.0 (pinned via `rust-toolchain.toml`)
- WebView2 runtime (only for the desktop host)

## Install

```
git clone https://github.com/avrcp/promptdock
cd promptdock
pnpm install
```

`pnpm install` runs once for the whole monorepo. Both JS workspaces
(`apps/desktop`, `apps/admin`) resolve from the root lockfile.

## Build the JS surfaces

```
pnpm build             # desktop + admin Vue bundles, each type-checked first
```

The desktop bundle is a static SPA that the Tauri host packages as an
executable; the admin bundle is a static site you serve behind a reverse
proxy. Neither depends on the server binary at build time. Neither command
produces `PromptDockDesktop.exe` — that is `pnpm tauri:build`, see below.

`pnpm build` is the validation build, and its admin output is the
**mock-capable** artifact: with `VITE_DATA_SOURCE_MODE` unset,
`apps/admin/src/app/environment.ts` resolves the data source to `mock`. A
production admin bundle is a separate, explicit step, and
`scripts/quality/quality.sh` runs exactly that pair:

```
pnpm --filter @promptdock/relay-admin build:production
node scripts/server/verify-admin-dist.mjs
```

`build:production` sets `VITE_DATA_SOURCE_MODE=production`, sources the release
commit from `git HEAD` and requires a clean tree;
`verify-admin-dist.mjs` then refuses source maps and the mock sentinel. So the
`pnpm build` output is not a production admin artifact and must not be shipped
as one.

## Build the server binary

```
cargo build --release  # produces target/release/promptdock-relay
```

The server binary embeds its release identity at build time (`build.rs`),
so `promptdock-relay --version` prints the commit it was built from without
needing a network round trip.

## Run tests

```
pnpm test                       # Vitest: desktop + admin unit tests
pnpm contracts:check            # every contract manifest SHA
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

## Run the relay locally

From the repository root:

```
cargo run --bin promptdock-relay -- init --config deploy/examples/config.local.example.toml
cargo run --bin promptdock-relay -- serve --config deploy/examples/config.local.example.toml
```

`init` creates `.local/relay.db` (the directory is created for you, and both
`.local/` and `*.db` are git-ignored). `serve` listens on `127.0.0.1:8080` and
exposes the admin API on `127.0.0.1:8081` in `read_only` mode, and the WeChat
provider is off, so nothing leaves the machine. Check it with:

```
curl -s http://127.0.0.1:8080/health/ready   # {"status":"ready"}
```

Do not use the two deployment examples for this.
`config.notification-only.example.toml` and `config.production.toml` both enable
the WeChat provider and put the database at `/var/lib/promptdock-relay/`, so
`serve` exits with `notification content encryption is unavailable` until a
`relay-master-key` is readable from `CREDENTIALS_DIRECTORY`. That shape is what
[../self-hosting/linux.md](../self-hosting/linux.md) covers.

## Run the admin console locally

The admin SPA has no API base URL to configure: `apps/admin/vite.config.ts`
proxies `/admin/api` to `127.0.0.1:8081`, so the browser stays on one origin and
the relay sees the loopback `Origin` its default `allowed_origin` expects. What
you do have to choose is the data source. Plain `pnpm dev` reads built-in mock
data; to browse the relay you just started, run the dev server with the
production data source:

```
# bash / zsh, repository root
VITE_DATA_SOURCE_MODE=production pnpm --filter @promptdock/relay-admin dev
```

```powershell
# PowerShell, repository root
$env:VITE_DATA_SOURCE_MODE = "production"
pnpm --filter @promptdock/relay-admin dev
```

Then open the URL Vite prints (by default `http://localhost:5173`). Reads work
from either `localhost` or `127.0.0.1`; writes additionally require
`[admin].allowed_origin` to equal the exact origin the browser sends — the local
example leaves it at the default `http://127.0.0.1:5173`.

With the config above every write answers `403 ADMIN_READ_ONLY`, because the
admin API is in `read_only` mode. That is the intended default; see
`SECURITY.md` for what `operator` would add.

## Run the desktop host locally

```
pnpm tauri:dev     # from the repository root
```

Requires WebView2 on Windows. On non-Windows hosts the Tauri dev workflow
falls back to a browser window — useful for UI iteration but not a substitute
for a real Windows smoke test.

## What "not run" means

The public test suite covers contract SHA drift, unit behaviour, and the
HTTP/gateway wire surfaces against frozen fixtures. It does not, in a public
PR run, exercise:

- A live Codex desktop installation with real Hook files.
- A real WeChat bot sending real notifications.
- A production server deployment against real traffic.

Where docs claim "supported but not exercised in CI", treat that as an
accurate statement, not a promise.
