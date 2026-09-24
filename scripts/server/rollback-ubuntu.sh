#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/server/lib/deploy-common.sh
source "$SCRIPT_DIR/lib/deploy-common.sh"

usage() {
    cat <<'EOF'
Usage: rollback-ubuntu.sh [--backup ABSOLUTE_PATH] [--destdir ABSOLUTE_PATH]

Rolls the complete unified release from current to the controlled previous
pointer and restores the matching pre-upgrade database transaction. When
--backup is omitted, the newest exact current/previous upgrade backup is used.
EOF
}

BACKUP_DIR=""
REQUESTED_DESTDIR="${DESTDIR:-}"
while (($# > 0)); do
    case "$1" in
        --backup) (($# >= 2)) || die "--backup requires a value"; BACKUP_DIR="$2"; shift 2 ;;
        --destdir) (($# >= 2)) || die "--destdir requires a value"; REQUESTED_DESTDIR="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

if [[ -n "$BACKUP_DIR" ]]; then [[ "$BACKUP_DIR" == /* ]] || die "--backup must be an absolute path"; fi
normalize_destdir "$REQUESTED_DESTDIR"
require_install_authority
for command in install ln mv python3 readlink sha256sum; do require_command "$command"; done

RELEASE_ROOT="$(rooted "$RELEASE_ROOT_PATH")"
CURRENT_LINK="$(rooted "$CURRENT_LINK_PATH")"
PREVIOUS_LINK="$(rooted "$PREVIOUS_LINK_PATH")"
CONFIG_TARGET="$(rooted /etc/promptdock-relay/config.toml)"
UNIT_TARGET="$(rooted /etc/systemd/system/promptdock-relay.service)"
MASTER_KEY="$(rooted /etc/promptdock-relay/master.key)"
STATE_DIR="$(rooted /var/lib/promptdock-relay)"
BACKUP_ROOT="$(rooted /var/backups/promptdock-relay)"

OLD_CURRENT="$(managed_link_reference "$CURRENT_LINK" "current release pointer")"
ROLLBACK_TARGET="$(managed_link_reference "$PREVIOUS_LINK" "previous release pointer")"
require_release_reference_directory "$OLD_CURRENT"
require_release_reference_directory "$ROLLBACK_TARGET"
[[ "$OLD_CURRENT" != "$ROLLBACK_TARGET" ]] || die "current and previous point to the same release"
require_regular_file "$CONFIG_TARGET" "installed configuration"
require_regular_file "$UNIT_TARGET" "installed systemd unit"
require_regular_file "$MASTER_KEY" "master credential"
require_regular_file "$STATE_DIR/relay.db" "installed database"

if [[ -z "$BACKUP_DIR" ]]; then
    BACKUP_DIR="$(find_upgrade_backup "$BACKUP_ROOT" "$ROLLBACK_TARGET" "$OLD_CURRENT")" \
        || die "no matching upgrade backup exists for current=$OLD_CURRENT previous=$ROLLBACK_TARGET"
fi
load_backup_metadata "$BACKUP_DIR"
[[ "$BACKUP_OPERATION" == "upgrade" && "$BACKUP_FROM_RELEASE" == "$ROLLBACK_TARGET" && "$BACKUP_TO_RELEASE" == "$OLD_CURRENT" ]] \
    || die "backup transaction does not match the current/previous release pair"

if [[ -z "$DESTDIR" ]]; then
    for command in curl systemctl; do require_command "$command"; done
    systemctl is-active --quiet "$RELAY_SERVICE" || die "relay service must be active before rollback"
    systemctl stop "$RELAY_SERVICE"
fi

ROLLBACK_COMPLETE=0
POINTERS_SWITCHED=0
SAFETY_BACKUP=""
rollback_failed_rollback() {
    local status=$?
    trap - EXIT
    if [[ "$ROLLBACK_COMPLETE" == "1" ]]; then exit "$status"; fi
    log "rollback failed; restoring $OLD_CURRENT"
    if [[ -z "$DESTDIR" ]]; then systemctl stop "$RELAY_SERVICE" >/dev/null 2>&1 || true; fi
    if [[ "$POINTERS_SWITCHED" == "1" ]]; then
        atomic_switch_release_link "$CURRENT_LINK" "$OLD_CURRENT"
        atomic_switch_release_link "$PREVIOUS_LINK" "$ROLLBACK_TARGET"
    fi
    if [[ -n "$SAFETY_BACKUP" ]]; then restore_runtime_backup "$SAFETY_BACKUP"; fi
    if [[ -z "$DESTDIR" ]]; then
        systemctl daemon-reload || true
        systemctl start "$RELAY_SERVICE" || true
        run_runtime_smoke "$CONFIG_TARGET" || log "original release smoke failed after rollback recovery"
    fi
    exit "$status"
}
trap rollback_failed_rollback EXIT

create_runtime_backup "$BACKUP_ROOT" pre-rollback "$OLD_CURRENT" "$ROLLBACK_TARGET" "$ROLLBACK_TARGET"
SAFETY_BACKUP="$CREATED_BACKUP_DIR"
POINTERS_SWITCHED=1
atomic_switch_release_link "$CURRENT_LINK" "$ROLLBACK_TARGET"
atomic_switch_release_link "$PREVIOUS_LINK" "$OLD_CURRENT"
restore_runtime_backup "$BACKUP_DIR"
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

ROLLBACK_COMPLETE=1
trap - EXIT
log "rollback completed; current=$ROLLBACK_TARGET previous=$OLD_CURRENT safety backup=$SAFETY_BACKUP"
