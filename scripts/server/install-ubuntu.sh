#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/server/lib/deploy-common.sh
source "$SCRIPT_DIR/lib/deploy-common.sh"

usage() {
    cat <<'EOF'
Usage: install-ubuntu.sh --release-bundle ABSOLUTE_PATH [--master-key ABSOLUTE_PATH] [--destdir ABSOLUTE_PATH]

Stages one immutable unified Relay/Admin release below /opt/promptdock-relay/releases
and atomically creates the current pointer. Caddy remains operator-managed; this
script validates or reports its configuration but never installs the placeholder.
EOF
}

RELEASE_BUNDLE_INPUT=""
MASTER_KEY=""
REQUESTED_DESTDIR="${DESTDIR:-}"
while (($# > 0)); do
    case "$1" in
        --release-bundle) (($# >= 2)) || die "--release-bundle requires a value"; RELEASE_BUNDLE_INPUT="$2"; shift 2 ;;
        --master-key) (($# >= 2)) || die "--master-key requires a value"; MASTER_KEY="$2"; shift 2 ;;
        --destdir) (($# >= 2)) || die "--destdir requires a value"; REQUESTED_DESTDIR="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[[ "$RELEASE_BUNDLE_INPUT" == /* ]] || die "--release-bundle must be an absolute path"
if [[ -n "$MASTER_KEY" ]]; then [[ "$MASTER_KEY" == /* ]] || die "--master-key must be an absolute path"; fi
normalize_destdir "$REQUESTED_DESTDIR"
require_install_authority
for command in cmp cp find install ln mv openssl python3 readlink; do require_command "$command"; done
load_release_bundle "$RELEASE_BUNDLE_INPUT"
require_bundle_entrypoint_identity "${BASH_SOURCE[0]}"
validate_production_loopback_config "$RELEASE_CONFIG"

if [[ -z "$DESTDIR" ]]; then
    for command in curl getent groupadd systemctl systemd-analyze useradd; do require_command "$command"; done
    if ! getent group "$RELAY_GROUP" >/dev/null; then groupadd --system "$RELAY_GROUP"; fi
    if ! getent passwd "$RELAY_USER" >/dev/null; then
        useradd --system --gid "$RELAY_GROUP" --home-dir /var/lib/promptdock-relay --shell /usr/sbin/nologin "$RELAY_USER"
    fi
    [[ "$(id -u "$RELAY_USER")" -ne 0 ]] || die "service user must not be root"
fi

RELEASE_ROOT="$(rooted "$RELEASE_ROOT_PATH")"
RELEASES_ROOT="$(rooted "$RELEASES_ROOT_PATH")"
CURRENT_LINK="$(rooted "$CURRENT_LINK_PATH")"
PREVIOUS_LINK="$(rooted "$PREVIOUS_LINK_PATH")"
CONFIG_DIR="$(rooted /etc/promptdock-relay)"
STATE_DIR="$(rooted /var/lib/promptdock-relay)"
UNIT_DIR="$(rooted /etc/systemd/system)"
CONFIG_TARGET="$CONFIG_DIR/config.toml"
KEY_TARGET="$CONFIG_DIR/master.key"
CONFIRMATION_KEY_TARGET="$CONFIG_DIR/confirmation.key"
UNIT_TARGET="$UNIT_DIR/promptdock-relay.service"

ensure_directory "$RELEASE_ROOT" 0755 root root
ensure_directory "$RELEASES_ROOT" 0755 root root
ensure_directory "$CONFIG_DIR" 0750 root "$RELAY_GROUP"
ensure_directory "$STATE_DIR" 0700 "$RELAY_USER" "$RELAY_GROUP"
ensure_directory "$UNIT_DIR" 0755 root root
if [[ -e "$CURRENT_LINK" || -L "$CURRENT_LINK" || -e "$PREVIOUS_LINK" || -L "$PREVIOUS_LINK" ]]; then
    die "a unified release is already installed; use upgrade-ubuntu.sh"
fi

rollback_failed_install() {
    local status=$?
    trap - EXIT
    if [[ "${INSTALL_COMPLETE:-0}" == "1" ]]; then exit "$status"; fi
    if [[ -z "$DESTDIR" ]]; then systemctl stop "$RELAY_SERVICE" >/dev/null 2>&1 || true; fi
    [[ -z "${TEMP_KEY:-}" ]] || rm -f -- "$TEMP_KEY"
    [[ -z "${TEMP_CONFIRMATION_KEY:-}" ]] || rm -f -- "$TEMP_CONFIRMATION_KEY"
    remove_managed_release_link "$CURRENT_LINK"
    if [[ "${STAGED_RELEASE_CREATED:-0}" == "1" && -n "${STAGED_RELEASE_DIR:-}" && "$STAGED_RELEASE_DIR" == "$RELEASES_ROOT/$RELEASE_ID" ]]; then
        rm -rf -- "$STAGED_RELEASE_DIR"
    fi
    exit "$status"
}
INSTALL_COMPLETE=0
trap rollback_failed_install EXIT

stage_immutable_release
atomic_switch_release_link "$CURRENT_LINK" "$RELEASE_REFERENCE"
atomic_install_file "$STAGED_RELEASE_DIR/deploy/config.production.toml" "$CONFIG_TARGET" 0640 root "$RELAY_GROUP"
atomic_install_file "$STAGED_RELEASE_DIR/deploy/systemd/promptdock-relay.service" "$UNIT_TARGET" 0644 root root

if [[ ! -e "$KEY_TARGET" ]]; then
    TEMP_KEY=""
    if [[ -n "$MASTER_KEY" ]]; then
        install_master_key "$MASTER_KEY" "$KEY_TARGET"
    else
        TEMP_KEY="$(mktemp)"
        openssl rand -base64 32 | tr -d '\r\n' > "$TEMP_KEY"
        printf '\n' >> "$TEMP_KEY"
        install_master_key "$TEMP_KEY" "$KEY_TARGET"
        rm -f -- "$TEMP_KEY"
        TEMP_KEY=""
    fi
elif [[ -n "$MASTER_KEY" ]]; then
    die "master key already exists; refusing to replace connection encryption authority"
fi
if [[ ! -e "$CONFIRMATION_KEY_TARGET" ]]; then
    TEMP_CONFIRMATION_KEY="$(mktemp)"
    openssl rand -base64 32 | tr -d '\r\n' > "$TEMP_CONFIRMATION_KEY"
    printf '\n' >> "$TEMP_CONFIRMATION_KEY"
    install_master_key "$TEMP_CONFIRMATION_KEY" "$CONFIRMATION_KEY_TARGET"
    rm -f -- "$TEMP_CONFIRMATION_KEY"
    TEMP_CONFIRMATION_KEY=""
fi

validate_operator_caddy_config
CURRENT_BINARY="$RELEASE_ROOT/current/bin/promptdock-relay"
if [[ -z "$DESTDIR" ]]; then
    systemctl daemon-reload
    systemd-analyze verify "$UNIT_TARGET"
    validate_v4_operations "$CURRENT_BINARY" "$CONFIG_TARGET" "$KEY_TARGET" || die "release operations acceptance failed"
    set_relay_database_ownership "$STATE_DIR/relay.db" "$STATE_DIR/relay.db-wal" "$STATE_DIR/relay.db-shm"
    systemctl enable --now "$RELAY_SERVICE"
    systemctl is-active --quiet "$RELAY_SERVICE" || die "relay service did not become active"
    run_runtime_smoke "$CONFIG_TARGET" || die "Relay/Admin/Caddy installation smoke failed"
    log "installed $RELEASE_REFERENCE and started $RELAY_SERVICE"
else
    if [[ "${PROMPTDOCK_VALIDATE_STAGED:-0}" == "1" ]]; then
        validate_v4_operations "$CURRENT_BINARY" "$CONFIG_TARGET" "$KEY_TARGET" || die "staged release operations acceptance failed"
    fi
    log "staged immutable installation under $DESTDIR; users and systemd were not changed"
fi
INSTALL_COMPLETE=1
trap - EXIT
