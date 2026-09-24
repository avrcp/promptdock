# WeChat delivery

PromptDock's relay fans notifications out to WeChat through the iLink bot
protocol. The client implementation lives in `crates/wechat-ilink/` and is
derived from Tencent's `openclaw-weixin` (MIT, commit `cef0bfc3…`).

## Configuration

Two `[wechat]` blocks matter:

```toml
[wechat]
enabled = true
app_id = "…"
bot_token = "…"
connection_file = "/var/lib/promptdock-relay/wechat-connection.enc"

[wechat.scopes]
notify_write = true
notify_read_own = true
```

- `notify_write` is required to deliver notifications.
- `notify_read_own` lets the admin console show per-recipient delivery state.
- Anything beyond those two scopes is out of PromptDock's design; do not
  enable it.

`connection_file` is an encrypted iLink session token. It is written once
after the QR-code login flow (see below) and reloaded on service start. The
file is encrypted at rest using the master key loaded via
`LoadCredential=relay-master-key` from the systemd unit; deleting the master
key oracles the connection file.

## QR login

```
promptdock-relay wechat login --config /etc/promptdock-relay/config.toml
```

Prints a QR code to the terminal. Scan with WeChat, confirm, and the CLI
persists the encrypted connection file. This is an interactive flow and is
deliberately not automated in CI.

## Delivery pipeline

1. Desktop POSTs a `NotificationCreate` to the relay's HTTP `/v1` API.
2. Relay stores the notification and enqueues it for the WeChat provider.
3. The provider sends the message via iLink, then records the delivery as
   `pending` in the outbox table.
4. Once the platform acknowledges, the delivery transitions to `delivered`.
5. If the delivery is a FullFinal, the notification carries a single
   `result_pages_v1` link, not a segmented bundle.

## Delivery hold

The desktop tray offers a "hold dispatch for N minutes" toggle. While active,
the desktop's sync worker stops pushing new events to the relay but does not
delete them; on resume the inbox drains in order. This is a UX convenience
for the desktop operator; it does not affect relay-side fanout to other
devices.

## Failure modes we care about

- **Provider outage.** The outbox worker retries with exponential backoff up
  to `notification_expires_at`; after that, the delivery is marked expired
  and stops counting against retention.
- **Duplicate delivery.** iLink delivery is idempotent on `notification_id`.
  Do not change that identifier shape without a version bump.
- **Master key rotation.** Rotation is manual. Copy the master key,
  re-encrypt the connection file, restart. PromptDock does not currently
  support two-key live rotation.

## Testing

The provider's wire-level behaviour is exercised in
`apps/server/tests/wechat_public_surface.rs` against synthetic iLink
fixtures. Real delivery against a live WeChat bot is only exercised inside
the source environment.
