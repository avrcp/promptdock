#!/usr/bin/env bash
set -Eeuo pipefail
set +x

readonly CONFIRMATION="DESTROY-PROMPTDOCK-RELAY-V4-DATA"
readonly SERVICE="promptdock-relay.service"

usage() {
    cat <<'EOF'
Usage: reset-relay-data.sh --config ABSOLUTE_PATH --confirm DESTROY-PROMPTDOCK-RELAY-V4-DATA

Permanently removes the configured Relay SQLite database, its WAL/SHM sidecars,
and the configured encrypted WeChat ConnectionBundle. The Relay service must be
stopped. This command does not remove release files, configuration, credentials,
or backups.
EOF
}

die() {
    printf '%s\n' "promptdock-relay: error: $*" >&2
    exit 1
}

config=""
confirmation=""
while (($# > 0)); do
    case "$1" in
        --config)
            (($# >= 2)) || die "--config requires a value"
            config="$2"
            shift 2
            ;;
        --confirm)
            (($# >= 2)) || die "--confirm requires a value"
            confirmation="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[[ "$config" == /* ]] || die "--config must be an absolute path"
[[ "$confirmation" == "$CONFIRMATION" ]] || die "destructive confirmation phrase is missing or incorrect"
[[ -f "$config" && ! -L "$config" ]] || die "config must be a regular non-symlink file"
for command in python3 readlink rm; do
    command -v "$command" >/dev/null 2>&1 || die "required command is unavailable: $command"
done
if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet "$SERVICE"; then
    die "$SERVICE must be stopped before reset"
fi

mapfile -t configured_paths < <(python3 - "$config" <<'PY'
import sys
import tomllib
from pathlib import Path

with open(sys.argv[1], "rb") as stream:
    value = tomllib.load(stream)
paths = [
    value.get("database", {}).get("path"),
    value.get("wechat", {}).get("connection_file"),
]
if any(not isinstance(path, str) or not path or "\n" in path or "\r" in path for path in paths):
    raise SystemExit("database.path and wechat.connection_file must be non-empty single-line strings")
for path in paths:
    candidate = Path(path)
    if not candidate.is_absolute():
        raise SystemExit("reset targets must be absolute paths")
    print(path)
PY
) || die "could not read reset targets from config"
[[ "${#configured_paths[@]}" -eq 2 ]] || die "config did not produce exactly two reset targets"

for configured_path in "${configured_paths[@]}"; do
    [[ ! -L "$configured_path" ]] || die "refusing to follow a symlink reset target: $configured_path"
    resolved_path="$(readlink -m -- "$configured_path")"
    [[ "$resolved_path" == "$configured_path" ]] || die "reset target must be canonical and contain no symlink components: $configured_path"
done

database="${configured_paths[0]}"
connection="${configured_paths[1]}"
[[ "$database" == /* && "$connection" == /* ]] || die "reset targets must resolve to absolute paths"
[[ "$database" != "/" && "$connection" != "/" ]] || die "refusing to target the filesystem root"
[[ "$database" != "$connection" ]] || die "database and ConnectionBundle targets must differ"

targets=("$database" "${database}-wal" "${database}-shm" "$connection")
for target in "${targets[@]}"; do
    if [[ -L "$target" ]]; then
        die "refusing to remove a symlink target: $target"
    fi
    if [[ -e "$target" && ! -f "$target" ]]; then
        die "refusing to remove a non-regular target: $target"
    fi
done

for target in "${targets[@]}"; do
    rm -f -- "$target"
done
printf '%s\n' "promptdock-relay: Relay database, WAL/SHM, and encrypted ConnectionBundle removed"
