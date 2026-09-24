# Contributing

Thanks for taking a look. This project is a fresh public extraction from an
internal monorepo, so a lot of the surrounding assumptions are documented here
rather than in Git history.

## Ground rules

- Do not open a PR that changes a contract under `contracts/` without a
  matching entry in `CHANGELOG.md` and a version bump. Contract drift is
  detected mechanically (`pnpm contracts:check`, `cargo test --test
  api_contract_manifest`, `--test gateway_contract_manifest`) and will fail CI.
- Do not rename shipped schema identities (e.g. `promptdock-relay-v4`).
  Those values are written to the database and asserted in retention tests.
- Follow the trust boundary in `SECURITY.md`. PromptDock is a notification
  bridge, not a task runner. Anything that could approve permissions, execute
  tasks, or decrypt traffic is out of scope and will not merge.

## Dev environment

- Node 24.18.0 (per `engines` in root `package.json`, and the top-level
  `.node-version` file).
- pnpm 11.10.0 (installed via `corepack enable pnpm`).
- Rust stable 1.98.0 (pinned by `rust-toolchain.toml`).
- For the Tauri desktop app on Windows: WebView2 runtime and the Visual Studio
  Build Tools.

`pnpm install` and `pnpm -r build` are the only two commands to bootstrap the
JS surface. `cargo build` covers the Rust side from the root workspace.

## Running the test suite

```
pnpm contracts:check            # desktop-side HTTP /v1 contract check
pnpm test                       # Vitest: desktop + admin unit tests
pnpm lint                       # ESLint
pnpm typecheck                  # vue-tsc --noEmit
pnpm design:check               # design-token SSOT gate

cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

Desktop-only work has its own aggregate, which is what the Windows CI job runs:

```
corepack pnpm quality:desktop   # fmt + metadata + clippy + test for src-tauri only
```

The three manifests and the generated admin client are checked by
`scripts/contracts/*-contract-manifest.sh check` and
`node scripts/contracts/check-admin-contract.mjs`; `bash
scripts/quality/quality.sh` runs all of them.

What CI actually runs lives in `.github/workflows/`:

- `ci-relay.yml` — the `quality.sh` aggregate, both container image targets with
  their unprivileged-runtime assertions, and shellcheck over the shell scripts
- `ci-desktop.yml` — the desktop web layer and the Tauri crate on Windows
- `security.yml` — the public-tree hygiene gate, gitleaks over full history, the
  Cargo and Node license policies, and a whitespace check on the merge base

The Linux shell scripts under `scripts/quality/` are the aggregate entry
points CI uses. They call every gate above plus contract manifest regeneration
and the deployment-asset checks.

## Contract drift workflow

Every contract has a `manifest.json` with a SHA-256 over each fixture file.
When you intentionally change a wire surface:

1. Edit the OpenAPI or wire fixture under `contracts/`.
2. Regenerate the manifest: `bash scripts/contracts/api-contract-manifest.sh
   update` (or the matching `admin-api-` / `gateway-` variant).
3. Regenerate the TypeScript client for the admin console: `pnpm
   contracts:generate-admin` from the root.
4. Commit the manifest, the fixtures, the generated client, and a
   `CHANGELOG.md` entry together.

If the manifest check fails locally with an unexpected SHA drift, you edited a
fixture without regenerating. Do not paper over it by pinning the old SHA.

## Testing the desktop host

Desktop packaging on Windows produces a single portable exe. The public
tree ships `apps/desktop/packaging/verify-versions.ps1` and
`apps/desktop/packaging/smoke-desktop-startup.ps1`. Full release attestation
(checksummed bundle, manifest schema, cross-repo commit pin) is intentionally
**not** part of the public tree; if you want to build a signed Windows
release, you will need to compose your own pipeline around those scripts.

The desktop Codex Hook integration is exercised end-to-end inside the source
environment via a synthetic Codex fixture
(`apps/desktop/fixtures/upstream/openai-codex-hook-trust-9688359/`) and
Rust integration tests. Do not attempt to install real Codex Hooks against a
live desktop install from a public PR run.

## Commits and pull requests

- Conventional commits (`feat:`, `fix:`, `chore:`, `docs:`, `perf:`,
  `refactor:`, `test:`). Scope is optional but encouraged when the change
  crosses app boundaries (`fix(desktop):`, `feat(server/admin):`).
- One logical change per PR. If you must rename a crate and update its
  tests, that's still one change.
- Include the exact commands you ran locally in the PR description so a
  reviewer can reproduce. Type "not run" for anything you skipped — a
  fabricated PASS wastes reviewer time.

## Issue intake

- Bug: repro command, expected vs actual, environment (`node --version`,
  `pnpm --version`, `cargo --version`, OS).
- Feature: use case first, then interface. PRs that lead with an implementation
  and skip the use-case narrative close with "not planned" more often than
  you'd think.
- Question: check `docs/` first. If it's not there, that's a bug — file an
  issue describing what was missing.

## Code of conduct

Adopted from the Contributor Covenant 2.1; see `CODE_OF_CONDUCT.md`.

## License

By contributing you agree that your contribution is licensed under the Apache
License, Version 2.0, and you confirm you have the right to grant that license.
A contribution that brings in third-party code must record its upstream,
version and license in `THIRD_PARTY_NOTICES.md` in the same pull request; an
unattributed copy-paste blocks the merge.
