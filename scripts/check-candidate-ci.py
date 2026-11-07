#!/usr/bin/env python3
"""Require executed full CI evidence for one immutable release candidate."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

REQUIRED_JOBS = {"fmt", "clippy", "test", "wasm", "frontend", "build-tests",
                 "test-extras", *(f"test-shard ({i}/3)" for i in range(1, 4))}
ENGINE_STEP = "Run doubleentry's Postgres conformance suite"
SHARED_STEP = "Run the process-shared suites"


def validate(run, jobs, sha):
    """A green path-filtered run is insufficient: every heavy job must execute."""
    if (run.get("head_sha") != sha or run.get("head_branch") != "main"
            or run.get("event") not in {"push", "schedule", "workflow_dispatch"}
            or run.get("status") != "completed" or run.get("conclusion") != "success"):
        return "not a successful completed main CI run on the candidate SHA"
    by_name = {job["name"]: job for job in jobs}
    for name in sorted(REQUIRED_JOBS):
        job = by_name.get(name, {})
        if job.get("status") != "completed" or job.get("conclusion") != "success":
            return f"required job did not pass: {name}"
    steps = {step["name"]: step for step in by_name["test-extras"].get("steps", [])}
    for name in (SHARED_STEP, ENGINE_STEP):
        if steps.get(name, {}).get("conclusion") != "success":
            return f"required step did not execute successfully: {name}"
    return None


def api(path):
    return json.loads(subprocess.check_output(["gh", "api", path], text=True))


def pages(path, field):
    page = 1
    while True:
        separator = "&" if "?" in path else "?"
        rows = api(f"{path}{separator}per_page=100&page={page}")[field]
        yield from rows
        if len(rows) < 100:
            return
        page += 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.sha):
        parser.error("sha must be a full lowercase commit SHA")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository):
        parser.error("repository must be owner/name")
    prefix = f"repos/{args.repository}/actions"
    failures = []
    for run in pages(f"{prefix}/workflows/ci.yml/runs?head_sha={args.sha}", "workflow_runs"):
        if run.get("status") != "completed" or run.get("conclusion") != "success":
            failures.append(f"run {run['id']}: {run.get('conclusion') or run.get('status')}")
            continue
        jobs = list(pages(f"{prefix}/runs/{run['id']}/attempts/{run['run_attempt']}/jobs", "jobs"))
        error = validate(run, jobs, args.sha)
        if error:
            failures.append(f"run {run['id']}: {error}")
            continue
        evidence = {"sha": args.sha, "run_id": run["id"], "run_url": run["html_url"],
                    "attempt": run["run_attempt"], "conclusion": "success",
                    "jobs": {j["name"]: j["conclusion"] for j in jobs
                             if j["name"] in REQUIRED_JOBS},
                    "postgres_conformance": "success", "process_shared": "success"}
        args.output.write_text(json.dumps(evidence, indent=2) + "\n")
        print(f"Full candidate CI passed: {run['html_url']}")
        return 0
    print("Candidate refused: no executed full green CI on " + args.sha, file=sys.stderr)
    for failure in failures:
        print(f"  {failure}", file=sys.stderr)
    print("Run ci.yml on this main commit, including all shards and engine conformance, then retry.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
