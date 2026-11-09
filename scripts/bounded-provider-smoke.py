#!/usr/bin/env python3
"""Two bounded whole/SSE provider calls through an isolated real oxsum listener.

Requires explicit artifacts and a pre-existing run-wide allowance. No build,
implicit network retry, ordinary database reset, or root environment import.
"""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from urllib.parse import urlparse

from provider_smoke.budget import Budget, MODEL, INPUT_BOUND, PRICE_URL, atomic_json, require
from provider_smoke.relay import Guard, PinnedTLS, payload, public_address, usage_from

REPO = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('recovery', Path(__file__).with_name('rehearse-artifact-recovery.py'))
recovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recovery)
spec = importlib.util.spec_from_file_location('demo', REPO/'demo/demo.py')
demo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(demo)


def provider_credential(path):
    require(path.is_file() and not path.is_symlink() and path.stat().st_mode & 0o077 == 0,
            'Provider credential file must be a private regular file')
    matches = []
    for line in path.read_text().splitlines():
        match = re.fullmatch(r'\s*OXSUM_E2E_UPSTREAM_API_KEY\s*=\s*(.*?)\s*', line)
        if match:
            matches.append(match[1].strip('"\''))
    require(len(matches) == 1 and matches[0] and not any(c.isspace() for c in matches[0]),
            'Exactly one provider credential field is required')
    return matches[0]


def resolved_address():
    # Authenticated public DNS, no API credential, and no inference. Ambient
    # proxy routing may fetch DNS; only the validated IP is used for provider TLS.
    opener = urllib.request.build_opener(recovery.NoRedirect())
    with opener.open('https://dns.google/resolve?name=api.deepseek.com&type=A', timeout=15) as response:
        require(response.status == 200, 'Public DNS failed')
        data = json.loads(response.read(65536))
    require(data.get('Status') == 0 and data.get('TC') is False, 'Incomplete public DNS answer')
    answers = data.get('Answer', [])
    names = {'api.deepseek.com.'}
    for _ in range(8):
        for answer in answers:
            if answer.get('type') == 5 and answer.get('name', '').lower() in names:
                names.add(answer['data'].lower())
    values = [public_address(answer['data']) for answer in answers
              if answer.get('type') == 1 and answer.get('name', '').lower() in names]
    require(bool(values), 'Public provider address unavailable')
    return values[0], {'source': 'https://dns.google/resolve', 'observed_at': datetime.now(timezone.utc).isoformat(),
                       'hostname': 'api.deepseek.com', 'addresses': values}


class FixtureTransport:
    """Local protocol fixture; never sends a socket or reads a provider credential."""
    def request(self, method, path, body, headers):
        self.body = json.loads(body)
        require(method == 'POST' and path == '/chat/completions', 'Wrong fixture request')
    def getresponse(self):
        self.status = 200
        usage = {'prompt_tokens': 23, 'completion_tokens': 11, 'total_tokens': 34}
        if self.body['stream']:
            chunks = [{'choices': [{'delta': {'content': 'OK'}, 'index': 0}]},
                      {'choices': [], 'usage': usage}]
            self.raw = (''.join('data: '+json.dumps(v)+'\n\n' for v in chunks)+'data: [DONE]\n\n').encode()
        else:
            self.raw = json.dumps({'id': 'fixture', 'model': MODEL, 'object': 'chat.completion',
                'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': 'OK'}, 'finish_reason': 'stop'}],
                'usage': usage}).encode()
        return self
    def read1(self, size):
        chunk, self.raw = self.raw[:size], self.raw[size:]
        return chunk
    def close(self):
        pass


def fixture_journal(path):
    require(not path.exists(), 'Fixture allowance must be a new path')
    atomic_json(path, {'version': 1, 'scope': 'operator-launch-2026-11-08', 'currency': 'CNY',
        'ceiling_micros': 100000, 'attempt_limit': 5, 'max_output_tokens': 32, 'thinking': 'disabled',
        'attempts': [], 'reserved_micros': 0, 'actual_micros': 0, 'input_bound_tokens': INPUT_BOUND,
        'pricing_verified_at': datetime.now(timezone.utc).isoformat(),
        'pricing': {'model': MODEL, 'input_micros_per_token': 2, 'output_micros_per_token': 8, 'source': PRICE_URL},
        'fixture_only': True})


def retain_turn(base, state, scenario, evidence, cumulative):
    session = state['customerSession']
    held = recovery.api(base, '/api/v1/log/head', session=session)
    key = recovery.api(base, '/api/v1/log/key', session=session)
    raw, headers = recovery.request(base, '/v1/chat/completions', token=state['registration']['apiKey']['secret'],
        body=payload(scenario == 'sse'), headers={'idempotency-key': 'provider-smoke-'+secrets.token_hex(16)})
    request_id = next((v for k,v in headers.items() if k.lower() == 'x-oxsum-request-id'), None)
    require(bool(request_id), 'Missing gateway request identity')
    usage = usage_from(raw, scenario == 'sse')
    require(isinstance(usage, dict), 'Provider omitted usage')
    prompt, completion = usage.get('prompt_tokens'), usage.get('completion_tokens')
    require(type(prompt) is int and 0 < prompt <= INPUT_BOUND and type(completion) is int and 0 < completion <= 32,
            'Provider usage exceeds the fixed bound')
    if scenario == 'sse':
        require(b'data: [DONE]' in raw, 'Provider stream did not terminate')
    else:
        require(json.loads(raw).get('object') == 'chat.completion', 'Invalid provider completion')
    expected = prompt + completion  # Explicit fixture sale: one minor unit per token.
    record = None
    for _ in range(200):
        rows = recovery.api(base, '/api/v1/billing-records?limit=100', session=session)['rows']
        record = next((row for row in rows if row['requestId'] == request_id), None)
        if record:
            break
        time.sleep(.1)
    require(record is not None and record['kind'] == 'usage' and record['chargedMinor'] == expected
            and record['upstreamAttempts'] == 1, 'Exact single-attempt settlement missing')
    balance = recovery.api(base, '/api/v1/balance', session=session)
    require(balance['availableMinor'] == 100_000_000 - cumulative - expected and balance['reservedMinor'] == 0,
            'Wallet or reservation conservation failed')
    entries = recovery.api(base, '/api/v1/entries?limit=100', session=session)['rows']
    entry = next((row for row in entries if row['id'] == record['entryId']), None)
    require(entry is not None, 'Missing retained entry hash')
    bundle = recovery.api(base, '/api/v1/entries/'+record['entryId']+'/proof', session=session)
    require(bundle.get('sale') and bundle.get('price'), 'Supply/sale bindings missing')
    head = recovery.api(base, '/api/v1/log/head', session=session)
    origin = 'oxsum/ledgers/'+state['registration']['organization']['tenantId']
    require(head['origin'] == held['origin'] == origin, 'Ledger origin changed')
    artifacts = {'held-head.json': {'size': held['size'], 'root': held['root']}, 'operator-key.json': key,
        'settlement-bundle.json': bundle, 'billing-record.json': record, 'signed-head.json': head,
        'consistency.json': recovery.api(base, '/api/v1/log/consistency?from='+str(held['size']), session=session),
        'checkpoint.json': {'data': recovery.api(base, '/api/v1/log/checkpoint', session=session)},
        'manifest.json': {'entryId': record['entryId'], 'contentHash': entry['contentHash'], 'origin': origin,
            'headSize': head['size'], 'headRoot': head['root'], 'requestId': request_id, 'chargedMinor': expected,
            'sale': state['sale'], 'keyTrust': 'Generated isolated operator identity retained before inference'},
        'usage.json': usage}
    evidence.mkdir()
    for name,value in artifacts.items():
        atomic_json(evidence/name, value)
    return expected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['binary', 'sdk', 'output', 'budget']:
        parser.add_argument('--'+name, type=Path, required=True)
    for name in ['sha', 'sha256', 'schema', 'sdk-sha256']:
        parser.add_argument('--'+name, required=True)
    parser.add_argument('--database-url', required=True, help='Local provisioning PostgreSQL URL; creates its own random database')
    parser.add_argument('--credential-file', type=Path, help='Existing private root .env; only the named provider field is read')
    parser.add_argument('--fixture', action='store_true', help='No external call; requires a new separate fixture allowance')
    args = parser.parse_args()
    parsed_database = urlparse(args.database_url)
    require(parsed_database.scheme in ('postgres', 'postgresql') and parsed_database.hostname in ('127.0.0.1', 'localhost')
            and not parsed_database.query and not parsed_database.fragment, 'Only explicit local database provisioning is supported')
    # Fixture mode may never initialize the real shared budget, even if absent.
    canonical = REPO
    while canonical.name != 'oxsum' and canonical.parent != canonical:
        canonical = canonical.parent
    real_budget = canonical/'local-notes/operator-launch/provider-budget.json'
    require(not args.fixture or args.budget.resolve() != real_budget.resolve(), 'Fixture cannot reset the real allowance')
    require(args.fixture or args.budget.resolve() == real_budget.resolve(), 'Live calls require the original shared run-wide allowance')
    require(args.fixture or args.credential_file is not None, 'Live provider credential required')
    require(not args.fixture or args.credential_file is None, 'Fixture does not read provider credentials')
    require(args.sdk.is_file() and not args.sdk.is_symlink() and recovery.backup.digest(args.sdk) == args.sdk_sha256,
            'Verifier artifact hash mismatch')
    args.output.mkdir(parents=True, exist_ok=False)
    args.output.chmod(0o700)
    output = args.output.resolve()
    artifact = recovery.freeze_artifact(args.binary.absolute(), args.sha, args.sha256, args.schema, '0.1.0', output/'oxsum')
    shutil.copyfile(args.sdk, output/'verify-oxsum-bill.tgz')
    require(recovery.backup.digest(output/'verify-oxsum-bill.tgz') == args.sdk_sha256, 'Frozen verifier bytes changed')
    if args.fixture:
        fixture_journal(args.budget)
    budget = Budget(args.budget)
    with budget.locked() as journal:
        budget.check_pricing(journal)
        require(args.fixture == (journal.get('fixture_only') is True), 'Fixture/live allowance mismatch')
        require(not journal.get('scenarios'), 'An earlier run has claimed a scenario; do not repeat it automatically')
    if args.fixture:
        provider_key, transport, dns = 'local-no-provider-credential', FixtureTransport, {'fixture_only': True}
    else:
        address, dns = resolved_address()
        provider_key = provider_credential(args.credential_file)
        transport = lambda: PinnedTLS(address)
    atomic_json(output/'network.json', dns)
    environment = recovery.clean_environment()
    run_id = secrets.token_hex(16)
    report = {'runtime': {k:v for k,v in artifact.items() if k != 'path'}, 'sdkSha256': args.sdk_sha256,
        'model': MODEL, 'runId': run_id, 'fixture': args.fixture, 'scenarios': {},
        'scope': 'Controlled loopback guard forwards fixed requests over public-pinned TLS; not direct production transport certification'}
    relay = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Guard)
    relay.budget, relay.run_id, relay.provider_key = budget, run_id, provider_key
    relay.local_token, relay.transport, relay.active_scenario = secrets.token_urlsafe(32), transport, None
    thread = threading.Thread(target=relay.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='oxsum-provider-runtime-') as private:
            directory = Path(private)
            (directory/'.env').write_text('')
            with demo.scratch_database(args.database_url) as (_name, database_url):
                env = {**environment, 'DATABASE_URL': database_url, 'OXSUM_SIGNUP': 'invite',
                    'OXSUM_ADMIN_TOKEN': secrets.token_urlsafe(32), 'OXSUM_SECRET_KEY': base64.b64encode(secrets.token_bytes(32)).decode(),
                    'OXSUM_HEAD_SIGNING_KEY': base64.b64encode(secrets.token_bytes(32)).decode(),
                    'OXSUM_DISPLAY_CURRENCY': 'USD', 'OXSUM_DISPLAY_RATE_PER_CREDIT': '1', 'OXSUM_DISPLAY_DECIMALS': '2',
                    'OXSUM_SESSION_COOKIE_SECURE': 'false', 'RUST_LOG': 'warn'}
                runtime = recovery.Runtime(artifact, env, directory, directory/'server.log')
                try:
                    report['observedRuntime'] = runtime.start()
                    state_path = directory/'customer.json'
                    recovery.run(['node', str(REPO/'scripts/provider-smoke-provision.mjs')], env={**env,
                        'OXSUM_DEMO_BASE': runtime.base, 'OXSUM_DEMO_UPSTREAM': f'http://127.0.0.1:{relay.server_port}/v1',
                        'OXSUM_DEMO_STATE': str(state_path), 'OXSUM_SMOKE_RELAY_TOKEN': relay.local_token},
                        cwd=directory, log=directory/'provision.log')
                    state = json.loads(state_path.read_text())
                    cumulative = 0
                    for scenario in ('whole', 'sse'):
                        budget.claim(scenario, run_id)
                        relay.active_scenario = scenario
                        charged = retain_turn(runtime.base, state, scenario, output/scenario, cumulative)
                        relay.active_scenario = None
                        cumulative += charged
                        budget.complete(scenario, run_id)
                        report['scenarios'][scenario] = {'chargedMinor': charged, 'captured': True}
                        atomic_json(output/'report.json', report)
                finally:
                    runtime.stop()
    finally:
        relay.shutdown()
        relay.server_close()
        relay.provider_key = ''
        with budget.locked(read_only=True) as journal:
            report['allowance'] = {'attempts': len(journal['attempts']), 'reservedMicros': journal['reserved_micros'],
                'observedUsagePeakCostMicros': journal['actual_micros'], 'halted': journal.get('halted'),
                'attemptRecords': journal['attempts']}
        atomic_json(output/'report.json', report)
    # Independent package, stopped listener and dropped database; verifier blocks fetch.
    with tempfile.TemporaryDirectory(prefix='oxsum-provider-consumer-') as private:
        consumer = Path(private)
        atomic_json(consumer/'package.json', {'name': 'oxsum-provider-check', 'private': True, 'type': 'module'})
        shutil.copyfile(REPO/'examples/verify-consumer/verify.mjs', consumer/'verify.mjs')
        shutil.copyfile(REPO/'examples/verify-consumer/scope.mjs', consumer/'scope.mjs')
        recovery.run(['npm', 'install', '--ignore-scripts', '--no-audit', '--no-fund', str(output/'verify-oxsum-bill.tgz')],
                     env=environment, cwd=consumer, log=output/'package.log')
        for scenario in ('whole', 'sse'):
            recovery.run(['node', 'verify.mjs', '--dir', str(output/scenario)], env=environment, cwd=consumer,
                         log=output/(scenario+'-verification.log'))
            report['scenarios'][scenario]['offlineVerified'] = True
    atomic_json(output/'report.json', report)
    atomic_json(output/'checksums.json', {str(p.relative_to(output)): recovery.backup.digest(p)
                for p in sorted(output.rglob('*')) if p.is_file() and p.name != 'checksums.json'})
    print('Completed whole/SSE '+('local fixture' if args.fixture else 'DeepSeek mediated-path')+' smoke: '+str(output))


if __name__ == '__main__':
    try:
        main()
    except Exception:
        # Exceptions may originate in HTTP libraries; do not print their URLs,
        # headers, bodies or inherited private configuration.
        print('provider smoke failed; retained scenario/attempt reservations forbid automatic repeats', file=sys.stderr)
        sys.exit(1)
