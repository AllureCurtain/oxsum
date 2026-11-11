"""Durable, process-shared bounds for the explicitly authorized N14 exercise."""
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import tempfile
import uuid

MODEL = 'deepseek-flash'
PROMPT = 'Reply with exactly: OK'
INPUT_BOUND = 4096  # Fixed ASCII payload <512 bytes plus conservative chat overhead.
OUTPUT_BOUND = 32
INPUT_RATE = 2  # CNY millionths per token, peak cache-miss tariff.
OUTPUT_RATE = 8
RESERVATION = INPUT_BOUND * INPUT_RATE + OUTPUT_BOUND * OUTPUT_RATE
PRICE_URL = 'https://api-docs.deepseek.com/zh-cn/quick_start/pricing'

# A scenario is one of the two protocols, optionally tagged for a distinct
# artifact under test — `whole`/`sse` for the development run, `whole-candidate`/
# `sse-candidate` (or another `-suffix`) to re-certify a different frozen binary
# without re-opening an already-completed scenario. Each named scenario still
# claims and reserves exactly once; the suffix does not reset the allowance.
import re as _re
SCENARIO = _re.compile(r'^(whole|sse)(-[a-z0-9]+)?$')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def now():
    return datetime.now(timezone.utc).isoformat()


def atomic_json(path, value):
    descriptor, temporary = tempfile.mkstemp(dir=path.parent, prefix='.' + path.name + '-')
    try:
        with os.fdopen(descriptor, 'w') as stream:
            json.dump(value, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def integer(value):
    return type(value) is int and value >= 0


class Budget:
    """Never creates/resets an allowance or releases ambiguous reservations."""
    def __init__(self, path):
        self.path = Path(path).absolute()

    def validate(self, state, *, allow_halted=False):
        require(state.get('version') == 1 and state.get('scope') == 'operator-launch-2026-11-08'
                and state.get('currency') == 'CNY' and state.get('ceiling_micros') == 100000
                and state.get('attempt_limit') == 5 and state.get('max_output_tokens') == 32
                and state.get('thinking') == 'disabled', 'Unrecognized shared allowance')
        attempts = state.get('attempts')
        require(isinstance(attempts, list) and len(attempts) <= 5, 'Invalid attempt journal')
        ids = set()
        for attempt in attempts:
            require(attempt.get('id') not in ids and isinstance(attempt.get('id'), str)
                    and attempt.get('reserved_micros') == RESERVATION
                    and attempt.get('model') == MODEL, 'Invalid retained attempt')
            ids.add(attempt['id'])
            actual = attempt.get('actual_micros')
            require(actual is None or (integer(actual) and actual <= RESERVATION), 'Invalid actual cost')
        require(state.get('reserved_micros') == sum(a['reserved_micros'] for a in attempts)
                and state['reserved_micros'] <= 100000
                and state.get('actual_micros') == sum(a.get('actual_micros') or 0 for a in attempts),
                'Inconsistent budget totals')
        require(isinstance(state.get('scenarios', {}), dict), 'Invalid scenario journal')
        require(allow_halted or not state.get('halted'), 'Prior provider bound violation requires review')

    @contextmanager
    def locked(self, *, read_only=False):
        require(self.path.is_file() and not self.path.is_symlink(), 'Existing shared journal required')
        lock_path = self.path.with_name(self.path.name + '.lock')
        with os.fdopen(os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600), 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            state = json.loads(self.path.read_text())
            self.validate(state, allow_halted=read_only)
            yield state
            if not read_only:
                atomic_json(self.path, state)

    def check_pricing(self, state):
        price = state.get('pricing', {})
        require(price.get('model') == MODEL and price.get('input_micros_per_token') == INPUT_RATE
                and price.get('output_micros_per_token') == OUTPUT_RATE and price.get('source') == PRICE_URL
                and state.get('input_bound_tokens') == INPUT_BOUND, 'Verified fixed-model pricing required')
        verified = datetime.fromisoformat(state['pricing_verified_at'])
        require(verified.tzinfo is not None and 0 <= (datetime.now(timezone.utc)-verified).total_seconds() <= 86400,
                'Pricing verification is absent, future or older than 24 hours')

    def claim(self, scenario, run_id):
        require(SCENARIO.fullmatch(scenario), 'Unknown scenario')
        with self.locked() as state:
            self.check_pricing(state)
            scenarios = state.setdefault('scenarios', {})
            require(scenario not in scenarios, 'Scenario already claimed; resume cannot repeat inference')
            require(len(state['attempts']) < 5 and state['reserved_micros'] + RESERVATION <= 100000,
                    'Provider allowance exhausted')
            scenarios[scenario] = {'run_id': run_id, 'claimed_at': now(), 'status': 'claimed'}

    def reserve(self, scenario, run_id, body_hash):
        with self.locked() as state:
            self.check_pricing(state)
            claimed = state.get('scenarios', {}).get(scenario, {})
            require(claimed.get('run_id') == run_id and claimed.get('status') == 'claimed',
                    'No active owned scenario')
            require(len(state['attempts']) < 5 and state['reserved_micros'] + RESERVATION <= 100000,
                    'Provider allowance exhausted')
            require(not any(a['scenario'] == scenario for a in state['attempts']),
                    'Scenario already attempted; no automatic retransmission')
            attempt_id = str(uuid.uuid4())
            state['attempts'].append({'id': attempt_id, 'scenario': scenario, 'run_id': run_id,
                'model': MODEL, 'reserved_at': now(), 'reserved_micros': RESERVATION,
                'actual_micros': None, 'request_sha256': body_hash, 'status': 'reserved'})
            state['reserved_micros'] += RESERVATION
        return attempt_id

    def finish(self, attempt_id, status, usage=None):
        with self.locked() as state:
            attempt = next((a for a in state['attempts'] if a['id'] == attempt_id), None)
            require(attempt is not None and attempt['status'] == 'reserved', 'Attempt already reconciled or missing')
            attempt.update(status=status, finished_at=now())
            if usage is not None:
                prompt, completion = usage.get('prompt_tokens'), usage.get('completion_tokens')
                valid = integer(prompt) and integer(completion) and prompt <= INPUT_BOUND and completion <= OUTPUT_BOUND
                if valid:
                    attempt.update(usage={'prompt_tokens': prompt, 'completion_tokens': completion},
                                   actual_micros=prompt * INPUT_RATE + completion * OUTPUT_RATE)
                    state['actual_micros'] += attempt['actual_micros']
                else:
                    state['halted'] = 'Provider reported usage outside the reserved bound'
                    attempt['status'] = 'bound_violation'
            # Reservations remain lifetime upper bounds even with known usage.

    def complete(self, scenario, run_id):
        with self.locked() as state:
            current = state.get('scenarios', {}).get(scenario, {})
            require(current.get('run_id') == run_id and current.get('status') == 'claimed', 'Scenario ownership changed')
            current.update(status='completed', completed_at=now())
