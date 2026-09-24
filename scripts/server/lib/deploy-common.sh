#!/usr/bin/env bash

# Shared by the Ubuntu deployment scripts. Callers must enable strict mode.

# shellcheck disable=SC2034
readonly RELAY_SERVICE="promptdock-relay.service"
readonly RELAY_USER="promptdock-relay"
readonly RELAY_GROUP="promptdock-relay"
# Assigned before being marked readonly: `readonly VAR=$(...)` reports readonly's
# own status, so a failed `cd` would leave the directory empty instead of aborting
# the caller's strict mode.
DEPLOY_COMMON_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly DEPLOY_COMMON_DIR
readonly RELEASE_ROOT_PATH="/opt/promptdock-relay"
readonly RELEASES_ROOT_PATH="$RELEASE_ROOT_PATH/releases"
readonly CURRENT_LINK_PATH="$RELEASE_ROOT_PATH/current"
readonly PREVIOUS_LINK_PATH="$RELEASE_ROOT_PATH/previous"

log() { printf '%s\n' "promptdock-relay: $*"; }
die() { printf '%s\n' "promptdock-relay: error: $*" >&2; exit 1; }

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command is unavailable: $1"
}

normalize_destdir() {
    local requested="${1:-}"
    if [[ -z "$requested" ]]; then DESTDIR=""; return; fi
    [[ "$requested" == /* ]] || die "DESTDIR must be an absolute path"
    [[ "$requested" != *"'"* && "$requested" != *$'\n'* && "$requested" != *$'\r'* ]] \
        || die "DESTDIR contains unsupported characters"
    require_command readlink
    requested="$(readlink -m -- "$requested")"
    [[ "$requested" != "/" ]] || die "use an empty DESTDIR for a real installation"
    if [[ -e "$requested" && ( -L "$requested" || ! -d "$requested" ) ]]; then
        die "DESTDIR exists but is not a real directory"
    fi
    mkdir -p -- "$requested"
    DESTDIR="${requested%/}"
}

rooted() {
    [[ "$1" == /* ]] || die "internal target is not absolute: $1"
    printf '%s%s\n' "$DESTDIR" "$1"
}

require_install_authority() {
    if [[ -z "$DESTDIR" && "$(id -u)" -ne 0 ]]; then die "a real installation must run as root"; fi
}

require_regular_file() {
    [[ -f "$1" && ! -L "$1" ]] || die "$2 is not a regular file: $1"
}

require_executable_file() {
    require_regular_file "$1" "$2"
    [[ -x "$1" ]] || die "$2 is not executable: $1"
}

ensure_directory() {
    local path="$1" mode="$2" owner="$3" group="$4"
    if [[ -e "$path" && ( -L "$path" || ! -d "$path" ) ]]; then
        die "refusing to replace a non-directory or symlink: $path"
    fi
    if [[ -z "$DESTDIR" ]]; then
        install -d -m "$mode" -o "$owner" -g "$group" -- "$path"
    else
        install -d -m "$mode" -- "$path"
    fi
}

validate_target_file() {
    if [[ -e "$1" && ( -L "$1" || ! -f "$1" ) ]]; then
        die "refusing to replace a non-regular file or symlink: $1"
    fi
}

atomic_install_file() {
    local source="$1" target="$2" mode="$3" owner="$4" group="$5"
    local temporary="${target}.new.$$"
    require_regular_file "$source" "installation source"
    validate_target_file "$target"
    rm -f -- "$temporary"
    if [[ -z "$DESTDIR" ]]; then
        install -m "$mode" -o "$owner" -g "$group" -- "$source" "$temporary"
    else
        install -m "$mode" -- "$source" "$temporary"
    fi
    mv -fT -- "$temporary" "$target"
}

same_file_content() { [[ -f "$1" && -f "$2" ]] && cmp -s -- "$1" "$2"; }

validate_release_bundle() {
    [[ "$1" == /* ]] || die "release bundle must be an absolute path"
    require_command caddy
    require_command python3
    python3 "$DEPLOY_COMMON_DIR/validate-release-bundle.py" "$1" || die "release bundle validation failed"
}

load_release_bundle() {
    local -a identity
    validate_release_bundle "$1"
    RELEASE_BUNDLE="$(readlink -m -- "$1")"
    RELEASE_BINARY="$RELEASE_BUNDLE/bin/promptdock-relay"
    RELEASE_ADMIN="$RELEASE_BUNDLE/admin"
    RELEASE_MANIFEST="$RELEASE_BUNDLE/RELEASE-MANIFEST.json"
    RELEASE_CHECKSUMS="$RELEASE_BUNDLE/SHA256SUMS.txt"
    RELEASE_CONFIG="$RELEASE_BUNDLE/deploy/config.production.toml"
    RELEASE_UNIT="$RELEASE_BUNDLE/deploy/systemd/promptdock-relay.service"
    RELEASE_CADDY="$RELEASE_BUNDLE/deploy/caddy/Caddyfile.example"
    require_executable_file "$RELEASE_BINARY" "release binary"
    require_regular_file "$RELEASE_ADMIN/index.html" "Admin entrypoint"
    mapfile -t identity < <(python3 - "$RELEASE_MANIFEST" <<'PY'
import json, re, sys
with open(sys.argv[1], encoding="utf-8") as stream: value = json.load(stream)
version, commit = value.get("releaseVersion"), value.get("sourceCommit")
if value.get("schemaVersion") != 4: raise SystemExit("release manifest is not schema v4")
if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", version): raise SystemExit("invalid releaseVersion")
if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit): raise SystemExit("invalid sourceCommit")
print(version); print(commit)
PY
    ) || die "release identity is invalid"
    [[ "${#identity[@]}" -eq 2 ]] || die "release identity is incomplete"
    RELEASE_VERSION="${identity[0]}"
    RELEASE_SOURCE_COMMIT="${identity[1]}"
    RELEASE_ID="${RELEASE_VERSION}-${RELEASE_SOURCE_COMMIT:0:12}"
    RELEASE_REFERENCE="releases/$RELEASE_ID"
}

require_bundle_entrypoint_identity() {
    local caller="$1" name
    require_command cmp
    name="$(basename -- "$caller")"
    same_file_content "$caller" "$RELEASE_BUNDLE/scripts/$name" || die "deployment entrypoint does not match the validated release bundle"
    same_file_content "$DEPLOY_COMMON_DIR/deploy-common.sh" "$RELEASE_BUNDLE/scripts/lib/deploy-common.sh" || die "deployment library does not match the validated release bundle"
    same_file_content "$DEPLOY_COMMON_DIR/validate-release-bundle.py" "$RELEASE_BUNDLE/scripts/lib/validate-release-bundle.py" || die "bundle validator does not match the validated release bundle"
}

normalize_release_reference() {
    [[ "$1" =~ ^releases/[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || die "release reference is not a controlled relative path: $1"
    printf '%s\n' "$1"
}

managed_link_reference() {
    [[ -L "$1" ]] || die "$2 is not a symbolic link: $1"
    normalize_release_reference "$(readlink -- "$1")"
}

optional_managed_link_reference() {
    if [[ ! -e "$1" && ! -L "$1" ]]; then return 0; fi
    managed_link_reference "$1" "$2"
}

require_release_reference_directory() {
    local reference target
    reference="$(normalize_release_reference "$1")"
    target="$(rooted "$RELEASE_ROOT_PATH")/$reference"
    [[ -d "$target" && ! -L "$target" ]] || die "release target is not a real directory: $target"
    require_executable_file "$target/bin/promptdock-relay" "release binary"
    require_regular_file "$target/admin/index.html" "release Admin entrypoint"
}

atomic_switch_release_link() {
    local link="$1" reference release_root temporary
    reference="$(normalize_release_reference "$2")"
    require_release_reference_directory "$reference"
    release_root="$(rooted "$RELEASE_ROOT_PATH")"
    temporary="$release_root/.link-$(basename -- "$link").$$"
    if [[ -e "$link" && ! -L "$link" ]]; then die "refusing to replace a non-symlink release pointer: $link"; fi
    rm -f -- "$temporary"
    ln -s -- "$reference" "$temporary"
    mv -Tf -- "$temporary" "$link"
}

remove_managed_release_link() {
    if [[ -e "$1" && ! -L "$1" ]]; then die "refusing to remove a non-symlink release pointer: $1"; fi
    rm -f -- "$1"
}

stage_immutable_release() {
    local releases_root target staging
    releases_root="$(rooted "$RELEASES_ROOT_PATH")"
    target="$releases_root/$RELEASE_ID"
    staging="$releases_root/.staging-$RELEASE_ID-$$"
    if [[ -e "$target" ]]; then
        [[ -d "$target" && ! -L "$target" ]] || die "existing release target is not a real directory: $target"
        python3 "$DEPLOY_COMMON_DIR/validate-release-bundle.py" "$target" || die "existing immutable release is invalid: $target"
        cmp -s -- "$RELEASE_CHECKSUMS" "$target/SHA256SUMS.txt" || die "release identity already exists with different content: $target"
        STAGED_RELEASE_DIR="$target"
        STAGED_RELEASE_CREATED=0
        return 0
    fi
    [[ ! -e "$staging" ]] || die "release staging path already exists: $staging"
    mkdir -m 0755 -- "$staging"
    if ! cp -a -- "$RELEASE_BUNDLE/." "$staging/"; then rm -rf -- "$staging"; die "release staging copy failed"; fi
    if ! python3 "$DEPLOY_COMMON_DIR/validate-release-bundle.py" "$staging"; then rm -rf -- "$staging"; die "staged release validation failed"; fi
    find "$staging" -type d -exec chmod 0755 {} +
    find "$staging" -type f -exec chmod 0644 {} +
    chmod 0755 "$staging/bin/promptdock-relay" "$staging"/scripts/*.sh "$staging/scripts/lib/deploy-common.sh" "$staging/scripts/lib/validate-release-bundle.py"
    if [[ -z "$DESTDIR" ]]; then chown -R root:root -- "$staging"; fi
    mv -- "$staging" "$target"
    STAGED_RELEASE_DIR="$target"
    STAGED_RELEASE_CREATED=1
}

validate_production_loopback_config() {
    require_regular_file "$1" "production configuration"
    python3 - "$1" <<'PY' || die "production listeners must bind loopback on ports 8080 and 8081"
import ipaddress, sys, tomllib
with open(sys.argv[1], "rb") as stream: value = tomllib.load(stream)
def valid(section, port):
    bind = section.get("bind")
    if not isinstance(bind, str): return False
    if bind.startswith("["): host, sep, found = bind[1:].partition("]:")
    else: host, sep, found = bind.rpartition(":")
    return bool(sep) and found == str(port) and ipaddress.ip_address(host).is_loopback
server, admin = value.get("server", {}), value.get("admin", {})
if server.get("exposure") != "loopback" or not valid(server, 8080): raise SystemExit(1)
if admin.get("enabled") is not True or not valid(admin, 8081): raise SystemExit(1)
PY
}

production_admin_origin() {
    python3 - "$1" <<'PY'
import sys, tomllib
with open(sys.argv[1], "rb") as stream: origin = tomllib.load(stream).get("admin", {}).get("allowed_origin")
if not isinstance(origin, str) or not origin.startswith("https://"): raise SystemExit(1)
print(origin)
PY
}

wait_for_relay_ready() {
    local _attempt
    require_command curl
    for _attempt in $(seq 1 100); do
        curl --silent --fail --max-time 2 http://127.0.0.1:8080/health/ready >/dev/null && return 0
        systemctl is-active --quiet "$RELAY_SERVICE" || break
        sleep 0.1
    done
    return 1
}

wait_for_admin_ready() {
    local origin _attempt
    require_command curl
    origin="$(production_admin_origin "$1")" || return 1
    for _attempt in $(seq 1 100); do
        curl --silent --fail --max-time 2 --header "Origin: $origin" http://127.0.0.1:8081/admin/api/v2/meta >/dev/null && return 0
        systemctl is-active --quiet "$RELAY_SERVICE" || break
        sleep 0.1
    done
    return 1
}

validate_operator_caddy_config() {
    local config
    require_command grep
    config="$(rooted /etc/caddy/Caddyfile)"
    if [[ ! -e "$config" ]]; then log "Caddy is operator-managed; /etc/caddy/Caddyfile is absent and must be installed separately"; return 0; fi
    require_regular_file "$config" "operator-managed Caddy configuration"
    if grep -Fq '<CADDY_HASH_ONLY>' "$config"; then log "Caddy is operator-managed; replace the hash placeholder before enabling external access"; return 0; fi
    grep -Fq 'root * /opt/promptdock-relay/current/admin' "$config" || die "operator-managed Caddy configuration does not use the unified Admin release root"
    grep -Fq 'reverse_proxy 127.0.0.1:8081' "$config" || die "operator-managed Caddy configuration does not proxy the Admin listener"
    if [[ -z "$DESTDIR" ]]; then require_command caddy; caddy validate --config "$config" --adapter caddyfile; fi
}

run_caddy_smoke() {
    if [[ "${PROMPTDOCK_FAIL_CADDY_SMOKE:-0}" == "1" ]]; then return 1; fi
    local base_url="${PROMPTDOCK_CADDY_SMOKE_URL:-}" curl_config="${PROMPTDOCK_CADDY_CURL_CONFIG:-}"
    if [[ -z "$base_url" || -z "$curl_config" ]]; then
        log "Caddy smoke is operator-managed; set PROMPTDOCK_CADDY_SMOKE_URL and PROMPTDOCK_CADDY_CURL_CONFIG to enforce it during deployment"
        return 0
    fi
    [[ "$base_url" == https://* ]] || die "Caddy smoke URL must use HTTPS"
    [[ "$curl_config" == /* ]] || die "Caddy curl config must be an absolute path"
    require_command stat
    require_regular_file "$curl_config" "Caddy smoke credential config"
    [[ "$(stat -c %a -- "$curl_config")" == "600" ]] || die "Caddy smoke credential config must have mode 0600"
    curl --silent --fail --max-time 5 --config "$curl_config" "$base_url/" >/dev/null
    curl --silent --fail --max-time 5 --config "$curl_config" "$base_url/admin/api/v2/meta" >/dev/null
}

run_runtime_smoke() {
    wait_for_relay_ready && wait_for_admin_ready "$1" && run_caddy_smoke
}

validate_v4_operations() (
    local binary="$1" config="$2" master_credential="${3:-}" credential_dir="" temp_parent output result=0 relay_uid
    local -a credential_env=(env -u CREDENTIALS_DIRECTORY)
    local -a operation_prefix=()
    # shellcheck disable=SC2329  # invoked by the EXIT trap below, not by name
    cleanup_operation_credentials() {
        local exit_status="$?"
        trap - EXIT HUP INT TERM
        if [[ -n "$credential_dir" ]] && ! rm -rf -- "$credential_dir"; then
            exit_status=1
        fi
        exit "$exit_status"
    }
    trap cleanup_operation_credentials EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    require_executable_file "$binary" "relay operations binary"
    require_regular_file "$config" "relay operations configuration"
    if [[ -z "$DESTDIR" ]]; then
        require_command runuser
        relay_uid="$(id -u "$RELAY_USER")" || die "service user is unavailable: $RELAY_USER"
        [[ "$relay_uid" -ne 0 ]] || die "service user must not be root"
        operation_prefix=(runuser -u "$RELAY_USER" --)
    fi
    if [[ -n "$master_credential" && -e "$master_credential" ]]; then
        require_regular_file "$master_credential" "master credential"
        temp_parent="$(readlink -m -- "${TMPDIR:-/tmp}")" || exit 1
        credential_dir="$(mktemp -d "$temp_parent/promptdock-operations.XXXXXX")" || exit 1
        chmod 0700 -- "$credential_dir" || exit 1
        if [[ -z "$DESTDIR" ]]; then
            install -m 0600 -o "$RELAY_USER" -g "$RELAY_GROUP" -- \
                "$master_credential" "$credential_dir/relay-master-key" || exit 1
            chown "$RELAY_USER:$RELAY_GROUP" -- "$credential_dir" || exit 1
        else
            install -m 0600 -- "$master_credential" "$credential_dir/relay-master-key" || exit 1
        fi
        credential_env=(env "CREDENTIALS_DIRECTORY=$credential_dir")
    fi
    if ! output="$("${operation_prefix[@]}" "${credential_env[@]}" "$binary" init --config "$config")" || ! python3 -c 'import json,sys; v=json.loads(sys.argv[1]); sys.exit(0 if v.get("status")=="initialized" else 1)' "$output"; then result=1
    elif ! output="$("${operation_prefix[@]}" "${credential_env[@]}" "$binary" status --config "$config")" || ! python3 -c 'import json,sys; sys.exit(0 if json.loads(sys.argv[1]).get("status")=="ok" else 1)' "$output"; then result=1
    elif ! output="$("${operation_prefix[@]}" "${credential_env[@]}" "$binary" doctor --config "$config")" || ! python3 -c 'import json,sys; sys.exit(0 if json.loads(sys.argv[1]).get("status")=="ok" else 1)' "$output"; then result=1
    fi
    exit "$result"
)

set_relay_database_ownership() {
    local target
    for target in "$@"; do
        [[ -e "$target" ]] || continue
        require_regular_file "$target" "relay database artifact"
        if [[ -z "$DESTDIR" ]]; then chown "$RELAY_USER:$RELAY_GROUP" -- "$target"; fi
        chmod 0600 -- "$target"
    done
}

validate_master_key() {
    local encoded decoded_length
    require_regular_file "$1" "master key"
    encoded="$(tr -d '\r\n' < "$1")"
    [[ "$encoded" =~ ^[A-Za-z0-9+/]{43}=$ ]] || die "master key is not canonical base64"
    decoded_length="$(printf '%s' "$encoded" | openssl base64 -d -A 2>/dev/null | wc -c)"
    [[ "$decoded_length" -eq 32 ]] || die "master key must decode to exactly 32 bytes"
}

install_master_key() {
    local source="$1" target="$2" temporary="${2}.new.$$" encoded
    validate_master_key "$source"
    encoded="$(tr -d '\r\n' < "$source")"
    validate_target_file "$target"
    rm -f -- "$temporary"
    if [[ -z "$DESTDIR" ]]; then install -m 0600 -o root -g root /dev/null "$temporary"; else install -m 0600 /dev/null "$temporary"; fi
    printf '%s\n' "$encoded" > "$temporary"
    chmod 0600 -- "$temporary"
    mv -fT -- "$temporary" "$target"
}

create_runtime_backup() {
    local backup_root="$1" label="$2" from_reference="$3" to_reference="$4" previous_reference="${5:-}"
    local backup_dir item source
    from_reference="$(normalize_release_reference "$from_reference")"
    to_reference="$(normalize_release_reference "$to_reference")"
    if [[ -n "$previous_reference" ]]; then previous_reference="$(normalize_release_reference "$previous_reference")"; fi
    backup_dir="${backup_root}/${label}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
    ensure_directory "$backup_root" 0700 root root
    mkdir -m 0700 -- "$backup_dir"
    local -a names=("etc/promptdock-relay/config.toml" "etc/systemd/system/promptdock-relay.service" "var/lib/promptdock-relay/relay.db" "var/lib/promptdock-relay/relay.db-wal" "var/lib/promptdock-relay/relay.db-shm" "var/lib/promptdock-relay/wechat-connection.enc") copied=()
    for item in "${names[@]}"; do
        source="$(rooted "/$item")"
        if [[ -e "$source" ]]; then
            require_regular_file "$source" "managed backup source"
            install -m 0600 -- "$source" "$backup_dir/$(basename -- "$item")"
            copied+=("$(basename -- "$item")")
        fi
    done
    printf '%s\n' 'promptdock-relay-backup-v2' > "$backup_dir/BACKUP_FORMAT"
    copied+=("BACKUP_FORMAT")
    python3 - "$backup_dir/TRANSACTION.json" "$label" "$from_reference" "$to_reference" "$previous_reference" <<'PY'
import json, sys
path, operation, source, target, previous = sys.argv[1:]
with open(path, "w", encoding="utf-8", newline="\n") as stream:
    json.dump({"schemaVersion": 1, "operation": operation, "fromRelease": source, "toRelease": target, "previousBefore": previous or None}, stream, ensure_ascii=False, indent=2)
    stream.write("\n")
PY
    chmod 0600 -- "$backup_dir/TRANSACTION.json"
    copied+=("TRANSACTION.json")
    (cd -- "$backup_dir" && sha256sum -- "${copied[@]}" > SHA256SUMS)
    chmod 0600 -- "$backup_dir/SHA256SUMS"
    CREATED_BACKUP_DIR="$backup_dir"
}

validate_backup() {
    local backup_dir="$1"
    [[ "$backup_dir" == /* ]] || die "backup directory must be absolute"
    [[ -d "$backup_dir" && ! -L "$backup_dir" ]] || die "backup is not a real directory"
    require_regular_file "$backup_dir/BACKUP_FORMAT" "backup format marker"
    require_regular_file "$backup_dir/SHA256SUMS" "backup checksum manifest"
    require_regular_file "$backup_dir/TRANSACTION.json" "backup transaction metadata"
    [[ "$(< "$backup_dir/BACKUP_FORMAT")" == "promptdock-relay-backup-v2" ]] || die "unsupported backup format"
    (cd -- "$backup_dir" && sha256sum -c --strict -- SHA256SUMS >/dev/null) || die "backup checksum verification failed"
    python3 - "$backup_dir" <<'PY' || die "backup inventory or transaction metadata is invalid"
import json, re, sys
from pathlib import Path
root = Path(sys.argv[1])
allowed = {"BACKUP_FORMAT", "SHA256SUMS", "TRANSACTION.json", "config.toml", "promptdock-relay.service", "relay.db", "relay.db-wal", "relay.db-shm", "wechat-connection.enc"}
actual = {p.name for p in root.iterdir()}
required = {"BACKUP_FORMAT", "SHA256SUMS", "TRANSACTION.json", "config.toml", "promptdock-relay.service", "relay.db"}
if not actual <= allowed or not required <= actual: raise SystemExit(1)
value = json.loads((root / "TRANSACTION.json").read_text(encoding="utf-8"))
if set(value) != {"schemaVersion", "operation", "fromRelease", "toRelease", "previousBefore"} or value["schemaVersion"] != 1: raise SystemExit(1)
pattern = re.compile(r"releases/[0-9A-Za-z][0-9A-Za-z.+-]*\Z")
if not pattern.fullmatch(value["fromRelease"]) or not pattern.fullmatch(value["toRelease"]): raise SystemExit(1)
if value["previousBefore"] is not None and not pattern.fullmatch(value["previousBefore"]): raise SystemExit(1)
PY
}

load_backup_metadata() {
    local -a metadata
    validate_backup "$1"
    mapfile -t metadata < <(python3 - "$1/TRANSACTION.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as stream: value=json.load(stream)
print(value["operation"]); print(value["fromRelease"]); print(value["toRelease"]); print(value["previousBefore"] or "")
PY
    )
    BACKUP_OPERATION="${metadata[0]}"
    BACKUP_FROM_RELEASE="${metadata[1]}"
    BACKUP_TO_RELEASE="${metadata[2]}"
    BACKUP_PREVIOUS_BEFORE="${metadata[3]}"
}

restore_runtime_backup() {
    local backup_dir="$1" state_dir connection_target item
    validate_backup "$backup_dir"
    state_dir="$(rooted /var/lib/promptdock-relay)"
    connection_target="$state_dir/wechat-connection.enc"
    atomic_install_file "$backup_dir/config.toml" "$(rooted /etc/promptdock-relay/config.toml)" 0640 root "$RELAY_GROUP"
    atomic_install_file "$backup_dir/promptdock-relay.service" "$(rooted /etc/systemd/system/promptdock-relay.service)" 0644 root root
    atomic_install_file "$backup_dir/relay.db" "$state_dir/relay.db" 0600 "$RELAY_USER" "$RELAY_GROUP"
    rm -f -- "$state_dir/relay.db-wal" "$state_dir/relay.db-shm"
    for item in relay.db-wal relay.db-shm; do
        if [[ -f "$backup_dir/$item" ]]; then atomic_install_file "$backup_dir/$item" "$state_dir/$item" 0600 "$RELAY_USER" "$RELAY_GROUP"; fi
    done
    if [[ -f "$backup_dir/wechat-connection.enc" ]]; then
        atomic_install_file "$backup_dir/wechat-connection.enc" "$connection_target" 0600 "$RELAY_USER" "$RELAY_GROUP"
    else
        rm -f -- "$connection_target"
    fi
}

find_upgrade_backup() {
    local backup_root="$1" expected_from="$2" expected_to="$3" candidate candidate_index
    shopt -s nullglob
    local -a candidates=("$backup_root"/upgrade-*)
    shopt -u nullglob
    for ((candidate_index=${#candidates[@]}-1; candidate_index>=0; candidate_index--)); do
        candidate="${candidates[$candidate_index]}"
        if load_backup_metadata "$candidate" 2>/dev/null && [[ "$BACKUP_OPERATION" == "upgrade" && "$BACKUP_FROM_RELEASE" == "$expected_from" && "$BACKUP_TO_RELEASE" == "$expected_to" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}
