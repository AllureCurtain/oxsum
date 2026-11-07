"""Resource/secret boundaries for the disposable switch tools (no Docker needed)."""
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch
import urllib.request

import merchant
import public_proxy
import stack


class SandboxTests(unittest.TestCase):
    def test_private_state_is_atomic_and_not_world_readable(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'private.json'
            for value in ['first', 'replacement']:
                stack.private_file(path, value)
                self.assertEqual(path.read_text(), value)
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual([p.name for p in Path(directory).iterdir()], ['private.json'])

    def test_compose_uses_only_the_named_project_and_pins(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'private.json').write_text(json.dumps({'project': 'oxsum-hs-1234abcd'}))
            with patch.object(stack, 'RUNTIME', root), patch.object(stack, 'run') as run:
                with patch.dict(os.environ, {'HS_ROUTER_IMAGE': 'untrusted:latest', 'COMPOSE_PROJECT_NAME': 'other'}):
                    stack.compose(['config', '--quiet'])
                args, kwargs = run.call_args
                self.assertEqual(args[0][2:4], ['--project-name', 'oxsum-hs-1234abcd'])
                self.assertNotIn('HS_ROUTER_IMAGE', kwargs['env'])
                self.assertNotIn('COMPOSE_PROJECT_NAME', kwargs['env'])
                (root / 'private.json').write_text(json.dumps({'project': 'other'}))
                with self.assertRaises(SystemExit):
                    stack.compose(['stop'])
                self.assertEqual(run.call_count, 1)

    def test_all_images_and_source_are_immutable(self):
        for image in stack.LOCK['images'].values():
            self.assertRegex(image, r'@sha256:[a-f0-9]{64}$')
        self.assertRegex(stack.LOCK['source']['commit'], r'^[a-f0-9]{40}$')
        for key in ('arm64', 'amd64'):
            self.assertRegex(stack.LOCK['diesel'][key]['sha256'], r'^[a-f0-9]{64}$')

    def test_origins_cannot_carry_credentials_or_extra_paths(self):
        for value in ['http://example.com', 'https://user:key@example.com', 'https://example.com/sdk',
                      'https://example.com?x=1', 'https://example.com/#fragment']:
            with self.assertRaises(ValueError):
                stack.public_origin(value)
        self.assertEqual(stack.public_origin('https://sdk.example/'), 'https://sdk.example')

    def test_public_api_never_forwards_admin_or_merchant_keys(self):
        client = {'api-key': 'pk_snd_fixture123'}
        for path in ['/accounts', '/api_keys/merchant/list', '/user', '/account/m/connectors', '/health',
                     '/payments/../accounts', '/payments/%2e%2e/accounts', '//evil/payments']:
            self.assertFalse(public_proxy.allowed('api', 'GET', path, client), path)
        for headers in [{}, {'api-key': 'snd_secret_fixture'}, {'authorization': 'admin-api-key=secret'},
                        {**client, 'authorization': 'Bearer token'}]:
            self.assertFalse(public_proxy.allowed('api', 'POST', '/payments/pay_123/confirm', headers))
        self.assertTrue(public_proxy.allowed('api', 'POST', '/payments/pay_123/confirm', client))
        self.assertTrue(public_proxy.allowed('api', 'OPTIONS', '/payments/pay_123', {}))
        self.assertFalse(public_proxy.allowed('api', 'DELETE', '/payments/pay_123', client))

    def test_sdk_and_callback_routes_are_separate(self):
        self.assertTrue(public_proxy.allowed('sdk', 'GET', '/HyperLoader.js', {}))
        self.assertFalse(public_proxy.allowed('sdk', 'POST', '/HyperLoader.js', {}))
        for path in ['/.env', '/package.json/../.env', '/%2eenv', '/payments', '/src/app.map']:
            self.assertFalse(public_proxy.allowed('sdk', 'GET', path, {}))
        callback = '/api/v1/webhooks/merchants/one/versions/two'
        self.assertTrue(public_proxy.allowed('callback', 'POST', callback, {}, callback))
        self.assertFalse(public_proxy.allowed('callback', 'GET', callback, {}, callback))
        self.assertFalse(public_proxy.allowed('callback', 'POST', callback+'?other=1', {}, callback))
        self.assertFalse(public_proxy.allowed('callback', 'POST', '/api/v1/admin', {}, callback))

    def test_administrative_client_refuses_redirects(self):
        request = urllib.request.Request('http://127.0.0.1:18080/accounts')
        self.assertIsNone(merchant.NoRedirect().redirect_request(request, None, 302, '', {}, 'https://outside.example/'))


if __name__ == '__main__':
    unittest.main()
