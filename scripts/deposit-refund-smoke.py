#!/usr/bin/env python3
"""Exercise legacy reversal recovery against the pinned local dummy switch."""
import argparse
import hashlib
import hmac
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import secrets
import sys
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sandbox', type=Path, required=True)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    sandbox = args.sandbox.resolve(strict=True)
    sys.path.insert(0, str(sandbox))
    import merchant
    import smoke
    import stack
    config = json.loads((stack.RUNTIME / 'private.json').read_text())
    state = json.loads((stack.RUNTIME / 'merchant.json').read_text())
    if state.get('tier') != 'local-simulation-not-psp':
        raise SystemExit('Only the explicitly marked local dummy merchant is permitted.')
    stats = {'refundPosts': 0, 'refundReads': 0, 'droppedResponses': 0}
    observed = {}
    lock = threading.Lock()
    target_port = config['ports']['ROUTER']

    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def dispatch(self):
            self.connection.settimeout(30)
            body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
            connection = http.client.HTTPConnection('127.0.0.1', target_port, timeout=30)
            try:
                headers = {k: v for k, v in self.headers.items()
                           if k.lower() not in ('host', 'connection', 'content-length')}
                headers['Content-Length'] = str(len(body))
                connection.request(self.command, self.path, body, headers)
                response = connection.getresponse()
                value = response.read()
                with lock:
                    if self.command == 'GET' and self.path.startswith('/refunds/'):
                        stats['refundReads'] += 1
                    if self.command == 'POST' and self.path == '/refunds':
                        stats['refundPosts'] += 1
                        if response.status < 300 and stats['droppedResponses'] == 0:
                            observed['refundId'] = json.loads(value)['refund_id']
                            stats['droppedResponses'] += 1
                            # The switch committed, but oxsum cannot read its result.
                            self.send_response(502)
                            self.send_header('Content-Length', '2')
                            self.end_headers()
                            self.wfile.write(b'{}')
                            return
                self.send_response(response.status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(value)))
                self.end_headers()
                self.wfile.write(value)
            finally:
                connection.close()

        do_GET = do_POST = dispatch

    proxy = ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
    worker = threading.Thread(target=proxy.serve_forever, daemon=True)
    worker.start()
    original_loads = smoke.json.loads
    original_exercise = smoke.exercise

    def load(text, *rest, **kwargs):
        value = original_loads(text, *rest, **kwargs)
        if isinstance(value, dict) and value.get('project') == config['project'] and 'ports' in value:
            value['ports']['ROUTER'] = proxy.server_port
        return value

    def exercise(base, admin, fixture_config, fixture_state, directory):
        email, password = f'owner-{smoke.key()}@example.test', secrets.token_urlsafe(24)
        minted, _ = smoke.oxsum(base, '/api/v1/admin/business/setup-tokens',
                               {'idempotencyKey': smoke.key(), 'ownerEmail': email}, token=admin)
        smoke.oxsum(base, '/api/v1/business/setup/register', {'token': minted['token'],
                    'businessName': 'Legacy reversal fixture', 'email': email, 'password': password})
        _, owner = smoke.oxsum(base, '/api/v1/auth/login', {'email': email, 'password': password})
        smoke.oxsum(base, '/api/v1/session/context',
                    {'idempotencyKey': smoke.key(), 'context': 'business'}, session=owner)
        email, password = f'customer-{smoke.key()}@example.test', secrets.token_urlsafe(24)
        invite, _ = smoke.oxsum(base, '/api/v1/business/customer-invitations', {
            'idempotencyKey': smoke.key(), 'email': email, 'name': 'Legacy fixture payer',
            'externalId': smoke.key(), 'reason': 'Local dummy only'}, session=owner)
        smoke.oxsum(base, '/api/v1/invitations/redeem',
                    {'token': invite['token'], 'email': email, 'password': password})
        _, customer = smoke.oxsum(base, '/api/v1/auth/login', {'email': email, 'password': password})
        # This old deposit API uses its historical units. Do not relabel them
        # as the purpose-scoped order's fiat-to-credit conversion contract.
        checkout, _ = smoke.oxsum(base, '/api/v1/topups/hyperswitch',
            {'idempotencyKey': smoke.key(), 'amountMinor': 200}, session=customer)
        deposit, payment = checkout['depositId'], checkout['paymentId']
        paid = merchant.request(fixture_config, 'POST', '/payments/' + payment + '/confirm', {
            'payment_method': 'card', 'payment_method_type': 'credit',
            'payment_method_data': {'card': {'card_number': '4242424242424242',
            'card_exp_month': '12', 'card_exp_year': '2030', 'card_holder_name': 'Local Fixture', 'card_cvc': '123'}},
            'authentication_type': 'no_three_ds'}, merchant_key=fixture_state['apiKey'])
        assert paid['status'] == 'succeeded' and paid['connector'] == 'paypal_test'

        def event(kind, obj, content_type):
            value = {'merchant_id': fixture_state['merchantId'], 'event_id': 'fixture_' + smoke.key(),
                     'event_type': kind, 'livemode': False, 'content': {'type': content_type, 'object': obj}}
            raw = json.dumps(value, separators=(',', ':')).encode()
            signature = hmac.new(fixture_state['webhookSecret'].encode(), raw, hashlib.sha512).hexdigest()
            return raw, signature

        raw, signature = event('payment_succeeded', paid, 'payment_details')
        smoke.oxsum(base, '/api/v1/webhooks/hyperswitch', raw, signature=signature)
        assert smoke.oxsum(base, '/api/v1/balance', session=customer)[0]['availableMinor'] == 200
        path = f'/api/v1/admin/deposits/{deposit}/reverse'
        reversed_row, _ = smoke.oxsum(base, path, {'reason': 'Local legacy reversal'}, token=admin)
        assert reversed_row['status'] == 'reversed' and reversed_row['refundStatus'] == 'pending'
        repeated, _ = smoke.oxsum(base, path, {'reason': 'Local legacy reversal'}, token=admin)
        assert repeated['reversalEntryId'] == reversed_row['reversalEntryId']
        assert smoke.oxsum(base, '/api/v1/balance', session=customer)[0]['availableMinor'] == 0
        with lock:
            assert stats == {'refundPosts': 1, 'refundReads': 2, 'droppedResponses': 1}, stats
        external = merchant.request(fixture_config, 'GET', '/refunds/' + observed['refundId'],
                                    merchant_key=fixture_state['apiKey'])
        assert external['status'] == 'succeeded' and external['amount'] == 200
        assert external['metadata']['deposit_id'] == deposit
        raw, signature = event('refund_succeeded', external, 'refund_details')
        smoke.oxsum(base, '/api/v1/webhooks/hyperswitch', raw, signature=signature)
        smoke.oxsum(base, '/api/v1/webhooks/hyperswitch', raw, signature=signature)
        final, _ = smoke.oxsum(base, path, {'reason': 'Local legacy reversal'}, token=admin)
        assert final['refundStatus'] == 'succeeded' and final['reversalEntryId'] == reversed_row['reversalEntryId']
        assert smoke.oxsum(base, '/api/v1/balance', session=customer)[0]['availableMinor'] == 0
        return {'tier': 'local-simulation-with-locally-signed-events', 'depositId': deposit,
                'providerPaymentId': payment, 'providerRefundId': external['refund_id'],
                'providerCurrency': 'USD', 'providerPaidMinor': 200, 'providerRefundedMinor': 200,
                'legacyWalletCreditMinor': 200, 'endingWalletMinor': 0,
                'refundRecovery': dict(stats), 'reversalEntryId': final['reversalEntryId'],
                'signedDuplicateEvents': 'one terminal effect; no additional wallet debit',
                'automaticCallback': 'unperformed', 'browserCheckout': 'unperformed',
                'pspCertification': 'unperformed: no real PSP credentials'}

    try:
        smoke.json.loads = load
        smoke.exercise = exercise
        sys.argv = ['smoke.py', '--binary', str(args.binary.resolve(strict=True))]
        smoke.main()
    finally:
        smoke.json.loads = original_loads
        smoke.exercise = original_exercise
        proxy.shutdown()
        proxy.server_close()
        worker.join(timeout=5)


if __name__ == '__main__':
    main()
