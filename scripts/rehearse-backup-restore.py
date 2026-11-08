#!/usr/bin/env python3
"""Exercise explicit backup/restore using two disposable PostgreSQL containers.

Requires Docker and an already available pinned postgres image. Containers and
anonymous volumes are task-owned and removed in finally; synthetic artifacts
and measured evidence remain in the explicitly new output directory.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

import backup_restore as backup

IMAGE = "postgres@sha256:266d3c257594e195b309ae9d1805e54c788f1d7b375f56bf6c367aac6c917e7b"


def docker(*arguments, env=None, source=None, output=None):
    result = subprocess.run(["docker", *arguments], env=env, input=source,
                            stdout=output or subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    backup.require(result.returncode == 0, "isolated rehearsal Docker command failed")
    return result.stdout.decode() if output is None else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(mode=0o700, exist_ok=False)
    slug = uuid.uuid4().hex[:12]
    names = ["oxsum-backup-rehearsal-" + slug + suffix for suffix in ["-source", "-target"]]
    password = uuid.uuid4().hex
    target_token = uuid.uuid4().hex
    created = []
    try:
        for index, name in enumerate(names):
            docker("run", "--detach", "--name", name, "--label", "org.oxsum.task=backup-restore-rehearsal", "--env", "POSTGRES_PASSWORD", "--env", "POSTGRES_DB=fixture", IMAGE,
                   env={**os.environ, "POSTGRES_PASSWORD": password if index == 0 else target_token})
            created.append(name)
            for _ in range(60):
                ready = subprocess.run(["docker", "exec", name, "pg_isready", "-U", "postgres", "-d", "fixture"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if ready.returncode == 0:
                    break
                time.sleep(0.5)
            else:
                raise ValueError("isolated PostgreSQL did not become ready")
        source = backup.Connection(container=names[0], database="fixture", environ=os.environ)
        target = backup.Connection(container=names[1], database="fixture", environ=os.environ)
        source.sql("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations(version text); INSERT INTO oxsum._migrations VALUES ('0110_fixture'); "
                   "CREATE TABLE oxsum.organizations(id int,private_key text); INSERT INTO oxsum.organizations VALUES (1,'synthetic-sealed-credential'),(2,'synthetic-signing-key'); "
                   "CREATE TABLE oxsum.statements(id int); INSERT INTO oxsum.statements VALUES(1); CREATE TABLE oxsum.deposits(id int); "
                   "CREATE TABLE oxsum.usage_records(id int); INSERT INTO oxsum.usage_records SELECT generate_series(1,1000); "
                   "CREATE SCHEMA ledger_fixture; CREATE TABLE ledger_fixture.entries(id int); INSERT INTO ledger_fixture.entries VALUES(1),(2)")
        # A transaction after snapshot export must not enter the dump even when
        # it commits before pg_dump finishes. Observe real metadata in that same
        # snapshot, not the changed source after the dump.
        original_run = source.run
        def concurrent_dump(program, arguments, **kwargs):
            if program == "pg_dump":
                source.sql("INSERT INTO oxsum.usage_records VALUES(1001)")
            return original_run(program, arguments, **kwargs)
        source.run = concurrent_dump
        manifest_path = backup.backup(source, args.output / "source")
        source.run = original_run
        manifest = json.loads(manifest_path.read_text())
        backup.require(manifest["counts"]["usageRecords"] == 1000 and source.sql("SELECT count(*) FROM oxsum.usage_records") == "1001", "snapshot boundary was not exercised")
        # Moving both files to a new directory/cwd must remain portable.
        moved = args.output / "transported"
        moved.mkdir(mode=0o700)
        for path in manifest_path.parent.iterdir():
            os.rename(path, moved/path.name)
        manifest_path = moved/manifest_path.name
        receipt = backup.restore(target, manifest_path, "restored")
        private_query = "SELECT md5(string_agg(private_key,',' ORDER BY id)) FROM oxsum.organizations"
        backup.require(source.sql(private_query) == target.sql(private_query, database="restored"), "restored private fixture differs")
        backup.require(target.sql("SELECT count(*) FROM ledger_fixture.entries", database="restored") == "2", "ledger fixture differs")
        try:
            backup.restore(target, manifest_path, "restored")
        except ValueError as error:
            backup.require("existing database" in str(error), "existing target failed for the wrong reason")
        else:
            raise ValueError("existing target accepted")
        # Exercise local libpq mode with real psql/pg_dump/pg_restore inside the
        # source container, targeting its TCP URL instead of container wrappers.
        docker("exec", "-u", "0", names[0], "apk", "add", "--no-cache", "python3")
        for filename in ["backup_restore.py"]:
            docker("cp", str(Path(__file__).with_name(filename)), names[0]+":/tmp/"+filename)
        env = {**os.environ, "DATABASE_URL": "postgresql://postgres:"+password+"@127.0.0.1:5432/fixture"}
        docker("exec", "--env", "DATABASE_URL", names[0], "python3", "/tmp/backup_restore.py", "backup", "/tmp/local-backup", env=env)
        local_manifest = docker("exec", names[0], "find", "/tmp/local-backup", "-name", "*.manifest.json").strip()
        backup.require(local_manifest.startswith("/tmp/local-backup/oxsum-backup-"), "unexpected local-mode manifest")
        docker("exec", "--env", "DATABASE_URL", names[0], "python3", "/tmp/backup_restore.py", "restore", local_manifest, "local_restored", env=env)
        backup.require(source.sql("SELECT count(*) FROM oxsum.usage_records", database="local_restored") == "1001", "local URL mode restored another database")
        evidence = {"formatVersion": 1, "scope": "synthetic-two-container-postgresql-rehearsal", "image": IMAGE,
                    "containerMode": "passed", "localUrlMode": "passed", "snapshotIsolation": "passed", "portablePair": "passed",
                    "privateFixturePreserved": True, "existingTargetRefused": True, "manifest": manifest_path.name,
                    "restoreReceipt": receipt.name, "candidateAcceptance": False}
        backup.private_json(args.output/"rehearsal.json", evidence)
        print("Isolated PostgreSQL backup/restore rehearsal passed: " + str(args.output/"rehearsal.json"))
    finally:
        for name in created:
            subprocess.run(["docker", "rm", "--force", "--volumes", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError):
        raise SystemExit("Isolated backup/restore rehearsal failed; inspect task artifacts privately")
