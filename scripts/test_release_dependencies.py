"""Reachability failures must not become optional-advisory exceptions."""
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("guard", Path(__file__).with_name("check-release-dependencies.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class Reachability(unittest.TestCase):
    def test_postgres_graph_with_repeated_edges_passes(self):
        guard.check("oxsum-server v0.1.0 (/repo/crates/server)\nsqlx v0.8.6\nsqlx-postgres v0.8.6\nsqlx v0.8.6 (*)\n")

    def test_native_or_driver_path_fails(self):
        for dependency in guard.FORBIDDEN:
            with self.assertRaisesRegex(ValueError, dependency):
                guard.check(f"oxsum-server v0.1.0\n{dependency} v0.30.1\n")

    def test_missing_root_empty_or_malformed_output_fails(self):
        for output in ["", "sqlx v0.8.6\n", "oxsum-server v0.1.0\nUnexpected output\n"]:
            with self.assertRaises(ValueError):
                guard.check(output)

    def test_similar_package_name_does_not_hide_exact_driver(self):
        with self.assertRaisesRegex(ValueError, "sqlx-sqlite"):
            guard.check("oxsum-server v0.1.0\nmy-sqlx-sqlite-wrapper v1.0\nsqlx-sqlite v0.8.6 (*)\n")


if __name__ == "__main__":
    unittest.main()
