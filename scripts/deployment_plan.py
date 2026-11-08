#!/usr/bin/env python3
"""Prepare a read-only, provenance-checked deployment plan for one published digest."""
import argparse
from datetime import datetime, timezone
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

import candidate_bundle as gate

SPEC = importlib.util.spec_from_file_location("promotion", Path(__file__).with_name("promote-candidate.py"))
promotion = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(promotion)
IMAGE_REF = re.compile(r"ghcr\.io/allurecurtain/oxsum@sha256:[0-9a-f]{64}")


def validate_plan(value):
    """Validate a trusted plan's shape; this is not a signature/provenance check."""
    fields = {"formatVersion", "evidenceTier", "sourceSha", "repository", "workflow", "candidateManifestSha256",
              "archiveSha256", "image", "runtime", "preparedAt"}
    gate.require(isinstance(value, dict) and set(value) == fields and value["formatVersion"] == 1,
                 "invalid deployment plan schema")
    gate.require(value["evidenceTier"] == "verified-candidate", "deployment requires verified-candidate evidence")
    gate.require(value["repository"] == "AllureCurtain/oxsum" and gate.SHA.fullmatch(value["sourceSha"]),
                 "invalid deployment source")
    for field in ["candidateManifestSha256", "archiveSha256"]:
        gate.require(gate.HEX.fullmatch(value[field]), "invalid deployment checksum")
    image = value["image"]
    gate.require(set(image) == {"reference", "configId", "platform", "registryDigest", "platformManifestDigest"}
                 and IMAGE_REF.fullmatch(image["reference"]) and image["platform"] == "linux/amd64",
                 "deployment requires an explicit authorized linux/amd64 digest")
    for field in ["configId", "registryDigest", "platformManifestDigest"]:
        gate.require(promotion.DIGEST.fullmatch(image[field]), "invalid deployment image identity")
    gate.require(image["reference"].endswith("@" + image["registryDigest"]), "deployment reference/digest differs")
    runtime = value["runtime"]
    gate.require(set(runtime) == {"gitSha", "version", "schemaVersion"} and runtime["gitSha"] == value["sourceSha"],
                 "deployment runtime source differs")
    gate.require(re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", runtime["version"])
                 and re.fullmatch(r"[0-9]{4}_[a-z0-9_]+", runtime["schemaVersion"]), "invalid deployment runtime version")
    workflow = value["workflow"]
    gate.require(set(workflow) == {"runId", "attempt", "workflowSha", "url"}
                 and re.fullmatch(r"[1-9][0-9]*", workflow["runId"])
                 and isinstance(workflow["attempt"], int) and workflow["attempt"] > 0
                 and gate.SHA.fullmatch(workflow["workflowSha"])
                 and workflow["url"] == f"https://github.com/AllureCurtain/oxsum/actions/runs/{workflow['runId']}",
                 "invalid deployment workflow identity")
    stamp = datetime.fromisoformat(value["preparedAt"].replace("Z", "+00:00"))
    gate.require(stamp.utcoffset() is not None and stamp.utcoffset().total_seconds() == 0, "deployment timestamp must be UTC")
    return value


def prepare(args):
    gate.require(IMAGE_REF.fullmatch(args.image_ref), "image must be an explicit ghcr.io/allurecurtain/oxsum digest")
    gate.require(gate.SHA.fullmatch(args.sha) and re.fullmatch(r"[1-9][0-9]*", args.run) and args.attempt > 0,
                 "explicit source and candidate run/attempt required")
    root = args.directory.resolve()
    output = args.output.resolve()
    gate.require(not output.is_relative_to(root) and output.parent.is_dir()
                 and not output.exists() and not args.output.is_symlink(), "plan must be a new file outside the candidate")
    with tempfile.TemporaryDirectory(prefix="oxsum-deployment-plan-") as name:
        work = Path(name)
        frozen = work / "candidate"
        shutil.copytree(root, frozen, symlinks=True)
        manifest = gate.validate(frozen, args.sha)
        gate.require(manifest["repository"] == "AllureCurtain/oxsum" and manifest["workflow"]["runId"] == args.run
                     and manifest["workflow"]["attempt"] == args.attempt, "candidate repository/run/attempt differs")
        runtime_file = manifest["evidence"].get("runtimeVersion")
        gate.require(runtime_file, "candidate lacks observed runtime version; regenerate its candidate bundle")
        runtime = gate.runtime_identity(gate.local_file(frozen, runtime_file), args.sha)
        gate.validate_online(frozen, manifest)
        archive = gate.local_file(frozen, manifest["image"]["archive"])
        image = {key: manifest["image"][key] for key in ["configId", "platform", "sourceSha"]}
        registry = promotion.Registry("ghcr.io", "allurecurtain/oxsum", write=False)
        identity = registry.identity(args.image_ref.split("@", 1)[1], image, promotion.archive_manifest(archive))
        registry.verify_pulled(identity, work, image)
        plan = {"formatVersion": 1, "evidenceTier": "verified-candidate", "sourceSha": args.sha,
                "repository": manifest["repository"], "workflow": manifest["workflow"],
                "candidateManifestSha256": gate.digest(frozen / "candidate-manifest.json"),
                "archiveSha256": manifest["artifacts"][manifest["image"]["archive"]]["sha256"],
                "image": {"reference": args.image_ref, "configId": image["configId"], "platform": image["platform"], **identity},
                "runtime": runtime, "preparedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")}
        validate_plan(plan)
        with output.open("x") as target:
            target.write(json.dumps(plan, indent=2) + "\n")
        return plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    for field in ["sha", "run", "image-ref"]:
        parser.add_argument("--" + field, required=True)
    parser.add_argument("--attempt", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    plan = prepare(parser.parse_args())
    print("Deployment plan verified: " + plan["image"]["reference"])


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError, tarfile.TarError, zipfile.BadZipFile) as error:
        raise SystemExit("Deployment plan refused: " + str(error))
