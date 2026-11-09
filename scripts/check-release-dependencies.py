#!/usr/bin/env python3
"""Fail if an excluded optional backend enters the server's active build graph."""
import re
import subprocess
import sys
from pathlib import Path

FORBIDDEN = {"libsqlite3-sys", "sqlx-sqlite"}
PACKAGE = re.compile(r"^([A-Za-z0-9_-]+) v\S+(?: .*)?$")


def check(tree):
    """Cargo tree follows active feature edges; metadata also lists inactive ones."""
    names = set()
    for line in tree.splitlines():
        match = PACKAGE.fullmatch(line)
        if not match:
            raise ValueError("unrecognized cargo tree output")
        names.add(match[1])
    if "oxsum-server" not in names:
        raise ValueError("missing oxsum-server root")
    found = sorted(names & FORBIDDEN)
    if found:
        raise ValueError("excluded SQLite backend reaches oxsum-server: " + ", ".join(found))


def main():
    root = Path(__file__).resolve().parents[1]
    # Include normal and build dependencies on every target, with the server's
    # declared features. --all-features would instead enable unrelated optional
    # workspace backends. Cargo metadata lists inactive optional edges too.
    result = subprocess.run(
        ["cargo", "tree", "--locked", "-p", "oxsum-server", "--target", "all",
         "--edges", "normal,build", "--prefix", "none", "--format", "{p}", "--color", "never"],
        cwd=root, check=True, capture_output=True, text=True,
    )
    try:
        check(result.stdout)
    except ValueError as error:
        print(f"release dependency guard: {error}", file=sys.stderr)
        return 1
    print("release dependency guard: no SQLite path in the server's active build graph")
    return 0


if __name__ == "__main__":
    sys.exit(main())
