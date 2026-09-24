#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "$repository_root"
cargo test --locked -p promptdock-server --lib \
    outbox::tests::sensitive_terminal_redaction_scans_database_wal_and_shm -- --exact
printf 'Relay privacy DB/WAL/SHM scan passed\n'
