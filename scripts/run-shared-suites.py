#!/usr/bin/env python3
"""Run process-shared modules from archived aggregate integration binaries."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

SUITES = {"core": ("generative", "seed", "jobs"), "server": ("jobs",)}

# `cargo nextest archive` does not guarantee an aggregate test binary lands
# under `target/debug/deps/<name>-<hash>`. On a cache-warm or incremental
# build the binary-path in `binaries-metadata.json` can point into a
# build-script `out/` directory instead (e.g.
# `target/debug/build/oxsum-core/<hash>/out/core-<hash>`), leaving `deps/`
# without a `core-*`/`server-*` executable at all. Resolve each suite through
# the metadata first and fall back to the deps glob, so both layouts run.


def _extract_root(bins):
    """The archive's extraction root — the ancestor holding `target/nextest/`."""
    for ancestor in (bins, *bins.parents):
        if (ancestor / "target" / "nextest" / "binaries-metadata.json").is_file():
            return ancestor
    return None


def _metadata_binary(extract_root, binary):
    """The recorded path for `<crate>::<binary>` inside the extraction root."""
    metadata = extract_root / "target" / "nextest" / "binaries-metadata.json"
    try:
        document = json.loads(metadata.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    binaries = document.get("rust-binaries")
    if not isinstance(binaries, dict):
        return None
    for entry in binaries.values():
        if not isinstance(entry, dict) or entry.get("binary-id") is None:
            continue
        # `binary-id` is `<package>::<binary>` (e.g. `oxsum-core::core`); the
        # suite names its aggregate test binary, so match the trailing segment.
        if entry["binary-id"].rsplit("::", 1)[-1] != binary:
            continue
        recorded = entry.get("binary-path")
        if not isinstance(recorded, str):
            return None
        anchor = recorded.find("target/")
        if anchor < 0:
            return None
        candidate = extract_root / recorded[anchor:]
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
        return None
    return None


def _resolve(bins, binary):
    """The aggregate test executable for `binary`, whichever layout archived it."""
    root = _extract_root(bins)
    if root is not None:
        resolved = _metadata_binary(root, binary)
        if resolved is not None:
            return resolved.resolve()
    matches = sorted(p for p in bins.glob(f"{binary}-*")
                     if re.fullmatch(rf"{binary}-[0-9a-f]+", p.name)
                     and p.is_file() and os.access(p, os.X_OK))
    if len(matches) != 1:
        raise ValueError(f"expected one executable {binary} aggregate, found {len(matches)}")
    return matches[0].resolve()


def plan(bins):
    """Resolve every suite before execution; missing or ambiguous coverage fails."""
    selected = []
    for binary, modules in SUITES.items():
        exe = _resolve(bins, binary)
        for module in modules:
            prefix = f"{module}::"
            result = subprocess.run([str(exe), prefix, "--list", "--format", "terse"],
                                    check=True, capture_output=True, text=True, timeout=30)
            names = [line.removesuffix(": test") for line in result.stdout.splitlines()
                     if line.endswith(": test")]
            if not names or any(not name.startswith(prefix) for name in names):
                raise ValueError(f"empty or out-of-scope selection: {binary}/{module}")
            selected.append((exe, prefix, len(names)))
    return selected


def run(bins, check_only=False):
    selected = plan(bins)
    if not check_only and not os.environ.get("DATABASE_URL", "").strip():
        raise ValueError("DATABASE_URL must be set; refusing database tests that would skip")
    for exe, prefix, count in selected:
        print(f"{exe.name} {prefix}: {count} discovered tests", flush=True)
        if check_only:
            continue
        result = subprocess.run([str(exe), prefix, "--test-threads=4"],
                                capture_output=True, text=True, timeout=600)
        print(result.stdout, end="", flush=True)
        print(result.stderr, end="", file=sys.stderr, flush=True)
        result.check_returncode()
        summary = re.search(r"test result: ok\. (\d+) passed; 0 failed; (\d+) ignored;", result.stdout)
        if not summary or int(summary[1]) != count or int(summary[2]) != 0:
            raise ValueError(f"incomplete execution: {exe.name}/{prefix}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bins", type=Path)
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()
    try:
        run(args.bins, args.check_only)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"shared-suite execution failed: {error}\n")


if __name__ == "__main__":
    main()
