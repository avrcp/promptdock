# Self-hosting via Docker

Two production-shaped images ship from `deploy/docker/`:

- `promptdock-relay:dev` — the server binary, listening on container port
  `8080`; Compose publishes it on `127.0.0.1:18080` (override with
  `PROMPTDOCK_RELAY_PORT`).
- `promptdock-relay-admin:dev` — a standalone admin image for teams that want
  the SPA served separately by Caddy.

`scripts/server/docker-verify.ps1` exercises these images end to end:
`schema_upgrade_docker.rs` seeds a legacy store on a named volume and is run from
that script, never from an ordinary `cargo test`.

## Compose topology

`deploy/docker/compose.yaml` defines five services. Only `relay` is always
available; the rest are behind a Compose profile.

- `relay` — on by default; mounts the `relay-data` volume at
  `/var/lib/promptdock-relay` and loads `/etc/promptdock-relay/config.toml`
  from the image.
- `relay-wechat` — profile `wechat-bootstrap`; the same image running
  `serve --config /etc/promptdock-relay/wechat.toml`, publishing
  `127.0.0.1:18081` (override with `PROMPTDOCK_RELAY_WECHAT_PORT`), with the
  `relay-credential` volume mounted read-only.
- `caddy-admin` — profile `admin`; shares the `relay` network namespace
  (`network_mode: service:relay`) and serves the SPA over TLS on `8443`.
  Needs `PROMPTDOCK_ADMIN_USER` and `PROMPTDOCK_ADMIN_PASSWORD_HASH`.
- `integration-verifier` — profile `admin`; a throwaway `curl` container that
  fails the run unless `GET /health/ready` answers.
- `quality` — profile `tools`; builds the `development` stage and runs
  `scripts/quality/quality.sh`, i.e. the whole gate suite inside Linux.

`deploy/docker/relay.toml` is baked into the image as
`/etc/promptdock-relay/config.toml` and sets `[admin].mode = "read_only"`, so
the `admin` profile gives you the whole console for inspection and answers
every write with `403 ADMIN_READ_ONLY`. Nothing in the stack needs more:
`scripts/server/docker-verify.ps1` drives the device-facing `/v1` API with
device tokens and issues no `/v2` write, and `integration-verifier` only polls
`GET /health/ready`. Change the mode to `"operator"` in your own copy if you
want to rotate devices, revoke results or run the WeChat login flow from the
browser; `SECURITY.md` explains why that is the elevated setting rather than a
safer one.

Select a profile with `docker compose --profile admin up`, and note that
`build.context` is `../..`, so Compose commands belong in `deploy/docker/`.

## Build

```
cd deploy/docker
docker build -t promptdock-relay:dev -f Dockerfile ../..
```

The Dockerfile uses a `rust-builder` stage with `cargo build --locked --release
--bin promptdock-relay`, then the `relay-runtime` stage: `ubuntu` with
`ca-certificates` and `curl`, the binary at `/usr/local/bin/promptdock-relay`,
and `deploy/docker/relay.toml` copied to
`/etc/promptdock-relay/config.toml`. `runtime` is an alias of `relay-runtime`,
not a separate image — the Admin SPA is not in it; Caddy serves the SPA from the
`admin-caddy-runtime` image. Final image runs as `promptdock:promptdock`
(uid/gid 10001), `USER`-set, with the config directory and
`/var/lib/promptdock-relay` owned by it.

## Run

```
cd deploy/docker
docker compose up relay
```

Logs go to stdout. `config.toml` is **baked into the image**, and the `relay`
service mounts only the data volume, so editing `deploy/docker/relay.toml`
means rebuilding the image. To change configuration without a rebuild, add a
read-only bind mount the way `relay-wechat` does:

```yaml
volumes:
  - type: bind
    source: ./relay.toml
    target: /etc/promptdock-relay/config.toml
    read_only: true
```

The container starts as `serve --config /etc/promptdock-relay/config.toml`;
`/health/ready` is the Compose healthcheck.

## Master key injection

Do **not** bake the master key into the image. Three options:

- systemd-credential layout (what Compose and `relay-wechat` use): mount a
  directory at `/run/credentials`, set `CREDENTIALS_DIRECTORY=/run/credentials`,
  and place the key material in files named exactly `relay-master-key` and
  `relay-confirmation-key`. Mount it read-only.
- Docker secrets, if your orchestrator materialises them as files you then
  name accordingly.
- Env var — least-preferred because it appears in `/proc/self/environ`.

There is no CLI flag for the master key; the process reads the fixed file
names from `CREDENTIALS_DIRECTORY`.

## Volume layout

| Mount | Content |
|---|---|
| `/var/lib/promptdock-relay` | SQLite file, WAL/SHM, encrypted connection file |
| `/etc/promptdock-relay` | `config.toml` and optional second-instance TOML |
| `/run/credentials` | read-only mount holding `relay-master-key` and `relay-confirmation-key` |

Back up the SQLite file (not the WAL or SHM siblings). Copy a fresh WAL and
SHM alongside a restored `.sqlite` file and you get silent corruption.

## Upgrading

Rebuild the image, then `docker compose up -d`. There is no `init` step in the
entrypoint: `serve` runs the embedded SQLx migrator every time it opens the
database and then verifies the schema identity and revision, so an
unrecognised store is rejected rather than migrated. `promptdock-relay init`
creates a fresh database on an empty volume and refuses to write to one it does
not recognise; Compose does not call it for you.

Roll back by pointing the compose service at the previous image tag and
restarting. There is no in-place rollback of a completed migration. Test against
a copy of your production database first. See
[../development/testing.md](../development/testing.md) for how
`schema_upgrade_docker.rs` simulates that.

## Result pages are off by default

`/v1/server-info` advertises `result_pages_v1` only when the `[results]` stanza
is usable, so a stock Compose deployment — whose `relay.toml` and
`relay-wechat.toml` set no `[results]` section — does not serve read-only result
pages, and a desktop client pointed at it reports that contract as unavailable.
To enable them:

```toml
[results]
enabled = true
public_origin = "https://relay.example.test"
share_ttl_days = 7
```

`public_origin` must be an `https` origin with a host and no path, userinfo,
query or fragment; `share_ttl_days` is 1–30. Page content is encrypted with
`relay-master-key`, read from `CREDENTIALS_DIRECTORY`. The default `relay`
service sets neither the variable nor the mount, so copy the pair from
`relay-wechat`: `CREDENTIALS_DIRECTORY=/run/credentials` in `environment`, and
the `relay-credential` volume mounted at `/run/credentials` read-only. Note
that this is not a soft switch: with `[results]
enabled = true`, an invalid `public_origin` fails config validation and a
missing or unreadable `relay-master-key` fails startup, so the process refuses
to boot rather than quietly serving a feature list without `result_pages_v1`.

## Not covered

- Kubernetes manifests. Not shipped.
- TLS termination inside the container. Terminate at your reverse proxy.
- Multi-node relay with a shared SQLite file. SQLite is single-node by
  design; do not attempt an NFS mount.
