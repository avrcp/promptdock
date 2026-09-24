#!/usr/bin/env bash
set -euo pipefail

command_name="${1:-}"
if [[ "$command_name" == "--version" ]]; then
    printf '%s\n' 'promptdock-relay 0.6.1-rc.1'
    exit 0
fi
if [[ "$command_name" == "build-info" ]]; then
    printf '%s\n' '{"releaseVersion":"0.6.1-rc.1","sourceCommit":"3333333333333333333333333333333333333333","adminApiMajor":2}'
    exit 0
fi
if [[ "${MOCK_RELAY_FAIL:-}" == "$command_name" ]]; then
    exit 70
fi
case "$command_name" in
    init)
        printf '%s\n' '{"status":"initialized","schemaIdentity":"promptdock-relay-v4","schemaRevision":3}'
        ;;
    status)
        printf '%s\n' '{"status":"ok","devices":{"total":0}}'
        ;;
    doctor)
        printf '%s\n' '{"status":"ok","checks":[]}'
        ;;
    *)
        exit 64
        ;;
esac
