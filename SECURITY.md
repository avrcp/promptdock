# Security policy

PromptDock runs a public HTTP relay and a Windows desktop host that installs Codex
Hooks. Both surfaces carry risk, so read this before you deploy.

## What this product does and does not do

- **Observes**, not intercepts. PromptDock attaches at Codex Hook boundaries
  (`UserPromptSubmit`, `Stop`, optional `PermissionRequest`). It never MITMs or
  decrypts traffic to or from the Codex desktop app.
- **Never executes tasks or approves permissions.** The desktop host and relay
  deliver notifications and read-only result pages. There is no remote agent,
  no command channel, no auto-approval.
- **Result pages are link-only.** FullFinal output uses `result_pages_v1` and
  one link per delivery. There is no segmented fallback bundle.
- **Local capture is scoped.** `capture-policy.json` is the sole durable policy
  authority. Reset tooling clears a fixed allowlist and preserves the policy
  file, so a mistaken reset does not silently re-enable capture.

## Supported versions

| Version | Supported |
|---|---|
| latest release | yes |
| previous minor | security patches only |
| anything older | no |

## Reporting a vulnerability

Email `security@` on the domain listed in the repo's GitHub Security tab. If
that is not available, open a **private security advisory** from the Security
tab. Do not file a public issue.

Include:

- The component (`apps/desktop`, `apps/server`, `apps/admin`, or a specific
  `crates/relay-*`).
- The wire surface involved (`contracts/server-http/v1`, `contracts/admin-api/v2`,
  `contracts/node-link/v5`).
- Steps or a PoC against the *default* configuration; anything requiring a
  non-default opt-in gets a lower severity by itself.

Expect an acknowledgement within one business week. We do not pay bounties.

## Hardening checklist for self-hosted deployments

- Bind the relay to a Unix socket behind Caddy/nginx, or to `127.0.0.1` with
  your own reverse proxy. Never expose the axum port directly.
- Rotate the master key and the confirmation key independently; both live under
  `/etc/promptdock-relay/` and are loaded via `LoadCredential=` from systemd.
- Keep `[admin].mode = "read_only"` — the compiled default — on any externally
  reachable deployment, unless device management, WeChat lifecycle management
  or retention maintenance is explicitly required. Read-only advertises only
  `admin_read_v2` and `admin_wechat_status_v2`, and `require_mutation_boundary`
  answers every method other than `GET`/`HEAD` with `403 ADMIN_READ_ONLY`.
- `operator` is the elevated mode, not the safer one. It expands the
  administrative capability surface: device create/rotate/enable/disable/revoke,
  result revocation, retention runs and — with the WeChat provider enabled —
  disconnect, test-send and the interactive login flow. Enable it only for
  trusted operators behind the documented authentication and network boundary.
- `operator` never hides anything. Both modes serve the same `/v2` read
  endpoints, the single exception being the WeChat login-session poll, which
  only exists under `operator`. If captured content must stay out of the
  console, restrict it with `capture-policy.json` on the desktop; `[admin].mode`
  is not a read filter.
- The mode is only the first gate. Under `operator` a request that is not
  `GET`/`HEAD` must also look like it came from the console: `Origin` has to
  equal `[admin].allowed_origin` exactly, `X-PromptDock-Admin-Action: 1` has to
  be present, a `Content-Type` has to be a single `application/json` value, and
  a `Sec-Fetch-Site` header, if the client sends one, has to say `same-origin`.
  Otherwise `403 ADMIN_MUTATION_FORBIDDEN`. The Admin SPA sets all of these, so
  you normally meet this boundary only when scripting writes yourself.
- Keep WeChat provider scopes to `notify:write` and `notify:read_own` unless
  you have a specific reason.
- Back up the SQLite file at `/var/lib/promptdock-relay/relay.db`, not the WAL
  or SHM siblings — those are transient.
- Set `filter = "warn,promptdock_server=warn"` in production. The default
  `info` level is fine for staging but includes identifiers you may prefer to
  keep out of long-lived logs.

## Not in scope for reports

- Findings that require physical access to the operator's workstation.
- Attacks that only affect a deployment that ignores the hardening checklist.
- Vulnerabilities in bundled third-party crates (`wechat-ilink`, `tauri`, `axum`,
  `sqlx`, ...) — those belong upstream; open an issue there and note the crate
  version we bundle.
