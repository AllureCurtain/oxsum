#!/usr/bin/env python3
"""Negative fixtures for release acceptance; no GitHub credentials required."""
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("candidate_ci", Path(__file__).with_name("check-candidate-ci.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
SHA = "a" * 40


class CandidateCiTests(unittest.TestCase):
    def setUp(self):
        self.run = dict(head_sha=SHA, head_branch="main", event="push", status="completed", conclusion="success")
        self.jobs = [dict(name=name, status="completed", conclusion="success",
                          steps=[dict(name=step, conclusion="success") for step in gate.REQUIRED_STEPS[name]])
                     for name in sorted(gate.REQUIRED_JOBS)]
        self.extras = next(j for j in self.jobs if j["name"] == "test-extras")
        self.extras["steps"] = [dict(name=name, conclusion="success")
                                for name in (gate.ENGINE_STEP, gate.SHARED_STEP)]

    def test_executed_full_run_passes(self):
        self.assertIsNone(gate.validate(self.run, self.jobs, SHA))

    def test_wrong_sha_branch_or_event_fails(self):
        for key, value in (("head_sha", "b" * 40), ("head_branch", "feature"), ("event", "pull_request")):
            with self.subTest(key=key):
                self.assertIsNotNone(gate.validate(self.run | {key: value}, self.jobs, SHA))

    def test_failed_or_running_run_fails(self):
        for change in ({"conclusion": "failure"}, {"status": "in_progress"}):
            self.assertIsNotNone(gate.validate(self.run | change, self.jobs, SHA))

    def test_missing_failed_or_skipped_shard_fails(self):
        for conclusion in ("failure", "skipped", "cancelled", None):
            jobs = copy.deepcopy(self.jobs)
            next(j for j in jobs if j["name"] == "test-shard (2/3)")["conclusion"] = conclusion
            self.assertIsNotNone(gate.validate(self.run, jobs, SHA))
        self.assertIsNotNone(gate.validate(self.run, [j for j in self.jobs if j["name"] != "test-shard (2/3)"], SHA))

    def test_green_extras_cannot_hide_skipped_conformance(self):
        self.extras["steps"][0]["conclusion"] = "skipped"
        self.assertIsNotNone(gate.validate(self.run, self.jobs, SHA))

    def test_green_job_with_skipped_build_or_tests_fails(self):
        for job_name in gate.REQUIRED_STEPS:
            with self.subTest(job=job_name):
                jobs = copy.deepcopy(self.jobs)
                next(j for j in jobs if j["name"] == job_name)["steps"][0]["conclusion"] = "skipped"
                self.assertIsNotNone(gate.validate(self.run, jobs, SHA))

    def test_missing_or_failed_shared_suites_fail(self):
        self.extras["steps"][1]["conclusion"] = "failure"
        self.assertIsNotNone(gate.validate(self.run, self.jobs, SHA))
        self.extras["steps"] = []
        self.assertIsNotNone(gate.validate(self.run, self.jobs, SHA))


if __name__ == "__main__":
    unittest.main()
