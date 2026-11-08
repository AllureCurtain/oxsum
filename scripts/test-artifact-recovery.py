#!/usr/bin/env python3
"""Refusal/identity checks for the isolated real-artifact rehearsal runner."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC=importlib.util.spec_from_file_location("recovery",Path(__file__).with_name("rehearse-artifact-recovery.py"))
recovery=importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(recovery)


class RecoveryTest(unittest.TestCase):
    def test_inputs_are_frozen_and_hash_bound_before_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            source=root/"source";source.write_bytes(b"fixture-binary")
            sha="a"*40
            digest=recovery.backup.digest(source)
            frozen=recovery.freeze_artifact(source,sha,digest,"0110_fixture","0.1.0",root/"frozen")
            source.write_bytes(b"changed-after-freeze")
            self.assertEqual(frozen["path"].read_bytes(),b"fixture-binary")
            self.assertEqual(frozen["path"].stat().st_mode&0o777,0o700)
            with self.assertRaises(ValueError):
                recovery.freeze_artifact(source,sha,digest,"0110_fixture","0.1.0",root/"mismatch")
            link=root/"link";link.symlink_to(source)
            with self.assertRaises(ValueError):
                recovery.freeze_artifact(link,sha,digest,"0110_fixture","0.1.0",root/"linked")

    def test_environment_cannot_borrow_existing_database_provider_or_node_settings(self):
        with mock.patch.dict(os.environ,{"DATABASE_URL":"private-database","OXSUM_ADMIN_TOKEN":"private-token","PGPASSWORD":"private-password",
                                         "NODE_OPTIONS":"private-preload","NPM_CONFIG_REGISTRY":"private-registry","HTTPS_PROXY":"private-proxy"}):
            clean=recovery.clean_environment()
        for name in ["DATABASE_URL","OXSUM_ADMIN_TOKEN","PGPASSWORD","NODE_OPTIONS","NPM_CONFIG_REGISTRY","HTTPS_PROXY"]:
            self.assertNotIn(name,clean)

    def test_remote_runtime_targets_refuse_before_http(self):
        for base in ["http://localhost:3000","https://real-host","http://127.0.0.1:3000/path","http://user:secret@127.0.0.1:3000"]:
            with self.assertRaises(ValueError):
                recovery.request(base,"/healthz")

    def test_runtime_identity_mismatch_is_fatal_and_not_a_wait_loop(self):
        class Process:
            def poll(self):return None
        artifact={"path":"/fixture/oxsum","sourceSha":"a"*40,"schemaVersion":"0111_fixture","version":"0.1.0"}
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            runtime=recovery.Runtime(artifact,{},root,root/"log")
            with mock.patch.object(recovery.subprocess,"Popen",return_value=Process()), \
                 mock.patch.object(recovery,"api",return_value={"version":"0.1.0","gitSha":"b"*40,"schemaVersion":"0111_fixture"}), \
                 mock.patch.object(recovery,"request",return_value=(b"ready (0111_fixture)",{})):
                with self.assertRaisesRegex(ValueError,"identity"):
                    runtime.start()

    def test_nonconserving_settlement_cannot_be_reported_as_a_pass(self):
        customer={"customerSession":"fixture"}
        row={"requestId":"request","kind":"usage","chargedMinor":34,"upstreamAttempts":1}
        with mock.patch.object(recovery,"api",side_effect=[{"rows":[row]},{"availableMinor":recovery.FUNDING-34,"reservedMinor":1}]):
            with self.assertRaisesRegex(ValueError,"conservation"):
                recovery.settlement("http://127.0.0.1:1",customer,"request",1)


if __name__=="__main__":unittest.main()
