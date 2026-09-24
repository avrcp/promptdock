# Contract drift workflow

Every wire surface lives under `contracts/` and has a `manifest.json` that
records a SHA-256 for each fixture. CI fails on drift. This document covers
both what to do when you intentionally change a contract and what to do when
a drift check fails unexpectedly.

## The three manifests

| Contract | Regeneration script (bash) | PowerShell |
|---|---|---|
| HTTP `/v1` | `scripts/contracts/api-contract-manifest.sh write` | `scripts/contracts/api-contract-manifest.ps1 write` |
| Admin `/v2` | `scripts/contracts/admin-api-contract-manifest.sh write` | `scripts/contracts/admin-api-contract-manifest.ps1 write` |
| Node-link `/v5` | `scripts/contracts/gateway-contract-manifest.sh write` | `scripts/contracts/gateway-contract-manifest.ps1 write` |

Each script has a `check` mode that CI uses. The `write` mode regenerates
`manifest.json` from the fixture directory.

## Intentional change

1. **Edit the source.** For HTTP `/v1` and admin `/v2`, that means updating
   the OpenAPI Rust annotation on the server side. For node-link `/v5`, that
   means editing the frame JSON fixtures under
   `contracts/node-link/v5/`.
2. **Regenerate the manifest.**
   ```
   bash scripts/contracts/api-contract-manifest.sh write
   ```
3. **Regenerate derived artifacts.** For admin, also run:
   ```
   pnpm contracts:generate-admin
   ```
   This regenerates the TypeScript client under
   `packages/admin-api-contract/`.
4. **Run every gate.**
   ```
   pnpm contracts:check
   pnpm test
   cargo test --locked --workspace --all-targets
   ```
5. **Commit the source, fixtures, manifest, generated client, and a
   `CHANGELOG.md` entry as one changeset.** Half-updates are the number one
   cause of "the drift gate failed on merge" tickets.

## Unintentional drift

If `pnpm contracts:check` fails on your branch:

1. Look at the exact fixture that changed. The gate prints the path and both
   expected and actual SHAs.
2. Ask: did you or a recent merge change that fixture? If yes, go back to the
   "intentional change" workflow.
3. If no, look for accidental whitespace or newline changes. The manifests
   compute SHA-256 over the file bytes; a trailing-newline edit counts.

Do **not** hand-edit `manifest.json` to accept a SHA you didn't intend.
That is the mechanical proof that CI is doing its job.

## Version bumps

Within a major, contracts are additive only. Removing a field, renaming a
type, or changing a required-to-optional relationship requires a major bump
and coordinated changes to:

- `contracts/<surface>/manifest.json` (`"contract"` and `"version"` fields)
- The Rust-side contract identity in the corresponding crate
  (e.g. `crates/admin-api-contract/src/lib.rs::CONTRACT_NAME`).
- Every fixture inside the surface.
- Server tests under `apps/server/tests/*_contract_fixtures.rs`.
- The admin console's generated client.

Because that touches so much, major bumps are done as their own dedicated
PR, not folded into a feature.

## What's *not* a contract change

Modifying a `--describe` doc comment, log line, or error message text does
not drift the manifest. Those surfaces are outside the SHA calculation.

Adding a new endpoint to the OpenAPI **is** a contract change (additive,
so within-major is fine) — regenerate the manifest.

## Fixture provenance

Every fixture inside `contracts/server-http/v1/` is a captured wire-level
example: an OpenAPI-generated `request.json` paired with a
`response.json`. They are not hand-written and they do not correspond to any
real user session. Treat them as canonical test data.

The 16 HTTP v1 fixtures hash to manifest SHA-256 `6fee4061…` — that SHA is
the version of the contract identity in the desktop validator.
