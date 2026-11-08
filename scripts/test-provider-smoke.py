#!/usr/bin/env python3
"""Budget/process/crash/network boundaries; never contacts a real provider."""
import concurrent.futures
from datetime import datetime, timedelta, timezone
import hashlib
import http.client
import http.server
import importlib.util
import json
import multiprocessing
from pathlib import Path
import tempfile
import threading
import unittest
from unittest import mock

from provider_smoke import budget, relay

SPEC = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('bounded-provider-smoke.py'))
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


def reserve_process(path):
    try:
        budget.Budget(path).reserve('whole', 'run', 'a'*64)
        return True
    except ValueError:
        return False


class BudgetFixture:
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name)/'budget.json'
        smoke.fixture_journal(self.path)
        self.budget = budget.Budget(self.path)
        self.budget.claim('whole', 'run')

    def state(self):
        return json.loads(self.path.read_text())



class BudgetTest(BudgetFixture, unittest.TestCase):
    def test_concurrent_processes_cannot_repeat_a_scenario(self):
        with concurrent.futures.ProcessPoolExecutor(max_workers=4, mp_context=multiprocessing.get_context('spawn')) as pool:
            results = list(pool.map(reserve_process, [str(self.path)]*12))
        self.assertEqual(sum(results), 1)
        self.assertEqual(self.state()['reserved_micros'], budget.RESERVATION)
        self.assertEqual(len({a['id'] for a in self.state()['attempts']}), 1)

    def test_crash_reserved_attempt_and_scenario_cannot_be_repeated(self):
        attempt = self.budget.reserve('whole', 'run', 'a'*64)
        recovered = budget.Budget(self.path)
        with self.assertRaisesRegex(ValueError, 'claimed'):
            recovered.claim('whole', 'other')
        recovered.finish(attempt, 'ambiguous')
        self.assertEqual(self.state()['reserved_micros'], budget.RESERVATION)
        self.assertEqual(self.state()['actual_micros'], 0)
        with self.assertRaises(ValueError):
            recovered.finish(attempt, 'again')

    def test_observed_usage_does_not_restore_lifetime_allowance(self):
        attempt = self.budget.reserve('whole', 'run', 'a'*64)
        self.budget.finish(attempt, 'http_200', {'prompt_tokens': 23, 'completion_tokens': 11})
        self.assertEqual(self.state()['actual_micros'], 134)
        self.assertEqual(self.state()['reserved_micros'], budget.RESERVATION)
        self.budget.complete('whole', 'run')
        with self.assertRaises(ValueError):
            self.budget.reserve('whole', 'run', 'b'*64)

    def test_usage_outside_bound_persists_halt_without_new_attempt(self):
        attempt = self.budget.reserve('whole', 'run', 'a'*64)
        self.budget.finish(attempt, 'http_200', {'prompt_tokens': 23, 'completion_tokens': 33})
        self.assertIn('halted', self.state())
        with self.budget.locked(read_only=True) as retained:
            self.assertIn('halted', retained)
            self.assertEqual(retained['attempts'][0]['status'], 'bound_violation')
        with self.assertRaises(ValueError):
            self.budget.reserve('whole', 'run', 'b'*64)
        self.assertEqual(len(self.state()['attempts']), 1)

    def test_invalid_missing_expired_or_changed_pricing_fails_closed(self):
        original = self.state()
        for change in [{'reserved_micros': 1}, {'attempt_limit': 6}, {'ceiling_micros': 200000},
                       {'pricing_verified_at': (datetime.now(timezone.utc)-timedelta(days=2)).isoformat()},
                       {'input_bound_tokens': 100}, {'pricing': {}}]:
            self.path.write_text(json.dumps({**original, **change}))
            with self.assertRaises(ValueError):
                budget.Budget(self.path).reserve('whole', 'run', 'a'*64)
        self.path.unlink()
        with self.assertRaises(ValueError):
            self.budget.reserve('whole', 'run', 'a'*64)

    def test_existing_five_attempts_prevent_another_scenario(self):
        state = self.state()
        state['attempts'] = [{'id': str(i), 'scenario': 'prior-'+str(i), 'model': budget.MODEL, 'reserved_micros': budget.RESERVATION, 'actual_micros': None} for i in range(5)]
        state['reserved_micros'] = 5*budget.RESERVATION
        self.path.write_text(json.dumps(state))
        with self.assertRaisesRegex(ValueError, 'exhausted'):
            self.budget.claim('sse', 'run')

    def test_atomic_write_failure_cannot_return_permission_to_transmit(self):
        before = self.path.read_bytes()
        with mock.patch.object(budget.os, 'replace', side_effect=OSError('disk failure')):
            with self.assertRaises(OSError):
                self.budget.reserve('whole', 'run', 'a'*64)
        self.assertEqual(self.path.read_bytes(), before)


class RelayTest(BudgetFixture, unittest.TestCase):
    def setUp(self):
        super().setUp()
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), relay.Guard)
        self.server.budget, self.server.run_id, self.server.active_scenario = self.budget, 'run', 'whole'
        self.server.local_token, self.server.provider_key = 'fixture-local', 'private-provider-key'
        self.transmissions = []
        owner = self
        class Transport(smoke.FixtureTransport):
            def request(self, method, path, body, headers):
                # Independent disk read at the transmission boundary proves
                # reservation committed before credential-bearing I/O.
                state = json.loads(owner.path.read_text())
                owner.assertEqual(state['attempts'][-1]['status'], 'reserved')
                owner.assertEqual(state['attempts'][-1]['request_sha256'], hashlib.sha256(body).hexdigest())
                owner.assertEqual(headers['Authorization'], 'Bearer private-provider-key')
                owner.transmissions.append(body)
                super().request(method, path, body, headers)
        self.server.transport = Transport
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.close)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def send(self, body=None, path='/v1/chat/completions', token='fixture-local'):
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_port, timeout=3)
        raw = json.dumps(body or relay.payload(False)).encode()
        connection.request('POST', path, raw, {'Authorization': 'Bearer '+token, 'Content-Type': 'application/json'})
        response = connection.getresponse()
        result = response.status, response.read()
        connection.close()
        return result

    def test_each_repeated_transport_request_reserves_before_network(self):
        self.assertEqual([self.send()[0] for _ in range(6)], [200]+[400]*5)
        self.assertEqual(len(self.transmissions), 1)
        self.assertEqual(self.state()['actual_micros'], 134)

    def test_wrong_token_path_and_unbounded_body_never_reach_network(self):
        for body in [dict(relay.payload(False), max_tokens=33), dict(relay.payload(False), tools=[]),
                     dict(relay.payload(False), thinking={'type': 'enabled'}),
                     dict(relay.payload(False), messages=[{'role':'user','content':'different'}])]:
            self.assertEqual(self.send(body)[0], 400)
        self.assertEqual(self.send(path='/other')[0], 400)
        self.assertEqual(self.send(token='wrong')[0], 400)
        self.assertEqual(self.transmissions, [])
        self.assertEqual(self.state()['attempts'], [])

    def test_io_timeout_stays_reserved_and_never_auto_retries(self):
        class Failure:
            def request(self, *_args, **_kwargs):
                raise TimeoutError('response unknown')
            def close(self):
                pass
        self.server.transport = Failure
        self.assertEqual(self.send()[0], 400)
        self.assertEqual(self.state()['attempts'][0]['status'], 'ambiguous')
        self.assertEqual(self.state()['reserved_micros'], budget.RESERVATION)

    def test_directory_sync_failure_preserves_reservation_without_transmission(self):
        original = budget.os.fsync
        calls = 0
        def fail_directory(descriptor):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError('directory fsync failed after replace')
            original(descriptor)
        with mock.patch.object(budget.os, 'fsync', side_effect=fail_directory):
            self.assertEqual(self.send()[0], 400)
        self.assertEqual(self.transmissions, [])
        self.assertEqual(self.state()['attempts'][0]['status'], 'reserved')
        self.assertEqual(self.send()[0], 400)
        self.assertEqual(self.transmissions, [])

    def test_failed_reservation_write_prevents_transmission(self):
        with mock.patch.object(budget.os, 'replace', side_effect=OSError('disk failure')):
            self.assertEqual(self.send()[0], 400)
        self.assertEqual(self.transmissions, [])
        self.assertEqual(self.state()['attempts'], [])


class InputTest(unittest.TestCase):
    def test_public_ip_policy_rejects_fake_private_metadata_and_transitions(self):
        for value in ['198.18.0.237','127.0.0.1','169.254.169.254','10.0.0.1','224.0.0.1','::ffff:3.173.21.63','2002::1']:
            with self.assertRaises(ValueError):
                relay.public_address(value)
        self.assertEqual(relay.public_address('3.173.21.63'), '3.173.21.63')

    def test_duplicate_fields_and_unexpected_parameters_refuse(self):
        with self.assertRaises(ValueError):
            relay.validate_payload(b'{"stream":false,"stream":true}')
        with self.assertRaises(ValueError):
            relay.validate_payload(json.dumps(dict(relay.payload(False), max_tokens=32.0)).encode())
        self.assertEqual(relay.validate_payload(json.dumps(relay.payload(True)).encode()), relay.payload(True))

    def test_credential_loader_reads_only_named_field_and_requires_private_file(self):
        with tempfile.TemporaryDirectory() as private:
            path = Path(private)/'.env'
            path.write_text('DATABASE_URL=not-imported\nOXSUM_E2E_UPSTREAM_API_KEY="private-test-token"\n')
            path.chmod(0o600)
            self.assertEqual(smoke.provider_credential(path), 'private-test-token')
            path.chmod(0o644)
            with self.assertRaises(ValueError):
                smoke.provider_credential(path)


if __name__ == '__main__':
    unittest.main()
