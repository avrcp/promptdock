$ErrorActionPreference = "Stop"
$repositoryRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Push-Location $repositoryRoot
try {
    # 1. Install the whole monorepo from one lockfile.
    & corepack pnpm@11.10.0 install --frozen-lockfile
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 2. Contract drift gate.
    & "$repositoryRoot/scripts/contracts/api-contract-manifest.ps1" check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & "$repositoryRoot/scripts/contracts/admin-api-contract-manifest.ps1" check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & "$repositoryRoot/scripts/contracts/gateway-contract-manifest.ps1" check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 3. Public-tree hygiene gate: refuses credential shapes, personal-data forms,
    #    runtime data files and audit scaffolding anywhere in the shipped tree. The
    #    self-test runs the same gate against planted violations, because a scan that
    #    cannot fail proves nothing.
    node scripts/oss/check-public-tree.mjs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    node scripts/oss/selftest-check-public-tree.mjs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 4. Rust workspace gates.
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo metadata --locked --format-version 1 --no-deps | Out-Null
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo test --locked --workspace --all-targets
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo build --locked --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 5. Server-side ops gates.
    & bash "scripts/server/privacy-verify.sh"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & bash "scripts/server/test-deploy-assets.sh"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    python scripts/server/test-release-metadata.py
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    python scripts/server/check-crate-boundaries.py
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 6. JS-side quality.
    & corepack pnpm@11.10.0 contracts:check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & corepack pnpm@11.10.0 lint
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & corepack pnpm@11.10.0 typecheck
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & corepack pnpm@11.10.0 test
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & corepack pnpm@11.10.0 design:check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & corepack pnpm@11.10.0 build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 7. Admin dist integrity. The verifier only accepts the production admin
    #    build (no source maps, no mock data source), so rebuild the admin SPA in
    #    production mode first; the default `pnpm build` above emits the
    #    mock-capable artifact. build-production sources the release commit from
    #    git HEAD and requires a clean tree (set PROMPTDOCK_ALLOW_DIRTY_BUILD=1
    #    and VITE_BUILD_COMMIT for a non-git/archived build).
    & corepack pnpm@11.10.0 --filter "@promptdock/relay-admin" build:production
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    node scripts/server/verify-admin-dist.mjs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # 8. Git whitespace check when this is a Git checkout.
    & git rev-parse --is-inside-work-tree 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) {
        & git diff --check
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
} finally {
    Pop-Location
}
