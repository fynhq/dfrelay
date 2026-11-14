#!/usr/bin/env python3
"""Regenerate or verify the Rust dependency table in THIRD_PARTY_NOTICES.md.

The table comes from `cargo metadata --locked`, which resolves the same graph
as Cargo.lock (including dev, build, optional, and platform-specific packages).
The surrounding prose is hand-maintained and left untouched.

Usage:
  python3 .github/scripts/gen_third_party.py          # rewrite the table
  python3 .github/scripts/gen_third_party.py --check  # fail if it is stale
"""

from __future__ import annotations

import difflib
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NOTICES = ROOT / "THIRD_PARTY_NOTICES.md"
HEADER = "| Crate | Version | License |"
DIVIDER = "|---|---|---|"
ROOT_PACKAGE = "dfrelay"


def cargo_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return json.loads(result.stdout)


def package_rows(metadata: dict) -> list[str]:
    rows = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["name"] == ROOT_PACKAGE:
            continue
        license_expr = package.get("license")
        if not license_expr:
            raise SystemExit(
                f"package {package['name']} {package['version']} has no SPDX license field"
            )
        rows.append(f"| `{package['name']}` | `{package['version']}` | {license_expr} |")
    return rows


def render(document: str, rows: list[str]) -> str:
    lines = document.splitlines()
    try:
        header = lines.index(HEADER)
    except ValueError:
        raise SystemExit(f"{NOTICES} is missing the dependency table header")
    if header + 1 >= len(lines) or lines[header + 1] != DIVIDER:
        raise SystemExit(f"{NOTICES} has an unexpected table header")

    end = header + 2
    while end < len(lines) and lines[end].startswith("|"):
        end += 1

    replacement = [HEADER, DIVIDER, *rows]
    return "\n".join(lines[:header] + replacement + lines[end:]) + "\n"


def main() -> int:
    args = sys.argv[1:]
    unknown = [arg for arg in args if arg != "--check"]
    if unknown:
        print(f"unknown arguments: {' '.join(unknown)}", file=sys.stderr)
        return 2

    document = NOTICES.read_text(encoding="utf-8")
    rows = package_rows(cargo_metadata())
    updated = render(document, rows)

    if "--check" in args:
        if updated == document:
            print(f"{NOTICES.name} matches Cargo.lock")
            return 0
        diff = difflib.unified_diff(
            document.splitlines(),
            updated.splitlines(),
            fromfile="current",
            tofile="regenerated",
            lineterm="",
            n=2,
        )
        print("\n".join(list(diff)[:80]))
        print(
            "::error file=THIRD_PARTY_NOTICES.md::dependency table is out of date; "
            "run python3 .github/scripts/gen_third_party.py",
            file=sys.stderr,
        )
        return 1

    if updated == document:
        print(f"{NOTICES.name} already up to date")
        return 0
    NOTICES.write_text(updated, encoding="utf-8", newline="\n")
    print(f"updated {NOTICES} with {len(rows)} packages")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
