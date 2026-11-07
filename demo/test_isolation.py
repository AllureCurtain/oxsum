"""Fail-closed database ownership checks; no database or model calls."""
import importlib.util
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("oxsum_demo", Path(__file__).with_name("demo.py"))
demo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(demo)


class DatabaseError(Exception):
    pass


class Sql:
    def __init__(self, value):
        self.value = value

    def format(self, identifier):
        return self.value.format(identifier)


class FakeConnection:
    def __init__(self, deny_create=False):
        self.commands = []
        self.deny_create = deny_create

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def execute(self, command):
        self.commands.append(command)
        if self.deny_create and command.startswith("CREATE"):
            raise DatabaseError("denied")


def fake_modules(connection):
    driver = types.ModuleType("psycopg")
    driver.Error = DatabaseError
    driver.connect = lambda *_args, **_kwargs: connection
    driver.sql = types.SimpleNamespace(SQL=Sql, Identifier=lambda name: name)
    conninfo = types.ModuleType("psycopg.conninfo")
    conninfo.conninfo_to_dict = lambda _value: {
        "user": "demo", "password": "private", "host": "localhost", "port": "5432", "dbname": "real_data",
    }
    return {"psycopg": driver, "psycopg.conninfo": conninfo}


class IsolationTests(unittest.TestCase):
    def test_create_denial_never_yields_application_database(self):
        connection = FakeConnection(deny_create=True)
        with patch.dict(sys.modules, fake_modules(connection)):
            with self.assertRaisesRegex(RuntimeError, "no fallback"):
                with demo.scratch_database("private provisioning URL"):
                    self.fail("Provisioning failure yielded an application database")
        self.assertEqual(len(connection.commands), 1)
        self.assertNotIn("real_data", connection.commands[0])

    def test_failure_drops_only_database_created_by_this_invocation(self):
        connection = FakeConnection()
        with patch.dict(sys.modules, fake_modules(connection)):
            with self.assertRaisesRegex(RuntimeError, "fixture failed"):
                with demo.scratch_database("private provisioning URL") as (name, url):
                    self.assertRegex(name, r"^oxsum_demo_[0-9a-f]{24}$")
                    self.assertTrue(url.endswith("/" + name))
                    raise RuntimeError("fixture failed")
        self.assertEqual(connection.commands, [f"CREATE DATABASE {name}", f"DROP DATABASE {name} WITH (FORCE)"])

    def test_fixture_does_not_inherit_provider_payment_or_database_config(self):
        with patch.dict("os.environ", {"OXSUM_E2E_UPSTREAM_API_KEY": "private", "OXSUM_SMTP_URL": "private",
                                       "DATABASE_URL": "private", "PGPASSWORD": "private", "PATH": "/tools"}, clear=True):
            self.assertEqual(demo.clean_environment(), {"PATH": "/tools"})


if __name__ == "__main__":
    unittest.main()
