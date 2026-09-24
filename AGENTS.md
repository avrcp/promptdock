# PromptDock Agent Guide

Rules that stay true across the whole repository, for humans and for automated
agents working in it. Read [ARCHITECTURE.md](ARCHITECTURE.md) first if you are
changing where a responsibility lives.

## Repository layout

| Path | What it is |
|---|---|
| `apps/desktop/` | Windows host: Tauri 2 shell, Rust crate `promptdock-desktop`, Vue front-end |
| `apps/server/` | Relay server: crate `promptdock-server`, binary `promptdock-relay` |
| `apps/admin/` | Operator console: Vue SPA, Admin `/v2` client |
| `crates/` | Relay's Rust crates: domain, application, storage, transports, WeChat provider |
| `contracts/` | Canonical wire fixtures for HTTP `/v1`, Admin `/v2`, node link `/v5` |
| `packages/` | Generated JS artifacts |
| `deploy/` | Docker, Caddy and systemd assets for self-hosting |
| `docs/` | Topic documentation; `docs/README.md` is the index |
| `scripts/` | Contract, quality, OSS-hygiene and server operations scripts |

The desktop crate is excluded from the Cargo workspace on purpose. Do not add it
as a member: it is Windows-only and would tie the relay's dependency lockfile to
Tauri's.

## Architectural invariants

1. **Relay never executes user code.** No command runner, no build/test
   invocation, no agent loop in the server. If a change needs the server to run
   something, the change belongs on the desktop side.
2. **Desktop does not own the WeChat lifecycle.** The provider session, its
   credentials and its retry policy live in Relay. Desktop posts events and reads
   delivery state.
3. **`contracts/` is the canonical protocol source.** Both sides of every wire
   surface reference it; neither keeps a private dialect.
4. **Do not duplicate protocol fixtures across applications.** A second copy of a
   fixture is a second source of truth that can pass while the real one drifts.
5. **Credentials must never be logged, echoed or persisted in plaintext** — not
   device tokens, verification codes, session or webhook URLs, raw hook payloads,
   encrypted envelopes, or sync cursors. Redaction is behavior with tests
   (`apps/desktop/src-tauri/src/notification/redaction.rs`); extend those tests
   rather than working around them.
6. **No production credentials or real user data in tests.** Fixtures are
   synthetic, and tests run against fresh isolated stores.
7. **Generated files are generated, not hand edited.** Edit the generator or the
   source contract, then regenerate.

Anything new inside the desktop data directory must be opted into the reset
allowlist in `apps/desktop/scripts/clear-local-data.ps1` deliberately; `capture-policy.json`
is preserved by a reset and must not become clearable as a side effect.

## Validation

Run the checks that cover the area you touched. GitHub Actions runs these same
scripts, so a local failure is a CI failure.

**Whole repository**

```sh
bash scripts/quality/quality.sh
```

**Relay changes** (Linux or any host with the pinned toolchain)

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets
```

**Desktop changes** (Windows)

```sh
corepack pnpm install --frozen-lockfile
corepack pnpm quality:desktop
```

That runs `cargo fmt`, `cargo metadata --locked`, `clippy -D warnings` and the
test suite against `apps/desktop/src-tauri` only. Do not substitute
`cargo test --workspace`: the desktop crate is not in that workspace.

**Contract changes**

```sh
bash scripts/contracts/api-contract-manifest.sh check
bash scripts/contracts/admin-api-contract-manifest.sh check
bash scripts/contracts/gateway-contract-manifest.sh check
corepack pnpm contracts:check
node scripts/contracts/check-admin-contract.mjs
```

To accept an intentional change, regenerate instead of editing a manifest:

```sh
bash scripts/contracts/api-contract-manifest.sh update
node scripts/contracts/generate-admin-api-contract.mjs
```

**Security and license hygiene**

```sh
corepack pnpm security:check
```

## Testing requirements

- A test that needs a real Codex installation, a live WeChat account or a
  production host is not a repository test. Say what is not covered rather than
  simulating that condition.
- Synthetic data means synthetic *everywhere* in the test: identifiers, phone
  numbers, URLs, timestamps and file paths.
- Desktop hook behavior is verified against the pinned upstream fixture in
  `apps/desktop/fixtures/upstream/`; changing it means re-pinning to an upstream
  revision, not adjusting an expectation until it passes.
- Unexecuted checks are reported as unexecuted. A passing adjacent test is not
  evidence for the one you did not run.

## No-secret policy

Nothing secret is committed, and the tree says so out loud:

- `.gitignore` excludes environment files, key material, runtime databases and
  logs.
- `scripts/oss/check-public-tree.mjs` refuses those shapes as tracked paths, and
  refuses live credential literals by content — including in its own pattern
  database. The rules in that database are generic, public-safe shapes only.
  An environment-specific identifier must never be stored here even in encoded
  form, because base64 is a reversible encoding rather than a redaction; the
  scanner rejects an encoded pattern field so that the rule is enforced instead
  of merely advised. What a given deployment calls its hosts and directories is
  audited when that deployment is prepared, outside this repository.
- `scripts/oss/selftest-check-public-tree.mjs` runs the gate against planted
  violations and a planted encoded database, so every claim above stays a
  measured one instead of a description of intent.
- `.gitleaks.toml` adds an independent engine, and its allowlist is written as
  patterns rather than sentinel text.
- Allowlists are per-file and per-detector. There is no global ignore, and no
  exemption for the scanner or its data.

Placeholder configuration — `deploy/examples/`, `deploy/docker/*.toml`,
`deploy/caddy/Caddyfile.example` — carries no real host, IP, account or token.
If you touch deployment configuration, keep it that way.

## Changes this repository is not for

The layout, the framework choices and the version scheme are settled. Please do
not open a change that rewrites the module structure, swaps Tauri or the web
framework, unifies the relay and desktop version lines, redesigns the UI, or
scaffolds an integration that has no implementation behind it.
