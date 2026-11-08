#!/usr/bin/env python3
"""Explicit-target private PostgreSQL backups and verified fresh-database restores."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import select
import stat
import subprocess
import tempfile
import time
import urllib.parse
import uuid

DATABASE = re.compile(r"[A-Za-z_][A-Za-z0-9_-]{0,62}")
SCHEMA = re.compile(r"[0-9]{4}_[a-z0-9_]{1,115}")
COUNTS = {"organizations", "ledgerSchemas", "statements", "deposits", "usageRecords"}
SETTINGS = {"databaseSizeBytes", "maxLocksPerTransaction", "maxPredLocksPerTransaction", "maxConnections", "sharedBuffersBytes"}
MANIFEST = {"formatVersion", "artifact", "createdUtc", "postgresVersionNum", "schemaVersion", "counts", "observations", "sizeBytes", "sha256", "dumpDurationMs"}
QUERY = """SELECT json_build_object(
 'postgresVersionNum', current_setting('server_version_num')::int,
 'schemaVersion', (SELECT max(version) FROM oxsum._migrations),
 'counts', json_build_object(
   'organizations',(SELECT count(*) FROM oxsum.organizations),
   'ledgerSchemas',(SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'ledger\\_%' ESCAPE '\\'),
   'statements',(SELECT count(*) FROM oxsum.statements),
   'deposits',(SELECT count(*) FROM oxsum.deposits),
   'usageRecords',(SELECT count(*) FROM oxsum.usage_records)),
 'observations', json_build_object(
   'databaseSizeBytes',pg_database_size(current_database()),
   'maxLocksPerTransaction',current_setting('max_locks_per_transaction')::int,
   'maxPredLocksPerTransaction',current_setting('max_pred_locks_per_transaction')::int,
   'maxConnections',current_setting('max_connections')::int,
   'sharedBuffersBytes',pg_size_bytes(current_setting('shared_buffers'))))"""


def require(condition, message):
    if not condition:
        raise ValueError(message)


def private_open(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    source = os.fdopen(descriptor, "rb")
    metadata = os.fstat(source.fileno())
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_mode & 0o077:
        source.close()
        raise ValueError("input must be a private regular file")
    return source


def private_text(path, maximum=32768):
    with private_open(path) as source:
        raw = source.read(maximum + 1)
    require(len(raw) <= maximum, "input is too large")
    return raw.decode()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON field")
        result[key] = value
    return result


def read_json(raw):
    return json.loads(raw, object_pairs_hook=unique_object)


def integer(value, minimum=0):
    return type(value) is int and minimum <= value <= 2**63 - 1


def metadata(value):
    require(isinstance(value, dict) and set(value) == {"postgresVersionNum", "schemaVersion", "counts", "observations"}, "invalid database metadata")
    require(integer(value["postgresVersionNum"], 100000), "invalid PostgreSQL version")
    require(isinstance(value["schemaVersion"], str) and SCHEMA.fullmatch(value["schemaVersion"]), "invalid schema version")
    for name, keys in [("counts", COUNTS), ("observations", SETTINGS)]:
        require(isinstance(value[name], dict) and set(value[name]) == keys and all(integer(v) for v in value[name].values()), "invalid database counts/settings")
    return value


def validate_manifest(value):
    require(isinstance(value, dict) and set(value) == MANIFEST and type(value["formatVersion"]) is int and value["formatVersion"] == 2,
            "a complete version-2 backup manifest is required")
    require(isinstance(value["artifact"], str) and re.fullmatch(r"oxsum-backup-[0-9]{8}T[0-9]{6}Z-[0-9a-f]{32}\.dump", value["artifact"]), "invalid portable artifact name")
    require(isinstance(value["createdUtc"], str) and re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z", value["createdUtc"]), "invalid backup timestamp")
    datetime.strptime(value["createdUtc"], "%Y-%m-%dT%H:%M:%SZ")
    require(integer(value["sizeBytes"], 1024) and integer(value["dumpDurationMs"]), "invalid backup size/duration")
    require(isinstance(value["sha256"], str) and re.fullmatch(r"[0-9a-f]{64}", value["sha256"]), "missing or invalid backup checksum")
    metadata({key: value[key] for key in ["postgresVersionNum", "schemaVersion", "counts", "observations"]})
    return value


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            checksum.update(chunk)
    return checksum.hexdigest()


def private_json(path, value):
    with tempfile.NamedTemporaryFile(mode="w", prefix=".oxsum-json-", dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(json.dumps(value, indent=2) + "\n")
            output.flush()
            os.fsync(output.fileno())
            os.link(temporary, path)
            sync_directory(path.parent)
        finally:
            temporary.unlink()


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def bounded_line(stream, timeout=35):
    """Bound the whole response, including a peer that sends only a partial line."""
    deadline = time.monotonic() + timeout
    raw = bytearray()
    while b"\n" not in raw:
        remaining = deadline - time.monotonic()
        require(remaining > 0, "database snapshot query timed out")
        ready, _, _ = select.select([stream], [], [], remaining)
        require(ready, "database snapshot query timed out")
        chunk = os.read(stream.fileno(), min(4096, 32769 - len(raw)))
        require(chunk, "database snapshot query failed")
        raw.extend(chunk)
        require(len(raw) <= 32768, "database snapshot response is too large")
    require(raw.endswith(b"\n") and raw.count(b"\n") == 1, "unexpected database snapshot response")
    return raw.decode().strip()


class Connection:
    """One explicit host/credential set; no ambient libpq, Docker discovery or .env."""

    def __init__(self, container=None, database=None, user=None, url_file=None, environ=None):
        supplied = dict(os.environ if environ is None else environ)
        self.container = container or supplied.get("CONTAINER") or None
        url = private_text(url_file).strip() if url_file else supplied.get("DATABASE_URL")
        require(bool(self.container) != bool(url), "choose exactly one explicit CONTAINER or DATABASE_URL/private URL file")
        self.env = {key: supplied[key] for key in ["PATH", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY"] if key in supplied}
        self.env.update({"LC_ALL": "C", "PGCONNECT_TIMEOUT": "10", "PGAPPNAME": "oxsum-backup-restore"})
        self.pg = {"PGPASSFILE": "/dev/null", "PGSERVICEFILE": "/dev/null", "PGOPTIONS": ""}
        if self.container:
            require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", self.container), "invalid explicit container")
            self.database = database or supplied.get("POSTGRES_DB")
            self.pg.update({"PGHOST": "/var/run/postgresql", "PGHOSTADDR": "", "PGPORT": "5432"})
            self.pg["PGUSER"] = user or supplied.get("PGUSER") or supplied.get("POSTGRES_USER") or "postgres"
            require(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]{0,62}", self.pg["PGUSER"]), "invalid database user")
            if supplied.get("PGPASSWORD"):
                self.pg["PGPASSWORD"] = supplied["PGPASSWORD"]
        else:
            require(database is None and user is None, "URL mode derives user and database from the explicit URL")
            parsed = urllib.parse.urlsplit(url)
            require(parsed.scheme in {"postgres", "postgresql"} and parsed.hostname and parsed.username and not parsed.fragment,
                    "invalid explicit PostgreSQL URL")
            require(parsed.port is None or 1 <= parsed.port <= 65535, "invalid PostgreSQL port")
            self.database = urllib.parse.unquote(parsed.path.removeprefix("/"))
            self.pg.update({"PGHOST": parsed.hostname, "PGHOSTADDR": "", "PGPORT": str(parsed.port or 5432), "PGUSER": urllib.parse.unquote(parsed.username)})
            if parsed.password is not None:
                self.pg["PGPASSWORD"] = urllib.parse.unquote(parsed.password)
            mapping = {"sslmode": "PGSSLMODE", "sslrootcert": "PGSSLROOTCERT", "sslcert": "PGSSLCERT", "sslkey": "PGSSLKEY"}
            options = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True)
            require(len({key for key, _ in options}) == len(options), "duplicate PostgreSQL URL option")
            for key, value in options:
                require(key in mapping and value and "\x00" not in value, "unsupported PostgreSQL URL option")
                self.pg[mapping[key]] = value
        require(isinstance(self.database, str) and DATABASE.fullmatch(self.database), "explicit database name is required")
        require(all("\x00" not in value for value in self.pg.values()), "invalid PostgreSQL credential field")
        self.pg["PGDATABASE"] = self.database

    def command(self, program, arguments, database=None):
        pg = {**self.pg, "PGDATABASE": database or self.database}
        env = {**self.env, **pg}
        if self.container:
            # docker receives variable names only; password bytes remain in the
            # subprocess environment and never appear in its argv/logs.
            command = ["docker", "exec", "-i"]
            for key in [*pg, "PGCONNECT_TIMEOUT", "PGAPPNAME", "LC_ALL"]:
                command += ["--env", key]
            command += [self.container, "env", "-u", "PGSERVICE", "-u", "PGSYSCONFDIR", program, *arguments]
        else:
            command = [program, *arguments]
        return command, env

    def run(self, program, arguments, *, database=None, source=None, output=None, text=None, timeout=3600):
        command, env = self.command(program, arguments, database)
        with tempfile.TemporaryFile() as error:
            result = subprocess.run(command, env=env, stdin=source, stdout=output or subprocess.PIPE,
                                    stderr=error, input=text.encode() if text is not None else None, timeout=timeout)
        require(result.returncode == 0, "PostgreSQL command failed; inspect the target privately")
        return result.stdout.decode() if output is None else None

    def sql(self, sql, database=None, variables=None):
        args = ["-X", "-qAt", "-w", "-v", "ON_ERROR_STOP=1"]
        for name, value in (variables or {}).items():
            args += ["--set", name + "=" + value]
        return self.run("psql", args, database=database, text=sql + ";\n", timeout=30).strip()


class Snapshot:
    """Keep psql's exported snapshot transaction alive until pg_dump completes."""

    def __init__(self, connection):
        self.connection = connection

    def __enter__(self):
        command, env = self.connection.command("psql", ["-X", "-qAt", "-w", "-v", "ON_ERROR_STOP=1"])
        self.errors = tempfile.TemporaryFile()
        self.process = subprocess.Popen(command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors, bufsize=0)
        try:
            self.query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout='30s'; SELECT pg_export_snapshot()")
            self.snapshot = self.answer
            require(re.fullmatch(r"[0-9A-Fa-f-]{1,100}", self.snapshot), "invalid exported snapshot")
            self.query(QUERY)
            self.metadata = metadata(read_json(self.answer))
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def query(self, sql):
        self.process.stdin.write((sql + ";\n").encode())
        self.process.stdin.flush()
        self.answer = bounded_line(self.process.stdout)
        require(self.answer and len(self.answer) <= 32768, "database snapshot query failed")

    def __exit__(self, *_):
        if self.process.poll() is None:
            try:
                self.process.stdin.write(b"ROLLBACK;\n\\q\n")
                self.process.stdin.flush()
                self.process.communicate(timeout=10)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                self.process.kill()
                self.process.communicate()
        self.errors.close()


def backup(connection, output_directory):
    output_directory.mkdir(mode=0o700, parents=False, exist_ok=True)
    require(output_directory.is_dir() and not output_directory.is_symlink(), "backup directory must be explicit and nonsymlinked")
    timestamp = datetime.now(timezone.utc)
    stem = "oxsum-backup-" + timestamp.strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex
    dump_name = stem + ".dump"
    manifest_name = stem + ".manifest.json"
    created = []
    try:
        with tempfile.TemporaryDirectory(prefix=".oxsum-backup-", dir=output_directory) as staging:
            staging = Path(staging)
            dump = staging / dump_name
            started = time.monotonic()
            with Snapshot(connection) as snapshot:
                descriptor = os.open(dump, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(descriptor, "wb") as output:
                    connection.run("pg_dump", ["--format=custom", "--no-password", "--snapshot", snapshot.snapshot], output=output)
                    output.flush()
                    os.fsync(output.fileno())
                observed = snapshot.metadata
            manifest = validate_manifest({"formatVersion": 2, "artifact": dump_name,
                "createdUtc": timestamp.strftime("%Y-%m-%dT%H:%M:%SZ"), **observed,
                "sizeBytes": dump.stat().st_size, "sha256": digest(dump), "dumpDurationMs": int((time.monotonic()-started)*1000)})
            private_json(staging / manifest_name, manifest)
            # Each link is non-overwriting. The manifest is the completion marker;
            # readers never accept an orphan dump as a complete backup.
            for name in [dump_name, manifest_name]:
                destination = output_directory / name
                os.link(staging / name, destination)
                created.append(destination)
                sync_directory(output_directory)
        return output_directory / manifest_name
    except BaseException:
        for path in reversed(created):
            path.unlink(missing_ok=True)
        raise


def restore(connection, manifest_path, target, receipt_path=None):
    require(isinstance(target, str) and DATABASE.fullmatch(target), "invalid target database name")
    require(target != connection.database and target not in {"postgres", "template0", "template1"}, "restore target must differ from the protected source/admin database")
    manifest = validate_manifest(read_json(private_text(manifest_path)))
    receipt_path = receipt_path or manifest_path.parent / ("oxsum-restore-" + uuid.uuid4().hex + ".json")
    require(receipt_path.parent.is_dir() and not receipt_path.exists() and not receipt_path.is_symlink(), "receipt must be a new path")
    created = False
    phase = "preflight"
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="oxsum-restore-") as frozen:
        frozen_dump = Path(frozen) / "backup.dump"
        with private_open(manifest_path.parent / manifest["artifact"]) as source:
            descriptor = os.open(frozen_dump, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as output:
                remaining = manifest["sizeBytes"]
                while remaining:
                    chunk = source.read(min(1024 * 1024, remaining))
                    if not chunk:
                        break
                    output.write(chunk)
                    remaining -= len(chunk)
                require(not source.read(1), "backup exceeds manifest size")
        require(frozen_dump.stat().st_size == manifest["sizeBytes"] and digest(frozen_dump) == manifest["sha256"], "backup size/checksum mismatch")
        try:
            version = connection.sql("SELECT current_setting('server_version_num')", database="postgres")
            require(version.isdigit() and int(version)//10000 >= manifest["postgresVersionNum"]//10000, "target PostgreSQL major is older than the backup")
            exists = connection.sql("SELECT 1 FROM pg_database WHERE datname=:'target'", database="postgres", variables={"target": target})
            require(not exists, "restore refuses an existing database")
            phase = "create"
            connection.sql('CREATE DATABASE :"target" TEMPLATE template0', database="postgres", variables={"target": target})
            created = True
            phase = "restore"
            with frozen_dump.open("rb") as source:
                connection.run("pg_restore", ["--no-password", "--single-transaction", "--exit-on-error", "--no-owner", "--no-privileges", "--dbname", target],
                               database=target, source=source)
            phase = "verify"
            observed = metadata(read_json(connection.sql("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; " + QUERY + "; COMMIT", database=target)))
            require(observed["schemaVersion"] == manifest["schemaVersion"] and observed["counts"] == manifest["counts"], "restored schema/counts differ from the frozen backup")
            receipt = {"formatVersion": 1, "operation": "restore", "status": "success", "targetDatabase": target,
                       "artifactSha256": manifest["sha256"], "restoreDurationMs": int((time.monotonic()-start)*1000), **observed}
            private_json(receipt_path, receipt)
            return receipt_path
        except BaseException:
            if created or phase == "create":
                private_json(receipt_path, {"formatVersion": 1, "operation": "restore", "status": "failed", "phase": phase,
                    "targetDatabase": target, "artifactSha256": manifest["sha256"], "targetRetained": created,
                    "creationOutcomeUnknown": not created})
            raise


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="operation", required=True)
    for operation in ["backup", "restore", "drill"]:
        command = commands.add_parser(operation)
        command.add_argument("--container", help="explicit PostgreSQL container; never auto-detected")
        command.add_argument("--database", help="source/protected database in container mode")
        command.add_argument("--user", help="container PostgreSQL user (default postgres)")
        command.add_argument("--database-url-file", type=Path, help="private URL file for local PostgreSQL tools")
        if operation in {"backup", "drill"}:
            command.add_argument("output_directory", type=Path)
            if operation == "drill":
                command.add_argument("target_database")
        else:
            command.add_argument("manifest", type=Path)
            command.add_argument("target_database")
            command.add_argument("--receipt", type=Path)
    return result


def main():
    args = parser().parse_args()
    connection = Connection(args.container, args.database, args.user, args.database_url_file)
    if args.operation == "drill":
        require(os.environ.get("SOURCE") in {None, connection.database}, "drill source differs from the explicit connection")
    if args.operation == "backup":
        path = backup(connection, args.output_directory)
        print("Private backup completed; manifest: " + str(path))
    else:
        manifest = backup(connection, args.output_directory) if args.operation == "drill" else args.manifest
        path = restore(connection, manifest, args.target_database, getattr(args, "receipt", None))
        print("Verified restore completed; receipt: " + str(path))
        print("Run the selected artifact's migration separately before starting service.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, UnicodeError, subprocess.SubprocessError, RecursionError):
        raise SystemExit("Backup/restore refused or failed. Check explicit target, private inputs, manifest and target state; no existing database was dropped.")
