#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/server/lib/deploy-common.sh
source "$SCRIPT_DIR/lib/deploy-common.sh"

usage() {
    cat <<'EOF'
Usage: restore-ubuntu.sh --backup ABSOLUTE_PATH [--destdir ABSOLUTE_PATH]

Verifies a PromptDock Relay backup, stops the active service, snapshots the
current installation, restores the selected backup, and starts the service.
The separately managed master key is never read or replaced by this script.
EOF
}

BACKUP_DIR=""
REQUESTED_DESTDIR="${DESTDIR:-}"
while (($# > 0)); do
    case "$1" in
        --backup)
            (($# >= 2)) || die "--backup requires a value"
            BACKUP_DIR="$2"
            shift 2
            ;;
        --destdir)
            (($# >= 2)) || die "--destdir requires a value"
            REQUESTED_DESTDIR="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *) die "unknown argument: $1" ;;
    esac
done

[[ "$BACKUP_DIR" == /* ]] || die "--backup must be an absolute path"
normalize_destdir "$REQUESTED_DESTDIR"
require_install_authority
require_command install
require_command sha256sum
validate_backup "$BACKUP_DIR"

CURRENT_DATABASE="$(rooted /var/lib/promptdock-relay/relay.db)"
RESTORED_BINARY="$(rooted /opt/promptdock-relay/current/bin/promptdock-relay)"
RESTORED_CONFIG="$(rooted /etc/promptdock-relay/config.toml)"
MASTER_CREDENTIAL="$(rooted /etc/promptdock-relay/master.key)"
require_regular_file "$CURRENT_DATABASE" "current relay database"
require_regular_file "$MASTER_CREDENTIAL" "master credential"
CURRENT_RELEASE="$(managed_link_reference "$(rooted /opt/promptdock-relay/current)" "current release pointer")"
PREVIOUS_RELEASE="$(optional_managed_link_reference "$(rooted /opt/promptdock-relay/previous)" "previous release pointer")"
load_backup_metadata "$BACKUP_DIR"
[[ "$BACKUP_FROM_RELEASE" == "$CURRENT_RELEASE" ]] \
    || die "backup release $BACKUP_FROM_RELEASE does not match current release $CURRENT_RELEASE"
if [[ -z "$DESTDIR" ]]; then
    require_command systemctl
    systemctl is-active --quiet "$RELAY_SERVICE" || die "relay service must be active before restore"
fi

restart_before_restore() {
    local status=$?
    trap - ERR
    if [[ -z "$DESTDIR" ]]; then
        systemctl start "$RELAY_SERVICE" || true
    fi
    exit "$status"
}
trap restart_before_restore ERR

if [[ -z "$DESTDIR" ]]; then
    systemctl stop "$RELAY_SERVICE"
fi
create_runtime_backup "$(rooted /var/backups/promptdock-relay)" pre-restore "$CURRENT_RELEASE" "$CURRENT_RELEASE" "$PREVIOUS_RELEASE"
SAFETY_BACKUP="$CREATED_BACKUP_DIR"
log "pre-restore safety backup created at $SAFETY_BACKUP"
trap - ERR

rollback_restore() {
    local status=$?
    trap - ERR
    log "restore failed; returning to $SAFETY_BACKUP"
    if [[ -z "$DESTDIR" ]]; then
        systemctl stop "$RELAY_SERVICE" || true
    fi
    restore_runtime_backup "$SAFETY_BACKUP"
    if [[ -z "$DESTDIR" ]]; then
        systemctl daemon-reload
        systemctl start "$RELAY_SERVICE" || true
    fi
    exit "$status"
}
trap rollback_restore ERR

restore_runtime_backup "$BACKUP_DIR"
if [[ -z "$DESTDIR" ]]; then
    systemctl daemon-reload
    systemctl start "$RELAY_SERVICE"
    systemctl is-active --quiet "$RELAY_SERVICE"
    wait_for_relay_ready
    validate_v4_operations "$RESTORED_BINARY" "$RESTORED_CONFIG" "$MASTER_CREDENTIAL"
elif [[ "${PROMPTDOCK_VALIDATE_STAGED:-0}" == "1" ]]; then
    validate_v4_operations "$RESTORED_BINARY" "$RESTORED_CONFIG" "$MASTER_CREDENTIAL"
fi

trap - ERR
log "restore completed; safety backup retained at $SAFETY_BACKUP"
