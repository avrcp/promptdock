#!/usr/bin/env bash
set -euo pipefail

bundle="$1"
binary="$2"
repo_root="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
mkdir -p "$bundle/bin" "$bundle/admin/assets" "$bundle/contracts/http-api/v1" "$bundle/contracts/admin-api/v2" "$bundle/contracts/gateway/v5" "$bundle/sbom"
install -m 0755 -- "$binary" "$bundle/bin/promptdock-relay"
printf '%s\n' 'console.log("PromptDock Admin")' > "$bundle/admin/assets/index-test.js"
printf '%s\n' '<!doctype html><script type="module" src="/assets/index-test.js"></script>' > "$bundle/admin/index.html"
cp -a -- "$repo_root/contracts/server-http/v1/." "$bundle/contracts/http-api/v1/"
cp -a -- "$repo_root/contracts/admin-api/v2/." "$bundle/contracts/admin-api/v2/"
cp -a -- "$repo_root/contracts/node-link/v5/." "$bundle/contracts/gateway/v5/"
for relative in \
    deploy/caddy/Caddyfile.example \
    deploy/config.production.toml \
    deploy/systemd/promptdock-relay.service \
    scripts/backup-ubuntu.sh \
    scripts/install-ubuntu.sh \
    scripts/lib/deploy-common.sh \
    scripts/lib/validate-release-bundle.py \
    scripts/reset-relay-data.sh \
    scripts/restore-ubuntu.sh \
    scripts/rollback-ubuntu.sh \
    scripts/upgrade-ubuntu.sh; do
    # The release bundle keeps its historical layout (config under deploy/, ops
    # scripts under scripts/); the monorepo stores these sources under
    # deploy/examples/ and scripts/server/ respectively.
    case "$relative" in
        deploy/config.production.toml) source="$repo_root/deploy/examples/config.production.toml" ;;
        scripts/*) source="$repo_root/scripts/server/${relative#scripts/}" ;;
        *) source="$repo_root/$relative" ;;
    esac
    mode=0644
    case "$relative" in scripts/*.sh|scripts/lib/*.py) mode=0755 ;; esac
    install -D -m "$mode" -- "$source" "$bundle/$relative"
done
python3 - "$bundle/deploy/caddy/Caddyfile.example" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
value = path.read_text(encoding="utf-8").replace(
    "<CADDY_HASH_ONLY>",
    "$2a$14$XDpLbSqcrKPi7t6628tkFOrQkD8QITsqF7LKBXgUSonXuGVfznjja",
)
path.write_text(value, encoding="utf-8", newline="\n")
PY
build_info="$("$binary" build-info)"
version="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["releaseVersion"])' "$build_info")"
commit="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["sourceCommit"])' "$build_info")"
python3 "$repo_root/scripts/server/generate-release-metadata.py" \
    --repository "$repo_root" --output "$bundle/sbom" \
    --release-version "$version" --source-commit "$commit" --desktop-commit "$(printf '2%.0s' {1..40})"
printf '%s\n' 'test unified release bundle' > "$bundle/BUILD-METADATA.txt"
python3 - "$bundle" "$version" "$commit" <<'PY'
import hashlib, json, sys
from pathlib import Path
root, version, commit = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
def digest(path): return hashlib.sha256((root / path).read_bytes()).hexdigest()
admin_files=[]
for path in sorted((root / "admin").rglob("*")):
    if path.is_file() and path.name != "admin-artifact-manifest.json":
        name=path.relative_to(root / "admin").as_posix(); data=path.read_bytes()
        admin_files.append({"path":name,"sha256":hashlib.sha256(data).hexdigest(),"bytes":len(data)})
artifact={"schemaVersion":1,"buildVersion":version,"sourceCommit":commit,"adminApiMajor":2,"files":admin_files}
(root / "admin/admin-artifact-manifest.json").write_text(json.dumps(artifact,indent=2)+"\n",encoding="utf-8")
def record(path): return {"path":path,"sha256":digest(path)}
manifest={
 "schemaVersion":4,"releaseVersion":version,"sourceCommit":commit,"desktopCommit":"2"*40,
 "target":"x86_64-unknown-linux-gnu","rustToolchain":"1.98.0",
 "binary":{**record("bin/promptdock-relay"),"reportedVersion":f"promptdock-relay {version}"},
 "adminUi":{"entrypoint":"admin/index.html","artifactManifest":record("admin/admin-artifact-manifest.json"),"buildVersion":version,"sourceCommit":commit,"adminApiMajor":2},
 "contracts":{"httpApiMajor":1,"adminApiMajor":2,"gatewayWireMajor":5,"httpApiManifest":record("contracts/http-api/v1/manifest.json"),"adminApiManifest":record("contracts/admin-api/v2/manifest.json"),"gatewayManifest":record("contracts/gateway/v5/manifest.json")},
 "databaseSchema":{"identity":"promptdock-relay-v4","revision":3},
 "features":{"server":["notifications","device_scopes_v1","remote_gateway_v5","remote_runs_v2","remote_harness_control_v2","result_pages_v1"],"desktopCapabilities":["notify:write","notify:read_own"]},
 "supplyChain":{"cargoCycloneDx":record("sbom/cargo.cdx.json"),"pnpmCycloneDx":record("sbom/pnpm.cdx.json"),"licenseInventory":record("sbom/LICENSE-INVENTORY.json")},
 "compatibilityMatrix":record("sbom/COMPATIBILITY-MATRIX.json"),
 "deploymentAssets":{"productionConfig":record("deploy/config.production.toml"),"systemdUnit":record("deploy/systemd/promptdock-relay.service"),"caddyConfig":record("deploy/caddy/Caddyfile.example"),"deployCommon":record("scripts/lib/deploy-common.sh"),"bundleValidator":record("scripts/lib/validate-release-bundle.py"),"installScript":record("scripts/install-ubuntu.sh"),"upgradeScript":record("scripts/upgrade-ubuntu.sh"),"rollbackScript":record("scripts/rollback-ubuntu.sh"),"resetDataScript":record("scripts/reset-relay-data.sh"),"backupScript":record("scripts/backup-ubuntu.sh"),"restoreScript":record("scripts/restore-ubuntu.sh")},
 "qualityGate":{"command":"corepack pnpm quality && corepack pnpm --filter @promptdock/relay-admin test:e2e:integration","status":"passed"}}
(root / "RELEASE-MANIFEST.json").write_text(json.dumps(manifest,indent=2)+"\n",encoding="utf-8")
PY
(
    cd "$bundle"
    # The listing is snapshotted before the digests are written, so the manifest
    # can never be walking the tree while it is also being added to that tree.
    listing="$(find . -type f ! -name SHA256SUMS.txt -printf '%P\n' | LC_ALL=C sort)"
    while IFS= read -r relative; do sha256sum -- "$relative"; done <<< "$listing" > SHA256SUMS.txt
)
