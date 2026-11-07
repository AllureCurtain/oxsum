#!/usr/bin/env python3
"""Inject lost switch responses into the isolated #757 smoke (local dummy only)."""
import argparse
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sandbox', type=Path, required=True, help='Directory containing the pinned #757 stack tools')
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
    target_port = config['ports']['ROUTER']
    stats = {'paymentPosts': 0, 'refundPosts': 0, 'droppedResponses': 0,
             'paymentReads': 0, 'refundReads': 0, 'replayedRefundReads': 0}
    lock = threading.Lock()

    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def dispatch(self):
            # No incoming URL/header/client-secret logging. This proxy remains
            # loopback-only and exists only for this single subprocess lifetime.
            self.connection.settimeout(30)
            body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
            if self.path.startswith('/payments/') and self.command == 'GET':
                stats['paymentReads'] += 1
            if self.path.startswith('/refunds/') and self.command == 'GET':
                stats['refundReads'] += 1
            connection = http.client.HTTPConnection('127.0.0.1', target_port, timeout=30)
            try:
                headers = {k: v for k, v in self.headers.items()
                           if k.lower() not in ('host', 'connection', 'content-length')}
                headers['Content-Length'] = str(len(body))
                connection.request(self.command, self.path, body, headers)
                response = connection.getresponse()
                payload = response.read()
                drop = self.command == 'POST' and self.path in ('/payments', '/refunds')
                if drop:
                    with lock:
                        stats['paymentPosts' if self.path == '/payments' else 'refundPosts'] += 1
                        stats['droppedResponses'] += 1
                    assert response.status == 200, f'Local switch create status {response.status}'
                    value = json.loads(payload)
                    if self.path == '/refunds':
                        # Validate that the exact server body ID was honored and
                        # an explicit duplicate POST never creates another refund.
                        original = json.loads(body)
                        assert value['refund_id'] == original['refund_id']
                        duplicate = http.client.HTTPConnection('127.0.0.1', target_port, timeout=30)
                        try:
                            duplicate.request('POST', '/refunds', body, headers)
                            duplicate_response = duplicate.getresponse()
                            duplicate_payload = json.loads(duplicate_response.read())
                            assert duplicate_response.status == 400 and duplicate_payload['error']['code'] == 'HE_01'
                        finally:
                            duplicate.close()
                        # Multiple reads of the same external operation preserve
                        # identity and amount, even after a duplicate rejection.
                        for _ in range(2):
                            observed = merchant.request(config, 'GET', '/refunds/' + value['refund_id'], merchant_key=state['apiKey'])
                            assert observed['refund_id'] == value['refund_id'] and observed['amount'] == value['amount']
                            stats['replayedRefundReads'] += 1
                    # Close before returning the successful body; oxsum must GET
                    # the precommitted identity and validate the retained object.
                    self.close_connection = True
                    return
                self.send_response(response.status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            finally:
                connection.close()

        do_GET = do_POST = dispatch

    proxy = ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
    worker = threading.Thread(target=proxy.serve_forever, daemon=True)
    worker.start()
    original_exercise = smoke.exercise
    original_loads = smoke.json.loads
    # Override only the in-memory config read by smoke.main; never change the
    # sandbox's retained origin, encryption identity or generated env files.
    def load(text, *rest, **kwargs):
        value = original_loads(text, *rest, **kwargs)
        if isinstance(value, dict) and value.get('project') == config['project'] and 'ports' in value:
            value['ports']['ROUTER'] = proxy.server_port
        return value

    def exercise(*params):
        result = original_exercise(*params)
        assert stats['paymentPosts'] == 1 and stats['refundPosts'] == 1
        assert stats['droppedResponses'] == 2 and stats['replayedRefundReads'] == 2
        assert stats['paymentReads'] >= 2 and stats['refundReads'] >= 3
        result['recoveryFaults'] = dict(stats)
        result['explicitDuplicateRefund'] = 'HE_01; original ID/amount preserved'
        return result

    try:
        smoke.json.loads = load
        smoke.exercise = exercise
        sys.argv = ['smoke.py', '--binary', str(args.binary.resolve(strict=True))]
        smoke.main()
    finally:
        smoke.json.loads = original_loads
        proxy.shutdown()
        proxy.server_close()
        worker.join(timeout=5)


if __name__ == '__main__':
    main()
