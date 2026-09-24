#!/usr/bin/env python3
"""Strict, fail-closed validator for a unified PromptDock Relay/Admin bundle."""

from __future__ import annotations

import hashlib
import json
import re
import stat
import subprocess
import sys
import tomllib
from pathlib import Path
from urllib.parse import urlsplit

HEX40 = re.compile(r"[0-9a-f]{40}\Z")
HEX64 = re.compile(r"[0-9a-f]{64}\Z")
SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?\Z")
FIXED_FILES = {
    "BUILD-METADATA.txt",
    "RELEASE-MANIFEST.json",
    "bin/promptdock-relay",
    "deploy/caddy/Caddyfile.example",
    "deploy/config.production.toml",
    "deploy/systemd/promptdock-relay.service",
    "scripts/backup-ubuntu.sh",
    "scripts/install-ubuntu.sh",
    "scripts/lib/deploy-common.sh",
    "scripts/lib/validate-release-bundle.py",
    "scripts/reset-relay-data.sh",
    "scripts/restore-ubuntu.sh",
    "scripts/rollback-ubuntu.sh",
    "scripts/upgrade-ubuntu.sh",
    "sbom/COMPATIBILITY-MATRIX.json",
    "sbom/LICENSE-INVENTORY.json",
    "sbom/cargo.cdx.json",
    "sbom/pnpm.cdx.json",
}
EXECUTABLE_FILES = {
    "bin/promptdock-relay",
    "scripts/backup-ubuntu.sh",
    "scripts/install-ubuntu.sh",
    "scripts/lib/deploy-common.sh",
    "scripts/lib/validate-release-bundle.py",
    "scripts/reset-relay-data.sh",
    "scripts/restore-ubuntu.sh",
    "scripts/rollback-ubuntu.sh",
    "scripts/upgrade-ubuntu.sh",
}
ADMIN_INVENTORIES = {
    "inventories/actions-v2.json",
    "inventories/capabilities-v2.json",
    "inventories/routes-v2.json",
    "inventories/states-v2.json",
}
FORBIDDEN_ADMIN_MARKERS = (
    "dev-scenario-select",
    "promptdock-relay-admin:dev-scenario",
    "mock-pdv2.not-a-real-secret",
    "sourceMappingURL=",
    "__ADMIN_MOCK_BUILD__",
)
EXTERNAL_ADMIN_REQUEST = re.compile(r"(?:fetch|XMLHttpRequest|WebSocket)\s*\([^)]*https?://", re.IGNORECASE)


def fail(message: str) -> None:
    raise ValueError(message)


def exact_keys(value: dict, expected: set[str], label: str) -> None:
    if not isinstance(value, dict) or set(value) != expected:
        fail(f"{label} keys differ from the frozen schema")


def regular_file(root: Path, relative: str) -> Path:
    if relative.startswith("/") or ".." in Path(relative).parts:
        fail("bundle path escapes its root")
    path = root / relative
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or path.is_symlink():
        fail(f"bundle member is not a regular file: {relative}")
    return path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def json_file(root: Path, relative: str) -> dict:
    value = json.loads(regular_file(root, relative).read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        fail(f"JSON object expected: {relative}")
    return value


def validate_hash_record(root: Path, record: dict, expected_path: str, label: str) -> None:
    exact_keys(record, {"path", "sha256"}, label)
    if record["path"] != expected_path or not isinstance(record["sha256"], str) or not HEX64.fullmatch(record["sha256"]):
        fail(f"{label} identity is invalid")
    if sha256(regular_file(root, expected_path)) != record["sha256"]:
        fail(f"{label} hash does not match")


def contract_inventory(root: Path, contract_root: str, identity: str, fixture_prefix: str = "") -> set[str]:
    manifest_path = f"{contract_root}/manifest.json"
    manifest = json_file(root, manifest_path)
    if manifest.get("manifestVersion") != 1 or manifest.get("contract") != identity or set(manifest) != {"manifestVersion", "contract", "fixtures"}:
        fail(f"{identity} manifest identity differs")
    fixtures = manifest["fixtures"]
    if not isinstance(fixtures, list) or not fixtures:
        fail(f"{identity} fixture inventory is empty")
    expected = {manifest_path}
    seen: set[str] = set()
    for entry in fixtures:
        exact_keys(entry, {"path", "sha256", "mediaType", "schemaVersion"}, f"{identity} fixture")
        name = entry["path"]
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9._-]+\.json", name) or name in seen:
            fail(f"{identity} fixture path is invalid")
        if not isinstance(entry["sha256"], str) or not HEX64.fullmatch(entry["sha256"]):
            fail(f"{identity} fixture hash is invalid")
        seen.add(name)
        relative = f"{contract_root}/{fixture_prefix}{name}"
        if sha256(regular_file(root, relative)) != entry["sha256"]:
            fail(f"{identity} fixture hash differs: {name}")
        expected.add(relative)
    return expected


def admin_inventory(root: Path) -> tuple[set[str], dict]:
    manifest_path = "admin/admin-artifact-manifest.json"
    manifest = json_file(root, manifest_path)
    exact_keys(manifest, {"schemaVersion", "buildVersion", "sourceCommit", "adminApiMajor", "files"}, "Admin artifact manifest")
    if manifest["schemaVersion"] != 1 or not SEMVER.fullmatch(str(manifest["buildVersion"])) or not HEX40.fullmatch(str(manifest["sourceCommit"])) or manifest["adminApiMajor"] != 2:
        fail("Admin artifact identity is invalid")
    expected = {manifest_path}
    seen: set[str] = set()
    files = manifest["files"]
    if not isinstance(files, list) or not files:
        fail("Admin artifact inventory is empty")
    for entry in files:
        exact_keys(entry, {"path", "sha256", "bytes"}, "Admin artifact file")
        name = entry["path"]
        if not isinstance(name, str) or name.startswith("/") or ".." in Path(name).parts or name in seen:
            fail("Admin artifact path is invalid")
        seen.add(name)
        relative = f"admin/{name}"
        path = regular_file(root, relative)
        if name.endswith(".map"):
            fail(f"Admin source map is forbidden: {name}")
        text = path.read_bytes().decode("utf-8", errors="ignore")
        for marker in FORBIDDEN_ADMIN_MARKERS:
            if marker in text:
                fail(f"Admin artifact contains forbidden marker {marker!r}: {name}")
        if EXTERNAL_ADMIN_REQUEST.search(text):
            fail(f"Admin artifact contains an unexpected external request: {name}")
        if not isinstance(entry["bytes"], int) or entry["bytes"] < 0 or path.stat().st_size != entry["bytes"]:
            fail(f"Admin artifact size differs: {name}")
        if not isinstance(entry["sha256"], str) or sha256(path) != entry["sha256"]:
            fail(f"Admin artifact hash differs: {name}")
        expected.add(relative)
    if "index.html" not in seen:
        fail("Admin artifact is missing index.html")
    index = regular_file(root, "admin/index.html").read_text(encoding="utf-8")
    if 'type="module"' not in index or "assets/" not in index:
        fail("Admin index is not a production module entrypoint")
    return expected, manifest


def parse_checksums(root: Path, expected_files: set[str]) -> None:
    checksum_path = regular_file(root, "SHA256SUMS.txt")
    inventory: set[str] = set()
    directories: set[str] = set()
    for path in root.rglob("*"):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            fail(f"bundle contains a symbolic link: {relative}")
        if path.is_file():
            inventory.add(relative)
        elif path.is_dir():
            directories.add(relative)
        else:
            fail(f"bundle contains an unsupported entry: {relative}")
    if inventory != expected_files | {"SHA256SUMS.txt"}:
        fail("on-disk inventory differs from the frozen bundle")
    expected_directories: set[str] = set()
    for relative in inventory:
        parent = Path(relative).parent
        while parent != Path("."):
            expected_directories.add(parent.as_posix())
            parent = parent.parent
    if directories != expected_directories:
        fail("on-disk directory inventory differs from the frozen bundle")
    actual: dict[str, str] = {}
    for line in checksum_path.read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9._/-]+)", line)
        if match is None or match.group(2) in actual:
            fail("SHA256SUMS.txt is malformed or contains duplicates")
        actual[match.group(2)] = match.group(1)
    if set(actual) != expected_files:
        fail("SHA256SUMS.txt inventory differs from the frozen bundle")
    for relative, expected_hash in actual.items():
        if sha256(regular_file(root, relative)) != expected_hash:
            fail(f"checksum mismatch: {relative}")


def route_paths(route: object) -> list[str]:
    if not isinstance(route, dict):
        return []
    paths: list[str] = []
    for matcher in route.get("match", []):
        if isinstance(matcher, dict) and isinstance(matcher.get("path"), list):
            paths.extend(path for path in matcher["path"] if isinstance(path, str))
    return paths


def contains_pair(value: object, key: str, expected: object) -> bool:
    if isinstance(value, dict):
        return value.get(key) == expected or any(contains_pair(item, key, expected) for item in value.values())
    if isinstance(value, list):
        return any(contains_pair(item, key, expected) for item in value)
    return False


def count_pair(value: object, key: str, expected: object) -> int:
    if isinstance(value, dict):
        return int(value.get(key) == expected) + sum(count_pair(item, key, expected) for item in value.values())
    if isinstance(value, list):
        return sum(count_pair(item, key, expected) for item in value)
    return 0


def route_lists(value: object) -> list[list[dict]]:
    found: list[list[dict]] = []
    if isinstance(value, dict):
        routes = value.get("routes")
        if isinstance(routes, list) and all(isinstance(route, dict) for route in routes):
            found.append(routes)
        for item in value.values():
            found.extend(route_lists(item))
    elif isinstance(value, list):
        for item in value:
            found.extend(route_lists(item))
    return found


def validate_caddy_routes(path: Path, expected_origin: str) -> None:
    adapted = subprocess.run(
        ["caddy", "adapt", "--config", str(path), "--adapter", "caddyfile"],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if adapted.returncode != 0:
        fail("Caddy configuration cannot be adapted")
    config = json.loads(adapted.stdout)
    origin = urlsplit(expected_origin)
    if origin.scheme != "https" or not origin.hostname or origin.path or origin.query or origin.fragment:
        fail("Admin allowed_origin must be a bare HTTPS origin")
    expected_port = origin.port or 443
    if not contains_pair(config, "host", [origin.hostname]) or not contains_pair(config, "listen", [f":{expected_port}"]):
        fail("Caddy site address differs from Admin allowed_origin")
    public_paths = {"/health/*", "/v1/*", "/v5/*"}
    candidates: list[tuple[list[dict], int, dict]] = []
    for routes in route_lists(config):
        for index, route in enumerate(routes):
            if set(route_paths(route)) == public_paths and contains_pair(route, "dial", "127.0.0.1:8080"):
                candidates.append((routes, index, route))
    if len(candidates) != 1:
        fail("Caddy must expose exactly one frozen public Relay route")
    siblings, public_index, public = candidates[0]
    group = public.get("group")
    result_routes = [route for route in siblings[:public_index]
                     if group is not None and route.get("group") == group
                     and set(route_paths(route)) == {"/r/*", "/result-assets/*"}
                     and contains_pair(route, "dial", "127.0.0.1:8080")
                     and not contains_pair(route, "handler", "authentication")]
    if len(result_routes) != 1:
        fail("Caddy must expose exactly one isolated result read route before Admin")
    operators = [
        route
        for route in siblings[public_index + 1 :]
        if group is not None
        and route.get("group") == group
        and not route.get("match")
        and contains_pair(route, "handler", "authentication")
        and contains_pair(route, "dial", "127.0.0.1:8081")
        and contains_pair(route, "root", "/opt/promptdock-relay/current/admin")
    ]
    if len(operators) != 1:
        fail("Caddy Admin API and SPA must share one authenticated fallback route")
    operator = operators[0]
    secured_lists = []
    for routes in route_lists(operator):
        auth = [index for index, route in enumerate(routes) if contains_pair(route, "handler", "authentication") and not route.get("match")]
        admin = [index for index, route in enumerate(routes) if set(route_paths(route)) == {"/admin/api/v2/*"} and contains_pair(route, "dial", "127.0.0.1:8081")]
        spa = [index for index, route in enumerate(routes) if not route.get("match") and contains_pair(route, "root", "/opt/promptdock-relay/current/admin") and contains_pair(route, "handler", "file_server")]
        if len(auth) == len(admin) == len(spa) == 1 and auth[0] < admin[0] < spa[0]:
            secured_lists.append((routes, auth[0], admin[0], spa[0]))
    if len(secured_lists) != 1:
        fail("Caddy authentication must execute before the Admin API and SPA fallback")
    routes, auth_index, admin_index, spa_index = secured_lists[0]
    if routes[admin_index].get("group") is None or routes[admin_index].get("group") != routes[spa_index].get("group"):
        fail("Caddy Admin API and SPA fallback must be mutually exclusive handles")
    auth_route = routes[auth_index]
    accounts = []
    for item in route_lists({"routes": [auth_route]}):
        for route in item:
            for handler in route.get("handle", []):
                if isinstance(handler, dict) and handler.get("handler") == "authentication":
                    accounts.extend(handler.get("providers", {}).get("http_basic", {}).get("accounts", []))
    if not accounts or any(not isinstance(account.get("password"), str) or not account.get("password") for account in accounts):
        fail("Caddy Basic Auth account inventory is empty")
    if count_pair(config, "dial", "127.0.0.1:8080") != count_pair(public, "dial", "127.0.0.1:8080") + count_pair(result_routes[0], "dial", "127.0.0.1:8080"):
        fail("Caddy contains an extra public Relay upstream")
    if count_pair(config, "dial", "127.0.0.1:8081") != count_pair(routes[admin_index], "dial", "127.0.0.1:8081"):
        fail("Caddy contains an Admin upstream outside the authenticated route")
    if count_pair(config, "root", "/opt/promptdock-relay/current/admin") != count_pair(routes[spa_index], "root", "/opt/promptdock-relay/current/admin"):
        fail("Caddy contains an Admin SPA root outside the authenticated route")


def validate_systemd_unit(unit: str) -> None:
    directives: dict[str, list[str]] = {}
    section = ""
    for raw_line in unit.splitlines():
        line = raw_line.strip()
        if not line or line.startswith(("#", ";")):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            continue
        if section == "Service" and "=" in line:
            key, value = line.split("=", 1)
            directives.setdefault(key, []).append(value)
    exact = {
        "Type": ["simple"],
        "User": ["promptdock-relay"],
        "Group": ["promptdock-relay"],
        "WorkingDirectory": ["/var/lib/promptdock-relay"],
        "ExecStart": ["/opt/promptdock-relay/current/bin/promptdock-relay serve --config /etc/promptdock-relay/config.toml"],
        "LoadCredential": ["relay-master-key:/etc/promptdock-relay/master.key", "relay-confirmation-key:/etc/promptdock-relay/confirmation.key"],
        "StateDirectory": ["promptdock-relay"],
        "StateDirectoryMode": ["0700"],
        "ConfigurationDirectory": ["promptdock-relay"],
        "ConfigurationDirectoryMode": ["0750"],
        "UMask": ["0077"],
        "NoNewPrivileges": ["true"],
        "PrivateDevices": ["true"],
        "PrivateMounts": ["true"],
        "PrivateTmp": ["true"],
        "ProtectClock": ["true"],
        "ProtectControlGroups": ["true"],
        "ProtectHome": ["true"],
        "ProtectHostname": ["true"],
        "ProtectKernelLogs": ["true"],
        "ProtectKernelModules": ["true"],
        "ProtectKernelTunables": ["true"],
        "ProtectProc": ["invisible"],
        "ProtectSystem": ["strict"],
        "ProcSubset": ["pid"],
        "LockPersonality": ["true"],
        "MemoryDenyWriteExecute": ["true"],
        "RemoveIPC": ["true"],
        "RestrictNamespaces": ["true"],
        "RestrictRealtime": ["true"],
        "RestrictSUIDSGID": ["true"],
        "RestrictAddressFamilies": ["AF_UNIX AF_INET AF_INET6"],
        "SocketBindDeny": ["any"],
        "SocketBindAllow": ["tcp:8080", "tcp:8081"],
        "CapabilityBoundingSet": [""],
        "AmbientCapabilities": [""],
    }
    for key, expected in exact.items():
        actual = directives.get(key)
        if key in {"LoadCredential", "SocketBindAllow"}:
            if actual is None or sorted(actual) != sorted(expected):
                fail(f"systemd hardening directive differs: {key}")
        elif actual != expected:
            fail(f"systemd hardening directive differs: {key}")


def validate(root: Path) -> None:
    if not root.is_absolute() or not root.is_dir() or root.is_symlink():
        fail("release bundle must be an absolute real directory")

    admin_files, admin_artifact = admin_inventory(root)
    http_files = contract_inventory(root, "contracts/http-api/v1", "promptdock-relay-api-v1")
    admin_contract_files = contract_inventory(root, "contracts/admin-api/v2", "promptdock-relay-admin-api-v2", "fixtures/") | {
        f"contracts/admin-api/v2/{name}" for name in ADMIN_INVENTORIES
    } | {"contracts/admin-api/v2/openapi.json"}
    gateway_files = contract_inventory(root, "contracts/gateway/v5", "promptdock-relay-gateway-v5")
    expected_files = FIXED_FILES | admin_files | http_files | admin_contract_files | gateway_files
    parse_checksums(root, expected_files)
    for relative in EXECUTABLE_FILES:
        if stat.S_IMODE(regular_file(root, relative).stat().st_mode) != 0o755:
            fail(f"bundle executable mode differs: {relative}")

    manifest = json_file(root, "RELEASE-MANIFEST.json")
    exact_keys(manifest, {"schemaVersion", "releaseVersion", "sourceCommit", "desktopCommit", "target", "rustToolchain", "binary", "adminUi", "contracts", "databaseSchema", "features", "supplyChain", "compatibilityMatrix", "deploymentAssets", "qualityGate"}, "manifest")
    if manifest["schemaVersion"] != 4 or not isinstance(manifest["releaseVersion"], str) or not SEMVER.fullmatch(manifest["releaseVersion"]):
        fail("unsupported release manifest schema or version")
    if not HEX40.fullmatch(str(manifest["sourceCommit"])) or not HEX40.fullmatch(str(manifest["desktopCommit"])):
        fail("sourceCommit and desktopCommit must be full lowercase commits")
    if manifest["target"] != "x86_64-unknown-linux-gnu" or manifest["rustToolchain"] != "1.98.0":
        fail("release target or Rust toolchain differs")

    binary = manifest["binary"]
    exact_keys(binary, {"path", "sha256", "reportedVersion"}, "binary")
    validate_hash_record(root, {"path": binary["path"], "sha256": binary["sha256"]}, "bin/promptdock-relay", "binary")
    if binary["reportedVersion"] != f"promptdock-relay {manifest['releaseVersion']}":
        fail("binary reported version differs")
    admin = manifest["adminUi"]
    exact_keys(admin, {"entrypoint", "artifactManifest", "buildVersion", "sourceCommit", "adminApiMajor"}, "adminUi")
    if admin["entrypoint"] != "admin/index.html" or admin["buildVersion"] != manifest["releaseVersion"] or admin["sourceCommit"] != manifest["sourceCommit"] or admin["adminApiMajor"] != 2:
        fail("Admin UI identity differs from the unified release")
    validate_hash_record(root, admin["artifactManifest"], "admin/admin-artifact-manifest.json", "Admin artifact manifest")
    if admin_artifact["buildVersion"] != admin["buildVersion"] or admin_artifact["sourceCommit"] != admin["sourceCommit"] or admin_artifact["adminApiMajor"] != admin["adminApiMajor"]:
        fail("Admin artifact manifest differs from adminUi")

    contracts = manifest["contracts"]
    exact_keys(contracts, {"httpApiMajor", "adminApiMajor", "gatewayWireMajor", "httpApiManifest", "adminApiManifest", "gatewayManifest"}, "contracts")
    if (contracts["httpApiMajor"], contracts["adminApiMajor"], contracts["gatewayWireMajor"]) != (1, 2, 5):
        fail("contract majors differ from the frozen release")
    validate_hash_record(root, contracts["httpApiManifest"], "contracts/http-api/v1/manifest.json", "HTTP API manifest")
    validate_hash_record(root, contracts["adminApiManifest"], "contracts/admin-api/v2/manifest.json", "Admin API manifest")
    validate_hash_record(root, contracts["gatewayManifest"], "contracts/gateway/v5/manifest.json", "Gateway manifest")
    if manifest["databaseSchema"] != {"identity": "promptdock-relay-v4", "revision": 3}:
        fail("database schema identity differs")

    expected_features = {"server": ["notifications", "device_scopes_v1", "remote_gateway_v5", "remote_runs_v2", "remote_harness_control_v2", "result_pages_v1"], "desktopCapabilities": ["notify:write", "notify:read_own"]}
    if manifest["features"] != expected_features:
        fail("Gateway feature/capability set differs")

    supply_chain = manifest["supplyChain"]
    exact_keys(supply_chain, {"cargoCycloneDx", "pnpmCycloneDx", "licenseInventory"}, "supplyChain")
    for name, path in {
        "cargoCycloneDx": "sbom/cargo.cdx.json",
        "pnpmCycloneDx": "sbom/pnpm.cdx.json",
        "licenseInventory": "sbom/LICENSE-INVENTORY.json",
    }.items():
        validate_hash_record(root, supply_chain[name], path, name)
    for path in ("sbom/cargo.cdx.json", "sbom/pnpm.cdx.json"):
        sbom = json_file(root, path)
        if sbom.get("bomFormat") != "CycloneDX" or sbom.get("specVersion") != "1.6" or not isinstance(sbom.get("components"), list) or not sbom["components"]:
            fail(f"CycloneDX inventory is invalid: {path}")
    licenses = json_file(root, "sbom/LICENSE-INVENTORY.json")
    if licenses.get("schemaVersion") != 1 or not isinstance(licenses.get("components"), list) or not licenses["components"]:
        fail("license inventory is invalid")
    validate_hash_record(root, manifest["compatibilityMatrix"], "sbom/COMPATIBILITY-MATRIX.json", "compatibilityMatrix")
    matrix = json_file(root, "sbom/COMPATIBILITY-MATRIX.json")
    expected_matrix = {
        "schemaVersion": 1,
        "releaseVersion": manifest["releaseVersion"],
        "sourceCommit": manifest["sourceCommit"],
        "desktopCommit": manifest["desktopCommit"],
        "contracts": {"httpApiMajor": 1, "adminApiMajor": 2, "gatewayWireMajor": 5},
        "database": {"identity": "promptdock-relay-v4", "revision": 3},
        "compatibilityPolicy": "exact",
    }
    if matrix != expected_matrix:
        fail("compatibility matrix differs from release identity")

    asset_paths = {"productionConfig": "deploy/config.production.toml", "systemdUnit": "deploy/systemd/promptdock-relay.service", "caddyConfig": "deploy/caddy/Caddyfile.example", "deployCommon": "scripts/lib/deploy-common.sh", "bundleValidator": "scripts/lib/validate-release-bundle.py", "installScript": "scripts/install-ubuntu.sh", "upgradeScript": "scripts/upgrade-ubuntu.sh", "rollbackScript": "scripts/rollback-ubuntu.sh", "resetDataScript": "scripts/reset-relay-data.sh", "backupScript": "scripts/backup-ubuntu.sh", "restoreScript": "scripts/restore-ubuntu.sh"}
    assets = manifest["deploymentAssets"]
    exact_keys(assets, set(asset_paths), "deploymentAssets")
    for name, path in asset_paths.items():
        validate_hash_record(root, assets[name], path, name)

    with regular_file(root, "deploy/config.production.toml").open("rb") as stream:
        config = tomllib.load(stream)
    if config.get("server", {}).get("bind") != "127.0.0.1:8080" or config.get("server", {}).get("exposure") != "loopback" or config.get("admin", {}).get("bind") != "127.0.0.1:8081" or config.get("admin", {}).get("enabled") is not True:
        fail("production listener boundaries differ")
    if config.get("database", {}).get("path") != "/var/lib/promptdock-relay/relay.db" or config.get("wechat", {}).get("connection_file") != "/var/lib/promptdock-relay/wechat-connection.enc":
        fail("production state paths differ from the managed backup/reset boundary")
    unit = regular_file(root, "deploy/systemd/promptdock-relay.service").read_text(encoding="utf-8")
    validate_systemd_unit(unit)
    caddy = regular_file(root, "deploy/caddy/Caddyfile.example").read_text(encoding="utf-8")
    public_route = caddy.find("handle @relayPublic")
    operator_auth = caddy.find("basic_auth")
    admin_route = caddy.find("handle @adminApi")
    admin_spa = caddy.find("root * /opt/promptdock-relay/current/admin")
    if (
        "@relayPublic path /health/* /v1/* /v5/*" not in caddy
        or "reverse_proxy 127.0.0.1:8080" not in caddy
        or "@adminApi path /admin/api/v2/*" not in caddy
        or "root * /opt/promptdock-relay/current/admin" not in caddy
        or "reverse_proxy 127.0.0.1:8081" not in caddy
        or public_route < 0
        or operator_auth < 0
        or admin_route < 0
        or admin_spa < 0
        or not public_route < operator_auth < admin_route < admin_spa
    ):
        fail("Caddy unified release routing differs")
    validate_caddy_routes(root / "deploy/caddy/Caddyfile.example", config.get("admin", {}).get("allowed_origin", ""))
    if manifest["qualityGate"] != {"command": "corepack pnpm quality && corepack pnpm --filter @promptdock/relay-admin test:e2e:integration", "status": "passed"}:
        fail("quality gate attestation differs")


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: validate-release-bundle.py ABSOLUTE_BUNDLE", file=sys.stderr)
        return 64
    try:
        validate(Path(sys.argv[1]))
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
        print(f"promptdock-relay: error: invalid release bundle: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
