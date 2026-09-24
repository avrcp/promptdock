#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/server/lib/deploy-common.sh
source "$SCRIPT_DIR/lib/deploy-common.sh"

usage() {
    cat <<'EOF'
Usage: backup-ubuntu.sh [--destdir ABSOLUTE_PATH]

Stops the active PromptDock Relay service and creates a checksummed full
runtime backup under /var/backups/promptdock-relay. The systemd master key is
intentionally excluded and must be backed up through a separate secret channel.
DESTDIR snapshots a staged installation without calling systemd.
EOF
}

REQUESTED_DESTDIR="${DESTDIR:-}"
while (($# > 0)); do
    case "$1" in
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

normalize_destdir "$REQUESTED_DESTDIR"
require_install_authority
require_command install
require_command sha256sum

BACKUP_BINARY="$(rooted /opt/promptdock-relay/current/bin/promptdock-relay)"
BACKUP_CONFIG="$(rooted /etc/promptdock-relay/config.toml)"
MASTER_CREDENTIAL="$(rooted /etc/promptdock-relay/master.key)"
require_executable_file "$BACKUP_BINARY" "installed relay binary"
require_regular_file "$BACKUP_CONFIG" "installed configuration"
require_regular_file "$MASTER_CREDENTIAL" "master credential"
require_regular_file "$(rooted /etc/systemd/system/promptdock-relay.service)" "installed systemd unit"
require_regular_file "$(rooted /var/lib/promptdock-relay/relay.db)" "installed database"
CURRENT_RELEASE="$(managed_link_reference "$(rooted /opt/promptdock-relay/current)" "current release pointer")"
PREVIOUS_RELEASE="$(optional_managed_link_reference "$(rooted /opt/promptdock-relay/previous)" "previous release pointer")"

restart_on_exit() {
    local status=$?
    local restart_status=0
    trap - EXIT
    systemctl start "$RELAY_SERVICE" || restart_status=$?
    if [[ "$restart_status" -ne 0 ]]; then
        log "service restart failed after backup"
    fi
    if [[ "$status" -eq 0 && "$restart_status" -ne 0 ]]; then
        status=$restart_status
    fi
    exit "$status"
}

if [[ -z "$DESTDIR" ]]; then
    require_command systemctl
    systemctl is-active --quiet "$RELAY_SERVICE" || die "relay service must be active before backup"
    trap restart_on_exit EXIT
    systemctl stop "$RELAY_SERVICE"
fi

create_runtime_backup "$(rooted /var/backups/promptdock-relay)" backup "$CURRENT_RELEASE" "$CURRENT_RELEASE" "$PREVIOUS_RELEASE"
BACKUP_DIR="$CREATED_BACKUP_DIR"
log "backup created at $BACKUP_DIR (master key intentionally excluded)"

if [[ -z "$DESTDIR" ]]; then
    systemctl start "$RELAY_SERVICE"
    systemctl is-active --quiet "$RELAY_SERVICE"
    wait_for_relay_ready
    validate_v4_operations "$BACKUP_BINARY" "$BACKUP_CONFIG" "$MASTER_CREDENTIAL"
    trap - EXIT
fi
