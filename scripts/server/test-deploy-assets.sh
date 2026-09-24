#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
bash "$SCRIPT_DIR/test-atomic-deploy-layout.sh"
printf '%s\n' 'Runtime principal verification runs in a dedicated root container gate; deferred here.'

TEST_PARENT="$(readlink -m -- "${CARGO_TARGET_DIR:-/tmp}")"
mkdir -p -- "$TEST_PARENT"
TEST_ROOT="$(mktemp -d "$TEST_PARENT/promptdock-deploy-assets.XXXXXX")"
case "$TEST_ROOT" in "$TEST_PARENT"/promptdock-deploy-assets.*) ;; *) exit 1 ;; esac
trap 'rm -rf -- "$TEST_ROOT"' EXIT

MOCK_RELAY="$SCRIPT_DIR/test-fixtures/mock-relay-v3.sh"
MOCK_RELAY_NEXT="$SCRIPT_DIR/test-fixtures/mock-relay-v3-next.sh"
BUNDLE_OLD="$TEST_ROOT/bundles/old"
BUNDLE_NEXT="$TEST_ROOT/bundles/next"
bash "$SCRIPT_DIR/test-fixtures/create-test-release-bundle.sh" "$BUNDLE_OLD" "$MOCK_RELAY"
bash "$SCRIPT_DIR/test-fixtures/create-test-release-bundle.sh" "$BUNDLE_NEXT" "$MOCK_RELAY_NEXT"
python3 - "$BUNDLE_NEXT" <<'PY'
import hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])
config = root / "deploy/config.production.toml"
config.write_text(config.read_text(encoding="utf-8").replace('filter = "info"', 'filter = "warn"'), encoding="utf-8")
manifest_path = root / "RELEASE-MANIFEST.json"
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
manifest["deploymentAssets"]["productionConfig"]["sha256"] = hashlib.sha256(config.read_bytes()).hexdigest()
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
lines = []
for path in sorted(p for p in root.rglob("*") if p.is_file() and p.name != "SHA256SUMS.txt"):
    relative = path.relative_to(root).as_posix()
    lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {relative}")
(root / "SHA256SUMS.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$BUNDLE_OLD"
python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$BUNDLE_NEXT"

rehash_bundle() {
    python3 - "$1" <<'PY'
import hashlib, json, sys
from pathlib import Path

root = Path(sys.argv[1])
manifest_path = root / "RELEASE-MANIFEST.json"
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

def digest(relative):
    return hashlib.sha256((root / relative).read_bytes()).hexdigest()

for record in manifest["deploymentAssets"].values():
    record["sha256"] = digest(record["path"])
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

lines = []
for path in sorted(p for p in root.rglob("*") if p.is_file() and p.name != "SHA256SUMS.txt"):
    relative = path.relative_to(root).as_posix()
    lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {relative}")
(root / "SHA256SUMS.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
}

expect_invalid_bundle() {
    local bundle="$1" description="$2"
    if python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$bundle"; then
        printf 'validator unexpectedly accepted %s\n' "$description" >&2
        exit 1
    fi
}

python3 - "$BUNDLE_OLD/RELEASE-MANIFEST.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    value = json.load(stream)
assert value["schemaVersion"] == 4
assert value["releaseVersion"] == "0.6.0-rc.1"
assert value["sourceCommit"] == "1" * 40
assert value["adminUi"]["sourceCommit"] == value["sourceCommit"]
assert value["adminUi"]["adminApiMajor"] == value["contracts"]["adminApiMajor"] == 2
PY

if bash "$SCRIPT_DIR/install-ubuntu.sh" --binary "$MOCK_RELAY" --destdir "$TEST_ROOT/loose"; then
    printf '%s\n' 'install unexpectedly accepted a loose binary' >&2
    exit 1
fi

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/tampered"
printf '%s\n' '# drift' >> "$TEST_ROOT/tampered/deploy/config.production.toml"
if python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$TEST_ROOT/tampered"; then
    printf '%s\n' 'validator unexpectedly accepted checksum drift' >&2
    exit 1
fi

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/cross-identity"
python3 - "$TEST_ROOT/cross-identity" <<'PY'
import hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])
path = root / "RELEASE-MANIFEST.json"
value = json.loads(path.read_text(encoding="utf-8"))
value["adminUi"]["sourceCommit"] = "2" * 40
path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
lines = [line for line in (root / "SHA256SUMS.txt").read_text(encoding="utf-8").splitlines() if not line.endswith("  RELEASE-MANIFEST.json")]
lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  RELEASE-MANIFEST.json")
(root / "SHA256SUMS.txt").write_text("\n".join(sorted(lines)) + "\n", encoding="utf-8")
PY
if python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$TEST_ROOT/cross-identity"; then
    printf '%s\n' 'validator unexpectedly accepted cross-commit Admin identity' >&2
    exit 1
fi

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/extra-file"
printf '%s\n' 'unexpected' > "$TEST_ROOT/extra-file/extra.txt"
if python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$TEST_ROOT/extra-file"; then
    printf '%s\n' 'validator unexpectedly accepted an extra file' >&2
    exit 1
fi

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/extra-directory"
mkdir -- "$TEST_ROOT/extra-directory/empty"
if python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$TEST_ROOT/extra-directory"; then
    printf '%s\n' 'validator unexpectedly accepted an extra directory' >&2
    exit 1
fi

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/non-executable"
chmod 0644 -- "$TEST_ROOT/non-executable/bin/promptdock-relay"
if python3 "$SCRIPT_DIR/lib/validate-release-bundle.py" "$TEST_ROOT/non-executable"; then
    printf '%s\n' 'validator unexpectedly accepted a non-executable binary' >&2
    exit 1
fi

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/database-path-drift"
python3 - "$TEST_ROOT/database-path-drift/deploy/config.production.toml" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
original = path.read_text(encoding="utf-8")
updated = original.replace(
    'path = "/var/lib/promptdock-relay/relay.db"',
    'path = "/srv/promptdock-relay/relay.db"',
)
if updated == original:
    raise SystemExit("database path fixture was not updated")
path.write_text(updated, encoding="utf-8")
PY
rehash_bundle "$TEST_ROOT/database-path-drift"
expect_invalid_bundle "$TEST_ROOT/database-path-drift" 'a database path outside the managed backup boundary'

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/connection-path-drift"
python3 - "$TEST_ROOT/connection-path-drift/deploy/config.production.toml" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
original = path.read_text(encoding="utf-8")
updated = original.replace(
    'connection_file = "/var/lib/promptdock-relay/wechat-connection.enc"',
    'connection_file = "/srv/promptdock-relay/wechat-connection.enc"',
)
if updated == original:
    raise SystemExit("ConnectionBundle path fixture was not updated")
path.write_text(updated, encoding="utf-8")
PY
rehash_bundle "$TEST_ROOT/connection-path-drift"
expect_invalid_bundle "$TEST_ROOT/connection-path-drift" 'a ConnectionBundle path outside the managed backup boundary'

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/origin-site-mismatch"
python3 - "$TEST_ROOT/origin-site-mismatch/deploy/config.production.toml" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
original = path.read_text(encoding="utf-8")
updated = original.replace(
    'allowed_origin = "https://admin.example.com"',
    'allowed_origin = "https://other.example.com"',
)
if updated == original:
    raise SystemExit("allowed_origin fixture was not updated")
path.write_text(updated, encoding="utf-8")
PY
rehash_bundle "$TEST_ROOT/origin-site-mismatch"
expect_invalid_bundle "$TEST_ROOT/origin-site-mismatch" 'a Caddy site that differs from Admin allowed_origin'

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/caddy-result-route-too-broad"
python3 - "$TEST_ROOT/caddy-result-route-too-broad/deploy/caddy/Caddyfile.example" <<'PY'
import sys
from pathlib import Path
path = Path(sys.argv[1])
original = path.read_text(encoding="utf-8")
updated = original.replace('@resultRead path /r/* /result-assets/*', '@resultRead path /* /result-assets/*')
if updated == original:
    raise SystemExit("result route fixture was not updated")
path.write_text(updated, encoding="utf-8")
PY
rehash_bundle "$TEST_ROOT/caddy-result-route-too-broad"
expect_invalid_bundle "$TEST_ROOT/caddy-result-route-too-broad" 'an expanded unauthenticated result route'

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/caddy-comment-disguise"
python3 - "$TEST_ROOT/caddy-comment-disguise/deploy/caddy/Caddyfile.example" <<'PY'
import sys
from pathlib import Path

Path(sys.argv[1]).write_text(
    """# @relayPublic path /health/* /v1/* /v5/*
# handle @relayPublic
# reverse_proxy 127.0.0.1:8080
# basic_auth
# @adminApi path /admin/api/v2/*
# handle @adminApi
# reverse_proxy 127.0.0.1:8081
# root * /opt/promptdock-relay/current/admin
admin.example.com {
    respond "unprotected route"
}
""",
    encoding="utf-8",
)
PY
rehash_bundle "$TEST_ROOT/caddy-comment-disguise"
expect_invalid_bundle "$TEST_ROOT/caddy-comment-disguise" 'Caddy security markers that exist only in comments'

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/caddy-unprotected-admin"
python3 - "$TEST_ROOT/caddy-unprotected-admin/deploy/caddy/Caddyfile.example" <<'PY'
import sys
from pathlib import Path

Path(sys.argv[1]).write_text(
    """admin.example.com {
    tls internal

    @relayPublic path /health/* /v1/* /v5/*
    handle @relayPublic {
        reverse_proxy 127.0.0.1:8080
    }

    @never path /__never__
    handle @never {
        basic_auth {
            operator <CADDY_HASH_ONLY>
        }
    }

    @adminApi path /admin/api/v2/*
    handle @adminApi {
        reverse_proxy 127.0.0.1:8081
    }

    handle {
        basic_auth {
            operator <CADDY_HASH_ONLY>
        }
        root * /opt/promptdock-relay/current/admin
        try_files {path} /index.html
        file_server
    }
}
""",
    encoding="utf-8",
)
PY
rehash_bundle "$TEST_ROOT/caddy-unprotected-admin"
expect_invalid_bundle "$TEST_ROOT/caddy-unprotected-admin" 'an Admin API route outside the Basic Auth boundary'

cp -a -- "$BUNDLE_OLD" "$TEST_ROOT/weakened-systemd"
python3 - "$TEST_ROOT/weakened-systemd/deploy/systemd/promptdock-relay.service" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
original = path.read_text(encoding="utf-8")
updated = original.replace("NoNewPrivileges=true\n", "")
if updated == original:
    raise SystemExit("systemd hardening fixture was not updated")
path.write_text(updated, encoding="utf-8")
PY
rehash_bundle "$TEST_ROOT/weakened-systemd"
expect_invalid_bundle "$TEST_ROOT/weakened-systemd" 'a systemd unit without NoNewPrivileges'

bash "$SCRIPT_DIR/install-ubuntu.sh" --release-bundle "$BUNDLE_OLD" --destdir "$TEST_ROOT"
OLD_REFERENCE='releases/0.6.0-rc.1-111111111111'
NEXT_REFERENCE='releases/0.6.1-rc.1-333333333333'
[[ "$(readlink "$TEST_ROOT/opt/promptdock-relay/current")" == "$OLD_REFERENCE" ]]
[[ ! -e "$TEST_ROOT/opt/promptdock-relay/previous" ]]
cmp -s "$MOCK_RELAY" "$TEST_ROOT/opt/promptdock-relay/current/bin/promptdock-relay"
[[ -f "$TEST_ROOT/opt/promptdock-relay/current/admin/index.html" ]]
[[ ! -e "$TEST_ROOT/var/www/promptdock-relay-admin" ]]
MASTER_KEY_HASH="$(sha256sum "$TEST_ROOT/etc/promptdock-relay/master.key")"
if bash "$SCRIPT_DIR/install-ubuntu.sh" --release-bundle "$BUNDLE_OLD" --destdir "$TEST_ROOT"; then
    printf '%s\n' 'install unexpectedly replaced an existing unified installation' >&2
    exit 1
fi
[[ "$(sha256sum "$TEST_ROOT/etc/promptdock-relay/master.key")" == "$MASTER_KEY_HASH" ]]

printf '%s\n' 'database-before-upgrade' > "$TEST_ROOT/var/lib/promptdock-relay/relay.db"
DATABASE_BEFORE_HASH="$(sha256sum "$TEST_ROOT/var/lib/promptdock-relay/relay.db")"
bash "$SCRIPT_DIR/backup-ubuntu.sh" --destdir "$TEST_ROOT"
shopt -s nullglob
MANUAL_BACKUPS=("$TEST_ROOT"/var/backups/promptdock-relay/backup-*)
shopt -u nullglob
[[ "${#MANUAL_BACKUPS[@]}" -eq 1 ]]
python3 - "${MANUAL_BACKUPS[0]}/TRANSACTION.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["operation"] == "backup"
assert value["fromRelease"] == "releases/0.6.0-rc.1-111111111111"
PY
printf '%s\n' 'database-after-manual-backup' > "$TEST_ROOT/var/lib/promptdock-relay/relay.db"
PROMPTDOCK_VALIDATE_STAGED=1 bash "$SCRIPT_DIR/restore-ubuntu.sh" \
    --backup "${MANUAL_BACKUPS[0]}" --destdir "$TEST_ROOT"
[[ "$(sha256sum "$TEST_ROOT/var/lib/promptdock-relay/relay.db")" == "$DATABASE_BEFORE_HASH" ]]

if PROMPTDOCK_VALIDATE_STAGED=1 MOCK_RELAY_FAIL=status \
    bash "$SCRIPT_DIR/upgrade-ubuntu.sh" --release-bundle "$BUNDLE_NEXT" --destdir "$TEST_ROOT"; then
    printf '%s\n' 'upgrade unexpectedly accepted a failed status check' >&2
    exit 1
fi
[[ "$(readlink "$TEST_ROOT/opt/promptdock-relay/current")" == "$OLD_REFERENCE" ]]
[[ ! -e "$TEST_ROOT/opt/promptdock-relay/previous" ]]
[[ "$(sha256sum "$TEST_ROOT/var/lib/promptdock-relay/relay.db")" == "$DATABASE_BEFORE_HASH" ]]
cmp -s "$BUNDLE_OLD/deploy/config.production.toml" "$TEST_ROOT/etc/promptdock-relay/config.toml"

PROMPTDOCK_VALIDATE_STAGED=1 bash "$SCRIPT_DIR/upgrade-ubuntu.sh" \
    --release-bundle "$BUNDLE_NEXT" --destdir "$TEST_ROOT"
[[ "$(readlink "$TEST_ROOT/opt/promptdock-relay/current")" == "$NEXT_REFERENCE" ]]
[[ "$(readlink "$TEST_ROOT/opt/promptdock-relay/previous")" == "$OLD_REFERENCE" ]]
cmp -s "$MOCK_RELAY_NEXT" "$TEST_ROOT/opt/promptdock-relay/current/bin/promptdock-relay"
cmp -s "$BUNDLE_NEXT/deploy/config.production.toml" "$TEST_ROOT/etc/promptdock-relay/config.toml"
if PROMPTDOCK_VALIDATE_STAGED=1 bash "$SCRIPT_DIR/restore-ubuntu.sh" \
    --backup "${MANUAL_BACKUPS[0]}" --destdir "$TEST_ROOT"; then
    printf '%s\n' 'restore unexpectedly accepted a backup from another release' >&2
    exit 1
fi
[[ "$(readlink "$TEST_ROOT/opt/promptdock-relay/current")" == "$NEXT_REFERENCE" ]]

printf '%s\n' 'database-after-upgrade' > "$TEST_ROOT/var/lib/promptdock-relay/relay.db"
PROMPTDOCK_VALIDATE_STAGED=1 bash "$SCRIPT_DIR/rollback-ubuntu.sh" --destdir "$TEST_ROOT"
[[ "$(readlink "$TEST_ROOT/opt/promptdock-relay/current")" == "$OLD_REFERENCE" ]]
[[ "$(readlink "$TEST_ROOT/opt/promptdock-relay/previous")" == "$NEXT_REFERENCE" ]]
cmp -s "$MOCK_RELAY" "$TEST_ROOT/opt/promptdock-relay/current/bin/promptdock-relay"
cmp -s "$BUNDLE_OLD/deploy/config.production.toml" "$TEST_ROOT/etc/promptdock-relay/config.toml"
[[ "$(sha256sum "$TEST_ROOT/var/lib/promptdock-relay/relay.db")" == "$DATABASE_BEFORE_HASH" ]]
[[ "$(sha256sum "$TEST_ROOT/etc/promptdock-relay/master.key")" == "$MASTER_KEY_HASH" ]]

if find "$TEST_ROOT/opt/promptdock-relay/releases" -maxdepth 1 -name '.staging-*' -print -quit | grep -q .; then
    printf '%s\n' 'deployment left a staging directory behind' >&2
    exit 1
fi

printf '%s\n' 'Schema v4 bundle, immutable install, transactional upgrade, and rollback passed.'
