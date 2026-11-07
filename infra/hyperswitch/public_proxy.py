#!/usr/bin/env python3
"""Expose only fixture SDK/client API or an exact callback through a local tunnel."""
import argparse
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import re
import urllib.parse

MAX_BODY = 1_048_576
MAX_RESPONSE = 33_554_432
HOP_HEADERS = {'connection', 'keep-alive', 'transfer-encoding', 'upgrade',
               'proxy-authenticate', 'proxy-authorization', 'te', 'trailer'}


def allowed(mode, method, path, headers, callback_path=None):
    parsed = urllib.parse.urlsplit(path)
    if parsed.scheme or parsed.netloc or parsed.fragment:
        return False
    route = urllib.parse.unquote(parsed.path)
    if route != parsed.path or '..' in route.split('/') or '\\' in route:
        return False
    if mode == 'callback':
        return method == 'POST' and route == callback_path and not parsed.query
    if mode == 'sdk':
        return method in ('GET', 'HEAD') and (route == '/' or bool(re.fullmatch(
            r'/[A-Za-z0-9_./-]+\.(?:js|css|html|json|svg|png|jpg|jpeg|webp|gif|ico|woff|woff2|ttf)', route)))
    # Only SDK client operations are public. Secret/admin keys are refused
    # before forwarding; authenticated merchant operations stay on loopback.
    if not re.fullmatch(r'/(?:payments(?:/[A-Za-z0-9_-]+){0,3}|payment_methods|v1/sdk/configs/web/sdk_config.json)', route):
        return False
    if method == 'OPTIONS':
        return True
    return method in ('GET', 'POST') and not headers.get('authorization') and bool(
        re.fullmatch(r'pk_[A-Za-z0-9_-]{8,200}', headers.get('api-key', '')))


def serve(mode, port, target_port, callback_path=None):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def log_message(self, *_):
            # URLs may carry a client secret. Do not produce request logs.
            pass

        def handle_request(self):
            self.connection.settimeout(30)
            headers = {key.lower(): value for key, value in self.headers.items()}
            if not allowed(mode, self.command, self.path, headers, callback_path):
                self.send_error(404)
                return
            try:
                size = int(headers.get('content-length', '0'))
            except ValueError:
                self.send_error(400)
                return
            if not 0 <= size <= MAX_BODY or 'transfer-encoding' in headers:
                self.send_error(413)
                return
            body = self.rfile.read(size)
            outgoing = {key: value for key, value in headers.items()
                        if key not in HOP_HEADERS | {'host', 'accept-encoding', 'content-length', 'cookie'}}
            outgoing['host'] = f'localhost:{target_port}'
            outgoing['content-length'] = str(len(body))
            connection = http.client.HTTPConnection('127.0.0.1', target_port, timeout=30)
            try:
                connection.request(self.command, self.path, body=body, headers=outgoing)
                response = connection.getresponse()
                payload = response.read(MAX_RESPONSE + 1)
                if len(payload) > MAX_RESPONSE:
                    self.send_error(502)
                    return
                self.send_response(response.status)
                for key, value in response.getheaders():
                    if key.lower() not in HOP_HEADERS | {'content-length', 'set-cookie'}:
                        self.send_header(key, value)
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(payload)
            except (OSError, http.client.HTTPException):
                self.send_error(502)
            finally:
                connection.close()

        do_GET = do_HEAD = do_POST = do_OPTIONS = do_PUT = do_DELETE = handle_request

    server = ThreadingHTTPServer(('127.0.0.1', port), Handler)
    print(f'Fixture {mode} proxy listening on 127.0.0.1:{server.server_port}', flush=True)
    server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['sdk', 'api', 'callback'])
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--target-port', type=int, required=True)
    parser.add_argument('--callback-path')
    args = parser.parse_args()
    if args.mode == 'callback' and not re.fullmatch(
            r'/api/v1/webhooks/merchants/[a-f0-9-]{36}/versions/[a-f0-9-]{36}', args.callback_path or ''):
        parser.error('callback requires one exact scoped merchant version callback path')
    serve(args.mode, args.port, args.target_port, args.callback_path)


if __name__ == '__main__':
    main()
