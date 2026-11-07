"""Isolation failure paths must not touch a caller's database or leak credentials."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch, MagicMock

spec = importlib.util.spec_from_file_location("runner", Path(__file__).with_name("verify-examples.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class Isolation(unittest.TestCase):
    def test_authority_is_explicit_and_query_cannot_redirect_scratch(self):
        source = "postgres://user:private@db.example:5433/original?sslmode=require"
        admin, scratch = runner.database_urls(source, "oxsum_examples_abc")
        self.assertEqual(admin, source)
        self.assertEqual(scratch, "postgres://user:private@db.example:5433/oxsum_examples_abc?sslmode=require")
        for suffix in ["?dbname=other", "?host=other", "?service=prod", "?options=-c%20search_path=x", "#fragment"]:
            with self.assertRaises(RuntimeError):
                runner.database_urls("postgres://user:pass@localhost/original" + suffix, "owned")

    def test_runtime_does_not_inherit_deployment_or_node_options(self):
        sample = {"PATH": "/bin", "OXSUM_E2E_UPSTREAM_API_KEY": "never", "DATABASE_URL": "never",
                  "DOUBLEENTRY_DATABASE_URL": "never", "PGSERVICE": "never", "NODE_OPTIONS": "never",
                  "HTTPS_PROXY": "never", "SOAK_SECONDS": "1"}
        with patch.dict(os.environ, sample, clear=True):
            self.assertEqual(runner.clean_environment(), {"PATH": "/bin"})

    def test_create_failure_has_no_yield_no_drop_and_no_fallback(self):
        import psycopg
        connection = MagicMock()
        connection.execute.side_effect = psycopg.Error("credential-secret must not escape")
        with patch.object(psycopg, "connect", return_value=connection) as connect:
            with self.assertRaisesRegex(RuntimeError, "no fallback"):
                with runner.scratch_database("postgres://user:pass@localhost/original", {}):
                    self.fail("Must not start an application")
            self.assertEqual(connect.call_count, 1)
            self.assertEqual(connection.execute.call_count, 1)

    def test_failure_after_create_drops_exact_owned_database_on_same_connection(self):
        import psycopg
        connection = MagicMock()
        evidence = {}
        with patch.object(psycopg, "connect", return_value=connection):
            with self.assertRaisesRegex(RuntimeError, "application failed"):
                with runner.scratch_database("postgres://user:pass@localhost/original", evidence) as url:
                    self.assertTrue(url.endswith("/" + evidence["scratchDatabase"]))
                    raise RuntimeError("application failed")
        create, drop = (str(call.args[0]) for call in connection.execute.call_args_list)
        self.assertIn(evidence["scratchDatabase"], create)
        self.assertIn(evidence["scratchDatabase"], drop)
        self.assertNotIn("original", drop)
        self.assertTrue(evidence["databaseRemoved"])

    def test_cleanup_failure_cannot_be_reported_as_success(self):
        import psycopg
        connection = MagicMock()
        connection.execute.side_effect = [None, psycopg.Error("cleanup credential")]
        evidence = {}
        with patch.object(psycopg, "connect", return_value=connection):
            with self.assertRaisesRegex(RuntimeError, "Cleanup failed"):
                with runner.scratch_database("postgres://user:pass@localhost/original", evidence):
                    pass
        self.assertFalse(evidence["databaseRemoved"])

    def test_skips_and_zero_assertions_fail_except_declared_absent_witness(self):
        for text in ["[PASS] x\n[FAIL] y", "[PASS] x\n[SKIP] metering", "done"]:
            with self.assertRaises(RuntimeError):
                runner.assertions(text)
        result = runner.assertions("[PASS] entry\n" + runner.OPTIONAL_WITNESS, optional_witness=True)
        self.assertEqual(result, {"passed": 1, "optionalWitnessAbsent": 1})
        with self.assertRaises(RuntimeError):
            runner.exported_credentials("OXSUM_CUSTOMER_ID=x")

    def test_copy_omits_private_env_and_keeps_caller_files(self):
        with tempfile.TemporaryDirectory() as temp:
            original = Path(temp) / "source"
            original.mkdir()
            (original / "package.json").write_text('{"dependencies":{"existing":"1"}}')
            (original / ".env").write_text("OXSUM_KEY=private")
            (original / ".npmrc").write_text("//registry/:_authToken=private")
            (original / "run.mjs").write_text("console.log('fixture')")
            before = {p.name:p.read_bytes() for p in original.iterdir()}
            copy = Path(temp) / "copy"
            runner.copy_example(original, copy)
            self.assertFalse((copy / ".npmrc").exists())
            self.assertEqual((copy / ".env").read_bytes(), b"")
            self.assertEqual({p.name:p.read_bytes() for p in original.iterdir()}, before)
            self.assertEqual((copy / ".env").stat().st_mode & 0o777, 0o600)

    def test_timed_out_child_is_stopped_and_output_remains_private(self):
        import sys
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / "step.log"
            with self.assertRaisesRegex(RuntimeError, "timed out"):
                runner.run([sys.executable, "-c", "import time; print('private', flush=True); time.sleep(10)"],
                           cwd=Path(temp), env=runner.clean_environment(), log=log, timeout=1)
            self.assertEqual(log.stat().st_mode & 0o777, 0o600)
            self.assertEqual(log.read_text().strip(), "private")


if __name__ == "__main__":
    unittest.main()
