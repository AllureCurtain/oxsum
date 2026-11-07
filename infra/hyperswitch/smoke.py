#!/usr/bin/env python3
"""Exercise oxsum orders against this local simulated switch, without real money."""
import argparse
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid

import merchant
import stack


def key():
    return str(uuid.uuid4())


def oxsum(base, path, body=None, session=None, token=None, signature=None, expect=200):
    headers = {'Content-Type': 'application/json'}
    if session:
        headers['Cookie'] = 'oxsum_session=' + session
    if token:
        headers['Authorization'] = 'Bearer ' + token
    if signature:
        headers['x-webhook-signature-512'] = signature
    data = body if isinstance(body, bytes) else None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(base + path, data=data, headers=headers)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), merchant.NoRedirect())
    try:
        response = opener.open(request, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw = response.read()
        payload = json.loads(raw) if raw else {}
        if response.status != expect:
            raise RuntimeError(f'Oxsum {path}: expected {expect}, got {response.status}, code={payload.get("error",{}).get("code")}')
        cookie = response.headers.get('Set-Cookie', '').split(';')[0].removeprefix('oxsum_session=')
        return payload.get('data'), cookie


def wait_for(read, predicate, message, seconds=30):
    deadline = time.monotonic() + seconds
    while True:
        value = read()
        if predicate(value):
            return value
        if time.monotonic() >= deadline:
            raise RuntimeError(message)
        time.sleep(0.5)


def exercise(base, admin, config, state, directory):
    email, password = f'owner-{key()}@example.test', secrets.token_urlsafe(24)
    minted, _ = oxsum(base, '/api/v1/admin/business/setup-tokens',
                     {'idempotencyKey': key(), 'ownerEmail': email}, token=admin)
    oxsum(base, '/api/v1/business/setup/register', {'token': minted['token'],
          'businessName': 'Isolated switch smoke', 'email': email, 'password': password})
    _, owner = oxsum(base, '/api/v1/auth/login', {'email': email, 'password': password})
    oxsum(base, '/api/v1/session/context', {'idempotencyKey': key(), 'context': 'business'}, session=owner)
    email, password = f'customer-{key()}@example.test', secrets.token_urlsafe(24)
    invitation, _ = oxsum(base, '/api/v1/business/customer-invitations', {
        'idempotencyKey': key(), 'email': email, 'name': 'Local simulated payer',
        'externalId': key(), 'reason': 'No real person or payment'}, session=owner)
    registered, _ = oxsum(base, '/api/v1/invitations/redeem',
                         {'token': invitation['token'], 'email': email, 'password': password})
    _, customer = oxsum(base, '/api/v1/auth/login', {'email': email, 'password': password})
    organization = registered['organization']['id']
    payload = {'idempotencyKey': key(), 'rail': 'hyperswitch', 'purpose': 'walletTopup',
               'displayAmountMinor': 200, 'displayCurrency': 'USD'}
    created, _ = oxsum(base, '/api/v1/orders', payload, session=customer)
    replay, _ = oxsum(base, '/api/v1/orders', payload, session=customer)
    order = created['order']; order_id = order['orderId']; payment = created['providerPaymentId']
    assert payment and replay['providerPaymentId'] == payment and replay['order']['orderId'] == order_id
    assert order['amountMinor'] == 2_000_000 and order['rateMantissa'] == 1 and order['rateScale'] == 0
    balance, _ = oxsum(base, '/api/v1/balance', session=customer)
    assert balance['availableMinor'] == 0
    paid = merchant.request(config, 'POST', '/payments/' + payment + '/confirm', {
        'payment_method': 'card', 'payment_method_type': 'credit',
        'payment_method_data': {'card': {'card_number': '4242424242424242',
        'card_exp_month': '12', 'card_exp_year': '2030', 'card_holder_name': 'Local Fixture', 'card_cvc': '123'}},
        'authentication_type': 'no_three_ds'}, merchant_key=state['apiKey'])
    assert paid['status'] == 'succeeded' and paid['connector'] == 'paypal_test'
    assert paid['amount'] == 200 and paid['currency'] == 'USD'
    assert paid['metadata']['order_id'] == order_id
    balance, _ = oxsum(base, '/api/v1/balance', session=customer)
    assert balance['availableMinor'] == 0, 'Provider success without authenticated event must not credit'
    hook = f'/api/v1/webhooks/merchants/{order["merchantId"]}/versions/{order["merchantVersionId"]}'

    def event(kind, obj, content_type='payment_details'):
        value = {'merchant_id': state['merchantId'], 'event_id': 'fixture_' + key(),
                 'event_type': kind, 'livemode': False, 'content': {'type': content_type, 'object': obj}}
        raw = json.dumps(value, separators=(',', ':')).encode()
        signature = hmac.new(state['webhookSecret'].encode(), raw, hashlib.sha512).hexdigest()
        # This is deliberately a locally signed event fixture, not an automatic
        # provider delivery or PSP certification. The switch result was read above.
        return raw, signature

    raw, signature = event('payment_succeeded', paid)
    oxsum(base, hook, raw, signature='0' * 128, expect=401)
    assert oxsum(base, '/api/v1/balance', session=customer)[0]['availableMinor'] == 0
    oxsum(base, hook, raw, signature=signature)
    oxsum(base, hook, raw, signature=signature)
    wait_for(lambda: oxsum(base, '/api/v1/orders/' + order_id, session=customer)[0],
             lambda value: value['status'] == 'fulfilled', 'Order worker did not fulfill')
    balance, _ = oxsum(base, '/api/v1/balance', session=customer)
    assert balance['availableMinor'] == 2_000_000 and balance['reservedMinor'] == 0
    request = {'idempotencyKey': key(), 'amountMinor': 500_000, 'reason': 'Local simulated partial refund'}
    refund, _ = oxsum(base, f'/api/v1/orders/{order_id}/refund-request', request, session=customer)
    duplicate, _ = oxsum(base, f'/api/v1/orders/{order_id}/refund-request', request, session=customer)
    assert duplicate['refundId'] == refund['refundId']
    decision = {'idempotencyKey': key(), 'decision': 'approve', 'reason': 'Local simulated review'}
    approved, _ = oxsum(base, f'/api/v1/business/refunds/{refund["refundId"]}/resolve', decision, session=owner)
    assert approved['state'] == 'provider_pending' and approved['displayAmountMinor'] == 50
    # Resolve returns its committed decision snapshot. Read the current row
    # separately for the provider handle acquired after that transaction.
    approved, _ = oxsum(base, f'/api/v1/business/refunds/{refund["refundId"]}', session=owner)
    stack.private_file(directory / 'refund-observed.json', json.dumps(approved, indent=2) + '\n')
    assert approved['providerRefundId']
    external = merchant.request(config, 'GET', '/refunds/' + approved['providerRefundId'], merchant_key=state['apiKey'])
    assert external['status'] == 'succeeded' and external['amount'] == 50 and external['payment_id'] == payment
    raw, signature = event('refund_succeeded', external, 'refund_details')
    oxsum(base, hook, raw, signature=signature)
    oxsum(base, hook, raw, signature=signature)
    result, _ = oxsum(base, f'/api/v1/business/refunds/{refund["refundId"]}', session=owner)
    balance, _ = oxsum(base, '/api/v1/balance', session=customer)
    assert result['state'] == 'succeeded' and balance['availableMinor'] == 1_500_000 and balance['reservedMinor'] == 0
    notes, _ = oxsum(base, '/api/v1/business/credit-notes', session=owner)
    matching_notes = [note for note in notes['notes'] if note['refundId'] == refund['refundId']]
    assert len(matching_notes) == 1 and matching_notes[0]['state'] == 'applied'
    assert matching_notes[0]['entryId']
    stack.private_file(directory / 'private-checkout.json', json.dumps(created, indent=2) + '\n')
    return {'tier': 'local-simulation-with-locally-signed-events', 'orderId': order_id,
            'providerPaymentId': payment, 'refundId': refund['refundId'],
            'providerRefundId': approved['providerRefundId'], 'balanceMinor': balance['availableMinor'],
            'currency': 'USD', 'rate': '1 credit = 1 USD', 'paidMinor': 200, 'refundedMinor': 50,
            'pendingNoCredit': 'passed', 'duplicateCreate': 'passed', 'badSignature': 'refused',
            'duplicateEvents': 'no duplicate effect', 'creditNote': 'one applied linked note with ledger entry',
            'automaticCallback': 'unperformed: public HTTPS tunnel unavailable',
            'browserCheckout': 'unperformed: browser space/public HTTPS unavailable',
            'pspCertification': 'unperformed: no real PSP credential'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--postgres-container', default='oxsum-postgres-1')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    config = json.loads((stack.RUNTIME / 'private.json').read_text())
    state = json.loads((stack.RUNTIME / 'merchant.json').read_text())
    merchant.inspect(config, state)
    nonce = secrets.token_hex(6); database = 'oxsum_hs_smoke_' + nonce
    directory = stack.RUNTIME / ('smoke-' + nonce); directory.mkdir(mode=0o700)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0)); port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('OXSUM_', 'DATABASE_', 'PG', 'SOAK_'))}
    admin = secrets.token_urlsafe(32)
    env.update(DATABASE_URL=f'postgres://postgres:postgres@127.0.0.1:5432/{database}',
               OXSUM_ADDR=f'127.0.0.1:{port}', OXSUM_ADMIN_TOKEN=admin,
               OXSUM_SECRET_KEY=base64.b64encode(secrets.token_bytes(32)).decode(),
               OXSUM_HEAD_SIGNING_KEY=base64.b64encode(secrets.token_bytes(32)).decode(),
               OXSUM_SESSION_COOKIE_SECURE='false', OXSUM_PUBLIC_URL=base,
               OXSUM_DISPLAY_CURRENCY='USD', OXSUM_DISPLAY_RATE_PER_CREDIT='1',
               OXSUM_HYPERSWITCH_API_KEY=state['apiKey'], OXSUM_HYPERSWITCH_WEBHOOK_SECRET=state['webhookSecret'],
               OXSUM_HYPERSWITCH_PUBLISHABLE_KEY=state['publishableKey'],
               OXSUM_HYPERSWITCH_BASE_URL=f'http://127.0.0.1:{config["ports"]["ROUTER"]}',
               # Unserved placeholder for API-only fixtures; never browser acceptance.
               OXSUM_HYPERSWITCH_SDK_URL=config['sdkUrl'] if config['sdkUrl'].startswith('https://') else 'https://sdk.example.test',
               OXSUM_HYPERSWITCH_MERCHANT_ACCOUNT_ID=state['merchantId'], OXSUM_HYPERSWITCH_MERCHANT_MODE='test',
               OXSUM_HYPERSWITCH_MERCHANT_DESTINATION_POLICY='{"network":"internal","allowedIps":["127.0.0.1"]}',
               OXSUM_SMTP_URL=f'smtp://127.0.0.1:{config["ports"]["SMTP"]}',
               OXSUM_MAIL_FROM='Oxsum Fixture <fixture@example.test>', RUST_LOG='warn')
    stack.private_file(directory / '.env', '')
    created = False; process = None
    try:
        stack.run(['docker', 'exec', args.postgres_container, 'createdb', '-U', 'postgres', database])
        created = True
        with (directory / 'server.log').open('w') as log:
            process = subprocess.Popen([str(binary)], cwd=directory, env=env, stdout=log, stderr=log)
        def healthy():
            if process.poll() is not None:
                raise RuntimeError('Fixture listener exited; inspect its private server.log')
            try:
                with urllib.request.urlopen(base + '/healthz', timeout=1) as response:
                    return response.status == 200
            except (urllib.error.URLError, TimeoutError):
                return False
        wait_for(healthy, bool, 'Fixture listener did not become healthy')
        result = exercise(base, admin, config, state, directory)
        result['binarySha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
        result['database'] = database
        stack.private_file(directory / 'result.json', json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        if process:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
        if created:
            stack.run(['docker', 'exec', args.postgres_container, 'dropdb', '-U', 'postgres', '--force', database])
        print(f'Fixture stopped; its own scratch database removed. Evidence: {directory}')


if __name__ == '__main__':
    main()
