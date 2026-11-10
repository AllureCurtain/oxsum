#!/usr/bin/env python3
"""Exercise missing coverage and failed execution with real child executables."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("shared_suites", Path(__file__).with_name("run-shared-suites.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class SharedSuitesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.bins = Path(self.temp.name)
        self.log = self.bins / "calls.jsonl"
        self.write_binary("core", ("generative", "seed", "jobs"))
        self.write_binary("server", ("jobs",))

    def write_binary(self, name, modules, mode="ok", suffix="abc123"):
        exe = self.bins / f"{name}-{suffix}"
        exe.write_text(f'''#!/usr/bin/env python3
import json, sys
from pathlib import Path
prefix = sys.argv[1]
modules = {modules!r}
mode = {mode!r}
if "--list" in sys.argv:
    if prefix.removesuffix("::") in modules:
        print(("nested::" if mode == "wrong_scope" else "") + prefix + "case: test")
else:
    with Path({str(self.log)!r}).open("a") as out:
        out.write(json.dumps(sys.argv[1:]) + "\\n")
    if mode == "failed":
        sys.exit(1)
    print("test result: ok. " + ("0" if mode == "empty" else "1") + " passed; 0 failed; 0 ignored; 0 measured; 0 filtered out")
''')
        exe.chmod(0o755)

    def test_every_suite_executes_in_one_process(self):
        with patch.dict(os.environ, {"DATABASE_URL": "postgres://fixture"}):
            runner.run(self.bins)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual([call[0] for call in calls], ["generative::", "seed::", "jobs::", "jobs::"])
        self.assertTrue(all(call[1:] == ["--test-threads=4"] for call in calls))

    def test_missing_and_ambiguous_aggregates_fail(self):
        (self.bins / "server-abc123").unlink()
        with self.assertRaises(ValueError):
            runner.plan(self.bins)
        self.write_binary("server", ("jobs",))
        self.write_binary("core", ("jobs",), suffix="def456")
        with self.assertRaises(ValueError):
            runner.plan(self.bins)

    def test_missing_and_wrong_scope_modules_fail_before_any_execution(self):
        for modules, mode in [(("generative", "seed"), "ok"), (("generative", "seed", "jobs"), "wrong_scope")]:
            with self.subTest(mode=mode):
                self.write_binary("core", modules, mode)
                with self.assertRaises(ValueError):
                    runner.run(self.bins)
                self.assertFalse(self.log.exists())

    def test_missing_database_fails(self):
        with patch.dict(os.environ, {"DATABASE_URL": ""}):
            with self.assertRaises(ValueError):
                runner.run(self.bins)
        self.assertFalse(self.log.exists())

    def test_failed_or_empty_execution_cannot_pass(self):
        for mode in ("failed", "empty"):
            with self.subTest(mode=mode):
                self.write_binary("core", ("generative", "seed", "jobs"), mode)
                with patch.dict(os.environ, {"DATABASE_URL": "postgres://fixture"}):
                    with self.assertRaises((ValueError, runner.subprocess.CalledProcessError)):
                        runner.run(self.bins)

    def test_inventory_only_never_executes(self):
        runner.run(self.bins, check_only=True)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
