#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# 1. pnpm install must resolve the whole monorepo from a single lockfile.
corepack pnpm@11.10.0 install --frozen-lockfile

# 2. Contract drift gate. Each script under scripts/contracts/ verifies that
#    the manifest SHA matches the fixture bytes on disk.
bash scripts/contracts/api-contract-manifest.sh check
bash scripts/contracts/admin-api-contract-manifest.sh check
bash scripts/contracts/gateway-contract-manifest.sh check

# 3. Public-tree hygiene gate: refuses credential shapes, personal-data forms,
#    runtime data files and audit scaffolding anywhere in the shipped tree. The
#    self-test runs the same gate against planted violations, because a scan that
#    cannot fail proves nothing.
node scripts/oss/check-public-tree.mjs
node scripts/oss/selftest-check-public-tree.mjs

# 4. Rust fmt + workspace-wide test/lint/build.
cargo fmt --all -- --check
cargo metadata --locked --format-version 1 --no-deps > /dev/null
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo build --locked --release

# 5. Server-side ops gates (privacy, deploy assets, release metadata, crate
#    boundary check).
bash scripts/server/privacy-verify.sh
bash scripts/server/test-deploy-assets.sh
python3 scripts/server/test-release-metadata.py
python3 scripts/server/check-crate-boundaries.py

# 6. JS-side quality: contract validation, ESLint, TypeScript, Vitest,
#    design-token gate.
pnpm contracts:check
pnpm lint
pnpm typecheck
pnpm test
pnpm design:check
pnpm build

# 7. Admin dist integrity. The verifier only accepts the production admin build
#    (no source maps, no mock data source), so rebuild the admin SPA in
#    production mode before verifying it; the default `pnpm build` above emits
#    the mock-capable artifact. build-production sources the release commit from
#    git HEAD and requires a clean tree (set PROMPTDOCK_ALLOW_DIRTY_BUILD=1 and
#    VITE_BUILD_COMMIT for a non-git/archived build).
corepack pnpm@11.10.0 --filter @promptdock/relay-admin build:production
node scripts/server/verify-admin-dist.mjs

# 8. Git whitespace check when the tree is a Git checkout.
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git diff --check
fi
