# Testing

## What runs in CI

Three workflow files live in `.github/workflows/`. None of them needs a real
WeChat account, a Codex installation or a production host — the next section
lists what that leaves untested.

| Workflow → job | What it runs |
|---|---|
| `ci-relay.yml` → Workspace quality | `bash scripts/quality/quality.sh` |
| `ci-relay.yml` → Container images | `docker build --target runtime` and `--target admin-caddy-runtime` from the shipped `deploy/docker/Dockerfile`, then asserts the relay binary reports the commit it was built from and that both images refuse to run privileged |
| `ci-relay.yml` → Shell lint | shellcheck over the shipped scripts, plain and with `-x` |
| `ci-desktop.yml` → Web layer and Tauri crate | `corepack pnpm quality:desktop` on Windows, plus `apps/desktop/packaging/verify-versions.ps1` |
| `security.yml` → Public-tree hygiene gate | `node scripts/oss/check-public-tree.mjs` |
| `security.yml` → Secret history scan | gitleaks over full history with `.gitleaks.toml` |
| `security.yml` → Dependency license policy | `cargo deny check licenses bans sources` for both Cargo graphs, and `node scripts/oss/check-license-policy.mjs` for the Node graph |
| `security.yml` → Whitespace | `git diff --check` against the PR merge base |

The individual gates behind those groups, and what each one tolerates:

| Gate | Command | Notes |
|---|---|---|
| JS unit | `pnpm -r test` | Vitest across `apps/desktop` and `apps/admin` |
| HTTP `/v1` contract | `pnpm contracts:check` | The desktop-side check of `/v1` only; the other two contracts are covered by the manifest scripts below |
| Contract SHA drift | `bash scripts/contracts/{api,admin-api,gateway}-contract-manifest.sh check` | One per contract |
| ESLint | `pnpm -r lint` | Fails on errors. 546 pre-existing style warnings, all in `apps/admin`, do not fail it |
| TypeScript | `pnpm -r typecheck` | `vue-tsc --noEmit` |
| Design tokens | `pnpm design:check` | SSOT gate, run in both Vue apps |
| Rust fmt | `cargo fmt --all -- --check` | Zero diff |
| Rust unit + integration | `cargo test --locked --workspace --all-targets` | Relay graph, including the contract fixture and manifest tests |
| Rust clippy | `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Zero warnings |
| Desktop Rust | `corepack pnpm quality:desktop` | `apps/desktop/src-tauri` only — that crate is not in the Cargo workspace |
| Admin dist integrity | `node scripts/server/verify-admin-dist.mjs` | Post-build verification |
| Deployment asset schema | `bash scripts/server/test-deploy-assets.sh` | systemd unit / compose / Caddyfile |
| Release metadata | `python3 scripts/server/test-release-metadata.py` | |
| Crate boundary check | `python3 scripts/server/check-crate-boundaries.py` | Enforces `relay-*` crate imports |
| Git whitespace | `git diff --check <base> <head>` | No trailing whitespace or space-before-tab *introduced by the change*. Eight `contracts/node-link/v5/*.json` fixtures carry a trailing blank line from their donor; a diff-scoped check leaves them alone, and normalising them would re-pin the fixture digests in `manifest.json` without any wire change to show for it |

`scripts/quality/quality.{sh,ps1}` is the aggregate that most of the table above
belongs to: the install, the three manifest checks, the public-tree gate, the
whole Rust graph, the server ops gates, the JS gates, the admin production build
and its verifier, and the whitespace check. Its whitespace step is a bare
`git diff --check`, so on a clean CI checkout it inspects nothing and only
catches uncommitted edits in a developer's own tree; `security.yml` carries the
merge-base form. The aggregate does not include the container
image builds, shellcheck, gitleaks, the two license gates or the desktop Rust
suite — those are separate jobs, and `corepack pnpm security:check` covers the
gate plus the Node license policy locally.

## What does NOT run in CI (and why)

- **Real Codex desktop Hook installation.** Requires a Windows desktop
  install with Codex present. Only the synthetic hook trust fixture at
  `apps/desktop/fixtures/upstream/openai-codex-hook-trust-9688359/` is
  exercised.
- **Real WeChat bot sending real notifications.** The provider is
  exercised through `crates/wechat-ilink`'s wire-level tests and
  `apps/server/tests/wechat_public_surface.rs` against synthetic fixtures.
- **Production deployments against real traffic.** Deployment scripts are
  tested by `test-deploy-assets.sh` and `test-atomic-deploy-layout.sh`, but
  no live deploy happens in CI.
- **Windows Tauri bundling.** The desktop CI job compiles, lints and tests the
  Tauri crate on Windows, but it does not run `tauri build`, and it does not
  launch the resulting window. `packaging/smoke-desktop-startup.ps1` proves a
  built exe reaches `--version`; that smoke run and the bundling step are manual.
- **Admin end-to-end.** `apps/admin` defines Playwright suites
  (`test:e2e`, `test:e2e:integration`); CI builds the production bundle and
  verifies its contents, but does not drive a browser.

When you run a manual test that CI does not, add a line to `CHANGELOG.md`'s
`### Not yet shipped` section explaining what was and wasn't exercised.

## Recording what you ran

The PR template (`.github/pull_request_template.md`) asks for a list of commands
you ran locally.
Answer honestly. If you didn't run `pnpm design:qa` because you didn't
change visuals, say "design:qa NOT RUN — no visual changes." Do not fabricate
a PASS.

## Isolated data directories

Every test that touches the desktop's local DB or inbox uses a temp directory
set via `PD_DESKTOP_DATA_DIR`. Never point a test at your real
`%LOCALAPPDATA%\promptdock-desktop\`. See
[../desktop/local-data.md](../desktop/local-data.md).

Server tests use `tempfile::TempDir` for the SQLite file. Docker-based tests
(`schema_upgrade_docker.rs`) create a disposable container.

## Contract fixture tests

`api_contract_fixtures.rs`, `admin_api_contract_fixtures.rs`, and
`gateway_protocol_fixtures.rs` iterate every fixture under the corresponding
`contracts/*/` directory and assert the Rust types round-trip through them
byte-for-byte. These tests are the reason we can freeze a wire surface
across an internal-to-public transition.

## Coverage tooling

No coverage number is a gate. The JS workspace does not install a Vitest coverage
provider, so `vitest --coverage` needs `@vitest/coverage-v8` added first;
`cargo llvm-cov` is likewise not a dependency of this repository. Both are
available to a contributor who wants them, and neither is what CI measures.

## Secret and forbidden-pattern scanning

Two independent engines, plus a license layer, all wired into CI.

**`scripts/oss/check-public-tree.mjs`** walks every file in the repository,
skipping only build and package-manager directories (`node_modules/`, `target/`,
`dist/`, `.git/`, `.pnpm-store/`). It refuses two things:

- **Paths**, for every file name it finds — including binaries and files too
  large to read: private working directories, credential material (`.pem`,
  `.key`, `.pfx`, `.p12`, `.dpapi`, `.enc`, OpenSSH key files), runtime
  databases and their WAL/SHM/journal siblings, run logs and traces, and
  environment files other than `.env.example`.
- **Content**, for text files: credential shapes (PEM private-key blocks, AWS
  access key IDs, GitHub tokens, `sk-` style API keys, `pdv2.` device tokens,
  `Authorization: Bearer` headers, and long literals assigned to a
  `secret`/`token`/`password`/`api_key`-style key), personal-data shapes
  (mainland-China mobile numbers, email addresses outside the reserved
  placeholder domains), and environment shapes described generically: an
  absolute path rooted in a developer machine layout, a per-user home
  directory, an IPv4 literal outside the loopback, private, link-local,
  shared and documentation ranges.

Every rule is a shape, not a value. The database carries no exact
infrastructure baseline — no host name, address, account, working directory,
retired package name or donor revision belonging to any particular deployment —
and it does not keep one in disguise either: the scanner refuses a pattern that
declares its signature in a base64 or otherwise encoded field, because a
reversible encoding is not a redaction. `scripts/oss/selftest-check-public-tree.mjs`
plants such a field and a personal path and asserts the gate goes red, so the
paragraph you are reading is checked rather than promised. Validating a specific
deployment against its own baseline belongs to private release preparation,
outside the published tree.

Exceptions are per-pattern, per-file allowlists declared in
`scripts/oss/patterns.json`; there is no global ignore, and no exemption for the
scanner or its own data file — the pattern database is scanned like every other
file. The full list lives in that file and is deliberately not reproduced here,
so that copying this documentation cannot itself trip the gate.

If the gate reports a legitimate match, add that one file to that one pattern's
allowlist with a one-line justification in the `allowlistPolicy` field. Narrowing
a detector, or ignoring a directory, is not a fix.

**gitleaks** runs as a second, independent engine over the full git history in
the `security.yml` workflow, configured by `.gitleaks.toml`. Its allowlist covers
only the synthetic sentinels in the redaction tests and is written as patterns
rather than sentinel text, so this repository's own gate can scan gitleaks'
configuration without tripping over it.

**License policy** is `deny.toml` (cargo-deny, for both Cargo graphs) plus
`scripts/oss/check-license-policy.mjs` for the pnpm graph. The pnpm script splits
production from development dependencies: an unknown license in the production
graph fails outright, while a development-only one needs a recorded
justification in the script. cargo-deny is configured with `include-dev = true`,
so both Cargo graphs are checked across the whole tree.

Run the repository's own gate and the Node license policy together with
`corepack pnpm security:check`.
