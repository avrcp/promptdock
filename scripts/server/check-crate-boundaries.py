#!/usr/bin/env python3
"""Fail when the modular-monolith dependency direction regresses."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path


FORBIDDEN = {
    "relay-domain": {"axum", "sqlx", "tokio", "reqwest", "wechat-ilink", "relay-provider-wechat"},
    "relay-application": {"axum", "sqlx", "reqwest", "wechat-ilink", "relay-provider-wechat"},
    "relay-storage-sqlite": {"axum", "reqwest", "wechat-ilink", "relay-provider-wechat"},
    "relay-transport-gateway": {"sqlx", "reqwest", "wechat-ilink", "relay-provider-wechat"},
    "relay-provider-wechat": {"axum", "sqlx"},
    "relay-admin-api": {"axum", "sqlx", "reqwest", "wechat-ilink", "relay-provider-wechat"},
}
INTERNAL_PACKAGES = {
    "promptdock-server",
    "relay-domain",
    "relay-application",
    "relay-storage-sqlite",
    "relay-transport-http",
    "relay-transport-gateway",
    "relay-provider-wechat",
    "relay-admin-api",
    "wechat-ilink",
}
ALLOWED_INTERNAL = {
    "promptdock-server": INTERNAL_PACKAGES - {"promptdock-server", "wechat-ilink"},
    "relay-domain": set(),
    "relay-application": {"relay-domain"},
    "relay-storage-sqlite": set(),
    "relay-transport-http": set(),
    "relay-transport-gateway": set(),
    "relay-provider-wechat": {"wechat-ilink"},
    "relay-admin-api": set(),
    "wechat-ilink": set(),
}


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    packages = {package["name"]: package for package in json.loads(result.stdout)["packages"]}
    required = INTERNAL_PACKAGES
    missing = required - packages.keys()
    if missing:
        raise SystemExit(f"modular-monolith crates are missing: {sorted(missing)}")
    for package_name, forbidden in FORBIDDEN.items():
        dependencies = {dependency["name"] for dependency in packages[package_name]["dependencies"]}
        violations = dependencies & forbidden
        if violations:
            raise SystemExit(f"{package_name} has forbidden dependencies: {sorted(violations)}")
    for package_name, allowed in ALLOWED_INTERNAL.items():
        dependencies = {dependency["name"] for dependency in packages[package_name]["dependencies"]}
        unexpected = (dependencies & INTERNAL_PACKAGES) - allowed
        if unexpected:
            raise SystemExit(f"{package_name} violates internal dependency direction: {sorted(unexpected)}")
    for relative in ("crates/relay-domain/src", "crates/relay-application/src"):
        source = "\n".join(path.read_text(encoding="utf-8") for path in sorted((root / relative).rglob("*.rs")))
        forbidden_source = {marker for marker in ("std::fs", "std::net", "tokio::net", "reqwest::", "sqlx::", "axum::") if marker in source}
        if forbidden_source:
            raise SystemExit(f"{relative} contains forbidden I/O or framework references: {sorted(forbidden_source)}")
    print("Modular-monolith crate dependency boundaries passed.")


if __name__ == "__main__":
    main()
