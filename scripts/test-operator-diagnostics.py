#!/usr/bin/env python3
"""Real loopback HTTP checks for the sanitized diagnostic report boundary."""
import argparse
import contextlib
import copy
import http.server
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest

SPEC = importlib.util.spec_from_file_location("diagnostic", Path(__file__).with_name("operator-diagnostics.py"))
diagnostic = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(diagnostic)
TOKEN = "private-operator-token-fixture"
IDENTITY = {"version": "0.1.0", "gitSha": "a" * 40, "schemaVersion": "0110_fixture"}
DIAGNOSTICS = {**IDENTITY, "databaseState": "ready", "appliedSchemaVersion": IDENTITY["schemaVersion"],
               "configuration": {key: True for key in diagnostic.CONFIG_FLAGS},
               "counts": {key: 0 for key in diagnostic.COUNTS}}


@contextlib.contextmanager
def server(routes=None):
    routes = routes or {}
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append((self.path, self.headers.get("Authorization")))
            status, headers, body = routes.get(self.path, (200, {}, {
                "/healthz": b"ok", "/readyz": b"ready (0110_fixture)",
                "/api/v1/version": json.dumps({"data": IDENTITY}).encode(),
                "/api/v1/admin/diagnostics": json.dumps({"data": DIAGNOSTICS}).encode(),
            }.get(self.path, b"private-unknown-path")))
            self.send_response(status)
            for key, value in headers.items():
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{httpd.server_port}", requests
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join()


class DiagnosticsTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.token = self.private("operator-token", TOKEN)
        self.output = self.root / "report.json"

    def tearDown(self):
        self.temporary.cleanup()

    def private(self, name, contents):
        path = self.root / name
        path.write_text(contents)
        path.chmod(0o600)
        return path

    def collect(self, url, **kwargs):
        return diagnostic.collect(argparse.Namespace(url=url, token_file=kwargs.get("token_file", self.token),
                                  env_file=kwargs.get("env_file"), output=kwargs.get("output", self.output)))

    def test_loopback_report_only_sends_operator_token_to_protected_endpoint(self):
        env = self.private("operator.env", "OXSUM_ADMIN_TOKEN=private-admin\nOXSUM_PUBLIC_URL=https://private-site.test\n"
                           "OXSUM_SMTP_URL=smtp://private-user:private-password@private-host\n"
                           "OXSUM_SECRET_KEY=$(touch never-execute-me)\nUNKNOWN_PRIVATE_SECRET=private-value\n")
        with server() as (url, requests):
            report = self.collect(url, env_file=env)
        self.assertEqual(requests, [("/healthz", None), ("/readyz", None), ("/api/v1/version", None),
                                    ("/api/v1/admin/diagnostics", "Bearer " + TOKEN)])
        self.assertEqual(report["operator"]["data"], DIAGNOSTICS)
        self.assertEqual(report["localConfigurationPresence"]["smtp"], "partial")
        self.assertEqual(report["localConfigurationPresence"]["stripe"], "absent")
        self.assertEqual(report["localConfigurationPresence"]["credentialSeal"], "present")
        self.assertEqual(self.output.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("private", self.output.read_text())
        self.assertNotIn(url, self.output.read_text())
        self.assertFalse(Path("never-execute-me").exists())

    def test_missing_token_collects_public_states_without_authenticated_probe(self):
        with server() as (url, requests):
            report = self.collect(url, token_file=None)
        self.assertEqual(len(requests), 3)
        self.assertEqual(report["operator"], {"status": "notRequested"})
        self.assertIsNone(report["localConfigurationPresence"])

    def test_error_response_body_and_redirect_credentials_are_never_retained(self):
        with server() as (other, received):
            routes = {"/healthz": (503, {}, b"private-password-in-error"),
                      "/readyz": (200, {}, b"private-garbage"),
                      "/api/v1/admin/diagnostics": (307, {"Location": other + "/private-redirect"}, b"private-redirect-body")}
            with server(routes) as (url, _):
                report = self.collect(url)
        self.assertEqual(received, [])
        self.assertEqual(report["liveness"], {"status": "refused", "httpStatus": 503})
        self.assertEqual(report["readiness"], {"status": "invalidResponse"})
        self.assertEqual(report["operator"], {"status": "refused", "httpStatus": 307})
        self.assertNotIn("private", self.output.read_text())

    def test_extra_fields_and_malformed_nested_values_never_reach_report(self):
        variants = []
        for mutate in [lambda value: value.update(extraSecret="private-secret"),
                       lambda value: value["configuration"].update(secret="private-secret"),
                       lambda value: value["counts"].update(jobsDue="private-error"),
                       lambda value: value["counts"].update(jobsDue=True),
                       lambda value: value["counts"].update(jobsDue=-1),
                       lambda value: value.update(gitSha="private-source"),
                       lambda value: value.update(databaseState=[]),
                       lambda value: value.update(appliedSchemaVersion="private-url")]:
            value = copy.deepcopy(DIAGNOSTICS)
            mutate(value)
            variants.append(value)
        for index, value in enumerate(variants):
            with self.subTest(index=index):
                routes = {"/api/v1/admin/diagnostics": (200, {}, json.dumps({"data": value}).encode())}
                with server(routes) as (url, _):
                    report = self.collect(url, output=self.root / f"invalid-{index}.json")
                self.assertEqual(report["operator"], {"status": "invalidResponse"})
                self.assertNotIn("private", json.dumps(report))

    def test_nonready_snapshots_keep_counts_null(self):
        for index, state in enumerate(["unmigrated", "schemaMismatch", "unavailable"]):
            value = {**DIAGNOSTICS, "databaseState": state, "appliedSchemaVersion": None, "counts": None}
            with server({"/api/v1/admin/diagnostics": (200, {}, json.dumps({"data": value}).encode())}) as (url, _):
                report = self.collect(url, output=self.root / f"nonready-{index}.json")
            self.assertIsNone(report["operator"]["data"]["counts"])
        with self.assertRaises(ValueError):
            diagnostic.diagnostics({**DIAGNOSTICS, "databaseState": "unavailable", "appliedSchemaVersion": None})

    def test_environment_is_literal_and_reports_only_fixed_presence_groups(self):
        env = self.private("env", "export POSTGRES_PASSWORD='private-password'\nOXSUM_HEAD_SIGNING_KEY=''\nOXSUM_SMTP_URL=''\n"
                           "OXSUM_PUBLIC_URL=https://private-url\nOXSUM_X402_NETWORK=private-network\n")
        presence = diagnostic.environment_presence(env)
        self.assertEqual(presence["database"], "present")
        self.assertEqual(presence["headSigning"], "absent")
        self.assertEqual(presence["smtp"], "partial")
        self.assertEqual(presence["x402Facilitator"], "partial")
        self.assertEqual(set(presence), set(diagnostic.ENV_GROUPS))
        env.write_text("OXSUM_ADMIN_TOKEN=one\nOXSUM_ADMIN_TOKEN=two\n")
        with self.assertRaises(ValueError):
            diagnostic.environment_presence(env)

    def test_private_input_and_existing_output_fail_before_network(self):
        with server() as (url, requests):
            self.token.chmod(0o644)
            with self.assertRaises(ValueError):
                self.collect(url)
            self.token.chmod(0o600)
            link = self.root / "link"
            link.symlink_to(self.token)
            with self.assertRaises(OSError):
                self.collect(url, token_file=link)
            self.output.write_text("retain existing file")
            with self.assertRaises(ValueError):
                self.collect(url)
            self.assertEqual(requests, [])
        self.assertEqual(self.output.read_text(), "retain existing file")

    def test_origins_refuse_credentials_paths_and_nonloopback_plaintext(self):
        for url in ["http://example.test", "http://localhost", "http://192.168.1.2", "https://user:secret@example.test",
                    "https://@example.test", "https://example.test/path", "https://example.test?token=x", "https://example.test/#x",
                    "file:///tmp/private", "http://127.0.0.1:0", "http://127.0.0.1:99999"]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                diagnostic.target_url(url)
        self.assertEqual(diagnostic.target_url("http://[::1]:8080/"), "http://[::1]:8080")
        self.assertEqual(diagnostic.target_url("https://example.test/"), "https://example.test")

    def test_oversized_response_and_wrong_envelope_are_discarded(self):
        with server({"/api/v1/version": (200, {}, b"x" * 32769),
                     "/api/v1/admin/diagnostics": (200, {}, json.dumps({"data": DIAGNOSTICS, "private": "secret"}).encode())}) as (url, _):
            report = self.collect(url)
        self.assertEqual(report["version"], {"status": "invalidResponse"})
        self.assertEqual(report["operator"], {"status": "invalidResponse"})

    def test_deeply_nested_json_is_invalid_instead_of_aborting_collection(self):
        body = b"[" * 4000 + b"0" + b"]" * 4000
        with server({"/api/v1/version": (200, {}, body)}) as (url, _):
            report = self.collect(url)
        self.assertEqual(report["version"], {"status": "invalidResponse"})
        self.assertEqual(report["operator"]["status"], "ok")


if __name__ == "__main__":
    unittest.main()
