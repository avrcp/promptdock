#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/server/lib/deploy-common.sh
source "$SCRIPT_DIR/lib/deploy-common.sh"

usage() {
    cat <<'EOF'
Usage: upgrade-ubuntu.sh --release-bundle ABSOLUTE_PATH [--destdir ABSOLUTE_PATH]

Stops Relay, creates a checksummed DB transaction backup, stages an immutable
unified release, atomically switches previous/current, and verifies Relay,
Admin API, and optional operator-configured Caddy smoke. Any failure restores
the old pointers, unit, configuration, and database.
EOF
}

RELEASE_BUNDLE_INPUT=""
REQUESTED_DESTDIR="${DESTDIR:-}"
while (($# > 0)); do
    case "$1" in
        --release-bundle) (($# >= 2)) || die "--release-bundle requires a value"; RELEASE_BUNDLE_INPUT="$2"; shift 2 ;;
        --destdir) (($# >= 2)) || die "--destdir requires a value"; REQUESTED_DESTDIR="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[[ "$RELEASE_BUNDLE_INPUT" == /* ]] || die "--release-bundle must be an absolute path"
normalize_destdir "$REQUESTED_DESTDIR"
require_install_authority
for command in cmp cp find install ln mv openssl python3 readlink sha256sum; do require_command "$command"; done
load_release_bundle "$RELEASE_BUNDLE_INPUT"
require_bundle_entrypoint_identity "${BASH_SOURCE[0]}"
validate_production_loopback_config "$RELEASE_CONFIG"

RELEASE_ROOT="$(rooted "$RELEASE_ROOT_PATH")"
RELEASES_ROOT="$(rooted "$RELEASES_ROOT_PATH")"
CURRENT_LINK="$(rooted "$CURRENT_LINK_PATH")"
PREVIOUS_LINK="$(rooted "$PREVIOUS_LINK_PATH")"
CONFIG_TARGET="$(rooted /etc/promptdock-relay/config.toml)"
UNIT_TARGET="$(rooted /etc/systemd/system/promptdock-relay.service)"
MASTER_KEY="$(rooted /etc/promptdock-relay/master.key)"
STATE_DIR="$(rooted /var/lib/promptdock-relay)"
BACKUP_ROOT="$(rooted /var/backups/promptdock-relay)"

[[ -d "$RELEASES_ROOT" && ! -L "$RELEASES_ROOT" ]] || die "unified releases root is not installed"
OLD_CURRENT="$(managed_link_reference "$CURRENT_LINK" "current release pointer")"
OLD_PREVIOUS="$(optional_managed_link_reference "$PREVIOUS_LINK" "previous release pointer")"
require_release_reference_directory "$OLD_CURRENT"
[[ "$OLD_CURRENT" != "$RELEASE_REFERENCE" ]] || die "requested release is already current"
require_regular_file "$CONFIG_TARGET" "installed configuration"
require_regular_file "$UNIT_TARGET" "installed systemd unit"
require_regular_file "$MASTER_KEY" "master credential"
require_regular_file "$STATE_DIR/relay.db" "installed database"

if [[ -z "$DESTDIR" ]]; then
    for command in curl systemctl; do require_command "$command"; done
    systemctl is-active --quiet "$RELAY_SERVICE" || die "relay service must be active before upgrade"
    systemctl stop "$RELAY_SERVICE"
fi

UPGRADE_COMPLETE=0
POINTERS_SWITCHED=0
BACKUP_DIR=""
rollback_failed_upgrade() {
    local status=$?
    trap - EXIT
    if [[ "$UPGRADE_COMPLETE" == "1" ]]; then exit "$status"; fi
    log "upgrade failed; restoring $OLD_CURRENT"
    if [[ -z "$DESTDIR" ]]; then systemctl stop "$RELAY_SERVICE" >/dev/null 2>&1 || true; fi
    if [[ "$POINTERS_SWITCHED" == "1" ]]; then
        atomic_switch_release_link "$CURRENT_LINK" "$OLD_CURRENT"
        if [[ -n "$OLD_PREVIOUS" ]]; then atomic_switch_release_link "$PREVIOUS_LINK" "$OLD_PREVIOUS"; else remove_managed_release_link "$PREVIOUS_LINK"; fi
    fi
    if [[ -n "$BACKUP_DIR" ]]; then restore_runtime_backup "$BACKUP_DIR"; fi
    if [[ -z "$DESTDIR" ]]; then
        systemctl daemon-reload || true
        systemctl start "$RELAY_SERVICE" || true
        run_runtime_smoke "$CONFIG_TARGET" || log "previous release smoke failed after automatic rollback"
    fi
    exit "$status"
}
trap rollback_failed_upgrade EXIT

create_runtime_backup "$BACKUP_ROOT" upgrade "$OLD_CURRENT" "$RELEASE_REFERENCE" "$OLD_PREVIOUS"
BACKUP_DIR="$CREATED_BACKUP_DIR"
log "upgrade transaction backup created at $BACKUP_DIR"
stage_immutable_release
POINTERS_SWITCHED=1
atomic_switch_release_link "$PREVIOUS_LINK" "$OLD_CURRENT"
atomic_switch_release_link "$CURRENT_LINK" "$RELEASE_REFERENCE"
atomic_install_file "$RELEASE_ROOT/current/deploy/config.production.toml" "$CONFIG_TARGET" 0640 root "$RELAY_GROUP"
atomic_install_file "$RELEASE_ROOT/current/deploy/systemd/promptdock-relay.service" "$UNIT_TARGET" 0644 root root
validate_operator_caddy_config

CURRENT_BINARY="$RELEASE_ROOT/current/bin/promptdock-relay"
if [[ -z "$DESTDIR" ]]; then
    systemctl daemon-reload
    validate_v4_operations "$CURRENT_BINARY" "$CONFIG_TARGET" "$MASTER_KEY"
    set_relay_database_ownership "$STATE_DIR/relay.db" "$STATE_DIR/relay.db-wal" "$STATE_DIR/relay.db-shm"
    systemctl start "$RELAY_SERVICE"
    systemctl is-active --quiet "$RELAY_SERVICE"
    run_runtime_smoke "$CONFIG_TARGET"
elif [[ "${PROMPTDOCK_VALIDATE_STAGED:-0}" == "1" ]]; then
    validate_v4_operations "$CURRENT_BINARY" "$CONFIG_TARGET" "$MASTER_KEY"
    if [[ "${PROMPTDOCK_FAIL_CADDY_SMOKE:-0}" == "1" ]]; then false; fi
fi

UPGRADE_COMPLETE=1
trap - EXIT
log "upgrade completed; current=$RELEASE_REFERENCE previous=$OLD_CURRENT rollback backup=$BACKUP_DIR"
