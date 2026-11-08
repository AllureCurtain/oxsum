#!/usr/bin/env python3
"""Controlled HTTPS and archive regressions for the pinned CI installer."""

from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import io
import os
from pathlib import Path
import ssl
import subprocess
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("ci_tools", ROOT / "scripts/install-ci-tools.py")
tools = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tools)


def archive(extra=None):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as bundle:
        binary = b"#!/bin/sh\necho 'mold 2.40.0 fixture'\n"
        member = tarfile.TarInfo("fixture/bin/mold")
        member.size = len(binary)
        member.mode = 0o755
        bundle.addfile(member, io.BytesIO(binary))
        link = tarfile.TarInfo("fixture/bin/ld.mold")
        link.type = tarfile.SYMTYPE
        link.linkname = "mold"
        bundle.addfile(link)
        if extra:
            bundle.addfile(extra, io.BytesIO(b"x") if extra.isfile() else None)
    return data.getvalue()


@contextmanager
def server(directory, replies):
    certificate = directory / "certificate.pem"
    key = directory / "key.pem"
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
         "-subj", "/CN=127.0.0.1", "-addext", "subjectAltName=IP:127.0.0.1",
         "-keyout", str(key), "-out", str(certificate)],
        check=True, capture_output=True,
    )
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(dict(self.headers))
            status, content = replies[min(len(requests) - 1, len(replies) - 1)]
            self.send_response(status)
            self.send_header("Content-Length", str(len(content)))
            self.end_headers()
            self.wfile.write(content)

        def log_message(self, *_args):
            pass

    http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certificate, key)
    http.socket = context.wrap_socket(http.socket, server_side=True)
    worker = threading.Thread(target=http.serve_forever, daemon=True)
    worker.start()
    try:
        with patch.dict(os.environ, {"CURL_CA_BUNDLE": str(certificate)}):
            yield f"https://127.0.0.1:{http.server_port}/asset", requests
    finally:
        http.shutdown()
        worker.join()
        http.server_close()


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bytes = archive()
        self.spec = {
            "version": "2.40.0", "root": "fixture", "binary": "bin/mold",
            "size": len(self.bytes), "sha256": hashlib.sha256(self.bytes).hexdigest(),
        }

    def destination(self):
        return self.root / "cache/mold/2.40.0/x64"

    def assert_clean_failure(self):
        self.assertFalse(self.destination().exists())
        self.assertFalse(self.destination().with_suffix(".complete").exists())
        self.assertEqual(list((self.root / "cache").rglob(".download-*")), [])
        self.assertEqual(list((self.root / "cache").rglob("*.part")), [])

    def test_transient_http_failure_retries_then_installs_verified_bytes(self):
        with server(self.root, [(504, b"temporarily unavailable"), (200, self.bytes)]) as (url, calls):
            self.spec["asset"] = url
            binary = tools.install("mold", self.spec, self.root / "cache")
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0]["Accept"], "application/octet-stream")
        self.assertTrue(binary.is_file())
        self.assertTrue((binary.parent / "ld.mold").is_symlink())
        self.assertTrue(self.destination().with_suffix(".complete").is_file())

    def test_persistent_http_failure_is_bounded_and_never_extracts(self):
        with server(self.root, [(504, b"unavailable")]) as (url, calls):
            self.spec["asset"] = url
            with patch.object(tools, "extract") as extract:
                with self.assertRaises(subprocess.CalledProcessError):
                    tools.install("mold", self.spec, self.root / "cache")
                extract.assert_not_called()
        self.assertEqual(len(calls), 4)
        self.assert_clean_failure()

    def test_http_success_with_html_or_truncation_never_extracts(self):
        for content in (b"<html>gateway error</html>", self.bytes[:-5]):
            with self.subTest(content=content[:10]):
                with server(self.root, [(200, content)]) as (url, _):
                    self.spec["asset"] = url
                    with patch.object(tools, "extract") as extract:
                        with self.assertRaisesRegex(ValueError, "size mismatch"):
                            tools.install("mold", self.spec, self.root / "cache")
                        extract.assert_not_called()
                self.assert_clean_failure()

    def test_matching_size_with_wrong_hash_never_extracts(self):
        altered = b"x" + self.bytes[1:]
        with server(self.root, [(200, altered)]) as (url, _):
            self.spec["asset"] = url
            with patch.object(tools, "extract") as extract:
                with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                    tools.install("mold", self.spec, self.root / "cache")
                extract.assert_not_called()
        self.assert_clean_failure()

    def test_timeout_removes_partial_download(self):
        def timeout(command, **kwargs):
            Path(command[command.index("--output") + 1]).write_bytes(b"partial")
            self.assertEqual(kwargs["timeout"], 250)
            raise subprocess.TimeoutExpired(command, 250)
        self.spec["asset"] = "https://api.github.com/example"
        with patch.object(tools.subprocess, "run", side_effect=timeout):
            with self.assertRaises(subprocess.TimeoutExpired):
                tools.install("mold", self.spec, self.root / "cache")
        self.assert_clean_failure()

    def test_existing_verified_cache_is_idempotent(self):
        with server(self.root, [(200, self.bytes)]) as (url, _):
            self.spec["asset"] = url
            binary = tools.install("mold", self.spec, self.root / "cache")
            before = binary.stat().st_ino
            self.assertEqual(tools.install("mold", self.spec, self.root / "cache"), binary)
        self.assertEqual(binary.stat().st_ino, before)

    def test_download_failure_does_not_touch_existing_installation(self):
        self.destination().mkdir(parents=True)
        existing = self.destination() / "retained"
        existing.write_bytes(b"old installation")
        self.spec["asset"] = "https://api.github.com/example"
        with patch.object(tools.subprocess, "run", side_effect=subprocess.CalledProcessError(22, "curl")):
            with self.assertRaises(subprocess.CalledProcessError):
                tools.install("mold", self.spec, self.root / "cache")
        self.assertEqual(existing.read_bytes(), b"old installation")

    def test_different_existing_cache_is_never_overwritten_or_marked_complete(self):
        self.destination().mkdir(parents=True)
        existing = self.destination() / "old"
        existing.write_text("keep")
        with server(self.root, [(200, self.bytes)]) as (url, _):
            self.spec["asset"] = url
            with self.assertRaisesRegex(ValueError, "refusing replacement"):
                tools.install("mold", self.spec, self.root / "cache")
        self.assertEqual(existing.read_text(), "keep")
        self.assertFalse(self.destination().with_suffix(".complete").exists())

    def test_verified_unsafe_archive_cannot_write_outside_staging(self):
        unsafe = tarfile.TarInfo("fixture/../../escape")
        unsafe.size = 1
        content = archive(unsafe)
        self.spec.update(size=len(content), sha256=hashlib.sha256(content).hexdigest())
        with server(self.root, [(200, content)]) as (url, _):
            self.spec["asset"] = url
            with self.assertRaisesRegex(ValueError, "escapes"):
                tools.install("mold", self.spec, self.root / "cache")
        self.assert_clean_failure()
        self.assertFalse((self.root / "escape").exists())

    def test_workflows_use_one_installer_and_pin_mozilla_cache_setup(self):
        for name, molds, caches in [("ci.yml", 4, 3), ("release-candidate.yml", 3, 5)]:
            text = (ROOT / ".github/workflows" / name).read_text()
            self.assertEqual(text.count("run: python3 scripts/install-ci-tools.py mold"), molds)
            self.assertEqual(text.count("run: python3 scripts/install-ci-tools.py sccache"), caches)
            self.assertEqual(text.count("version: v0.19.0"), caches)
            self.assertEqual(text.count("mozilla-actions/sccache-action@v0.0.11"), caches)
            self.assertNotIn("curl -sL", text)
            self.assertIn('SCCACHE_GHA_ENABLED: "true"', text)


if __name__ == "__main__":
    unittest.main()
