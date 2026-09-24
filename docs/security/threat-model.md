# Threat model

This is the working threat model as of `0.6.0-rc.1`. Anything not listed here is
outside the current design intent.

## Assets

- **Codex hook payloads.** Local run inputs and FullFinal text.
- **Local run inbox.** DPAPI-encrypted JSONL on Windows.
- **Server-side SQLite database.** Deliveries, devices, result pages.
- **WeChat iLink connection token.** Encrypted at rest on the server.
- **Master key and confirmation key.** Loaded from `LoadCredential=`.

## Attackers in scope

- Another user on the same Windows workstation (they can attempt to read
  your local inbox or DPAPI blob — DPAPI's per-user scope is the control).
- A network-position attacker on the LAN between desktop and relay (they see
  HTTPS only; anything weaker is a bug).
- A malicious notification recipient who tries to escalate via a
  result-page link (single-use token, no auth context, no cross-user reads).
- A compromised relay process (they already have DB and keys; the model
  assumes that's game-over and relies on process isolation).
- A rogue WeChat bot contact (they can only send into scopes the operator
  provisioned).

## Attackers out of scope

- A kernel-level adversary on the desktop workstation. If they can read your
  DPAPI key material they can read anything you can.
- A malicious Codex installation. If the Codex binary itself is tampered
  with, hook payloads are untrusted at the source and there is nothing
  PromptDock can do.
- Sub-version downgrade attacks on the OS or WeChat client. Keep them
  patched.

## Non-goals (design commitments)

- PromptDock does not execute remote commands.
- PromptDock does not approve Codex permission requests on the user's
  behalf, ever.
- PromptDock does not intercept or decrypt HTTPS traffic to or from the
  Codex desktop binary.
- PromptDock does not require an outbound internet connection to function
  locally. The relay can be reachable only from a LAN or a tailnet.
- PromptDock does not send the user's private working directory contents
  to the relay. The `cwd` field is a path string, not file bytes.

## Data-in-transit

- Desktop → relay: TLS 1.2+ (rustls on the client, axum behind Caddy in the
  reference deploy).
- Relay → WeChat iLink: TLS to the Tencent endpoints; the connection token
  is a bearer credential inside that channel.
- Admin console → relay: TLS; same HTTP API as the browser origin.

## Data-at-rest

- Windows: local inbox JSONL records are wrapped as `pdenc1:<base64>` where
  the base64 decodes to a DPAPI (CurrentUser scope) envelope. Only the same
  Windows user can decrypt. The `promptdock-protected-content` frame is
  versioned so a future change to the envelope format can be rejected
  cleanly.
- Server: SQLite file in WAL mode, no encryption at rest. Protect via
  filesystem permissions and, if needed, dm-crypt. The connection file
  containing the iLink bearer token is AES-GCM-encrypted with the master key.

## Cryptographic primitives in use

| Purpose | Primitive | Where |
|---|---|---|
| Local inbox envelope | Windows DPAPI (CurrentUser scope) | `apps/desktop/src-tauri/src/content_crypto.rs` via `platform/dpapi.rs` |
| Result page link token | HMAC-SHA256 over run ID, expiring | `apps/server/src/results.rs` |
| iLink connection file | AES-256-GCM | `crates/relay-provider-wechat/` |
| Content addressing | BLAKE3 | desktop inbox, relay retention |

No hand-rolled primitives. No deprecated ciphers.

## What we do to keep this honest

- Every notification field is either an opaque id or a hash unless the
  operator opted into FullFinal.
- Redaction is exercised by tests in
  `apps/desktop/src-tauri/src/notification/redaction.rs` and
  `apps/server/src/outbox/mod.rs` — synthetic AWS keys, bearer tokens and
  password literals assert that the scanner catches them.
- Retention worker deletes expired result pages, expired notifications and
  stale device scopes on a schedule. See
  `apps/server/tests/result_retention.rs`.

## Where to report a finding

`SECURITY.md`.
