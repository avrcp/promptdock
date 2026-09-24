#!/usr/bin/env bash
set -euo pipefail

set +x

for command in curl grep mktemp python3 sed seq; do
    command -v "$command" >/dev/null 2>&1 || {
        printf 'required command is unavailable: %s\n' "$command" >&2
        exit 1
    }
done

script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(CDPATH='' cd -- "$script_dir/.." && pwd)"
relay_bin="${RELAY_BIN:-$repo_root/target/x86_64-unknown-linux-gnu/release/promptdock-relay}"
[[ -x "$relay_bin" ]] || {
    printf 'relay binary is not executable: %s\n' "$relay_bin" >&2
    exit 1
}

temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/promptdock-linux-smoke.XXXXXX")"
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
        printf 'smoke directory retained by request: %s\n' "$temporary_root" >&2
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

config="$temporary_root/relay.toml"
database="$temporary_root/relay.db"
auth_config="$temporary_root/curl-auth.conf"
cat >"$config" <<EOF
[server]
bind = "127.0.0.1:$port"
request_body_limit_bytes = 65536
route_timeout_seconds = 10
shutdown_timeout_seconds = 15

[database]
path = "$database"
max_connections = 4
busy_timeout_ms = 5000

[wechat]
enabled = false
connection_file = "$temporary_root/wechat-connection.enc"

[retention]
accepted_days = 30
dead_letter_days = 90

[logging]
filter = "info,promptdock_server=info"
format = "json"
EOF
chmod 600 "$config"

"$relay_bin" init --config "$config" >/dev/null
create_output="$($relay_bin device create --name LINUX-SMOKE --scope notify:write --scope notify:read_own --scope channel:read --config "$config")"
device_id="$(printf '%s\n' "$create_output" | sed -n 's/^device_id=//p')"
device_token="$(printf '%s\n' "$create_output" | sed -n 's/^device_token=//p')"
[[ "$device_id" =~ ^[0-9a-f-]{36}$ && "$device_token" == pdv2.* ]] || {
    printf 'device create output did not match the credential contract\n' >&2
    exit 1
}
device_secret="${device_token##*.}"
printf 'header = "Authorization: Bearer %s"\n' "$device_token" >"$auth_config"
chmod 600 "$auth_config"

base_url="http://127.0.0.1:$port"
start_server() {
    local log_file=$1
    "$relay_bin" serve --config "$config" >"$log_file" 2>&1 &
    server_pid=$!
    for _ in $(seq 1 100); do
        if curl --silent --fail --max-time 2 "$base_url/health/ready" >/dev/null; then
            return 0
        fi
        kill -0 "$server_pid" 2>/dev/null || {
            printf 'relay exited before readiness; see %s\n' "$log_file" >&2
            return 1
        }
        sleep 0.1
    done
    printf 'relay did not become ready in time; see %s\n' "$log_file" >&2
    return 1
}

stop_server() {
    local log_file=$1
    kill -TERM "$server_pid"
    for _ in $(seq 1 200); do
        kill -0 "$server_pid" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$server_pid" 2>/dev/null; then
        printf 'relay exceeded its graceful shutdown deadline\n' >&2
        return 1
    fi
    local exit_code=0
    wait "$server_pid" || exit_code=$?
    server_pid=''
    [[ "$exit_code" == "0" ]] || {
        printf 'relay exited with code %s after SIGTERM\n' "$exit_code" >&2
        return 1
    }
    grep -q 'shutdown requested' "$log_file"
    grep -q 'shutdown complete' "$log_file"
}

first_log="$temporary_root/relay-first.log"
start_server "$first_log"
[[ "$(curl --silent --fail "$base_url/health/live")" == '{"status":"ok"}' ]]
[[ "$(curl --silent --fail "$base_url/health/ready")" == '{"status":"ready"}' ]]
unauthorized_code="$(curl --silent --output "$temporary_root/unauthorized.json" --write-out '%{http_code}' "$base_url/v1/server-info")"
[[ "$unauthorized_code" == "401" ]]
curl --silent --fail --config "$auth_config" "$base_url/v1/server-info" >/dev/null

created_at="$(python3 -c 'import time; print(time.time_ns() // 1_000_000)')"
expires_at=$((created_at + 300000))
notification_id="linux-smoke-$device_id"
notification_body='SENTINEL_LINUX_SMOKE_BODY_DO_NOT_LOG'
notification_json="$(printf '{"schemaVersion":1,"notificationId":"%s","dedupeKey":"%s","kind":"test","priority":100,"title":"Linux smoke","body":"%s","correlationKey":"linux-smoke","createdAt":%s,"expiresAt":%s}' "$notification_id" "$notification_id" "$notification_body" "$created_at" "$expires_at")"
accepted_code="$(curl --silent --show-error --config "$auth_config" --output "$temporary_root/accepted.json" --write-out '%{http_code}' --header 'Content-Type: application/json' --data "$notification_json" "$base_url/v1/notifications")"
[[ "$accepted_code" == "202" ]]
accepted_at="$(python3 -c 'import json,sys; value=json.load(sys.stdin); assert value["relayStatus"]=="accepted" and value["existing"] is False; print(value["acceptedAt"])' <"$temporary_root/accepted.json")"

blocked=''
for _ in $(seq 1 50); do
    curl --silent --fail --config "$auth_config" "$base_url/v1/notifications/$notification_id" >"$temporary_root/status.json"
    blocked="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])' <"$temporary_root/status.json")"
    [[ "$blocked" == "blocked_reconnect" ]] && break
    sleep 0.1
done
[[ "$blocked" == "blocked_reconnect" ]]
stop_server "$first_log"

second_log="$temporary_root/relay-second.log"
start_server "$second_log"
curl --silent --fail --config "$auth_config" "$base_url/v1/notifications/$notification_id" >"$temporary_root/restarted-status.json"
[[ "$(python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])' <"$temporary_root/restarted-status.json")" == "blocked_reconnect" ]]
replay_code="$(curl --silent --show-error --config "$auth_config" --output "$temporary_root/replay.json" --write-out '%{http_code}' --header 'Content-Type: application/json' --data "$notification_json" "$base_url/v1/notifications")"
[[ "$replay_code" == "202" ]]
python3 -c 'import json,sys; value=json.load(sys.stdin); assert value["existing"] is True and value["acceptedAt"] == int(sys.argv[1])' "$accepted_at" <"$temporary_root/replay.json"
stop_server "$second_log"

scan_files=("$first_log" "$second_log" "$database")
[[ -f "${database}-wal" ]] && scan_files+=("${database}-wal")
[[ -f "${database}-shm" ]] && scan_files+=("${database}-shm")
if grep -aFq "$device_token" "${scan_files[@]}"; then
    printf 'device bearer leaked into a log or SQLite file\n' >&2
    exit 1
fi
if grep -aFq "$device_secret" "${scan_files[@]}"; then
    printf 'device secret leaked into a log or SQLite file\n' >&2
    exit 1
fi
if grep -aFq "$notification_body" "$first_log" "$second_log" 2>/dev/null; then
    printf 'notification body leaked into logs\n' >&2
    exit 1
fi

printf 'Linux smoke passed: init, device, health, auth, 202, blocked_reconnect, restart, replay, SIGTERM\n'
