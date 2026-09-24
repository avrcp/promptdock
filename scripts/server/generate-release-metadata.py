#!/usr/bin/env python3
"""Generate deterministic CycloneDX and compatibility metadata for a release."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path
from typing import Any
from urllib.parse import quote


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def license_entries(value: object) -> list[dict[str, object]]:
    if isinstance(value, list):
        entries: list[dict[str, object]] = []
        for item in value:
            entries.extend(license_entries(item))
        return entries or [{"license": {"name": "NOASSERTION"}}]
    if isinstance(value, dict):
        value = value.get("type") or value.get("name")
    if not isinstance(value, str) or not value.strip():
        return [{"license": {"name": "NOASSERTION"}}]
    value = value.strip()
    if re.fullmatch(r"[A-Za-z0-9.+-]+", value):
        return [{"license": {"id": value}}]
    if re.search(r"(?:^|\s)(?:AND|OR|WITH)(?:\s|$)|[()]", value):
        return [{"expression": value}]
    return [{"license": {"name": value}}]


def cargo_components(repository: Path) -> list[dict[str, Any]]:
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    metadata = json.loads(result.stdout)
    components: dict[tuple[str, str], dict[str, Any]] = {}
    for package in metadata["packages"]:
        name = package["name"]
        version = package["version"]
        component: dict[str, Any] = {
            "type": "library",
            "name": name,
            "version": version,
            "bom-ref": f"pkg:cargo/{name}@{version}",
            "purl": f"pkg:cargo/{name}@{version}",
            "licenses": license_entries(package.get("license")),
        }
        if package.get("repository"):
            component["externalReferences"] = [
                {"type": "vcs", "url": package["repository"]}
            ]
        components[(name, version)] = component
    return [components[key] for key in sorted(components)]


def npm_package_files(repository: Path) -> list[Path]:
    roots = [repository / "apps", repository / "packages"]
    files = [path for root in roots if root.exists() for path in root.glob("*/package.json")]
    store = repository / "node_modules" / ".pnpm"
    if store.exists():
        files.extend(store.glob("*/node_modules/*/package.json"))
        files.extend(store.glob("*/node_modules/@*/*/package.json"))
    return sorted(set(files))


def npm_components(repository: Path) -> list[dict[str, Any]]:
    components: dict[tuple[str, str], dict[str, Any]] = {}
    for path in npm_package_files(repository):
        try:
            package = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError):
            continue
        name = package.get("name")
        version = package.get("version")
        if not isinstance(name, str) or not name or not isinstance(version, str) or not version:
            continue
        if name.startswith("@") and "/" in name:
            scope, package_name = name.split("/", 1)
            qualified_name = f"{quote(scope, safe='')}/{quote(package_name, safe='')}"
        else:
            qualified_name = quote(name, safe="")
        purl = f"pkg:npm/{qualified_name}@{quote(version, safe='.-_~')}"
        component: dict[str, Any] = {
            "type": "library",
            "name": name,
            "version": version,
            "bom-ref": purl,
            "purl": purl,
            "licenses": license_entries(package.get("license") or package.get("licenses")),
        }
        repository_value = package.get("repository")
        repository_url = repository_value.get("url") if isinstance(repository_value, dict) else repository_value
        if isinstance(repository_url, str) and repository_url:
            component["externalReferences"] = [{"type": "vcs", "url": repository_url}]
        components[(name, version)] = component
    if not components:
        raise RuntimeError("pnpm dependency inventory is empty; run pnpm install first")
    return [components[key] for key in sorted(components)]


def cyclonedx(name: str, version: str, components: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {"component": {"type": "application", "name": name, "version": version}},
        "components": components,
    }


def license_inventory(*groups: tuple[str, list[dict[str, Any]]]) -> dict[str, Any]:
    entries = []
    for ecosystem, components in groups:
        for component in components:
            entries.append(
                {
                    "ecosystem": ecosystem,
                    "name": component["name"],
                    "version": component["version"],
                    "licenses": component["licenses"],
                }
            )
    return {"schemaVersion": 1, "components": sorted(entries, key=lambda item: (item["ecosystem"], item["name"], item["version"]))}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--release-version", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--desktop-commit", required=True)
    args = parser.parse_args()
    repository = args.repository.resolve(strict=True)
    output = args.output.resolve()
    cargo = cargo_components(repository)
    npm = npm_components(repository)
    write_json(output / "cargo.cdx.json", cyclonedx("promptdock-relay-rust", args.release_version, cargo))
    write_json(output / "pnpm.cdx.json", cyclonedx("promptdock-relay-web", args.release_version, npm))
    write_json(output / "LICENSE-INVENTORY.json", license_inventory(("cargo", cargo), ("npm", npm)))
    write_json(
        output / "COMPATIBILITY-MATRIX.json",
        {
            "schemaVersion": 1,
            "releaseVersion": args.release_version,
            "sourceCommit": args.source_commit,
            "desktopCommit": args.desktop_commit,
            "contracts": {"httpApiMajor": 1, "adminApiMajor": 2, "gatewayWireMajor": 5},
            "database": {"identity": "promptdock-relay-v4", "revision": 3},
            "compatibilityPolicy": "exact",
        },
    )


if __name__ == "__main__":
    main()
