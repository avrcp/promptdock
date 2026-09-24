#!/usr/bin/env bash
# Desktop-only quality aggregate: the Windows host's web layer and its
# workspace-excluded Tauri crate.
#
# The relay aggregate (quality.sh) cannot cover this crate. It is listed under
# `exclude` in the root Cargo.toml, so `cargo test --workspace` never compiles it,
# and its dependency graph is deliberately separate so a Tauri upgrade cannot drag
# the relay lockfile with it. Run from the repository root.
set -euo pipefail

ROOT="$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

DESKTOP_MANIFEST="apps/desktop/src-tauri/Cargo.toml"

# 1. Install the whole monorepo from a single lockfile: the desktop web layer and
#    the admin console share pnpm-lock.yaml, so this also proves the two graphs are
#    still consistent.
corepack pnpm@11.10.0 install --frozen-lockfile

# 2. Desktop web layer. `contracts:check` validates the desktop consumer against
#    the canonical contract in contracts/, which is the cross-boundary gate that
#    used to need a vendored snapshot copy.
corepack pnpm@11.10.0 --filter @promptdock/desktop lint
corepack pnpm@11.10.0 --filter @promptdock/desktop typecheck
corepack pnpm@11.10.0 --filter @promptdock/desktop test
corepack pnpm@11.10.0 --filter @promptdock/desktop design:check
corepack pnpm@11.10.0 --filter @promptdock/desktop contracts:check
corepack pnpm@11.10.0 --filter @promptdock/desktop build

# 3. Desktop Rust crate. fmt first: it is the cheapest way to tell a contributor
#    that a change needs `cargo fmt` before the compile. The clippy invocation
#    matches the one the desktop crate was verified with: `--all-targets`, no
#    `--all-features`, because the crate is Windows-only and its optional features
#    are not a supported build shape.
cargo fmt --manifest-path "$DESKTOP_MANIFEST" --check
cargo metadata --locked --format-version 1 --no-deps --manifest-path "$DESKTOP_MANIFEST" >/dev/null
cargo clippy --locked --manifest-path "$DESKTOP_MANIFEST" --all-targets -- -D warnings
cargo test --locked --manifest-path "$DESKTOP_MANIFEST" --all-targets
