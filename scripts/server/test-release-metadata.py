#!/usr/bin/env python3
"""Exercise release metadata generation twice and reject nondeterminism."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> None:
    repository = Path(__file__).resolve().parents[2]
    generator = repository / "scripts" / "server" / "generate-release-metadata.py"
    commit = "1" * 40
    desktop = "2" * 40
    with tempfile.TemporaryDirectory(prefix="promptdock-release-metadata-") as temporary:
        first = Path(temporary) / "first"
        second = Path(temporary) / "second"
        for output in (first, second):
            subprocess.run(
                [
                    sys.executable,
                    str(generator),
                    "--repository",
                    str(repository),
                    "--output",
                    str(output),
                    "--release-version",
                    "0.6.0-rc.1",
                    "--source-commit",
                    commit,
                    "--desktop-commit",
                    desktop,
                ],
                check=True,
            )
        first_files = sorted(path.relative_to(first) for path in first.rglob("*") if path.is_file())
        second_files = sorted(path.relative_to(second) for path in second.rglob("*") if path.is_file())
        if first_files != second_files or any(
            (first / relative).read_bytes() != (second / relative).read_bytes()
            for relative in first_files
        ):
            raise SystemExit("release metadata generation is not deterministic")
        for name in ("cargo.cdx.json", "pnpm.cdx.json"):
            value = json.loads((first / name).read_text(encoding="utf-8"))
            if value.get("bomFormat") != "CycloneDX" or value.get("specVersion") != "1.6" or not value.get("components"):
                raise SystemExit(f"invalid CycloneDX document: {name}")
        pnpm = json.loads((first / "pnpm.cdx.json").read_text(encoding="utf-8"))
        scoped_purls = [component["purl"] for component in pnpm["components"] if component["name"].startswith("@")]
        if not scoped_purls or any("%2F" in purl.upper() for purl in scoped_purls):
            raise SystemExit("scoped npm package purls are not canonical")
        cargo = json.loads((first / "cargo.cdx.json").read_text(encoding="utf-8"))
        if not any("expression" in entry for component in cargo["components"] for entry in component["licenses"]):
            raise SystemExit("SPDX license expressions were flattened")
        matrix = json.loads((first / "COMPATIBILITY-MATRIX.json").read_text(encoding="utf-8"))
        if matrix.get("compatibilityPolicy") != "exact" or matrix.get("desktopCommit") != desktop:
            raise SystemExit("compatibility matrix is not exact")
    print("Deterministic CycloneDX, license inventory, and compatibility metadata passed.")


if __name__ == "__main__":
    main()
