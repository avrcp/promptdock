# Local data lifecycle

The desktop host stores everything under `%LOCALAPPDATA%\promptdock-desktop\`.
This document is the authoritative list of what lives there and what a reset
clears.

## Layout

| Path | Contents | Cleared by `clear-local-data.cmd`? |
|---|---|---|
| `capture-policy.json` | The sole durable policy authority | **No** |
| `runtime.db` | SQLite inbox, delivery watermarks, acknowledgement state | Yes |
| `runtime.db-wal` | SQLite WAL sibling | Yes |
| `runtime.db-shm` | SQLite SHM sibling | Yes |
| `inbox/events.jsonl` | Local run inbox (DPAPI envelopes `pdenc1:<base64>`) | Yes |
| `inbox/events.jsonl.errors.log` | Structured capture errors | Yes |
| `hook-registration.json` | Local identifier binding hook entries to this install | Yes |
| `hook-target.json` | Absolute path the hook commands should target | Yes |
| `.reset-pending` | Marker written before reset, removed after | Yes |
| `logs/desktop.log` | Structured tracing output | Yes |
| `state/attention.json` | Attention acknowledgement watermarks | Yes |

The list is fixed and lives in `apps/desktop/scripts/clear-local-data.ps1`.
Anything new added to the data directory must be deliberately opted in.

## Why `capture-policy.json` is preserved

`capture-policy.json` is the sole durable authority that controls whether the
desktop observes turns, records start/end events, includes result content, or
notifies on attention. A user who resets their local data because the DB is
corrupt or a schema version drifted must not silently lose the policy they
configured. Reset tooling never touches that file.

If you need to change the policy, edit `capture-policy.json` directly or use
the "Notifications" view in the desktop UI.

## Reset flow

1. `clear-local-data.cmd` (in the bundle root, or as a copy in `scripts/`)
   prompts for confirmation. Pass `-Confirm` non-interactively.
2. Script writes `.reset-pending` with the value `pending`.
3. Script enumerates the fixed allowlist and deletes each entry that exists.
4. Script removes `.reset-pending`.
5. Script exits `0` on success, `23` if the user aborted at the prompt.

If the process crashes between step 2 and step 4, the next desktop start
refuses with `RUNTIME_RESET_INCOMPLETE`. Run the reset script again to
complete.

Do **not** delete `.reset-pending` by hand. Its purpose is to prove that the
reset ran to completion.

## Fresh isolated data for tests

Every desktop test that touches the DB creates a fresh temp directory and
sets `PD_DESKTOP_DATA_DIR` to it. Never reuse a user's real `%LOCALAPPDATA%`
path inside a test. The test at `apps/desktop/scripts/clear-local-data.test.ps1`
demonstrates the pattern.

## What reset does not clear

- Codex Hook entries in `%USERPROFILE%\.codex\hooks.json`. Those are removed
  explicitly by the "uninstall hooks" tray action. Reset alone leaves them
  pointing at the executable, which will fail noisily on the next prompt if
  you have also deleted the executable.
- Windows DPAPI master keys. Those are managed by Windows.
- Your `remoteEndpoint` config or trust-pin cache. Those live inside
  `runtime.db` and *are* cleared by reset. If you rely on a custom relay URL,
  note it before resetting.

## Backup guidance

`capture-policy.json` is the only file users normally want to back up. The
inbox and DB are ephemeral by design; a lost run can be re-executed.
