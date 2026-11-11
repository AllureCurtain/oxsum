"""Check retained provider rotation observations without credentials or a server."""
import hashlib
import json
from datetime import datetime, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def read(name):
    return json.loads((ROOT / (name + '.json')).read_text())


def data(name):
    return read(name)['data']


def instant(value):
    return datetime.fromisoformat(value.replace('Z', '+00:00'))


manifest = read('manifest')
assert manifest['listenersClosed'] and manifest['browser']['closed']
assert manifest['browser']['documentWidth'] == 390
assert data('balance-after') == {
    'availableMinor': 9999930, 'creditLimitMinor': 0, 'creditUsedMinor': 0,
    'reservedMinor': 0, 'spentThisMonthMinor': 70,
}
assert data('holds-after')['holds'] == []
bills = data('bills')['rows']
assert len(bills) == 7 and sum(b['amountMinor'] for b in bills) == 9999930
requests = data('customer-requests')['rows']
assert len(requests) == 6 and sum(r['chargeMinor'] for r in requests) == 70
assert len({r['requestId'] for r in requests}) == 6
for label in ['before-rotation', 'during-rotation', 'after-disable', 'after-repoint', 'after-enable']:
    response = read(label + '-response')
    assert response['status'] == 200
    request = next(r for r in requests if r['requestId'] == response['headers']['x-oxsum-request-id'])
    assert (request['status'], request['chargeMinor'], request['upstreamAttempts']) == ('settled', 14, 1)
    assert (request['inputTokens'], request['outputTokens']) == (10, 2)
    bill = next(b for b in bills if b['entryId'] == request['entryId'])
    assert bill['amountMinor'] == -14
    assert '[DONE]' in (ROOT / (label + '.sse')).read_text()
failed = next(r for r in requests if r['status'] == 'errored')
assert failed['chargeMinor'] == failed['upstreamAttempts'] == 0
assert next(b for b in bills if b['entryId'] == failed['entryId'])['amountMinor'] == 0
assert read('while-disabled-error')['status'] == 502
# Preserve the observed mismatch rather than silently declaring all acceptance green.
assert failed['httpStatus'] == 200 and manifest['knownDefect']['issue'] == 1024
upstream = read('requests')
assert upstream[0] == {'method': 'HEAD', 'path': '/v1'}
assert len(upstream) == 6
assert [r['keyLabel'] for r in upstream[1:]] == ['oldKey', 'oldKey', 'newKey', 'repointKey', 'repointKey']
assert [r['path'] for r in upstream[1:]] == ['/v1/chat/completions'] * 3 + ['/v2/chat/completions'] * 2
assert [r['gate'] for r in upstream[1:]] == [False, True, False, False, False]
assert len({r['keySha256'] for r in upstream[1:]}) == 3
channel = data('channels-after')['channels'][0]
assert len(channel['credentials']) == 1 and channel['billKeyLast4'] is None
assert channel['credentials'][0]['apiKeyLast4'] == 'NEW3'
assert channel['credentials'][0]['status'] == 'active'
assert channel['baseUrl'].endswith('/v2')
assert json.loads(read('owner-channel-read'))['data'] == data('channels-after')
assert read('admin-denial')['status'] == 403
assert read('admin-read')['status'] == 200
assert json.loads(read('admin-read')['body'])['data'] == data('channels-after')
audit = read('provider-audit')
assert len(audit) == 12 and len({r['actor'] for r in audit}) == 1
assert sum(r['action'].endswith('credential.remove') for r in audit) == 1
pinned_id = read('during-rotation-response')['headers']['x-oxsum-request-id']
pinned = next(r for r in requests if r['requestId'] == pinned_id)
finished = instant(pinned['createdAt'])
started = finished - timedelta(milliseconds=pinned['totalMs'])
removed = next(r for r in audit if r['action'].endswith('credential.remove'))
repointed = [r for r in audit if r['action'].endswith('.connection')][-1]
assert started < instant(removed['recorded_at']) < instant(repointed['recorded_at']) < finished
verdicts = read('offline-verdicts')
assert verdicts['fetchAttempts'] == 0 and len(verdicts['results']) == 6
for result in verdicts['results']:
    assert result['verdict']['ok'] and result['price']['ok'] and result['sale']['ok']
    assert len(result['rejections']) == 6 and all(r['rejected'] for r in result['rejections'].values())
for name, expected in read('sha256').items():
    assert hashlib.sha256((ROOT / name).read_bytes()).hexdigest() == expected, name
print('PASS: five charged streams, one zero-charge error, pinned rotation, authority, six offline proofs and 36 rejections; known HTTP status defect #1024 retained')
