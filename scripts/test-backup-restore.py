#!/usr/bin/env python3
"""Fault and target-boundary checks for the PostgreSQL backup/restore pair."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("backup_restore", Path(__file__).with_name("backup_restore.py"))
backup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup)
META = {"postgresVersionNum": 170010, "schemaVersion": "0110_fixture", "counts": {key: 0 for key in backup.COUNTS},
        "observations": {key: 1 for key in backup.SETTINGS}}


class FakeConnection:
    database = "source"

    def __init__(self, exists=False, failure=None):
        self.calls = []
        self.exists = exists
        self.failure = failure

    def sql(self, sql, database=None, variables=None):
        self.calls.append((sql, database, variables))
        if self.failure == "create" and "CREATE DATABASE" in sql:
            raise ValueError("fixture create failed")
        if "current_setting('server_version_num')" == sql.removeprefix("SELECT "):
            return "170010"
        if "FROM pg_database WHERE" in sql:
            return "1" if self.exists else ""
        if "CREATE DATABASE" in sql:
            return ""
        return json.dumps(META if self.failure != "verify" else {**META, "schemaVersion": "0001_wrong"})

    def run(self, program, arguments, **kwargs):
        self.calls.append((program, arguments, kwargs.get("database")))
        if self.failure == "restore":
            raise ValueError("fixture restore failed")
        if program == "pg_dump":
            kwargs["output"].write(b"P" * 2048)


class BackupRestoreTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.dump = self.root / ("oxsum-backup-20261108T000000Z-" + "a" * 32 + ".dump")
        self.dump.write_bytes(b"P" * 2048)
        self.dump.chmod(0o600)
        self.manifest = {"formatVersion": 2, "artifact": self.dump.name, "createdUtc": "2026-11-08T00:00:00Z",
                         **copy.deepcopy(META), "sizeBytes": 2048, "sha256": backup.digest(self.dump), "dumpDurationMs": 1}
        self.path = self.root / "fixture.manifest.json"
        backup.private_json(self.path, self.manifest)

    def tearDown(self):
        self.temp.cleanup()

    def test_connection_selection_is_explicit_and_never_discovers_docker(self):
        with mock.patch.object(subprocess, "run") as run:
            for environ in [{}, {"CONTAINER": ""}, {"CONTAINER": "target", "POSTGRES_DB": "source", "DATABASE_URL": "postgres://u@host/source"}]:
                with self.assertRaises(ValueError):
                    backup.Connection(environ=environ)
            run.assert_not_called()
        container = backup.Connection(container="explicit", database="source", environ={"PATH": "/bin", "PGSERVICE": "private-service", "PGPASSWORD": "private-password"})
        command, env = container.command("psql", ["-X"])
        self.assertIn("explicit", command)
        self.assertNotIn("private-password", " ".join(command))
        self.assertEqual(env["PGPASSWORD"], "private-password")
        self.assertNotIn("PGSERVICE", env)
        self.assertIn("-u", command)

    def test_local_url_controls_every_command_without_credential_argv(self):
        url = self.root / "url"
        url.write_text("postgresql://user:private-password@127.0.0.1:6543/source?sslmode=verify-full")
        url.chmod(0o600)
        connection = backup.Connection(url_file=url, environ={"PATH": "/bin", "PGHOST": "wrong", "PGSERVICE": "wrong"})
        for program in ["psql", "pg_dump", "pg_restore"]:
            command, env = connection.command(program, [], database="restored")
            self.assertEqual(command, [program])
            self.assertEqual(env["PGHOST"], "127.0.0.1")
            self.assertEqual(env["PGPORT"], "6543")
            self.assertEqual(env["PGDATABASE"], "restored")
            self.assertEqual(env["PGSSLMODE"], "verify-full")
            self.assertNotIn("PGSERVICE", env)
        for bad in ["postgres://u@host/source?options=secret", "postgres://u@host/source?sslmode=require&sslmode=disable", "postgres://u@host/", "postgres://u@host/db%27%3Bdrop"]:
            with self.assertRaises(ValueError):
                backup.Connection(environ={"DATABASE_URL": bad})

    def test_manifest_is_strict_portable_and_rejects_missing_checksum(self):
        self.assertEqual(backup.validate_manifest(self.manifest), self.manifest)
        mutations = [lambda value: value.pop("sha256"), lambda value: value.update(extra="private"),
                     lambda value: value.update(artifact="../private.dump"), lambda value: value.update(artifact="/tmp/private.dump"),
                     lambda value: value.update(sizeBytes=True), lambda value: value.update(formatVersion=1),
                     lambda value: value.update(createdUtc="2026-99-99T00:00:00Z"),
                     lambda value: value["counts"].update(organizations=-1)]
        for mutate in mutations:
            value = copy.deepcopy(self.manifest)
            mutate(value)
            with self.assertRaises(ValueError):
                backup.validate_manifest(value)
        with self.assertRaises(ValueError):
            backup.read_json('{"sha256":"one","sha256":"two"}')

    def test_tampered_or_unprivate_dump_refuses_before_database(self):
        for mutation in [lambda: self.dump.write_bytes(b"Q"*2048), lambda: self.dump.write_bytes(b"P"*2049), lambda: self.dump.chmod(0o644)]:
            connection = FakeConnection()
            self.dump.write_bytes(b"P"*2048)
            self.dump.chmod(0o600)
            mutation()
            with self.assertRaises(ValueError):
                backup.restore(connection, self.path, "restored")
            self.assertEqual(connection.calls, [])
        self.dump.unlink()
        self.dump.symlink_to(self.path)
        with self.assertRaises(OSError):
            backup.restore(FakeConnection(), self.path, "restored")

    def test_existing_and_unsafe_target_never_restore_or_drop(self):
        connection = FakeConnection(exists=True)
        with self.assertRaisesRegex(ValueError, "existing"):
            backup.restore(connection, self.path, "existing")
        self.assertFalse(any("CREATE" in str(call) or "DROP" in str(call) or "pg_restore" in str(call) for call in connection.calls))
        for target in ["source", "postgres", "template0", "template1", "quoted'; DROP DATABASE live;--", "-option", "x"*64]:
            with self.assertRaises(ValueError):
                backup.restore(FakeConnection(), self.path, target)

    def test_restore_is_transactional_parameterized_and_retains_failure_receipt(self):
        for failure in [None, "restore", "verify"]:
            connection = FakeConnection(failure=failure)
            path = self.root / f"receipt-{failure}.json"
            if failure:
                with self.assertRaises(ValueError):
                    backup.restore(connection, self.path, "safe-target", path)
            else:
                backup.restore(connection, self.path, "safe-target", path)
            receipt = json.loads(path.read_text())
            self.assertEqual(receipt["status"], "failed" if failure else "success")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            commands = [call for call in connection.calls if call[0] == "pg_restore"]
            self.assertIn("--single-transaction", commands[0][1])
            self.assertTrue(any(call[0] == 'CREATE DATABASE :"target" TEMPLATE template0' and call[2] == {"target": "safe-target"} for call in connection.calls))
            self.assertNotIn("DROP DATABASE", str(connection.calls))

    def test_backup_uses_snapshot_and_publishes_private_completion_manifest_last(self):
        connection = FakeConnection()
        class Snapshot:
            snapshot = "00000003-00000001-1"
            metadata = copy.deepcopy(META)
            def __init__(self, _): pass
            def __enter__(self): return self
            def __exit__(self, *_): pass
        links = []
        real_link = os.link
        def link(source, destination):
            links.append(Path(destination).suffix)
            real_link(source, destination)
        with mock.patch.object(backup, "Snapshot", Snapshot), mock.patch.object(backup.os, "link", link):
            path = backup.backup(connection, self.root / "backup")
        value = backup.validate_manifest(json.loads(path.read_text()))
        self.assertEqual(value["counts"], META["counts"])
        self.assertEqual(links[-2:], [".dump", ".json"])
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual((path.parent/value["artifact"]).stat().st_mode & 0o777, 0o600)
        self.assertIn("--snapshot", connection.calls[0][1])

    def test_command_errors_never_expose_database_stderr_or_password(self):
        connection = backup.Connection(environ={"DATABASE_URL": "postgres://u:private-password@127.0.0.1/source", "PATH": "/bin"})
        with mock.patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 1, b"", b"private-error")):
            with self.assertRaises(ValueError) as error:
                connection.sql("SELECT 1")
        self.assertNotIn("private-password", str(error.exception))
        self.assertNotIn("private-error", str(error.exception))

    def test_failed_create_is_retained_as_unknown_and_json_never_overwrites(self):
        receipt = self.root / "failed-create.json"
        with self.assertRaises(ValueError):
            backup.restore(FakeConnection(failure="create"), self.path, "target", receipt)
        self.assertTrue(json.loads(receipt.read_text())["creationOutcomeUnknown"])
        before = receipt.read_bytes()
        with self.assertRaises(FileExistsError):
            backup.private_json(receipt, {"status": "overwritten"})
        self.assertEqual(receipt.read_bytes(), before)
        self.assertEqual(list(self.root.glob(".oxsum-json-*")), [])

    def test_partial_snapshot_line_cannot_block_past_deadline(self):
        reader, writer = os.pipe()
        try:
            os.write(writer, b"partial")
            with os.fdopen(reader, "rb", buffering=0) as stream:
                with self.assertRaisesRegex(ValueError, "timed out"):
                    backup.bounded_line(stream, timeout=0.02)
        finally:
            os.close(writer)


if __name__ == "__main__":
    unittest.main()
