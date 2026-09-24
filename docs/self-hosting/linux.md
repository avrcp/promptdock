# Self-hosting on Linux

Reference deployment: Ubuntu 24.04 LTS, single-tenant relay + admin SPA
behind Caddy.

## Layout

| Path | Purpose |
|---|---|
| `/opt/promptdock-relay/releases/<version>/` | Immutable release bundles |
| `/opt/promptdock-relay/current` | Symlink to active release |
| `/etc/promptdock-relay/config.toml` | Runtime config, mode `0640` |
| `/etc/promptdock-relay/master.key` | Loaded via systemd `LoadCredential` |
| `/etc/promptdock-relay/confirmation.key` | Same |
| `/var/lib/promptdock-relay/` | State dir: SQLite file, WAL, connection file |
| `/var/backups/promptdock-relay/` | Where `backup-ubuntu.sh` writes |
| `/etc/systemd/system/promptdock-relay.service` | Unit file |

The service account is `promptdock-relay:promptdock-relay`. It is
non-loginable and owns nothing outside `/var/lib/promptdock-relay` and
`/etc/promptdock-relay`.

## Install

The server has no bundled release publisher; the `*-ubuntu.sh` scripts install
from a **release-bundle directory** you assemble in your own CI. The bundle is a
directory with the layout `deploy/systemd/promptdock-relay.service`,
`deploy/config.production.toml`, `deploy/caddy/Caddyfile.example`,
`bin/promptdock-relay`, `admin/`, `contracts/…`, `sbom/…`, a schema-v4
`RELEASE-MANIFEST.json` and a matching `SHA256SUMS.txt`; the Admin console
README points at the generated client it needs. `scripts/server/lib/validate-release-bundle.py`
is the authoritative checker for that layout.

Build the binary from source:

```
cargo build --locked --release --bin promptdock-relay
```

Copy the bundle to the target host and run the installer with its absolute
path:

```
sudo bash scripts/server/install-ubuntu.sh --release-bundle /abs/path/to/bundle
```

`install-ubuntu.sh`:

1. Creates the service account if missing.
2. Unpacks the bundle to `/opt/promptdock-relay/releases/<version>-<commit>/`.
3. Writes `/etc/systemd/system/promptdock-relay.service`.
4. Runs `systemctl daemon-reload`.
5. Refuses to overwrite an existing `/etc/promptdock-relay/config.toml`.

Then initialise the database:

```
sudo -u promptdock-relay /opt/promptdock-relay/current/bin/promptdock-relay init
```

Start the service:

```
sudo systemctl enable --now promptdock-relay.service
```

## Master and confirmation keys

Both must exist before the service starts. Generate them:

```
sudo install -o root -g promptdock-relay -m 0640 /dev/null /etc/promptdock-relay/master.key
sudo install -o root -g promptdock-relay -m 0640 /dev/null /etc/promptdock-relay/confirmation.key
sudo openssl rand -hex 32 | sudo tee /etc/promptdock-relay/master.key >/dev/null
sudo openssl rand -hex 32 | sudo tee /etc/promptdock-relay/confirmation.key >/dev/null
```

The systemd unit loads them via `LoadCredential=` so the process sees them
on a tmpfs, not the real file.

## Admin console access mode

`deploy/examples/config.production.toml` ships `[admin].mode = "read_only"`,
which is also the compiled default. Keep it. Read-only serves every console read
and rejects each write with `403 ADMIN_READ_ONLY`.

Switch it to `"operator"` only when you actually intend to rotate or revoke
devices, revoke a result page, run retention from the console, or drive the
WeChat login/disconnect/test flow from it — `operator` only widens the write
surface, it does not restrict what the console can read. `[admin].bind` is
validated to a loopback address in both modes, so the console stays reachable
only through the Caddy site below. Writes are gated a second time by request
headers the SPA already sends; `SECURITY.md` lists them.

## Caddy reverse proxy

Copy `deploy/caddy/Caddyfile.example` to `/etc/caddy/Caddyfile.d/promptdock`
and adjust the site host. The example listens on `admin.example.com` and
proxies to `127.0.0.1:8080` (the axum server). It also serves the admin SPA
statically from `/opt/promptdock-relay/current/admin/`.

Reload Caddy: `sudo systemctl reload caddy`.

## WeChat login

Interactive once, right after install:

```
sudo -u promptdock-relay /opt/promptdock-relay/current/bin/promptdock-relay wechat login
```

Scan the QR with WeChat. The connection file lands in
`/var/lib/promptdock-relay/wechat-connection.enc`, encrypted under the master
key.

## Upgrade

`scripts/server/upgrade-ubuntu.sh` unpacks a new bundle, atomically swaps
`current`, runs migrations, restarts the service, and rolls back automatically
if the health check fails within 30 seconds.

## Backup / restore / rollback

- `scripts/server/backup-ubuntu.sh` — VACUUM INTO a snapshot file plus
  `config.toml` and both keys (the keys are stored in a separate file with
  mode `0400` for you to move off-host).
- `scripts/server/restore-ubuntu.sh` — restores a snapshot; refuses if the
  running binary version is older than the snapshot's version.
- `scripts/server/rollback-ubuntu.sh` — flips `current` back to a prior
  release directory and restarts.

## Health

- `systemctl status promptdock-relay` — service state.
- `journalctl -u promptdock-relay -n 200` — recent logs. Default filter is
  `info,promptdock_server=info`; production should override to `warn`.
- `/health` — plain liveness, returns `ok`.
- `/status` — secret-free operational snapshot as JSON.

See [../development/testing.md](../development/testing.md) for what's
exercised in CI vs. what needs manual verification.
