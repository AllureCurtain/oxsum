#!/usr/bin/env python3
"""Candidate promotion refusal tests; optional isolated real registry rehearsal."""
import argparse
import copy
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch
import urllib.error
import urllib.request
import uuid

import candidate_bundle as gate

SPEC = importlib.util.spec_from_file_location("promotion", Path(__file__).with_name("promote-candidate.py"))
promotion = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(promotion)
SHA = "a" * 40
SOURCE_TAG = "ghcr.io/owner/repo:sha-" + SHA
REGISTRY_IMAGE = "registry:2.8.3@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373"
REMOTE = {"registryDigest": "sha256:" + "1" * 64, "platformManifestDigest": "sha256:" + "1" * 64}


def archive(path, files):
    with tarfile.open(path, "w:gz") as saved:
        for name, value in files.items():
            blob = value if isinstance(value, bytes) else json.dumps(value).encode()
            info = tarfile.TarInfo(name)
            info.size = len(blob)
            saved.addfile(info, io.BytesIO(blob))


def response(body=b"", status=200, headers=None):
    value = io.BytesIO(body)
    value.status = status
    value.headers = headers or {}
    return value


def fixture(root, content=b"promotion fixture\n", compressed=False):
    """A loadable linux/amd64 image, with no Docker build or external base image."""
    root.mkdir()
    layer = io.BytesIO()
    with tarfile.open(fileobj=layer, mode="w") as saved:
        info = tarfile.TarInfo("fixture.txt")
        info.size = len(content)
        saved.addfile(info, io.BytesIO(content))
    layer = layer.getvalue()
    config = json.dumps({"os": "linux", "architecture": "amd64",
        "config": {"Labels": {"org.opencontainers.image.revision": SHA}},
        "rootfs": {"type": "layers", "diff_ids": ["sha256:" + hashlib.sha256(layer).hexdigest()]},
        "history": [{"created_by": "oxsum local promotion fixture"}]}).encode()
    config_name = hashlib.sha256(config).hexdigest() + ".json"
    archive(root / "image.tar.gz", {config_name: config,
        "manifest.json": [{"Config": config_name, "RepoTags": [SOURCE_TAG], "Layers": ["layer.tar"]}],
        "layer.tar": gzip.compress(layer, mtime=0) if compressed else layer})
    archive(root / "sdk.tgz", {"package/package.json": {"name": "verify-oxsum-bill", "version": "0.1.0"},
        "package/dist/index.js": b"export {}", "package/dist/index.d.ts": b"export {}",
        "package/wasm/verify_wasm.js": b"export {}", "package/wasm/verify_wasm_bg.wasm": b"\x00asm" + b"\x00" * 1500})
    (root / "ci.json").write_text(json.dumps({"sha": SHA, "run_id": 1, "attempt": 1,
        "run_url": "https://github.com/owner/repo/actions/runs/1", "conclusion": "success",
        "jobs": dict.fromkeys(gate.CI.REQUIRED_JOBS, "success"),
        "postgres_conformance": "success", "process_shared": "success",
        "executed_steps": {name: sorted(steps) for name, steps in gate.CI.REQUIRED_STEPS.items()}}))
    for name in ["smoke.log", "consumer.log", "RELEASE-NOTES-DRAFT.md"]:
        (root / name).write_text("Synthetic fixture; not candidate acceptance.\n")
    gate.create(argparse.Namespace(directory=root, sha=SHA, workflow_sha=SHA, repository="owner/repo", run="2", attempt=1,
        image="image.tar.gz", sdk="sdk.tgz", full_ci="ci.json", smoke_log="smoke.log", consumer_log="consumer.log"))
    return gate.validate(root, SHA)


class PromotionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name)
        self.root = self.work / "bundle"
        self.manifest = fixture(self.root)
        self.image = {k: self.manifest["image"][k] for k in ["configId", "platform", "sourceSha"]}
        self.args = argparse.Namespace(directory=self.root, sha=SHA, repository="owner/repo", run="2", attempt=1,
            destination="ghcr.io/owner/repo:v1.0.0", receipt=self.work / "receipt.json", local_fixture=False)

    def test_explicit_version_and_loopback_fixture_boundaries(self):
        for value in ["ghcr.io/owner/repo:latest", "ghcr.io/owner/repo:sha-" + SHA,
                      "ghcr.io/owner/repo:v1.2.3\n", "ghcr.io/owner/repo:v1.0.0;id", "ghcr.io/owner/../repo:v1.0.0",
                      "example.com/owner/repo:v1.0.0", "--config/owner/repo:v1.0.0"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                promotion.destination(value, False)
        for value in ["localhost:5000/owner/repo:v1.0.0", "127.0.0.1:99999/owner/repo:v1.0.0",
                      "127.0.0.1.example.com:5000/owner/repo:v1.0.0", "ghcr.io/owner/repo:v1.0.0"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                promotion.destination(value, True)
        self.assertEqual(promotion.destination("127.0.0.1:5000/owner/repo:v1.0.0", True)[0], "127.0.0.1:5000")

    def test_failed_provenance_never_writes_registry(self):
        for reason in ["expired", "wrong SHA", "skipped CI", "wrong workflow", "different manifest"]:
            with patch.object(gate, "validate_online", side_effect=ValueError(reason)), \
                    patch.object(promotion.Registry, "publish") as docker, self.assertRaisesRegex(ValueError, reason):
                promotion.promote(self.args)
            docker.assert_not_called()

    def test_wrong_selector_and_tampered_archive_fail_before_provenance(self):
        for field, value in [("sha", "b" * 40), ("sha", "a" * 7), ("run", "3"), ("attempt", 2), ("repository", "other/repo")]:
            args = copy.copy(self.args)
            setattr(args, field, value)
            with patch.object(gate, "validate_online") as online, patch.object(promotion.Registry, "publish") as docker, self.assertRaises(ValueError):
                promotion.promote(args)
            online.assert_not_called()
            docker.assert_not_called()
        (self.root / "image.tar.gz").write_bytes(b"replaced")
        with patch.object(promotion.Registry, "publish") as docker, self.assertRaises(ValueError):
            promotion.promote(self.args)
        docker.assert_not_called()

    def test_failed_gates_wrong_config_and_platform_never_publish(self):
        for change in [lambda m: m["gates"].update(smoke="skipped"),
                       lambda m: m["image"].update(configId="sha256:" + "0" * 64),
                       lambda m: m["image"].update(platform="linux/arm64")]:
            manifest = copy.deepcopy(self.manifest)
            change(manifest)
            (self.root / "candidate-manifest.json").write_text(json.dumps(manifest))
            gate.write_sums(self.root)
            with patch.object(promotion.Registry, "publish") as docker, self.assertRaises(ValueError):
                promotion.promote(self.args)
            docker.assert_not_called()

    def test_snapshot_survives_original_replacement_and_rejects_frozen_replacement(self):
        def alter_original(frozen, _manifest):
            (self.root / "image.tar.gz").write_bytes(b"replaced original")
        with patch.object(gate, "validate_online", side_effect=alter_original), \
                patch.object(promotion.Registry, "identity", return_value=REMOTE), \
                patch.object(promotion.Registry, "publish"), patch.object(promotion.Registry, "verify_pulled"):
            receipt = promotion.promote(self.args)
        self.assertEqual(receipt["archiveSha256"], self.manifest["artifacts"]["image.tar.gz"]["sha256"])
        self.args.receipt.unlink()
        self.root.rename(self.work / "old-bundle")
        fixture(self.root)
        def alter_frozen(frozen, _manifest):
            (frozen / "image.tar.gz").write_bytes(b"replaced frozen")
        with patch.object(gate, "validate_online", side_effect=alter_frozen), \
                patch.object(promotion.Registry, "publish") as docker, self.assertRaisesRegex(ValueError, "changed after validation"):
            promotion.promote(self.args)
        docker.assert_not_called()

    def test_existing_same_version_is_idempotent_without_push(self):
        with patch.object(gate, "validate_online") as online, patch.object(promotion.Registry, "identity", return_value=REMOTE), \
                patch.object(promotion.Registry, "publish") as docker, patch.object(promotion.Registry, "verify_pulled") as verify:
            receipt = promotion.promote(self.args)
        online.assert_called_once()
        self.assertEqual(receipt["result"], "already-present")
        self.assertEqual(receipt["evidenceTier"], "verified-candidate")
        docker.assert_not_called()
        verify.assert_called_once()
        with patch.object(promotion.Registry, "publish") as docker, self.assertRaisesRegex(ValueError, "must not already exist"):
            promotion.promote(self.args)
        docker.assert_not_called()

    def test_absent_version_uploads_original_archive(self):
        with patch.object(gate, "validate_online"), patch.object(promotion.Registry, "identity", side_effect=[None, None, REMOTE, REMOTE]), \
                patch.object(promotion.Registry, "publish", return_value=(REMOTE["registryDigest"], b"manifest")) as upload, \
                patch.object(promotion.Registry, "tag") as tag, patch.object(promotion.Registry, "verify_pulled") as verify:
            receipt = promotion.promote(self.args)
        self.assertEqual(receipt["result"], "published")
        upload.assert_called_once()
        self.assertEqual(upload.call_args.args[0].name, "image.tar.gz")
        tag.assert_called_once_with("v1.0.0", REMOTE["registryDigest"], b"manifest")
        verify.assert_called_once()

    def test_conflicting_version_and_failed_pull_produce_no_receipt(self):
        with patch.object(gate, "validate_online"), patch.object(promotion.Registry, "identity", side_effect=ValueError("different image config")), \
                patch.object(promotion.Registry, "publish") as docker, self.assertRaisesRegex(ValueError, "different image config"):
            promotion.promote(self.args)
        docker.assert_not_called()
        with patch.object(gate, "validate_online"), patch.object(promotion.Registry, "identity", return_value=REMOTE), \
                patch.object(promotion.Registry, "publish"), patch.object(promotion.Registry, "verify_pulled", side_effect=ValueError("layer differs")), \
                self.assertRaisesRegex(ValueError, "layer differs"):
            promotion.promote(self.args)
        self.assertFalse(self.args.receipt.exists())

    def test_changed_tag_and_receipt_inside_bundle_are_rejected(self):
        with patch.object(gate, "validate_online"), patch.object(promotion.Registry, "identity", side_effect=[REMOTE, REMOTE | {"registryDigest": "sha256:" + "2" * 64}]), \
                patch.object(promotion.Registry, "publish"), patch.object(promotion.Registry, "verify_pulled"), self.assertRaisesRegex(ValueError, "changed during promotion"):
            promotion.promote(self.args)
        self.args.receipt = self.root / "receipt.json"
        with self.assertRaisesRegex(ValueError, "outside the immutable"):
            promotion.promote(self.args)

    def test_registry_manifest_config_and_index_selection(self):
        registry = promotion.Registry("ghcr.io", "owner/repo")
        manifest = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "config": {"digest": self.image["configId"], "size": 100, "mediaType": "application/vnd.oci.image.config.v1+json"},
                    "layers": [{"digest": "sha256:" + "3" * 64, "size": 100, "mediaType": "application/vnd.oci.image.layer.v1.tar"}]}
        with patch.object(registry, "manifest", return_value=(REMOTE["registryDigest"], manifest)):
            self.assertEqual(registry.identity("v1.0.0", self.image, manifest), REMOTE)
        child = {"digest": REMOTE["platformManifestDigest"], "platform": {"os": "linux", "architecture": "amd64"}}
        index = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json", "manifests": [child]}
        with patch.object(registry, "manifest", side_effect=[(REMOTE["registryDigest"], index), (child["digest"], manifest)]):
            self.assertEqual(registry.identity("v1.0.0", self.image, manifest), REMOTE)
        for children in [[], [child, child], [child | {"platform": {"os": "linux", "architecture": "arm64"}}]]:
            with patch.object(registry, "manifest", return_value=(REMOTE["registryDigest"], index | {"manifests": children})), self.assertRaises(ValueError):
                registry.identity("v1.0.0", self.image, manifest)
        with patch.object(registry, "manifest", return_value=(REMOTE["registryDigest"], manifest | {"config": {"digest": "sha256:" + "4" * 64}})), self.assertRaisesRegex(ValueError, "different image config"):
            registry.identity("v1.0.0", self.image, manifest)
        with patch.object(registry, "manifest", return_value=(REMOTE["registryDigest"], manifest | {"layers": [{"digest": "sha256:" + "4" * 64, "size": 100}]})), self.assertRaisesRegex(ValueError, "layer bytes differ"):
            registry.identity("v1.0.0", self.image, manifest)

    def test_registry_http_absence_is_not_auth_or_network_failure(self):
        registry = promotion.Registry("127.0.0.1:5000", "owner/repo", True)
        for code, body, missing in [(404, b'{"errors":[{"code":"MANIFEST_UNKNOWN"}]}', True),
                                    (404, b"proxy not found", False), (403, b"denied", False), (500, b"failed", False)]:
            error = urllib.error.HTTPError("http://127.0.0.1:5000", code, "fixture", {}, io.BytesIO(body))
            with patch.object(registry.opener, "open", side_effect=error):
                if missing:
                    self.assertIsNone(registry.manifest("v1.0.0", missing=True))
                else:
                    with self.assertRaisesRegex(ValueError, "registry GET failed"):
                        registry.manifest("v1.0.0", missing=True)
            error.close()
        with patch.object(registry.opener, "open", side_effect=urllib.error.URLError("offline")), self.assertRaisesRegex(ValueError, "registry GET failed"):
            registry.manifest("v1.0.0", missing=True)
        with self.assertRaises(ValueError):
            registry.authenticate('Bearer realm="https://evil.invalid/token",service="ghcr.io",scope="repository:owner/repo:pull"')

    def test_registry_hash_mismatch_is_rejected(self):
        registry = promotion.Registry("ghcr.io", "owner/repo")
        response = io.BytesIO(b"{}")
        response.headers = {"Docker-Content-Digest": "sha256:" + "0" * 64}
        with patch.object(registry.opener, "open", return_value=response), self.assertRaisesRegex(ValueError, "digest differs"):
            registry.manifest("v1.0.0")

    def test_authentication_realm_and_upload_locations_do_not_leak_credentials(self):
        registry = promotion.Registry("ghcr.io", "owner/repo")
        challenge = 'Bearer scope="repository:owner/repo:pull",realm="https://ghcr.io/token",service="ghcr.io"'
        with patch.dict(os.environ, {"OXSUM_REGISTRY_USERNAME": "fixture", "OXSUM_REGISTRY_TOKEN": "secret"}), \
                patch.object(registry.opener, "open", return_value=response(b'{"token":"bearer-fixture"}')) as opened:
            registry.authenticate(challenge)
        request = opened.call_args.args[0]
        self.assertEqual(urllib.parse.urlsplit(request.full_url).netloc, "ghcr.io")
        self.assertIn("pull%2Cpush", request.full_url)
        self.assertEqual(registry.authorization, "Bearer bearer-fixture")
        for bad in [challenge.replace("https://ghcr.io/token", "https://evil.invalid/token"),
                    challenge.replace("owner/repo", "other/repo"), challenge.replace(":pull", ":delete")]:
            with patch.object(registry.opener, "open") as opened, self.assertRaises(ValueError):
                registry.authenticate(bad)
            opened.assert_not_called()
        for location in ["https://evil.invalid/v2/owner/repo/blobs/uploads/id", "http://ghcr.io/v2/owner/repo/blobs/uploads/id",
                         "https://ghcr.io/v2/other/repo/blobs/uploads/id", "https://ghcr.io/v2/owner/repo/blobs/uploads/id?digest=x"]:
            with patch.object(registry, "open", return_value=response(status=202, headers={"Location": location})) as opened, self.assertRaises(ValueError):
                registry.upload_blob(io.BytesIO(b"fixture"), 7, "sha256:" + "0" * 64)
            self.assertEqual(opened.call_count, 1)

    def test_blob_redirect_is_allowlisted_and_strips_authorization(self):
        registry = promotion.Registry("ghcr.io", "owner/repo")
        registry.authorization = "Bearer secret"
        source = "https://ghcr.io/v2/owner/repo/blobs/sha256:" + "0" * 64
        target = "https://pkg-containers.githubusercontent.com/fixture?sig=fixture"
        redirect = urllib.error.HTTPError(source, 307, "fixture", {"Location": target}, io.BytesIO())
        with patch.object(registry.opener, "open", side_effect=[redirect, response(b"fixture")]) as opened:
            registry.open("GET", source, blob_redirect=True).close()
        self.assertEqual(opened.call_args_list[0].args[0].get_header("Authorization"), "Bearer secret")
        self.assertIsNone(opened.call_args_list[1].args[0].get_header("Authorization"))
        for target in ["http://pkg-containers.githubusercontent.com/blob", "https://evil.invalid/blob",
                       "https://pkg-containers.githubusercontent.com.evil.invalid/blob"]:
            redirect = urllib.error.HTTPError(source, 307, "fixture", {"Location": target}, io.BytesIO())
            with patch.object(registry.opener, "open", side_effect=redirect) as opened, self.assertRaises(ValueError):
                registry.open("GET", source, blob_redirect=True)
            opened.assert_called_once()

    def test_download_checks_raw_blob_size_and_digest(self):
        registry = promotion.Registry("127.0.0.1:5000", "owner/repo", True)
        descriptor = {"digest": "sha256:" + hashlib.sha256(b"fixture").hexdigest(), "size": 7}
        for index, data in enumerate([b"altered", b"too long fixture", b"short"]):
            with patch.object(registry, "open", return_value=response(data)), self.assertRaisesRegex(ValueError, "blob"):
                registry.download_blob(descriptor, self.work / f"bad-{index}")

    def test_pulled_layer_checks_uncompressed_rootfs(self):
        registry = promotion.Registry("127.0.0.1:5000", "owner/repo", True)
        manifest = promotion.archive_manifest(self.root / "image.tar.gz")
        with tarfile.open(self.root / "image.tar.gz") as saved:
            entries = json.loads(saved.extractfile("manifest.json").read())[0]
            config = saved.extractfile(entries["Config"]).read()
        def download(descriptor, path):
            path.write_bytes(config if path.name == "pulled-config.json" else b"wrong rootfs")
        with patch.object(registry, "manifest", return_value=(REMOTE["registryDigest"], manifest)), \
                patch.object(registry, "download_blob", side_effect=download), self.assertRaisesRegex(ValueError, "rootfs"):
            registry.verify_pulled(REMOTE, self.work, self.image)

    def test_gzip_layer_keeps_original_blob_and_uncompressed_identity(self):
        compressed = self.work / "compressed"
        fixture(compressed, compressed=True)
        plain = promotion.archive_manifest(self.root / "image.tar.gz")
        packed = promotion.archive_manifest(compressed / "image.tar.gz")
        self.assertEqual(plain["config"], packed["config"])
        self.assertNotEqual(plain["layers"][0]["digest"], packed["layers"][0]["digest"])
        self.assertEqual(packed["layers"][0]["mediaType"], "application/vnd.oci.image.layer.v1.tar+gzip")


def local_registry_rehearsal(output):
    """Run only a task-owned loopback registry, clean it up, and retain evidence."""
    output = output.resolve()
    gate.require(not output.exists(), "rehearsal output must be a new directory")
    output.mkdir(parents=True)
    # Never pull implicitly: inspect the local image first, then let the caller
    # explicitly fetch this pinned small dependency if it is unavailable.
    subprocess.run(["docker", "image", "inspect", REGISTRY_IMAGE], check=True, stdout=subprocess.DEVNULL)
    name = "oxsum-promotion-" + uuid.uuid4().hex[:12]
    created = False
    try:
        subprocess.run(["docker", "run", "--detach", "--name", name, "--publish", "127.0.0.1::5000",
                        "--tmpfs", "/var/lib/registry", REGISTRY_IMAGE], check=True, stdout=subprocess.DEVNULL)
        created = True
        port = subprocess.check_output(["docker", "port", name, "5000/tcp"], text=True).strip()
        gate.require(re.fullmatch(r"127\.0\.0\.1:[0-9]+", port), "registry did not bind loopback")
        for _ in range(100):
            try:
                with urllib.request.urlopen("http://" + port + "/v2/", timeout=2) as response:
                    if response.status == 200:
                        break
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.1)
        else:
            raise ValueError("local registry never became ready")
        root = output / "bundle"
        manifest = fixture(root)
        target = f"{port}/fixtures/oxsum:v1.0.0"
        args = argparse.Namespace(directory=root, sha=SHA, repository="owner/repo", run="2", attempt=1,
            destination=target, receipt=output / "published.json", local_fixture=True)
        first = promotion.promote(args)
        args.receipt = output / "repeated.json"
        second = promotion.promote(args)
        gate.require(first["result"] == "published" and second["result"] == "already-present", "publication was not idempotent")
        gate.require(first["registryDigest"] == second["registryDigest"], "idempotent result changed digest")
        packed = output / "compressed-bundle"
        fixture(packed, compressed=True)
        args.directory = packed
        args.destination = f"{port}/fixtures/oxsum:v1.0.1"
        args.receipt = output / "compressed.json"
        compressed_receipt = promotion.promote(args)
        gate.require(compressed_receipt["configId"] == first["configId"] and compressed_receipt["registryDigest"] != first["registryDigest"],
                     "compressed layer did not preserve config with distinct original blob bytes")
        different = output / "different-bundle"
        fixture(different, b"conflicting fixture\n")
        args.directory = different
        args.destination = target
        args.receipt = output / "conflict.json"
        try:
            promotion.promote(args)
        except ValueError as error:
            gate.require("different image config" in str(error), "conflict failed for an unexpected reason")
        else:
            raise ValueError("conflicting version was not refused")
        summary = {"evidenceTier": "local-fixture", "registryImage": REGISTRY_IMAGE,
            "archiveSha256": manifest["artifacts"]["image.tar.gz"]["sha256"], "configId": manifest["image"]["configId"],
            "registryDigest": first["registryDigest"], "platformManifestDigest": first["platformManifestDigest"],
            "platform": "linux/amd64", "published": True, "pullAndLayerVerification": True,
            "idempotentRepeat": True, "conflictingVersionRefused": True,
            "originalGzipLayerPreserved": True,
            "publicPublication": False, "applicationExecution": False}
        (output / "rehearsal.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary, indent=2))
    finally:
        if created:
            subprocess.run(["docker", "rm", "--force", "--volumes", name], check=True, stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--local-registry", type=Path, help="run real loopback-registry rehearsal and retain output here")
    args, remaining = parser.parse_known_args()
    if args.local_registry:
        local_registry_rehearsal(args.local_registry)
    else:
        unittest.main(argv=[__file__, *remaining])
