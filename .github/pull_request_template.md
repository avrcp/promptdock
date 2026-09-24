## What changed

One or two sentences on the behavior this changes, and why it belongs in this
component rather than another. Link an issue if there is one.

## What you ran

List the commands you actually ran, with the result. If a gate does not apply,
say so instead of deleting the line — `not run, no visual changes` is a useful
answer and a claimed PASS that nobody measured is not.

```text
[ ] corepack pnpm quality          # relay + JS aggregate (Linux/macOS; needs pnpm + Rust)
[ ] corepack pnpm quality:desktop  # desktop web layer + Tauri crate (Windows)
[ ] corepack pnpm security:check   # public-tree gate + Node license policy
[ ] pnpm test / lint / typecheck / design:check
[ ] cargo fmt --all -- --check
[ ] cargo test --locked --workspace --all-targets
[ ] cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

## Contract changes

Does this touch `contracts/`, a generated manifest, or
`packages/admin-api-contract`? If yes:

- [ ] fixtures edited once, in `contracts/`, not copied into an application
- [ ] manifest regenerated with `scripts/contracts/*-contract-manifest.sh update`
- [ ] generated client regenerated with `pnpm contracts:generate-admin`
- [ ] `CHANGELOG.md` entry, and a version bump where the contract identity changed

## Notes for the reviewer

Anything a reader needs to judge this that the diff does not show: a platform you
could not test on, a gate that behaves differently in CI, a decision you would
have made differently under a different constraint.
