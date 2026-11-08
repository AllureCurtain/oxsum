#!/usr/bin/env python3
"""Apply a trusted candidate plan on one host; no build, publication or down migration."""
import argparse
import copy
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request

import candidate_bundle as gate
import deployment_plan

MANAGED = "org.oxsum.deployment.managed"


def docker(*arguments, capture=False):
    """Arguments never pass through a shell; container logs may contain secrets."""
    result = subprocess.run(["docker", *arguments], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    gate.require(result.returncode == 0, "docker " + " ".join(arguments[:2]) + " failed; inspect the host privately")
    return result.stdout if capture else None


def json_get(url):
    opener = urllib.request.build_opener(deployment_plan.promotion.NoRedirect())
    with opener.open(url, timeout=3) as response:
        raw = response.read(32769)
        gate.require(len(raw) <= 32768, "oversized runtime response")
        return raw.decode()


def runtime(port, expected, wait=False):
    """Readiness means exact schema, and version must match all candidate fields."""
    deadline = time.monotonic() + (120 if wait else 0)
    while True:
        try:
            ready = json_get(f"http://127.0.0.1:{port}/readyz")
            gate.require(ready.strip() == "ready (" + expected["schemaVersion"] + ")", "running readiness schema differs")
            value = json.loads(json_get(f"http://127.0.0.1:{port}/api/v1/version"))
            gate.require(value == {"data": expected}, "running source/package/schema differs from candidate")
            return value["data"]
        except (urllib.error.URLError, ConnectionError, TimeoutError):
            if time.monotonic() >= deadline:
                raise ValueError("runtime did not become ready; no automatic rollback was attempted")
            time.sleep(2)


def inspect_container(name):
    identifiers = docker("container", "ls", "--all", "--filter", "name=^/" + name + "$", "--format", "{{.ID}}", capture=True).split()
    gate.require(len(identifiers) <= 1, "ambiguous deployment container")
    if not identifiers:
        return None
    value = json.loads(docker("container", "inspect", identifiers[0], capture=True))
    gate.require(isinstance(value, list) and len(value) == 1, "invalid deployment container inspection")
    return value[0]


def read_receipt(path):
    gate.require(path.is_file() and not path.is_symlink(), "managed deployment receipt is missing or symlinked")
    value = json.loads(path.read_text())
    gate.require(value.get("formatVersion") == 1 and value.get("result") == "success"
                 and value.get("evidenceTier") == "verified-candidate", "invalid retained deployment receipt")
    deployment_plan.validate_plan(value["plan"])
    return value


def atomic_json(path, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, prefix=".oxsum-state-", delete=False) as output:
        output.write(json.dumps(value, indent=2) + "\n")
        temporary = Path(output.name)
    os.replace(temporary, path)


def validate_current(container, current, name, port):
    if container is None:
        gate.require(current is None, "retained state exists without its container; explicit operator recovery is required")
        return
    gate.require(current is not None, "refusing to replace an unmanaged existing container")
    gate.require(current.get("containerName") == name and current.get("port") == port, "retained deployment target differs")
    plan = current["plan"]
    config = container.get("Config", {})
    gate.require(config.get("Image") == plan["image"]["reference"]
                 and config.get("Labels", {}).get(MANAGED) == "1" and container.get("State", {}).get("Running") is True,
                 "running container differs from retained deployment")
    runtime(port, plan["runtime"])


def choose_plan(args, current, previous):
    if args.operation == "deploy":
        plan = deployment_plan.validate_plan(json.loads(args.plan.read_text()))
        if current:
            gate.require(plan["runtime"]["schemaVersion"] >= current["plan"]["runtime"]["schemaVersion"],
                         "deployment would downgrade schema; restore a separate database for this artifact")
        return plan
    gate.require(args.confirm_compatible_rollback, "rollback requires an explicit compatibility decision")
    gate.require(current and previous, "rollback requires current and previous successful deployment receipts")
    plan = previous["plan"]
    gate.require(plan["image"]["reference"] == args.image_ref and plan["sourceSha"] == args.source_sha,
                 "rollback selectors differ from the retained previous artifact")
    gate.require(plan["runtime"]["schemaVersion"] == current["plan"]["runtime"]["schemaVersion"],
                 "rollback schema differs; restore a separate database instead of downgrading in place")
    return plan


def apply_locked(args, state):
    current = read_receipt(state / "active.json") if (state / "active.json").exists() else None
    previous = read_receipt(state / "previous.json") if (state / "previous.json").exists() else None
    container = inspect_container(args.container_name)
    validate_current(container, current, args.container_name, args.port)
    env_digest = gate.digest(args.env_file)
    if args.operation == "rollback" and current:
        gate.require(previous and current.get("operatorEnvSha256") == previous.get("operatorEnvSha256") == env_digest,
                     "operator environment changed; rollback requires the same database/key configuration")
    plan = choose_plan(args, current, previous)
    reference = plan["image"]["reference"]
    docker("image", "pull", "--platform", plan["image"]["platform"], reference)
    with tempfile.TemporaryDirectory(prefix="oxsum-deploy-image-") as name:
        archive = Path(name) / "image.tar"
        docker("image", "save", "--platform", plan["image"]["platform"], "--output", str(archive), reference)
        expected = {"sourceSha": plan["sourceSha"], "configId": plan["image"]["configId"], "platform": plan["image"]["platform"]}
        gate.require(gate.image_identity(archive, plan["sourceSha"]) == expected, "pulled image differs from deployment plan")
    options = ["--network", "host", "--env-file", str(args.env_file.resolve()),
               "--env", f"OXSUM_ADDR=127.0.0.1:{args.port}"]
    docker("run", "--rm", *options, reference, "--check-config")
    # All target identity/config checks precede downtime. A failure after this
    # point keeps the state for diagnosis; never guess that a down migration or
    # restarting the previous image is safe after a partially applied migration.
    if container:
        docker("container", "stop", "--time", "15", args.container_name)
    docker("run", "--rm", *options, reference, "--migrate-only")
    if container:
        docker("container", "rm", args.container_name)
    docker("run", "--detach", "--name", args.container_name, "--restart", "unless-stopped", *options,
           "--label", MANAGED + "=1", "--label", "org.oxsum.deployment.source=" + plan["sourceSha"], reference)
    observed = runtime(args.port, plan["runtime"], wait=True)
    landed = inspect_container(args.container_name)
    gate.require(landed and landed.get("Config", {}).get("Image") == reference
                 and landed.get("Config", {}).get("Labels", {}).get(MANAGED) == "1"
                 and landed.get("State", {}).get("Running") is True, "landed container is not the selected immutable artifact")
    receipt = {"formatVersion": 1, "evidenceTier": "verified-candidate", "result": "success",
               "operation": args.operation, "containerName": args.container_name, "port": args.port,
               "operatorEnvSha256": env_digest,
               "plan": plan, "runtime": observed, "previousImageReference": current["plan"]["image"]["reference"] if current else None,
               "completedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")}
    # Create the requested historical receipt before changing the active pointer.
    with args.receipt.open("x") as output:
        output.write(json.dumps(receipt, indent=2) + "\n")
    if current:
        atomic_json(state / "previous.json", current)
    atomic_json(state / "active.json", receipt)
    return receipt


def apply(args):
    gate.require(re.fullmatch(r"[a-z][a-z0-9-]{0,62}", args.container_name), "invalid container name")
    gate.require(1024 <= args.port <= 65535, "deployment port must be between 1024 and 65535")
    gate.require(args.env_file.is_file() and not args.env_file.is_symlink(), "explicit regular operator env file required")
    state = args.state_directory.resolve()
    gate.require(not args.state_directory.is_symlink(), "state directory cannot be symlinked")
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    gate.require(state.stat().st_mode & 0o077 == 0, "state directory must be private (mode 0700)")
    gate.require(args.receipt.parent.is_dir() and not args.receipt.exists() and not args.receipt.is_symlink(),
                 "receipt must be a new file in an existing directory")
    gate.require(args.receipt.resolve() not in {state / "active.json", state / "previous.json", state / "lock"},
                 "historical receipt cannot replace managed state")
    for name in ["active.json", "previous.json", "lock"]:
        gate.require(not (state / name).is_symlink(), "managed state cannot contain symlinks")
    with (state / "lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError("another deployment owns this state directory") from error
        # One private snapshot supplies both migration and serving; a concurrent
        # edit of the operator file cannot change database/key authority mid-run.
        with tempfile.TemporaryDirectory(prefix="oxsum-deploy-env-") as name:
            frozen = Path(name) / "operator.env"
            shutil.copyfile(args.env_file, frozen)
            frozen.chmod(0o600)
            selected = copy.copy(args)
            selected.env_file = frozen
            return apply_locked(selected, state)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="operation", required=True)
    for operation in ["deploy", "rollback"]:
        child = sub.add_parser(operation)
        child.add_argument("--env-file", type=Path, required=True)
        child.add_argument("--state-directory", type=Path, required=True)
        child.add_argument("--receipt", type=Path, required=True)
        child.add_argument("--container-name", default="oxsum")
        child.add_argument("--port", type=int, required=True)
        if operation == "deploy":
            child.add_argument("--plan", type=Path, required=True)
        else:
            child.add_argument("--image-ref", required=True)
            child.add_argument("--source-sha", required=True)
            child.add_argument("--confirm-compatible-rollback", action="store_true")
    receipt = apply(parser.parse_args())
    print("Verified runtime: " + receipt["plan"]["image"]["reference"])


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, tarfile.TarError) as error:
        raise SystemExit("Deployment refused or incomplete: " + str(error))
