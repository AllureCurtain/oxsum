#!/usr/bin/env python3
"""Digest deployment refusal and state-transition tests; no live host or publication."""
import argparse
import copy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import patch

import candidate_bundle as gate
import deployment_plan


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


deploy = load("deploy", "deploy-candidate.py")
fixtures = load("promotion_fixtures", "test-candidate-promotion.py")
SHA = fixtures.SHA
REF = "ghcr.io/allurecurtain/oxsum@sha256:" + "1" * 64
RUNTIME = {"gitSha": SHA, "version": "0.1.0", "schemaVersion": "0110_fixture"}


class DeploymentTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name)
        self.root = self.work / "bundle"
        self.manifest = fixtures.fixture(self.root)
        (self.root / "runtime.json").write_text(json.dumps({"data": RUNTIME}))
        self.manifest["repository"] = "AllureCurtain/oxsum"
        self.manifest["workflow"]["url"] = "https://github.com/AllureCurtain/oxsum/actions/runs/2"
        self.manifest["evidence"]["runtimeVersion"] = "runtime.json"
        ci = json.loads((self.root / "ci.json").read_text())
        ci["run_url"] = "https://github.com/AllureCurtain/oxsum/actions/runs/1"
        (self.root / "ci.json").write_text(json.dumps(ci))
        self.write_manifest()
        self.plan = {"formatVersion": 1, "evidenceTier": "verified-candidate", "sourceSha": SHA,
            "repository": "AllureCurtain/oxsum", "workflow": self.manifest["workflow"],
            "candidateManifestSha256": gate.digest(self.root / "candidate-manifest.json"),
            "archiveSha256": self.manifest["artifacts"]["image.tar.gz"]["sha256"],
            "image": {"reference": REF, "configId": self.manifest["image"]["configId"], "platform": "linux/amd64",
                "registryDigest": "sha256:" + "1" * 64, "platformManifestDigest": "sha256:" + "1" * 64},
            "runtime": RUNTIME, "preparedAt": "2026-11-08T00:00:00Z"}
        self.plan_path = self.work / "plan.json"
        self.plan_path.write_text(json.dumps(self.plan))
        self.env_file = self.work / "operator.env"
        self.env_file.write_text("DATABASE_URL=fixture-only\n")
        self.args = argparse.Namespace(operation="deploy", plan=self.plan_path, env_file=self.env_file,
            state_directory=self.work / "state", receipt=self.work / "receipt.json", container_name="fixture-oxsum", port=3333)

    def write_manifest(self):
        self.manifest["artifacts"] = gate.file_map(self.root)
        (self.root / "candidate-manifest.json").write_text(json.dumps(self.manifest))
        gate.write_sums(self.root)

    def receipt(self, plan=None):
        return {"formatVersion": 1, "evidenceTier": "verified-candidate", "result": "success", "plan": plan or self.plan,
                "containerName": self.args.container_name, "port": self.args.port,
                "operatorEnvSha256": gate.digest(self.env_file)}

    def container(self, plan=None, running=True):
        plan = plan or self.plan
        return {"Config": {"Image": plan["image"]["reference"], "Labels": {deploy.MANAGED: "1"}}, "State": {"Running": running}}

    def seed(self, current=None, previous=None):
        self.args.state_directory.mkdir(mode=0o700)
        for filename, value in [("active.json", current), ("previous.json", previous)]:
            if value:
                (self.args.state_directory / filename).write_text(json.dumps(value))

    def fake_docker(self, *args, **kwargs):
        if args[:2] == ("image", "save"):
            shutil.copyfile(self.root / "image.tar.gz", args[args.index("--output") + 1])
        return ""

    def test_plan_binds_candidate_runtime_and_registry_without_write(self):
        args = argparse.Namespace(directory=self.root, sha=SHA, run="2", attempt=1, image_ref=REF, output=self.work / "prepared.json")
        with patch.object(gate, "validate_online") as online, \
                patch.object(deployment_plan.promotion.Registry, "identity", return_value=fixtures.REMOTE), \
                patch.object(deployment_plan.promotion.Registry, "verify_pulled") as verify, \
                patch.object(deployment_plan.promotion.Registry, "publish") as publish:
            plan = deployment_plan.prepare(args)
        online.assert_called_once()
        verify.assert_called_once()
        publish.assert_not_called()
        self.assertEqual(plan["runtime"], RUNTIME)
        self.assertEqual(plan["image"]["reference"], REF)

    def test_plan_rejects_missing_runtime_and_failed_provenance(self):
        args = argparse.Namespace(directory=self.root, sha=SHA, run="2", attempt=1, image_ref=REF, output=self.work / "prepared.json")
        with patch.object(gate, "validate_online", side_effect=ValueError("expired candidate")), \
                patch.object(deployment_plan.promotion.Registry, "identity") as identity, self.assertRaisesRegex(ValueError, "expired"):
            deployment_plan.prepare(args)
        identity.assert_not_called()
        del self.manifest["evidence"]["runtimeVersion"]
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "lacks observed runtime"):
            deployment_plan.prepare(args)

    def test_candidate_runtime_source_schema_and_version_refusals(self):
        for field, value in [("gitSha", "b" * 40), ("schemaVersion", "unknown"), ("version", "unversioned")]:
            (self.root / "runtime.json").write_text(json.dumps({"data": RUNTIME | {field: value}}))
            self.write_manifest()
            with self.assertRaises(ValueError):
                gate.validate(self.root, SHA)

    def test_mutable_wrong_platform_and_fixture_plans_refused(self):
        for field, value in [("reference", "ghcr.io/allurecurtain/oxsum:latest"), ("platform", "linux/arm64"),
                             ("reference", REF.replace("allurecurtain", "other")), ("registryDigest", "sha256:" + "2" * 64)]:
            plan = copy.deepcopy(self.plan)
            plan["image"][field] = value
            with self.assertRaises(ValueError):
                deployment_plan.validate_plan(plan)
        with self.assertRaisesRegex(ValueError, "verified-candidate"):
            deployment_plan.validate_plan(self.plan | {"evidenceTier": "local-fixture"})

    def test_first_deploy_uses_one_digest_and_retains_success(self):
        with patch.object(deploy, "docker", side_effect=self.fake_docker) as docker, \
                patch.object(deploy, "inspect_container", side_effect=[None, self.container()]), \
                patch.object(deploy, "runtime", return_value=RUNTIME):
            receipt = deploy.apply(self.args)
        commands = [call.args for call in docker.call_args_list]
        self.assertIn(("image", "pull", "--platform", "linux/amd64", REF), commands)
        runs = [command for command in commands if command[0] == "run"]
        self.assertEqual(len(runs), 3)
        self.assertTrue(all(REF in command for command in runs))
        self.assertEqual(runs[0][-1], "--check-config")
        self.assertEqual(runs[1][-1], "--migrate-only")
        self.assertEqual(receipt["runtime"], RUNTIME)
        self.assertEqual(json.loads((self.args.state_directory / "active.json").read_text()), receipt)
        self.assertFalse((self.args.state_directory / "previous.json").exists())

    def test_unmanaged_container_refused_before_pull(self):
        with patch.object(deploy, "inspect_container", return_value=self.container()), patch.object(deploy, "docker") as docker, \
                self.assertRaisesRegex(ValueError, "unmanaged"):
            deploy.apply(self.args)
        docker.assert_not_called()

    def test_wrong_pulled_image_refused_before_stopping_current(self):
        self.seed(self.receipt())
        with patch.object(deploy, "inspect_container", return_value=self.container()), patch.object(deploy, "runtime", return_value=RUNTIME), \
                patch.object(deploy, "docker", side_effect=self.fake_docker) as docker, \
                patch.object(gate, "image_identity", return_value={"sourceSha": SHA, "configId": "wrong", "platform": "linux/amd64"}), \
                self.assertRaisesRegex(ValueError, "pulled image differs"):
            deploy.apply(self.args)
        self.assertFalse(any(call.args[:2] == ("container", "stop") for call in docker.call_args_list))

    def test_migration_failure_keeps_previous_state_without_restart(self):
        self.seed(self.receipt())
        def command(*args, **kwargs):
            if args[-1] == "--migrate-only":
                raise ValueError("migration failed")
            return self.fake_docker(*args, **kwargs)
        with patch.object(deploy, "inspect_container", return_value=self.container()), patch.object(deploy, "runtime", return_value=RUNTIME), \
                patch.object(deploy, "docker", side_effect=command) as docker, self.assertRaisesRegex(ValueError, "migration failed"):
            deploy.apply(self.args)
        self.assertFalse(self.args.receipt.exists())
        self.assertEqual(json.loads((self.args.state_directory / "active.json").read_text()), self.receipt())
        self.assertFalse(any(call.args[:2] == ("run", "--detach") for call in docker.call_args_list))

    def test_start_or_readiness_failure_never_reports_success(self):
        for failure in ["start", "readiness"]:
            with self.subTest(failure=failure):
                def command(*args, **kwargs):
                    if failure == "start" and args[:2] == ("run", "--detach"):
                        raise ValueError("start failed")
                    return self.fake_docker(*args, **kwargs)
                with patch.object(deploy, "inspect_container", return_value=None), \
                        patch.object(deploy, "docker", side_effect=command), \
                        patch.object(deploy, "runtime", side_effect=ValueError("readiness failed")), self.assertRaises(ValueError):
                    deploy.apply(self.args)
                self.assertFalse(self.args.receipt.exists())
                self.assertFalse((self.args.state_directory / "active.json").exists())

    def test_same_schema_rollback_requires_retained_artifact_and_compatibility(self):
        args = argparse.Namespace(operation="rollback", image_ref=REF, source_sha=SHA, confirm_compatible_rollback=True)
        self.assertEqual(deploy.choose_plan(args, self.receipt(), self.receipt()), self.plan)
        args.confirm_compatible_rollback = False
        with self.assertRaisesRegex(ValueError, "compatibility"):
            deploy.choose_plan(args, self.receipt(), self.receipt())
        args.confirm_compatible_rollback = True
        previous = copy.deepcopy(self.plan)
        previous["runtime"]["schemaVersion"] = "0109_prior"
        with self.assertRaisesRegex(ValueError, "rollback schema differs"):
            deploy.choose_plan(args, self.receipt(), self.receipt(previous))
        args.image_ref = REF.replace("1" * 64, "2" * 64)
        with self.assertRaisesRegex(ValueError, "selectors differ"):
            deploy.choose_plan(args, self.receipt(), self.receipt())

    def test_rollback_rejects_changed_operator_environment(self):
        self.seed(self.receipt(), self.receipt())
        self.env_file.write_text("DATABASE_URL=different\n")
        self.args.operation = "rollback"
        with patch.object(deploy, "inspect_container", return_value=self.container()), patch.object(deploy, "runtime", return_value=RUNTIME), \
                patch.object(deploy, "docker") as docker, self.assertRaisesRegex(ValueError, "environment changed"):
            deploy.apply(self.args)
        docker.assert_not_called()

    def test_rollback_rejects_different_previous_environment(self):
        self.seed(self.receipt(), self.receipt() | {"operatorEnvSha256": "0" * 64})
        self.args.operation = "rollback"
        with patch.object(deploy, "inspect_container", return_value=self.container()), patch.object(deploy, "runtime", return_value=RUNTIME), \
                patch.object(deploy, "docker") as docker, self.assertRaisesRegex(ValueError, "environment changed"):
            deploy.apply(self.args)
        docker.assert_not_called()

    def test_loopback_runtime_and_retained_same_schema_rollback(self):
        # HTTP is real. Docker is a controlled state model; this does not claim
        # an oxsum image ran or prove a genuine candidate/registry deployment.
        observed = {"container": None, "runtime": RUNTIME}
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                body = ("ready (" + observed["runtime"]["schemaVersion"] + ")").encode() if self.path == "/readyz" else json.dumps({"data": observed["runtime"]}).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            def log_message(self, *_args):
                pass
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.args.port = server.server_address[1]
        second = copy.deepcopy(self.plan)
        second_ref = REF.replace("1" * 64, "2" * 64)
        second["image"].update(reference=second_ref, registryDigest="sha256:" + "2" * 64, platformManifestDigest="sha256:" + "2" * 64)
        selected = {REF: self.plan, second_ref: second}
        commands = []
        def command(*args, **kwargs):
            commands.append(args)
            if args[:2] == ("run", "--detach"):
                plan = selected[args[-1]]
                observed.update(container=self.container(plan), runtime=plan["runtime"])
            if args[:2] == ("container", "stop"):
                observed["container"]["State"]["Running"] = False
            if args[:2] == ("container", "rm"):
                observed["container"] = None
            return self.fake_docker(*args, **kwargs)
        with patch.object(deploy, "docker", side_effect=command), \
                patch.object(deploy, "inspect_container", side_effect=lambda _name: copy.deepcopy(observed["container"])):
            first = deploy.apply(self.args)
            self.plan_path.write_text(json.dumps(second))
            self.args.receipt = self.work / "second.json"
            deploy.apply(self.args)
            self.args.operation = "rollback"
            self.args.image_ref = REF
            self.args.source_sha = SHA
            self.args.confirm_compatible_rollback = True
            self.args.receipt = self.work / "rollback.json"
            rolled_back = deploy.apply(self.args)
        self.assertEqual(rolled_back["plan"]["image"]["reference"], first["plan"]["image"]["reference"])
        self.assertEqual(rolled_back["previousImageReference"], second_ref)
        self.assertEqual([row[-1] for row in commands if row[:2] == ("image", "pull")], [REF, second_ref, REF])
        self.assertEqual([row[-2] for row in commands if row[-1] == "--migrate-only"], [REF, second_ref, REF])

    def test_version_and_readiness_match_all_fields(self):
        for values in [["ready (0109_other)", json.dumps({"data": RUNTIME})],
                       ["ready (0110_fixture)", json.dumps({"data": RUNTIME | {"gitSha": "b" * 40}})],
                       ["ready (0110_fixture)", json.dumps({"data": RUNTIME | {"version": "0.2.0"}})]]:
            with patch.object(deploy, "json_get", side_effect=values), self.assertRaises(ValueError):
                deploy.runtime(3333, RUNTIME)

    def test_read_only_registry_client_cannot_publish(self):
        registry = deployment_plan.promotion.Registry("ghcr.io", "allurecurtain/oxsum", write=False)
        with self.assertRaisesRegex(ValueError, "read-only"):
            registry.publish(None, None)
        with self.assertRaisesRegex(ValueError, "read-only"):
            registry.tag(None, None, None)

    def test_compose_requires_and_shares_explicit_reference(self):
        source = Path(__file__).resolve().parent.parent / "deploy/docker-compose.core.yml"
        compose_root = self.work / "compose"
        compose_root.mkdir()
        shutil.copyfile(source, compose_root / "compose.yml")
        (compose_root / ".env").write_text("POSTGRES_PASSWORD=fixture\nOXSUM_IMAGE_REF=" + REF + "\n")
        command = ["docker", "compose", "--project-directory", str(compose_root), "--env-file", str(compose_root / ".env"),
                   "-f", str(compose_root / "compose.yml"), "config", "--format", "json"]
        # Compose config renders locally and never contacts the Docker daemon.
        value = json.loads(subprocess.check_output(command, text=True))
        self.assertEqual(value["services"]["oxsum"]["image"], REF)
        self.assertEqual(value["services"]["oxsum-migrate"]["image"], REF)
        (compose_root / ".env").write_text("POSTGRES_PASSWORD=fixture\n")
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("OXSUM_IMAGE_REF", result.stderr)


if __name__ == "__main__":
    unittest.main()
