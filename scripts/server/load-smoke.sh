#!/usr/bin/env bash
set -euo pipefail

set +x

for command in awk curl mktemp python3 sed seq sqlite3 stat wc; do
    command -v "$command" >/dev/null 2>&1 || {
        printf 'required command is unavailable: %s\n' "$command" >&2
        exit 1
    }
done

script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH='' cd -- "$script_dir/.." && pwd)"
relay_bin="${RELAY_BIN:-$repo_root/target/x86_64-unknown-linux-gnu/release/promptdock-relay}"
[[ -x "$relay_bin" ]] || { printf 'relay binary is not executable: %s\n' "$relay_bin" >&2; exit 1; }

ingress_count="${INGRESS_COUNT:-1000}"
ingress_concurrency="${INGRESS_CONCURRENCY:-50}"
status_reads="${STATUS_READS:-100}"
status_concurrency="${STATUS_CONCURRENCY:-100}"
soak_seconds="${SOAK_SECONDS:-60}"
rss_interval="${RSS_INTERVAL_SECONDS:-10}"
for value in "$ingress_count" "$ingress_concurrency" "$status_reads" "$status_concurrency" "$soak_seconds" "$rss_interval"; do
    [[ "$value" =~ ^[0-9]+$ ]] || { printf 'load settings must be non-negative integers\n' >&2; exit 1; }
done
((ingress_count >= 1 && ingress_concurrency >= 1 && status_reads >= 1 && status_concurrency >= 1 && rss_interval >= 1)) || {
    printf 'counts, concurrency, and RSS interval must be positive\n' >&2
    exit 1
}
((ingress_count <= 100000 && ingress_concurrency <= 500 && status_reads <= 100000 && status_concurrency <= 500)) || {
    printf 'load settings exceed the script safety ceiling\n' >&2
    exit 1
}

temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/promptdock-load-smoke.XXXXXX")"
chmod 700 "$temporary_root"
server_pid=''
cleanup() {
    local exit_code=$?
    trap - EXIT
    if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
        kill -TERM "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    if [[ "${KEEP_SMOKE_DIR:-0}" == "1" ]]; then
        printf 'load-smoke directory retained by request: %s\n' "$temporary_root" >&2
    else
        rm -rf -- "$temporary_root"
    fi
    exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

port="${RELAY_PORT:-$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)}"
if ! [[ "$port" =~ ^[0-9]+$ ]] || ((port < 1 || port > 65535)); then
    printf 'RELAY_PORT must be between 1 and 65535\n' >&2
    exit 1
fi
database="$temporary_root/relay.db"
config="$temporary_root/relay.toml"
cat >"$config" <<EOF
[server]
bind = "127.0.0.1:$port"
[database]
path = "$database"
max_connections = 4
busy_timeout_ms = 5000
[wechat]
enabled = false
connection_file = "$temporary_root/wechat-connection.enc"
[logging]
filter = "warn"
format = "json"
EOF
chmod 600 "$config"
"$relay_bin" init --config "$config" >/dev/null

# The API allows a burst of 20 notification writes per device. Distribute the
# default 1000-write smoke across enough disposable devices instead of weakening
# or bypassing the production limiter.
device_count=$(((ingress_count + 19) / 20))
if ((status_reads > device_count * 40)); then
    printf 'STATUS_READS exceeds the safe per-device status burst for this INGRESS_COUNT\n' >&2
    exit 1
fi
mkdir -m 700 "$temporary_root/auth"
for device in $(seq 1 "$device_count"); do
    output="$($relay_bin device create --name "LOAD-$device" --scope notify:write --scope notify:read_own --config "$config")"
    token="$(printf '%s\n' "$output" | sed -n 's/^device_token=//p')"
    [[ "$token" == pdv2.* ]] || { printf 'could not create load device %s\n' "$device" >&2; exit 1; }
    printf 'header = "Authorization: Bearer %s"\n' "$token" >"$temporary_root/auth/$device.conf"
    chmod 600 "$temporary_root/auth/$device.conf"
done

base_url="http://127.0.0.1:$port"
log_file="$temporary_root/relay.log"
"$relay_bin" serve --config "$config" >"$log_file" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
    curl --silent --fail --max-time 2 "$base_url/health/ready" >/dev/null && break
    kill -0 "$server_pid" 2>/dev/null || { printf 'relay exited before readiness\n' >&2; exit 1; }
    sleep 0.1
done
curl --silent --fail "$base_url/health/ready" >/dev/null

rss_csv="$temporary_root/rss.csv"
printf 'elapsed_seconds,rss_kib\n' >"$rss_csv"
sample_rss() {
    local elapsed=0
    while ((elapsed <= soak_seconds)); do
        if kill -0 "$server_pid" 2>/dev/null; then
            rss="$(awk '/^VmRSS:/ {print $2}' "/proc/$server_pid/status")"
            printf '%s,%s\n' "$elapsed" "${rss:-0}" >>"$rss_csv"
        fi
        ((elapsed == soak_seconds)) && break
        sleep_for=$rss_interval
        ((elapsed + sleep_for > soak_seconds)) && sleep_for=$((soak_seconds - elapsed))
        sleep "$sleep_for"
        elapsed=$((elapsed + sleep_for))
    done
}
sample_rss &
rss_pid=$!

run_limited() {
    local limit=$1
    shift
    while (( $(jobs -pr | wc -l) >= limit )); do
        wait -n || true
    done
    "$@" &
}

post_one() {
    local index=$1
    local device=$(((index - 1) % device_count + 1))
    local now=$((created_at + index))
    local payload
    payload="$(printf '{"schemaVersion":1,"notificationId":"load-%s","dedupeKey":"load-%s","kind":"test","priority":100,"title":"Load smoke","body":"Synthetic load %s","correlationKey":"load-smoke","createdAt":%s,"expiresAt":%s}' "$index" "$index" "$index" "$now" "$expires_at")"
    curl --silent --show-error --config "$temporary_root/auth/$device.conf" \
        --output "$temporary_root/results/ingress-$index.json" \
        --write-out '%{http_code}' --header 'Content-Type: application/json' \
        --data "$payload" "$base_url/v1/notifications" \
        >"$temporary_root/results/ingress-$index.code"
}

mkdir -m 700 "$temporary_root/results"
created_at="$(python3 -c 'import time; print(time.time_ns() // 1_000_000)')"
expires_at=$((created_at + 3600000))
ingress_start="$(python3 -c 'import time; print(time.monotonic_ns())')"
(
    for index in $(seq 1 "$ingress_count"); do
        run_limited "$ingress_concurrency" post_one "$index"
    done
    wait
)
ingress_end="$(python3 -c 'import time; print(time.monotonic_ns())')"
for index in $(seq 1 "$ingress_count"); do
    [[ "$(<"$temporary_root/results/ingress-$index.code")" == "202" ]] || {
        printf 'ingress %s failed with HTTP %s\n' "$index" "$(<"$temporary_root/results/ingress-$index.code")" >&2
        exit 1
    }
done

status_one() {
    local index=$1
    local notification=$(((index - 1) % ingress_count + 1))
    local device=$(((notification - 1) % device_count + 1))
    curl --silent --show-error --config "$temporary_root/auth/$device.conf" \
        --output "$temporary_root/results/status-$index.json" \
        --write-out '%{http_code}' "$base_url/v1/notifications/load-$notification" \
        >"$temporary_root/results/status-$index.code"
}

status_start="$(python3 -c 'import time; print(time.monotonic_ns())')"
(
    for index in $(seq 1 "$status_reads"); do
        run_limited "$status_concurrency" status_one "$index"
    done
    wait
)
status_end="$(python3 -c 'import time; print(time.monotonic_ns())')"
for index in $(seq 1 "$status_reads"); do
    [[ "$(<"$temporary_root/results/status-$index.code")" == "200" ]] || {
        printf 'status read %s failed with HTTP %s\n' "$index" "$(<"$temporary_root/results/status-$index.code")" >&2
        exit 1
    }
done

wal_before="$(stat -c %s "${database}-wal" 2>/dev/null || printf '0')"
checkpoint="$(sqlite3 "$database" 'PRAGMA wal_checkpoint(PASSIVE);')"
wal_after="$(stat -c %s "${database}-wal" 2>/dev/null || printf '0')"
wait "$rss_pid"

kill -TERM "$server_pid"
wait "$server_pid"
server_pid=''

scan_files=("$log_file" "$database")
[[ -f "${database}-wal" ]] && scan_files+=("${database}-wal")
[[ -f "${database}-shm" ]] && scan_files+=("${database}-shm")
for auth_file in "$temporary_root"/auth/*.conf; do
    token="$(sed -n 's/^header = "Authorization: Bearer \(.*\)"$/\1/p' "$auth_file")"
    secret="${token##*.}"
    if [[ -z "$token" ]] || grep -aFq "$token" "${scan_files[@]}" || grep -aFq "$secret" "${scan_files[@]}"; then
        printf 'a load-smoke device credential leaked into logs or SQLite\n' >&2
        exit 1
    fi
done

python3 - "$rss_csv" "$ingress_start" "$ingress_end" "$status_start" "$status_end" <<'PY'
import csv, sys
path, ingress_start, ingress_end, status_start, status_end = sys.argv[1:]
with open(path, newline="", encoding="utf-8") as source:
    values = [int(row["rss_kib"]) for row in csv.DictReader(source)]
print(f"ingress_elapsed_ms={(int(ingress_end)-int(ingress_start))/1_000_000:.3f}")
print(f"status_elapsed_ms={(int(status_end)-int(status_start))/1_000_000:.3f}")
print(f"rss_samples={len(values)}")
print(f"rss_min_kib={min(values)}")
print(f"rss_max_kib={max(values)}")
print(f"rss_delta_kib={values[-1]-values[0]}")
PY
printf 'ingress_count=%s\n' "$ingress_count"
printf 'ingress_concurrency=%s\n' "$ingress_concurrency"
printf 'status_reads=%s\n' "$status_reads"
printf 'status_concurrency=%s\n' "$status_concurrency"
printf 'wal_bytes_before_checkpoint=%s\n' "$wal_before"
printf 'wal_checkpoint_passive=%s\n' "$checkpoint"
printf 'wal_bytes_after_checkpoint=%s\n' "$wal_after"
printf 'soak_seconds=%s\n' "$soak_seconds"
if ((soak_seconds >= 86400)); then
    printf 'soak_24h=true\n'
else
    printf 'soak_24h=false\n'
fi
printf 'Load smoke passed. RSS delta is an observation, not a leak verdict.\n'
