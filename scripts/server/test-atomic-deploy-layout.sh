#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/server/lib/deploy-common.sh
source "$SCRIPT_DIR/lib/deploy-common.sh"

TEST_PARENT="$(readlink -m -- "${CARGO_TARGET_DIR:-/tmp}")"
mkdir -p -- "$TEST_PARENT"
TEST_ROOT="$(mktemp -d "$TEST_PARENT/promptdock-atomic-layout.XXXXXX")"
case "$TEST_ROOT" in "$TEST_PARENT"/promptdock-atomic-layout.*) ;; *) die "unexpected test root" ;; esac
trap 'rm -rf -- "$TEST_ROOT"' EXIT
DESTDIR="$TEST_ROOT"

RELEASE_ROOT="$(rooted "$RELEASE_ROOT_PATH")"
RELEASES_ROOT="$(rooted "$RELEASES_ROOT_PATH")"
CURRENT_LINK="$(rooted "$CURRENT_LINK_PATH")"
PREVIOUS_LINK="$(rooted "$PREVIOUS_LINK_PATH")"
ensure_directory "$RELEASE_ROOT" 0755 root root
ensure_directory "$RELEASES_ROOT" 0755 root root

for release in old-111111111111 new-222222222222; do
    mkdir -p -- "$RELEASES_ROOT/$release/bin" "$RELEASES_ROOT/$release/admin"
    install -m 0755 -- "$SCRIPT_DIR/test-fixtures/mock-relay-v3.sh" "$RELEASES_ROOT/$release/bin/promptdock-relay"
    printf '%s\n' '<!doctype html><title>Admin</title>' > "$RELEASES_ROOT/$release/admin/index.html"
done

OLD_REFERENCE="releases/old-111111111111"
NEW_REFERENCE="releases/new-222222222222"
atomic_switch_release_link "$CURRENT_LINK" "$OLD_REFERENCE"
[[ "$(managed_link_reference "$CURRENT_LINK" current)" == "$OLD_REFERENCE" ]]
ln -sfn -- "$TEST_ROOT/escape" "$PREVIOUS_LINK"
if (managed_link_reference "$PREVIOUS_LINK" previous >/dev/null 2>&1); then
    die "absolute previous link was accepted"
fi
remove_managed_release_link "$PREVIOUS_LINK"
atomic_switch_release_link "$PREVIOUS_LINK" "$OLD_REFERENCE"

ensure_directory "$(rooted /etc/promptdock-relay)" 0750 root root
ensure_directory "$(rooted /etc/systemd/system)" 0755 root root
ensure_directory "$(rooted /var/lib/promptdock-relay)" 0700 root root
printf '%s\n' 'config-v1' > "$(rooted /etc/promptdock-relay/config.toml)"
printf '%s\n' 'unit-v1' > "$(rooted /etc/systemd/system/promptdock-relay.service)"
printf '%s\n' 'database-v1' > "$(rooted /var/lib/promptdock-relay/relay.db)"

create_runtime_backup "$(rooted /var/backups/promptdock-relay)" upgrade "$OLD_REFERENCE" "$NEW_REFERENCE" ""
BACKUP="$CREATED_BACKUP_DIR"
load_backup_metadata "$BACKUP"
[[ "$BACKUP_OPERATION" == upgrade && "$BACKUP_FROM_RELEASE" == "$OLD_REFERENCE" && "$BACKUP_TO_RELEASE" == "$NEW_REFERENCE" ]]
printf '%s\n' 'database-v2' > "$(rooted /var/lib/promptdock-relay/relay.db)"
restore_runtime_backup "$BACKUP"
grep -Fxq 'database-v1' "$(rooted /var/lib/promptdock-relay/relay.db)"

atomic_switch_release_link "$PREVIOUS_LINK" "$OLD_REFERENCE"
atomic_switch_release_link "$CURRENT_LINK" "$NEW_REFERENCE"
[[ "$(readlink -- "$CURRENT_LINK")" == "$NEW_REFERENCE" ]]
[[ "$(readlink -- "$PREVIOUS_LINK")" == "$OLD_REFERENCE" ]]
printf '%s\n' 'database-v2' > "$(rooted /var/lib/promptdock-relay/relay.db)"
openssl rand -base64 32 > "$(rooted /etc/promptdock-relay/master.key)"
if PROMPTDOCK_VALIDATE_STAGED=1 PROMPTDOCK_FAIL_CADDY_SMOKE=1 \
    bash "$SCRIPT_DIR/rollback-ubuntu.sh" --backup "$BACKUP" --destdir "$TEST_ROOT"; then
    die "rollback unexpectedly accepted a failed Caddy smoke"
fi
[[ "$(readlink -- "$CURRENT_LINK")" == "$NEW_REFERENCE" ]]
[[ "$(readlink -- "$PREVIOUS_LINK")" == "$OLD_REFERENCE" ]]
grep -Fxq 'database-v2' "$(rooted /var/lib/promptdock-relay/relay.db)"
bash "$SCRIPT_DIR/rollback-ubuntu.sh" --backup "$BACKUP" --destdir "$TEST_ROOT"
[[ "$(readlink -- "$CURRENT_LINK")" == "$OLD_REFERENCE" ]]
[[ "$(readlink -- "$PREVIOUS_LINK")" == "$NEW_REFERENCE" ]]
grep -Fxq 'database-v1' "$(rooted /var/lib/promptdock-relay/relay.db)"
[[ -z "$(find "$RELEASES_ROOT" -maxdepth 1 -name '.staging-*' -print -quit)" ]]

printf '%s\n' 'Atomic release layout, controlled links, transaction backup, and rollback passed.'
