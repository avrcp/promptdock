#!/usr/bin/env bash
set -euo pipefail

mode="${1:-check}"
case "$mode" in
    check|update) ;;
    *) printf 'usage: %s [check|update]\n' "$0" >&2; exit 2 ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "$script_dir/../.." && pwd)"
contract_dir="$repository_root/contracts/admin-api/v2"
fixture_dir="$contract_dir/fixtures"
manifest="$contract_dir/manifest.json"
contract_name='promptdock-relay-admin-api-v2'

[[ -d "$fixture_dir" ]] || { printf 'contracts/admin-api/v2/fixtures is missing\n' >&2; exit 1; }
mapfile -t files < <(printf '%s\n' "$fixture_dir"/*-v2.json | LC_ALL=C sort)
if ((${#files[@]} == 0)) || [[ ! -f "${files[0]}" ]]; then
    printf 'no Admin API v2 fixtures were found\n' >&2
    exit 1
fi

if [[ "$mode" == "check" ]]; then
    # Checks must work when the repository is mounted read-only.
    temporary="$(mktemp "${TMPDIR:-/tmp}/promptdock-admin-api-manifest.XXXXXX")"
else
    temporary="$(mktemp "$contract_dir/.manifest.XXXXXX")"
fi
trap 'rm -f -- "$temporary"' EXIT

hash_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -- "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -- "$1" | awk '{print $1}'
    else
        printf 'sha256sum or shasum is required\n' >&2
        exit 1
    fi
}

{
    printf '{\n'
    printf '  "manifestVersion": 1,\n'
    printf '  "contract": "%s",\n' "$contract_name"
    printf '  "fixtures": [\n'
    file_count=${#files[@]}
    for index in "${!files[@]}"; do
        file="${files[$index]}"
        name="$(basename -- "$file")"
        [[ "$name" =~ ^[a-z0-9][a-z0-9.-]*-v2\.json$ ]] || {
            printf 'fixture name is not canonical: %s\n' "$name" >&2
            exit 1
        }
        sha256="$(hash_file "$file")"
        printf '    {\n'
        printf '      "path": "%s",\n' "$name"
        printf '      "sha256": "%s",\n' "$sha256"
        printf '      "mediaType": "application/json",\n'
        printf '      "schemaVersion": 2\n'
        if ((index == file_count - 1)); then
            printf '    }\n'
        else
            printf '    },\n'
        fi
    done
    printf '  ]\n'
    printf '}\n'
} >"$temporary"

if [[ "$mode" == check ]]; then
    [[ -f "$manifest" ]] || { printf 'contracts/admin-api/v2/manifest.json is missing\n' >&2; exit 1; }
    cmp --silent -- "$temporary" "$manifest" || {
        printf 'Admin API contract manifest drifted; run scripts/contracts/admin-api-contract-manifest.sh update\n' >&2
        exit 1
    }
    printf 'Admin API contract manifest is current\n'
    exit 0
fi

mv -- "$temporary" "$manifest"
trap - EXIT
printf 'Admin API contract manifest updated\n'
