#!/usr/bin/env python3
"""SDK publication gates and real npm publish against an isolated local registry."""
import argparse
import base64
import copy
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import patch
import urllib.error
import urllib.request

import candidate_bundle as gate


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sdk_publish = load("sdk_publish", "publish-candidate-sdk.py")
fixtures = load("sdk_fixtures", "test-candidate-promotion.py")
SHA = fixtures.SHA


def fixture(root, marker=None, version="0.1.0", content=b"export {}"):
    manifest = fixtures.fixture(root)
    scripts = {} if marker is None else {name: f'node -e "require(\'fs\').writeFileSync(\'{marker}\',\'ran\')"'
                                      for name in ["prepare", "prepack", "prepublishOnly", "publish", "postpublish"]}
    fixtures.archive(root / "sdk.tgz", {
        "package/package.json": {"name": "verify-oxsum-bill", "version": version, "scripts": scripts},
        "package/dist/index.js": content, "package/dist/index.d.ts": b"export {}",
        "package/wasm/verify_wasm.js": b"export {}", "package/wasm/verify_wasm_bg.wasm": b"\x00asm" + b"\x00" * 1500})
    manifest["repository"] = "AllureCurtain/oxsum"
    manifest["workflow"]["url"] = "https://github.com/AllureCurtain/oxsum/actions/runs/2"
    ci = json.loads((root / "ci.json").read_text())
    ci["run_url"] = "https://github.com/AllureCurtain/oxsum/actions/runs/1"
    (root / "ci.json").write_text(json.dumps(ci))
    manifest["sdk"] = {"tarball": "sdk.tgz", **gate.sdk_identity(root / "sdk.tgz")}
    manifest["artifacts"] = gate.file_map(root)
    (root / "candidate-manifest.json").write_text(json.dumps(manifest))
    gate.write_sums(root)
    return manifest


class FixtureRegistry:
    """Minimal npm registry protocol, deliberately not a public npm service."""

    def __init__(self):
        self.package = None
        self.blobs = {}
        self.puts = 0
        self.auth = []
        self.redirect = None
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def send(self, status, value):
                raw = value if isinstance(value, bytes) else json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                owner.auth.append(self.headers.get("Authorization"))
                if self.path == "/verify-oxsum-bill":
                    self.send(200 if owner.package else 404, owner.package or {"error": "Not found"})
                elif self.path in owner.blobs:
                    self.send(200, owner.blobs[self.path])
                else:
                    self.send(404, {"error": "Not found"})

            def do_PUT(self):
                owner.auth.append(self.headers.get("Authorization"))
                if owner.redirect:
                    self.send_response(307)
                    self.send_header("Location", owner.redirect)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                if self.path != "/verify-oxsum-bill":
                    self.send(403, {"error": "unexpected path"})
                    return
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                version = next(iter(body["versions"]))
                if owner.package and version in owner.package["versions"]:
                    self.send(409, {"error": "version exists"})
                    return
                if owner.package is None:
                    owner.package = {"name": "verify-oxsum-bill", "versions": {}, "dist-tags": {}}
                owner.package["versions"].update(body["versions"])
                owner.package["dist-tags"].update(body["dist-tags"])
                for name, row in body["_attachments"].items():
                    owner.blobs["/verify-oxsum-bill/-/" + name] = base64.b64decode(row["data"])
                owner.puts += 1
                self.send(201, {"ok": True})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/"

    def __enter__(self):
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class SdkTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name)
        self.root = self.work / "bundle"
        self.manifest = fixture(self.root)
        self.args = argparse.Namespace(directory=self.root, sha=SHA, run="2", attempt=1, version="0.1.0", tag="candidate-0-1-0",
            registry="https://registry.npmjs.org/", receipt=self.work / "receipt.json", local_fixture=False)

    def test_registry_origin_and_fixture_boundaries(self):
        for value in ["http://registry.npmjs.org/", "https://registry.npmjs.org.evil.invalid/", "https://registry.npmjs.org:8443/",
                      "https://user:token@registry.npmjs.org/", "https://registry.npmjs.org/path/"]:
            with self.assertRaises(ValueError):
                sdk_publish.registry_url(value, False)
        for value in ["http://localhost:5000/", "http://127.0.0.1:99999/", "https://registry.npmjs.org/", "http://127.0.0.1:1/path/"]:
            with self.assertRaises(ValueError):
                sdk_publish.registry_url(value, True)

    def test_failed_provenance_and_wrong_selectors_never_publish(self):
        with patch.dict(os.environ, {"OXSUM_NPM_TOKEN": "test-token"}), \
                patch.object(gate, "validate_online", side_effect=ValueError("expired candidate")), \
                patch.object(sdk_publish, "run_npm") as run, self.assertRaisesRegex(ValueError, "expired"):
            sdk_publish.publish(self.args)
        run.assert_not_called()
        for field, value in [("sha", "b" * 40), ("run", "3"), ("attempt", 2), ("version", "0.2.0"), ("tag", "0.1.0"), ("tag", "latest;id")]:
            args = copy.copy(self.args)
            setattr(args, field, value)
            with patch.dict(os.environ, {"OXSUM_NPM_TOKEN": "test-token"}), patch.object(sdk_publish, "run_npm") as run, self.assertRaises(ValueError):
                sdk_publish.publish(args)
            run.assert_not_called()

    def test_existing_identical_version_reverifies_without_npm(self):
        with patch.dict(os.environ, {"OXSUM_NPM_TOKEN": "test-token"}), patch.object(gate, "validate_online"), \
                patch.object(sdk_publish.Registry, "check", return_value=True), patch.object(sdk_publish, "run_npm") as run:
            receipt = sdk_publish.publish(self.args)
        run.assert_not_called()
        self.assertEqual(receipt["result"], "already-present")
        self.assertEqual(receipt["tarballSha256"], self.manifest["artifacts"]["sdk.tgz"]["sha256"])
        with patch.dict(os.environ, {"OXSUM_NPM_TOKEN": "test-token"}), self.assertRaisesRegex(ValueError, "new file"):
            sdk_publish.publish(self.args)

    def test_tarball_replacement_after_gate_refused(self):
        def replace(frozen, _manifest):
            (frozen / "sdk.tgz").write_bytes(b"altered")
        with patch.dict(os.environ, {"OXSUM_NPM_TOKEN": "test-token"}), patch.object(gate, "validate_online", side_effect=replace), \
                patch.object(sdk_publish, "run_npm") as run, self.assertRaisesRegex(ValueError, "changed after validation"):
            sdk_publish.publish(self.args)
        run.assert_not_called()

    def test_publish_config_overrides_refused_before_registry(self):
        with tarfile.open(self.root / "sdk.tgz") as saved:
            files = {member.name: saved.extractfile(member).read() for member in saved.getmembers() if member.isfile()}
        package = json.loads(files["package/package.json"])
        package["publishConfig"] = {"registry": "https://evil.invalid/", "ignore-scripts": False}
        files["package/package.json"] = package
        fixtures.archive(self.root / "sdk.tgz", files)
        manifest = copy.deepcopy(self.manifest)
        manifest["sdk"] = {"tarball": "sdk.tgz", **gate.sdk_identity(self.root / "sdk.tgz")}
        manifest["artifacts"] = gate.file_map(self.root)
        (self.root / "candidate-manifest.json").write_text(json.dumps(manifest))
        gate.write_sums(self.root)
        with patch.dict(os.environ, {"OXSUM_NPM_TOKEN": "test-token"}), patch.object(gate, "validate_online"), \
                patch.object(sdk_publish.Registry, "check") as check, self.assertRaisesRegex(ValueError, "publishConfig"):
            sdk_publish.publish(self.args)
        check.assert_not_called()

    def test_registry_absence_is_not_auth_proxy_or_network_failure(self):
        registry = sdk_publish.Registry("https://registry.npmjs.org/", "token")
        for code, body, absent in [(404, b'{"error":"Not found"}', True), (404, b"proxy", False), (401, b"denied", False), (500, b"failed", False)]:
            error = urllib.error.HTTPError(registry.url, code, "fixture", {}, io.BytesIO(body))
            with patch.object(registry.opener, "open", side_effect=error):
                if absent:
                    self.assertIsNone(registry.metadata())
                else:
                    with self.assertRaises(ValueError):
                        registry.metadata()
        with patch.object(registry.opener, "open", side_effect=urllib.error.URLError("offline")), self.assertRaises(ValueError):
            registry.metadata()

    def test_tarball_url_redirects_and_foreign_origins_refused(self):
        registry = sdk_publish.Registry("https://registry.npmjs.org/", "token")
        sdk = self.manifest["sdk"]
        version = {"name": sdk["name"], "version": sdk["version"], "dist": {"integrity": sdk["integrity"], "tarball": "https://evil.invalid/sdk.tgz"}}
        metadata = {"name": sdk["name"], "versions": {sdk["version"]: version}, "dist-tags": {self.args.tag: sdk["version"]}}
        with patch.object(registry, "metadata", return_value=metadata), self.assertRaisesRegex(ValueError, "tarball URL"):
            registry.check(sdk, self.args.tag, 1, "a" * 64)
        error = urllib.error.HTTPError(registry.url, 307, "redirect", {"Location": "https://evil.invalid"}, io.BytesIO())
        with patch.object(registry.opener, "open", side_effect=error) as opened, self.assertRaisesRegex(ValueError, "HTTP 307"):
            registry.request("PUT", "verify-oxsum-bill", b"{}")
        opened.assert_called_once()

    def test_registry_path_and_downloaded_bytes_are_exact(self):
        registry = sdk_publish.Registry("https://registry.npmjs.org/", "token")
        for path in ["verify-oxsum-bill/../../other", "verify-oxsum-bill-extra", "verify-oxsum-bill?other", "verify-oxsum-bill/-/../other.tgz"]:
            with patch.object(registry.opener, "open") as opened, self.assertRaises(ValueError):
                registry.request("GET", path)
            opened.assert_not_called()
        sdk = self.manifest["sdk"]
        version = {"name": sdk["name"], "version": sdk["version"], "dist": {"integrity": sdk["integrity"],
                   "tarball": registry.url + "verify-oxsum-bill/-/verify-oxsum-bill-0.1.0.tgz"}}
        metadata = {"name": sdk["name"], "versions": {sdk["version"]: version}, "dist-tags": {self.args.tag: sdk["version"]}}
        with patch.object(registry, "metadata", return_value=metadata), patch.object(registry, "request", return_value=b"altered"), \
                self.assertRaisesRegex(ValueError, "tarball bytes differ"):
            registry.check(sdk, self.args.tag, len(b"altered"), "0" * 64)

    def test_failed_post_publish_check_never_writes_receipt(self):
        with patch.dict(os.environ, {"OXSUM_NPM_TOKEN": "test-token"}), patch.object(gate, "validate_online"), \
                patch.object(sdk_publish.Registry, "check", side_effect=[False, False]), patch.object(sdk_publish, "run_npm"), \
                self.assertRaisesRegex(ValueError, "unavailable"):
            sdk_publish.publish(self.args)
        self.assertFalse(self.args.receipt.exists())

    def test_relay_rejects_changed_attachment_and_multiple_versions(self):
        sdk = self.manifest["sdk"]
        registry = sdk_publish.Registry("http://127.0.0.1:1/", "")
        raw = (self.root / "sdk.tgz").read_bytes()
        body = {"_id": sdk["name"], "name": sdk["name"], "dist-tags": {self.args.tag: sdk["version"]},
            "versions": {sdk["version"]: {"dist": {"integrity": sdk["integrity"], "shasum": hashlib.sha1(raw).hexdigest()}}},
            "_attachments": {f"{sdk['name']}-{sdk['version']}.tgz": {"data": base64.b64encode(raw).decode(), "length": len(raw)}}}
        with sdk_publish.PublishRelay(registry, sdk, self.args.tag, self.root / "sdk.tgz") as relay:
            bad = copy.deepcopy(body)
            bad["_attachments"][f"{sdk['name']}-{sdk['version']}.tgz"]["data"] = base64.b64encode(b"changed").decode()
            with self.assertRaisesRegex(ValueError, "original candidate tarball"):
                relay.forward(bad)
            bad = copy.deepcopy(body)
            bad["versions"]["2.0.0"] = {}
            with self.assertRaisesRegex(ValueError, "metadata differs"):
                relay.forward(bad)

    def test_real_npm_local_registry_rehearsal(self):
        result = local_rehearsal(self.work / "rehearsal")
        self.assertTrue(result["originalTarballUploaded"] and result["downloadedBytesMatched"])
        self.assertTrue(result["versionConflictRefused"] and result["tagConflictRefused"])
        self.assertTrue(result["lifecycleHooksIgnored"] and result["redirectCredentialBoundary"])


def local_rehearsal(output):
    gate.require(not output.exists(), "rehearsal output must be a new directory")
    output.mkdir(parents=True)
    marker = output / "lifecycle-ran"
    root = output / "bundle"
    manifest = fixture(root, marker)
    with FixtureRegistry() as registry:
        args = argparse.Namespace(directory=root, sha=SHA, run="2", attempt=1, version="0.1.0", tag="candidate-0-1-0",
            registry=registry.url, receipt=output / "published.json", local_fixture=True)
        # An ambient hostile npm configuration must not redirect the child.
        with patch.dict(os.environ, {"npm_config_registry": "https://invalid.example/", "NPM_CONFIG_IGNORE_SCRIPTS": "false",
                                    "NODE_OPTIONS": "--require=/nonexistent/preload.js", "OXSUM_NPM_TOKEN": "must-not-reach-fixture"}):
            first = sdk_publish.publish(args)
        original = (root / "sdk.tgz").read_bytes()
        gate.require(registry.blobs["/verify-oxsum-bill/-/verify-oxsum-bill-0.1.0.tgz"] == original, "upload bytes differ")
        gate.require(not marker.exists(), "npm ran a lifecycle hook")
        gate.require(all(value is None for value in registry.auth), "fixture registry received a credential")
        args.receipt = output / "repeated.json"
        repeated = sdk_publish.publish(args)
        gate.require(repeated["result"] == "already-present" and registry.puts == 1, "repeat wrote a second version")
        conflict = output / "conflicting-bundle"
        fixture(conflict, content=b"export const changed = true")
        args.directory, args.receipt = conflict, output / "conflict.json"
        try:
            sdk_publish.publish(args)
        except ValueError as error:
            gate.require("different identity or integrity" in str(error), "unexpected version conflict refusal")
        else:
            raise ValueError("different version bytes were accepted")
        new = output / "new-version-bundle"
        fixture(new, version="0.2.0")
        args.directory, args.version, args.receipt = new, "0.2.0", output / "tag-conflict.json"
        try:
            sdk_publish.publish(args)
        except ValueError as error:
            gate.require("tag already points" in str(error), "unexpected tag conflict refusal")
        else:
            raise ValueError("occupied tag was overwritten")
        gate.require(registry.puts == 1, "conflict mutated registry")
        # A real npm PUT reaches the bounded relay. Registry redirect never
        # reaches this second listener or receives its scoped fake credential.
        with FixtureRegistry() as redirected:
            registry.redirect = redirected.url + "verify-oxsum-bill"
            args.tag, args.receipt = "candidate-0-2-0", output / "redirect.json"
            sdk = gate.sdk_identity(new / "sdk.tgz")
            with tempfile.TemporaryDirectory() as name:
                try:
                    sdk_publish.run_npm(sdk_publish.Registry(registry.url, "scoped-fixture-secret"), sdk, args.tag, new / "sdk.tgz", Path(name))
                except ValueError as error:
                    gate.require("HTTP 307" in str(error), "unexpected redirect refusal")
                else:
                    raise ValueError("redirect publication succeeded")
            gate.require(not redirected.auth and redirected.puts == 0, "redirect leaked a credential/request")
        result = {"evidenceTier": "local-fixture", "npmVersion": subprocess.check_output(["npm", "--version"], text=True).strip(),
            "package": first["name"], "version": first["version"], "integrity": first["integrity"], "tarballSha256": first["tarballSha256"],
            "originalTarballUploaded": True, "downloadedBytesMatched": True, "idempotentRepeat": True,
            "versionConflictRefused": True, "tagConflictRefused": True, "lifecycleHooksIgnored": True,
            "redirectCredentialBoundary": True, "publicPublication": False}
        (output / "rehearsal.json").write_text(json.dumps(result, indent=2) + "\n")
        return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--local-registry", type=Path, help="retain real npm loopback-registry rehearsal artifacts")
    args, remaining = parser.parse_known_args()
    if args.local_registry:
        print(json.dumps(local_rehearsal(args.local_registry.resolve()), indent=2))
    else:
        unittest.main(argv=[__file__, *remaining])
