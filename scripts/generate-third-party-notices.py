#!/usr/bin/env python3
"""Write a deterministic inventory of licenses reachable from the CLI."""

import json
import subprocess
import sys
from pathlib import Path


def main() -> int:
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1"], text=True
        )
    )
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = [package_id for package_id, package in packages.items() if package["name"] == "eggtunnel-cli"]
    if len(roots) != 1:
        raise RuntimeError(f"expected one eggtunnel-cli package, found {len(roots)}")

    reachable: set[str] = set()
    pending = [roots[0]]
    while pending:
        package_id = pending.pop()
        if package_id in reachable:
            continue
        reachable.add(package_id)
        pending.extend(dependency["pkg"] for dependency in nodes[package_id]["deps"])

    rows = []
    for package_id in sorted(reachable, key=lambda value: (packages[value]["name"], packages[value]["version"])):
        package = packages[package_id]
        if package_id == roots[0]:
            continue
        license_id = package.get("license") or "License metadata unavailable"
        repository = package.get("repository") or package.get("homepage") or ""
        rows.append(f"| `{package['name']}` | `{package['version']}` | {license_id} | {repository} |")

    output = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("THIRD_PARTY_NOTICES.md")
    output.write_text(
        "# Third-party notices\n\n"
        "This release includes the following Rust packages as dependencies. "
        "Each package remains subject to its own license; this inventory does "
        "not replace included license texts or grant additional rights. The "
        "exact dependency graph is recorded in `Cargo.lock`.\n\n"
        "| Package | Version | SPDX license expression | Project |\n"
        "|---|---:|---|---|\n"
        + "\n".join(rows)
        + "\n",
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
