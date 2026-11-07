#!/usr/bin/env python3
"""Provision a private local merchant and explicitly simulated payment connector."""
import argparse
import fcntl
import json
import re
import secrets
import urllib.error
import urllib.parse
import urllib.request

import stack


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def request(config, method, path, body=None, *, merchant_key=None, missing=False):
    # Credentials go only to this sandbox's loopback-published router. Public
    # origins configure the browser; they never redirect administrative calls.
    url = f'http://127.0.0.1:{config["ports"]["ROUTER"]}' + path
    headers = {'Content-Type': 'application/json'}
    if merchant_key:
        headers['api-key'] = merchant_key
    else:
        headers['Authorization'] = 'admin-api-key=' + config['adminKey']
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open(req, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        # v1.129 routes missing merchants through authentication, so the
        # lookup returns 401. Creation still authenticates the admin key and
        # rejects an existing ID; it never replaces an existing account.
        if missing and error.code in (401, 404):
            return None
        # Never include provider response bodies or credential-bearing headers.
        raise SystemExit(f'Local Hyperswitch {method} {path} returned HTTP {error.code}. Inspect private service logs.') from None
    except (urllib.error.URLError, TimeoutError):
        raise SystemExit(f'Local Hyperswitch {method} {path} outcome is unknown. Preserve state and reconcile before retrying.') from None


def save(state):
    stack.private_file(stack.RUNTIME / 'merchant.json', json.dumps(state, indent=2) + '\n')


def state_file():
    path = stack.RUNTIME / 'merchant.json'
    if path.exists():
        return json.loads(path.read_text())
    state = {'merchantId': 'oxsum_fixture_' + secrets.token_hex(6),
             'webhookSecret': secrets.token_hex(32), 'apiKeyName': 'oxsum-fixture-' + secrets.token_hex(6)}
    save(state)
    return state


def provision(config, state):
    merchant_id = state['merchantId']
    account = request(config, 'GET', '/accounts/' + merchant_id, missing=True)
    if account is None:
        account = request(config, 'POST', '/accounts', {
            'merchant_id': merchant_id, 'merchant_name': 'Oxsum local payment fixture',
            'enable_payment_response_hash': True, 'payment_response_hash_key': state['webhookSecret'],
        })
    if account.get('payment_response_hash_key') != state['webhookSecret']:
        raise SystemExit('Merchant signing identity differs; refusing to overwrite it.')
    state['publishableKey'] = account['publishable_key']
    state['profileId'] = account['default_profile']
    save(state)
    if 'apiKey' not in state:
        existing = request(config, 'GET', f'/api_keys/{merchant_id}/list')
        if any(key['name'] == state['apiKeyName'] for key in existing):
            raise SystemExit('A key exists but its one-time value was not retained. Revoke that exact key and reconcile before retrying.')
        key = request(config, 'POST', '/api_keys/' + merchant_id, {
            'name': state['apiKeyName'], 'description': 'Disposable local fixture only', 'expiration': 'never',
        })
        state.update(apiKey=key['api_key'], keyId=key['key_id'])
        save(state)
    connector_label = 'oxsum_local_simulation'
    connectors = request(config, 'GET', f'/account/{merchant_id}/connectors', merchant_key=state['apiKey'])
    connector = next((row for row in connectors if row['connector_label'] == connector_label), None)
    if connector is None:
        connector = request(config, 'POST', f'/account/{merchant_id}/connectors', {
            'connector_type': 'payment_processor', 'connector_name': 'paypal_test',
            'connector_label': connector_label, 'profile_id': state['profileId'],
            'disabled': False, 'test_mode': True, 'status': 'active',
            'connector_account_details': {'auth_type': 'HeaderKey', 'api_key': 'local_fixture_only'},
            'payment_methods_enabled': [{'payment_method': 'card', 'payment_method_types': [
                {'payment_method_type': kind, 'card_networks': ['Visa', 'Mastercard'],
                 'minimum_amount': 1, 'maximum_amount': 10000,
                 'recurring_enabled': False, 'installment_payment_enabled': False}
                for kind in ('debit', 'credit')]}],
        }, merchant_key=state['apiKey'])
    if connector['connector_name'] != 'paypal_test' or connector['profile_id'] != state['profileId']:
        raise SystemExit('Existing connector identity differs; refusing to replace it.')
    state['connectorId'] = connector['merchant_connector_id']
    state['tier'] = 'local-simulation-not-psp'
    save(state)
    print('Local simulated merchant/profile/key/connector ready; private values retained in .runtime/merchant.json.')


def callback(config, state, url):
    # Hyperswitch additionally validates DNS and destination addresses itself.
    parsed = urllib.parse.urlsplit(url)
    stack.public_origin(urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, '', '', '')))
    if parsed.query or parsed.fragment or not parsed.path.startswith('/api/v1/'):
        raise SystemExit('Callback must be an HTTPS /api/v1/ URL without query or fragment.')
    request(config, 'POST', f'/account/{state["merchantId"]}/business_profile/{state["profileId"]}', {
        'enable_payment_response_hash': True, 'payment_response_hash_key': state['webhookSecret'],
        'webhook_details': {'webhook_url': url, 'payment_created_enabled': True,
                            'payment_succeeded_enabled': True, 'payment_failed_enabled': True,
                            'refund_statuses_enabled': ['success', 'failure']},
    }, merchant_key=state['apiKey'])
    state['callbackUrl'] = url
    save(state)
    print('Callback stored on the local merchant profile; delivery still requires an actual fixture event.')


def inspect(config, state):
    account = request(config, 'GET', '/accounts/' + state['merchantId'])
    profiles = request(config, 'GET', f'/account/{state["merchantId"]}/business_profile', merchant_key=state['apiKey'])
    connectors = request(config, 'GET', f'/account/{state["merchantId"]}/connectors', merchant_key=state['apiKey'])
    keys = request(config, 'GET', f'/api_keys/{state["merchantId"]}/list')
    assert account['publishable_key'] == state['publishableKey']
    assert account['payment_response_hash_key'] == state['webhookSecret']
    assert any(row['profile_id'] == state['profileId'] for row in profiles)
    assert any(row['merchant_connector_id'] == state['connectorId'] for row in connectors)
    assert any(row['key_id'] == state['keyId'] for row in keys)
    print(json.dumps({'merchantId': state['merchantId'], 'profileId': state['profileId'],
                      'connectorId': state['connectorId'], 'tier': state['tier'],
                      'identityChecks': 'passed', 'callbackConfigured': 'callbackUrl' in state}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['provision', 'inspect', 'callback'])
    parser.add_argument('--url')
    args = parser.parse_args()
    config = json.loads((stack.RUNTIME / 'private.json').read_text())
    if not re.fullmatch(r'oxsum-hs-[a-f0-9]{8}', config['project']):
        raise SystemExit('Unrecognized sandbox project; no request sent.')
    with (stack.RUNTIME / 'merchant.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = state_file()
        if args.command == 'provision':
            provision(config, state)
        elif args.command == 'callback':
            if not args.url:
                parser.error('callback requires --url')
            callback(config, state, args.url)
        else:
            inspect(config, state)


if __name__ == '__main__':
    main()
