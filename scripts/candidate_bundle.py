#!/usr/bin/env python3
"""Create and validate version-2 candidate bundles; never publish or deploy."""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import gzip
import importlib.util
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

SPEC = importlib.util.spec_from_file_location("candidate_ci", Path(__file__).with_name("check-candidate-ci.py"))
CI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CI)
GATES = {"fmt", "clippy", "libDocTests", "wasm", "frontend", "consumer", "examples", "image", "smoke"}
CANDIDATE_JOBS = {"resolve the promoted SHA", "fmt", "clippy", "test (lib + doc)",
                  "wasm (browser verifier)", "frontend", "verify consumer (SDK tarball)",
                  "developer examples (fresh install)", "candidate image (no push)",
                  "candidate manifest + release notes draft"}
HEX = re.compile(r"[0-9a-f]{64}")
SHA = re.compile(r"[0-9a-f]{40}")
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path, algorithm="sha256"):
    value = hashlib.new(algorithm)
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def safe_name(name):
    require(isinstance(name, str) and bool(name) and "\\" not in name and "\n" not in name
            and "\r" not in name and "\x00" not in name, "invalid artifact path")
    path = PurePosixPath(name)
    require(not path.is_absolute() and ".." not in path.parts and str(path) == name and name != ".",
            "unsafe artifact path: " + name)
    return path


def local_file(root, name):
    path = root.joinpath(*safe_name(name).parts)
    require(not any(p.is_symlink() for p in [path, *path.parents] if p.is_relative_to(root)),
            "symlinked artifact: " + name)
    require(path.is_file() and path.resolve().is_relative_to(root.resolve()), "missing artifact: " + name)
    return path


def file_map(root):
    files = {}
    for path in sorted(root.rglob("*")):
        require(not path.is_symlink(), "symlinked bundle path")
        name = path.relative_to(root).as_posix()
        if path.is_file() and name not in {"candidate-manifest.json", "SHA256SUMS"}:
            safe_name(name)
            files[name] = {"sha256": digest(path), "size": path.stat().st_size}
    return files


def archive_members(archive):
    members = {}
    for member in archive.getmembers():
        # Tar directories often have a trailing slash; files must be canonical.
        name = member.name.rstrip("/") if member.isdir() else member.name
        safe_name(name)
        require(member.isfile() or member.isdir(), "archive contains a link or special file")
        require(name not in members, "duplicate archive member")
        members[name] = member
    return members


def archive_json(archive, members, name):
    require(name in members and members[name].isfile() and members[name].size <= 2 * 1024 * 1024,
            "missing or oversized archive metadata: " + name)
    return archive.extractfile(members[name]).read()


def image_identity(path, sha):
    with tarfile.open(path, "r:*") as archive:
        members = archive_members(archive)
        manifest = json.loads(archive_json(archive, members, "manifest.json"))
        require(isinstance(manifest, list) and len(manifest) == 1, "image archive must contain exactly one image")
        config_path = manifest[0]["Config"]
        safe_name(config_path)
        config_bytes = archive_json(archive, members, config_path)
        config = json.loads(config_bytes)
        config_id = "sha256:" + hashlib.sha256(config_bytes).hexdigest()
        platform = config.get("os", "") + "/" + config.get("architecture", "")
        require(platform == "linux/amd64", "candidate platform must be linux/amd64")
        revision = (config.get("config", {}).get("Labels") or {}).get("org.opencontainers.image.revision")
        require(revision == sha, "image source revision differs from candidate")
        layers = manifest[0].get("Layers", [])
        require(bool(layers) and len(layers) == len(config.get("rootfs", {}).get("diff_ids", [])),
                "image layer count differs from rootfs")
        for name, expected_diff in zip(layers, config["rootfs"]["diff_ids"]):
            safe_name(name)
            require(name in members and members[name].isfile() and members[name].size > 0, "missing image layer")
            stream = archive.extractfile(members[name])
            compressed = stream.read(2) == b"\x1f\x8b"
            stream.seek(0)
            if compressed:
                stream = gzip.GzipFile(fileobj=stream)
            hashed = hashlib.sha256()
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                hashed.update(block)
            require("sha256:" + hashed.hexdigest() == expected_diff, "image layer differs from rootfs digest")
        return {"configId": config_id, "platform": platform, "sourceSha": revision}


def sdk_identity(path):
    with tarfile.open(path, "r:gz") as archive:
        members = archive_members(archive)
        package = json.loads(archive_json(archive, members, "package/package.json"))
        require(package.get("name") == "verify-oxsum-bill", "wrong SDK package")
        require(re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", package.get("version", "")), "invalid SDK version")
        for name in ["package/dist/index.js", "package/dist/index.d.ts", "package/wasm/verify_wasm.js"]:
            require(name in members and members[name].isfile() and members[name].size > 0, "SDK is incomplete")
        wasm = members.get("package/wasm/verify_wasm_bg.wasm")
        require(wasm is not None and wasm.isfile() and wasm.size > 1000, "SDK WASM is missing or a placeholder")
        require(archive.extractfile(wasm).read(4) == b"\x00asm", "SDK WASM has wrong magic")
    integrity = "sha512-" + base64.b64encode(bytes.fromhex(digest(path, "sha512"))).decode()
    return {"name": package["name"], "version": package["version"], "integrity": integrity}


def validate_ci_record(ci, sha, repository):
    require(ci.get("sha") == sha and ci.get("conclusion") == "success", "CI evidence has wrong SHA or verdict")
    require(str(ci.get("run_id", "")).isdigit() and isinstance(ci.get("attempt"), int) and ci["attempt"] > 0,
            "CI run/attempt missing")
    require(ci.get("run_url") == f"https://github.com/{repository}/actions/runs/{ci['run_id']}", "CI URL differs")
    require(all(ci.get("jobs", {}).get(job) == "success" for job in CI.REQUIRED_JOBS), "full CI jobs incomplete")
    require(ci.get("postgres_conformance") == ci.get("process_shared") == "success", "engine/shared evidence incomplete")
    require(ci.get("executed_steps") == {name: sorted(steps) for name, steps in CI.REQUIRED_STEPS.items()},
            "executed full-CI steps incomplete")


def runtime_identity(path, sha):
    """Validate the actual version response retained by candidate image smoke."""
    value = json.loads(path.read_text())
    require(set(value) == {"data"} and isinstance(value["data"], dict), "invalid runtime version response")
    value = value["data"]
    require(set(value) == {"gitSha", "version", "schemaVersion"} and value["gitSha"] == sha,
            "runtime source revision differs from candidate")
    require(isinstance(value["version"], str) and re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", value["version"]),
            "invalid runtime package version")
    require(isinstance(value["schemaVersion"], str) and re.fullmatch(r"[0-9]{4}_[a-z0-9_]+", value["schemaVersion"]),
            "invalid runtime schema version")
    return value


def write_sums(root):
    paths = sorted(p for p in root.rglob("*") if p.is_file() and p != root / "SHA256SUMS")
    (root / "SHA256SUMS").write_text("".join(f"{digest(p)}  {p.relative_to(root).as_posix()}\n" for p in paths))


def validate(root, expected_sha):
    root = root.resolve()
    require(SHA.fullmatch(expected_sha), "expected SHA must be a full lowercase commit")
    manifest = json.loads(local_file(root, "candidate-manifest.json").read_text())
    require(manifest.get("formatVersion") == 2 and manifest.get("sourceSha") == expected_sha, "wrong candidate format/SHA")
    repository = manifest.get("repository", "")
    require(REPOSITORY.fullmatch(repository), "invalid repository")
    workflow = manifest.get("workflow", {})
    require(isinstance(workflow.get("runId"), str) and workflow["runId"].isdigit()
            and isinstance(workflow.get("attempt"), int) and workflow["attempt"] > 0, "invalid workflow run/attempt")
    require(SHA.fullmatch(workflow.get("workflowSha", "")), "workflow SHA missing")
    require(workflow.get("url") == f"https://github.com/{repository}/actions/runs/{workflow['runId']}", "workflow URL differs")
    gated_at = datetime.fromisoformat(manifest["gatedAt"].replace("Z", "+00:00"))
    require(gated_at.utcoffset() is not None and gated_at.utcoffset().total_seconds() == 0,
            "gatedAt must be UTC")
    require(manifest.get("gates") == dict.fromkeys(sorted(GATES), "passed"), "candidate gates incomplete")
    actual_files = file_map(root)
    require(manifest.get("artifacts") == actual_files, "artifact file set, size or digest differs")
    expected_sums = {**{name: row["sha256"] for name, row in actual_files.items()},
                     "candidate-manifest.json": digest(root / "candidate-manifest.json")}
    observed = {}
    for line in local_file(root, "SHA256SUMS").read_text().splitlines():
        checksum, separator, name = line.partition("  ")
        require(separator and HEX.fullmatch(checksum) and name not in observed, "invalid checksum list")
        safe_name(name)
        observed[name] = checksum
    require(observed == expected_sums, "checksum list incomplete or changed")
    image = manifest["image"]
    identity = image_identity(local_file(root, image["archive"]), expected_sha)
    require(image == {"archive": image["archive"], **identity}, "image config/platform identity differs")
    sdk = manifest["sdk"]
    require(sdk == {"tarball": sdk["tarball"], **sdk_identity(local_file(root, sdk["tarball"]))}, "SDK identity differs")
    require(local_file(root, "RELEASE-NOTES-DRAFT.md").stat().st_size > 0, "release notes draft missing")
    evidence = manifest["evidence"]
    ci = json.loads(local_file(root, evidence["fullCi"]).read_text())
    validate_ci_record(ci, expected_sha, repository)
    for kind in ["smokeLog", "consumerLog"]:
        require(local_file(root, evidence[kind]).stat().st_size > 0, "missing executed evidence: " + kind)
    if "runtimeVersion" in evidence:
        runtime_identity(local_file(root, evidence["runtimeVersion"]), expected_sha)
    return manifest


def check_run(run, jobs, manifest, workflow_id):
    identity = manifest["workflow"]
    require(run.get("workflow_id") == workflow_id and run.get("path") == ".github/workflows/release-candidate.yml",
            "wrong candidate workflow")
    require(str(run.get("id")) == identity["runId"] and run.get("run_attempt") == identity["attempt"]
            and run.get("head_sha") == identity["workflowSha"], "candidate run identity differs")
    require(run.get("status") == "completed" and run.get("conclusion") == "success"
            and run.get("event") in {"workflow_dispatch", "workflow_call"}, "candidate workflow did not pass")
    by_name = {job["name"]: job for job in jobs}
    for name in CANDIDATE_JOBS:
        job = by_name.get(name, {})
        require(job.get("status") == "completed" and job.get("conclusion") == "success", "candidate job missing/failed: " + name)


def extract_zip(path, destination):
    with zipfile.ZipFile(path) as archive:
        seen = set()
        for info in archive.infolist():
            name = info.filename.rstrip("/") if info.is_dir() else info.filename
            safe_name(name)
            require(name not in seen and (info.external_attr >> 16) & 0o170000 != 0o120000, "unsafe/duplicate artifact zip member")
            seen.add(name)
            output = destination.joinpath(*PurePosixPath(name).parts)
            if info.is_dir():
                output.mkdir(parents=True, exist_ok=True)
            else:
                output.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(info) as source, output.open("xb") as target:
                    shutil.copyfileobj(source, target)


def validate_online(root, manifest):
    repository = manifest["repository"]
    prefix = f"repos/{repository}/actions"
    workflow = manifest["workflow"]
    workflow_id = CI.api(f"{prefix}/workflows/release-candidate.yml")["id"]
    run = CI.api(f"{prefix}/runs/{workflow['runId']}/attempts/{workflow['attempt']}")
    jobs = list(CI.pages(f"{prefix}/runs/{workflow['runId']}/attempts/{workflow['attempt']}/jobs", "jobs"))
    check_run(run, jobs, manifest, workflow_id)
    artifacts = list(CI.pages(f"{prefix}/runs/{workflow['runId']}/artifacts", "artifacts"))
    candidates = [a for a in artifacts if a["name"] == "candidate-bundle" and not a.get("expired")]
    require(len(candidates) == 1, "candidate bundle unavailable or expired")
    with tempfile.TemporaryDirectory(prefix="oxsum-candidate-provenance-") as work:
        work = Path(work)
        with (work / "bundle.zip").open("wb") as output:
            subprocess.run(["gh", "api", f"{prefix}/artifacts/{candidates[0]['id']}/zip"], stdout=output, check=True)
        trusted = work / "trusted"
        extract_zip(work / "bundle.zip", trusted)
        validated = validate(trusted, manifest["sourceSha"])
        require(validated == manifest and digest(trusted / "candidate-manifest.json") == digest(root / "candidate-manifest.json"),
                "local manifest differs from successful workflow artifact")
    ci = json.loads(local_file(root, manifest["evidence"]["fullCi"]).read_text())
    ci_run = CI.api(f"{prefix}/runs/{ci['run_id']}/attempts/{ci['attempt']}")
    require(ci_run.get("workflow_id") == CI.api(f"{prefix}/workflows/ci.yml")["id"], "wrong full-CI workflow")
    ci_jobs = list(CI.pages(f"{prefix}/runs/{ci['run_id']}/attempts/{ci['attempt']}/jobs", "jobs"))
    error = CI.validate(ci_run, ci_jobs, manifest["sourceSha"])
    require(error is None, error or "invalid full CI")
    for revision in {manifest["sourceSha"], workflow["workflowSha"]}:
        ancestry = CI.api(f"repos/{repository}/compare/{revision}...main")
        require(ancestry.get("status") in {"ahead", "identical"}, "source/workflow revision is not merged into main")


def create(args):
    root = args.directory.resolve()
    require(SHA.fullmatch(args.sha) and SHA.fullmatch(args.workflow_sha), "full source/workflow SHA required")
    image = image_identity(local_file(root, args.image), args.sha)
    sdk = sdk_identity(local_file(root, args.sdk))
    manifest = {"formatVersion": 2, "sourceSha": args.sha, "repository": args.repository,
        "workflow": {"runId": args.run, "attempt": args.attempt, "workflowSha": args.workflow_sha,
                     "url": f"https://github.com/{args.repository}/actions/runs/{args.run}"},
        "gatedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "gates": dict.fromkeys(sorted(GATES), "passed"),
        "image": {"archive": args.image, **image}, "sdk": {"tarball": args.sdk, **sdk},
        "evidence": {"fullCi": args.full_ci, "smokeLog": args.smoke_log, "consumerLog": args.consumer_log},
        "artifacts": file_map(root)}
    if getattr(args, "runtime_version", None):
        runtime_identity(local_file(root, args.runtime_version), args.sha)
        manifest["evidence"]["runtimeVersion"] = args.runtime_version
    (root / "candidate-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    write_sums(root)
    validate(root, args.sha)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    make = sub.add_parser("create")
    for name in ["sha", "repository", "workflow-sha", "run", "image", "sdk", "full-ci", "smoke-log", "consumer-log"]:
        make.add_argument("--" + name, required=True)
    make.add_argument("--attempt", type=int, required=True)
    make.add_argument("--runtime-version", help="observed version JSON required for digest deployment")
    make.add_argument("directory", type=Path)
    check = sub.add_parser("validate")
    check.add_argument("directory", type=Path)
    check.add_argument("--sha", required=True)
    check.add_argument("--online", action="store_true")
    args = parser.parse_args()
    if args.action == "create":
        create(args)
    else:
        manifest = validate(args.directory, args.sha)
        if args.online:
            validate_online(args.directory.resolve(), manifest)
    print("Candidate " + ("online provenance and bytes" if getattr(args, "online", False) else "offline bytes") + " validated")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError, tarfile.TarError, zipfile.BadZipFile) as error:
        raise SystemExit("Candidate refused: " + str(error))
