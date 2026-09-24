#!/usr/bin/env bash
set -euo pipefail

mode="${1:-check}"
case "$mode" in
    check|update) ;;
    *) printf 'usage: %s [check|update]\n' "$0" >&2; exit 2 ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd -- "$script_dir/../.." && pwd)"
fixture_dir="$repository_root/contracts/server-http/v1"
manifest="$fixture_dir/manifest.json"
mapfile -t files < <(printf '%s\n' "$fixture_dir"/*-v1.json | LC_ALL=C sort)
if ((${#files[@]} == 0)) || [[ ! -f "${files[0]}" ]]; then
    printf 'no public API v1 fixtures were found\n' >&2
    exit 1
fi

if [[ "$mode" == "check" ]]; then
    # The quality container mounts the repository read-only. Contract checks
    # only need a comparison file, so keep it in the writable system temp dir.
    temporary="$(mktemp "${TMPDIR:-/tmp}/promptdock-api-manifest.XXXXXX")"
else
    # Updates stay in the fixture directory so the final rename is atomic.
    temporary="$(mktemp "$fixture_dir/.manifest.XXXXXX")"
fi
trap 'rm -f -- "$temporary"' EXIT
{
    printf '{\n'
    printf '  "manifestVersion": 1,\n'
    printf '  "contract": "promptdock-relay-api-v1",\n'
    printf '  "fixtures": [\n'
    for index in "${!files[@]}"; do
        file="${files[$index]}"
        name="$(basename -- "$file")"
        [[ "$name" =~ ^[a-z0-9][a-z0-9.-]*-v1\.json$ ]] || {
            printf 'fixture name is not canonical: %s\n' "$name" >&2
            exit 1
        }
        sha256="$(sha256sum -- "$file" | awk '{print $1}')"
        printf '    {\n'
        printf '      "path": "%s",\n' "$name"
        printf '      "sha256": "%s",\n' "$sha256"
        printf '      "mediaType": "application/json",\n'
        printf '      "schemaVersion": 1\n'
        if ((index == ${#files[@]} - 1)); then
            printf '    }\n'
        else
            printf '    },\n'
        fi
    done
    printf '  ]\n'
    printf '}\n'
} >"$temporary"

if [[ "$mode" == check ]]; then
    [[ -f "$manifest" ]] || { printf 'contracts/server-http/v1/manifest.json is missing\n' >&2; exit 1; }
    cmp --silent -- "$temporary" "$manifest" || {
        printf 'API contract manifest drifted; run scripts/contracts/api-contract-manifest.sh update\n' >&2
        exit 1
    }
    printf 'API contract manifest is current\n'
    exit 0
fi

mv -- "$temporary" "$manifest"
trap - EXIT
printf 'API contract manifest updated\n'
