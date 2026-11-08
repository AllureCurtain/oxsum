#!/usr/bin/env python3
"""Install pinned Linux CI tools from verified official GitHub asset bytes."""

import argparse
import hashlib
import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import subprocess
import tarfile
import tempfile


TOOLS = {
    "mold": {
        "version": "2.40.0",
        "asset": "https://api.github.com/repos/rui314/mold/releases/assets/258128381",
        "size": 5083010,
        "sha256": "143e9d9624a2a7ffa25565a8bd3818479029c83b4f5913ed11dd2acaace3fab3",
        "root": "mold-2.40.0-x86_64-linux",
        "binary": "bin/mold",
    },
    "sccache": {
        "version": "0.19.0",
        "asset": "https://api.github.com/repos/mozilla/sccache/releases/assets/671951070",
        "size": 9879308,
        "sha256": "aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd",
        "root": "sccache-v0.19.0-x86_64-unknown-linux-musl",
        "binary": "sccache",
    },
}


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def download(spec, destination):
    """Four bounded attempts, with no extraction or final path on failure."""
    pending = destination.with_suffix(".part")
    try:
        subprocess.run(
            [
                "curl", "--fail", "--silent", "--show-error", "--location",
                "--proto", "=https", "--proto-redir", "=https",
                "--retry", "3", "--retry-all-errors", "--retry-delay", "1",
                "--connect-timeout", "10", "--max-time", "60",
                "--max-filesize", str(spec["size"]),
                "--header", "Accept: application/octet-stream",
                "--header", "X-GitHub-Api-Version: 2022-11-28",
                "--output", str(pending), spec["asset"],
            ],
            check=True,
            timeout=250,
        )
        if pending.stat().st_size != spec["size"]:
            raise ValueError("official asset size mismatch")
        if digest(pending) != spec["sha256"]:
            raise ValueError("official asset SHA-256 mismatch")
        os.replace(pending, destination)
    finally:
        pending.unlink(missing_ok=True)


def extract(archive, destination, spec):
    """Only verified bytes reach here; still reject unsafe archive members."""
    destination = destination.resolve()
    with tarfile.open(archive, "r:gz") as bundle:
        files = []
        links = []
        names = set()
        for member in bundle.getmembers():
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or path.parts[0] != spec["root"]:
                raise ValueError("archive member escapes the pinned root")
            relative = Path(*path.parts[1:])
            if str(relative) in names:
                raise ValueError("duplicate archive member")
            names.add(str(relative))
            target = destination / relative
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                files.append((member, target))
            elif member.issym():
                resolved = (target.parent / member.linkname).resolve()
                if Path(member.linkname).is_absolute() or not resolved.is_relative_to(destination):
                    raise ValueError("archive symlink escapes the pinned root")
                links.append((member, target))
            else:
                raise ValueError("unsupported archive member type")
        # Files precede links, so no archive-created symlink can redirect a write.
        for member, target in files:
            target.parent.mkdir(parents=True, exist_ok=True)
            with bundle.extractfile(member) as source, target.open("xb") as output:
                shutil.copyfileobj(source, output)
            target.chmod(0o755 if member.mode & 0o111 else 0o644)
        for member, target in links:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.symlink_to(member.linkname)
    binary = destination / spec["binary"]
    if not binary.is_file() or binary.is_symlink() or not os.access(binary, os.X_OK):
        raise ValueError("verified archive has no expected executable")


def tree_facts(root):
    facts = {}
    for path in sorted(root.rglob("*")):
        relative = str(path.relative_to(root))
        if path.is_symlink():
            facts[relative] = ("link", os.readlink(path))
        elif path.is_file():
            facts[relative] = ("file", digest(path), path.stat().st_mode & 0o777)
        elif path.is_dir():
            facts[relative] = ("dir",)
        else:
            raise ValueError("unsupported installed member type")
    return facts


def install(tool, spec, cache):
    """Commit a whole verified directory before its Actions tool-cache marker."""
    version_dir = cache / tool / spec["version"]
    version_dir.mkdir(parents=True, exist_ok=True)
    destination = version_dir / "x64"
    with tempfile.TemporaryDirectory(prefix=".download-", dir=version_dir) as temporary:
        work = Path(temporary)
        archive = work / "archive.tar.gz"
        download(spec, archive)
        staged = work / "tree"
        staged.mkdir()
        extract(archive, staged, spec)
        if destination.exists() or destination.is_symlink():
            if destination.is_symlink() or tree_facts(destination) != tree_facts(staged):
                raise ValueError("existing tool cache differs from verified release; refusing replacement")
        else:
            os.replace(staged, destination)
        marker = work / "complete"
        marker.touch()
        os.replace(marker, version_dir / "x64.complete")
    return destination / spec["binary"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tool", choices=TOOLS)
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "AMD64"):
        parser.error("pinned CI tools support only x86_64 Linux")
    cache = Path(os.environ["RUNNER_TOOL_CACHE"]).resolve()
    binary = install(args.tool, TOOLS[args.tool], cache)
    result = subprocess.run([str(binary), "--version"], check=True, capture_output=True, text=True)
    if TOOLS[args.tool]["version"] not in result.stdout:
        raise ValueError("installed tool reported an unexpected version")
    if args.tool == "mold":
        with Path(os.environ["GITHUB_PATH"]).open("a") as path:
            path.write(str(binary.parent) + "\n")
    # The pinned Mozilla action finds the verified sccache cache directory and
    # retains its runtime-token export, cache-v2 setup and post-run statistics.
    print(result.stdout.strip())
    print(f"Verified {args.tool} archive SHA-256 {TOOLS[args.tool]['sha256']}")


if __name__ == "__main__":
    main()
