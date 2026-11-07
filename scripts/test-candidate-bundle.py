#!/usr/bin/env python3
"""Candidate provenance and byte-identity negative tests; no external calls."""
import argparse
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location("candidate_bundle", Path(__file__).with_name("candidate_bundle.py"))
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)
SHA = "a" * 40


def archive(path, files):
    with tarfile.open(path, "w:gz") as tar:
        for name, value in files.items():
            blob = value if isinstance(value, bytes) else json.dumps(value).encode()
            info = tarfile.TarInfo(name)
            info.size = len(blob)
            tar.addfile(info, io.BytesIO(blob))


class CandidateBundleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        layer = b"fixture layer bytes"
        self.image = {
            "config.json": {"os": "linux", "architecture": "amd64",
                "config": {"Labels": {"org.opencontainers.image.revision": SHA}},
                "rootfs": {"diff_ids": ["sha256:" + hashlib.sha256(layer).hexdigest()]}},
            "manifest.json": [{"Config": "config.json", "Layers": ["layer.tar"]}], "layer.tar": layer,
        }
        archive(self.root / "image.tar.gz", self.image)
        archive(self.root / "sdk.tgz", {
            "package/package.json": {"name": "verify-oxsum-bill", "version": "0.1.0"},
            "package/dist/index.js": b"export {}", "package/dist/index.d.ts": b"export {}",
            "package/wasm/verify_wasm.js": b"export {}", "package/wasm/verify_wasm_bg.wasm": b"\x00asm" + b"\x00" * 1500,
        })
        self.ci = {"sha": SHA, "run_id": 1, "attempt": 1,
            "run_url": "https://github.com/owner/repo/actions/runs/1", "conclusion": "success",
            "jobs": dict.fromkeys(gate.CI.REQUIRED_JOBS, "success"),
            "postgres_conformance": "success", "process_shared": "success",
            "executed_steps": {name: sorted(steps) for name, steps in gate.CI.REQUIRED_STEPS.items()}}
        (self.root / "ci.json").write_text(json.dumps(self.ci))
        for name in ["smoke.log", "consumer.log", "RELEASE-NOTES-DRAFT.md"]:
            (self.root / name).write_text("fixture evidence")
        self.args = argparse.Namespace(directory=self.root, sha=SHA, workflow_sha=SHA, repository="owner/repo",
            run="2", attempt=1, image="image.tar.gz", sdk="sdk.tgz", full_ci="ci.json",
            smoke_log="smoke.log", consumer_log="consumer.log")
        gate.create(self.args)
        self.manifest = json.loads((self.root / "candidate-manifest.json").read_text())

    def edit_manifest(self, change):
        value = copy.deepcopy(self.manifest)
        change(value)
        (self.root / "candidate-manifest.json").write_text(json.dumps(value))
        gate.write_sums(self.root)

    def reject(self):
        with self.assertRaises((ValueError, KeyError, tarfile.TarError)):
            gate.validate(self.root, SHA)

    def test_complete_bundle_passes(self):
        self.assertEqual(gate.validate(self.root, SHA), self.manifest)

    def test_wrong_source_and_old_format_rejected(self):
        for changes in [{"sourceSha": "b" * 40}, {"formatVersion": 1}]:
            self.edit_manifest(lambda m: m.update(changes))
            self.reject()

    def test_missing_failed_skipped_gate_rejected(self):
        for value in ["failed", "skipped", "pending", None]:
            self.edit_manifest(lambda m: m["gates"].update(frontend=value))
            self.reject()

    def test_missing_changed_or_unlisted_artifact_rejected(self):
        (self.root / "consumer.log").write_text("changed")
        self.reject()
        (self.root / "consumer.log").unlink()
        self.reject()

    def test_extra_artifact_and_incomplete_checksums_rejected(self):
        (self.root / "unlisted.json").write_text("{}")
        self.reject()
        (self.root / "unlisted.json").unlink()
        (self.root / "SHA256SUMS").write_text("")
        self.reject()

    def test_config_id_or_sdk_identity_cannot_be_substituted(self):
        for key, field, value in [("image", "configId", "sha256:" + "0" * 64),
                                  ("image", "platform", "linux/arm64"), ("sdk", "integrity", "sha512-invalid")]:
            self.edit_manifest(lambda m: m[key].update({field: value}))
            self.reject()

    def test_image_wrong_revision_platform_or_layer_rejected(self):
        for modification in ["revision", "platform", "layer"]:
            image = copy.deepcopy(self.image)
            if modification == "revision":
                image["config.json"]["config"]["Labels"]["org.opencontainers.image.revision"] = "b" * 40
            elif modification == "platform":
                image["config.json"]["architecture"] = "arm64"
            else:
                image["layer.tar"] = b"altered"
            archive(self.root / "bad-image.tar.gz", image)
            with self.assertRaises(ValueError):
                gate.image_identity(self.root / "bad-image.tar.gz", SHA)

    def test_unsafe_archive_path_rejected(self):
        for name in ["../escape", "/absolute", "a/../../escape", "a\\escape"]:
            archive(self.root / "unsafe.tgz", {name: b"bad"})
            with self.assertRaises(ValueError):
                gate.sdk_identity(self.root / "unsafe.tgz")

    def test_unsafe_zip_path_rejected(self):
        with zipfile.ZipFile(self.root / "unsafe.zip", "w") as archive:
            archive.writestr("../escape", "bad")
        with self.assertRaises(ValueError):
            gate.extract_zip(self.root / "unsafe.zip", self.root / "extracted")

    def test_symlink_artifact_rejected(self):
        (self.root / "link").symlink_to(self.root / "ci.json")
        self.reject()

    def test_full_ci_record_requires_executed_builds(self):
        for field in ["sha", "executed_steps", "jobs", "postgres_conformance"]:
            ci = copy.deepcopy(self.ci)
            ci.pop(field)
            with self.assertRaises(ValueError):
                gate.validate_ci_record(ci, SHA, "owner/repo")

    def test_remote_workflow_failed_skipped_wrong_identity_rejected(self):
        run = {"id": 2, "workflow_id": 123, "path": ".github/workflows/release-candidate.yml",
               "head_sha": SHA, "run_attempt": 1, "event": "workflow_dispatch",
               "status": "completed", "conclusion": "success"}
        jobs = [dict(name=name, status="completed", conclusion="success") for name in gate.CANDIDATE_JOBS]
        gate.check_run(run, jobs, self.manifest, 123)
        for changes in [{"head_sha": "b" * 40}, {"workflow_id": 456}, {"conclusion": "failure"}, {"run_attempt": 2}]:
            with self.assertRaises(ValueError):
                gate.check_run(run | changes, jobs, self.manifest, 123)
        jobs[0]["conclusion"] = "skipped"
        with self.assertRaises(ValueError):
            gate.check_run(run, jobs, self.manifest, 123)

    def test_remote_success_cannot_attest_a_different_local_manifest(self):
        altered = copy.deepcopy(self.manifest)
        altered["workflow"]["workflowSha"] = "b" * 40
        def write_zip(command, stdout, **_kwargs):
            with zipfile.ZipFile(stdout, "w") as saved:
                for path in self.root.rglob("*"):
                    if path.is_file():
                        saved.write(path, path.relative_to(self.root).as_posix())
        with patch.object(gate.CI, "api", return_value={"id": 123}), \
                patch.object(gate, "check_run"), \
                patch.object(gate.CI, "pages", side_effect=[[], [{"id": 9, "name": "candidate-bundle", "expired": False}]]), \
                patch.object(gate.subprocess, "run", side_effect=write_zip):
            with self.assertRaisesRegex(ValueError, "local manifest differs"):
                gate.validate_online(self.root, altered)

    def test_online_provenance_rechecks_download_ci_and_merged_sources(self):
        def write_zip(command, stdout, **_kwargs):
            with zipfile.ZipFile(stdout, "w") as saved:
                for path in self.root.rglob("*"):
                    if path.is_file():
                        saved.write(path, path.relative_to(self.root).as_posix())
        def api(path):
            if "/compare/" in path:
                return {"status": "ahead"}
            if path.endswith("workflows/ci.yml"):
                return {"id": 88}
            if path.endswith("/runs/1/attempts/1"):
                return {"workflow_id": 88, "head_sha": SHA, "head_branch": "main",
                        "event": "workflow_dispatch", "status": "completed", "conclusion": "success"}
            return {"id": 123}
        ci_jobs = [dict(name=name, status="completed", conclusion="success",
                       steps=[dict(name=step, conclusion="success") for step in gate.CI.REQUIRED_STEPS[name]])
                   for name in gate.CI.REQUIRED_JOBS]
        with patch.object(gate.CI, "api", side_effect=api), patch.object(gate, "check_run"), \
                patch.object(gate.CI, "pages", side_effect=[[], [{"id": 9, "name": "candidate-bundle", "expired": False}], ci_jobs]), \
                patch.object(gate.subprocess, "run", side_effect=write_zip):
            gate.validate_online(self.root, self.manifest)

    def test_missing_or_expired_remote_bundle_rejected_before_download(self):
        for artifacts in [[], [{"name": "candidate-bundle", "expired": True}]]:
            with patch.object(gate.CI, "api", return_value={"id": 123}), \
                    patch.object(gate, "check_run"), patch.object(gate.CI, "pages", side_effect=[[], artifacts]):
                with self.assertRaisesRegex(ValueError, "unavailable or expired"):
                    gate.validate_online(self.root, self.manifest)


if __name__ == "__main__":
    unittest.main()
