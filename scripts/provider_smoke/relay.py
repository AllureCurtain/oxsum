"""Fixed DeepSeek TLS relay; every external attempt is durably bounded first."""
import hashlib
import hmac
import http.client
import http.server
import ipaddress
import json
import socket
import ssl
import time

from provider_smoke.budget import MODEL, PROMPT, OUTPUT_BOUND, require

HOST = 'api.deepseek.com'
PATH = '/chat/completions'
MAX_RESPONSE = 1024 * 1024


def public_address(value):
    address = ipaddress.ip_address(value)
    require(address.version == 4 and address.is_global and not address.is_multicast
            and not address.is_reserved and not address.is_unspecified, 'A public IPv4 address is required')
    return str(address)


def payload(stream):
    value = {'model': MODEL, 'messages': [{'role': 'user', 'content': PROMPT}],
             'max_tokens': OUTPUT_BOUND, 'thinking': {'type': 'disabled'}, 'stream': stream}
    if stream:
        value['stream_options'] = {'include_usage': True}
    return value


def validate_payload(raw):
    require(len(raw) <= 512, 'Request exceeds the fixed input bound')
    def unique(pairs):
        value = {}
        for key, item in pairs:
            require(key not in value, 'Duplicate request field')
            value[key] = item
        return value
    value = json.loads(raw, object_pairs_hook=unique)
    require(isinstance(value, dict) and type(value.get('stream')) is bool, 'Explicit stream mode required')
    require(type(value.get('max_tokens')) is int and value == payload(value['stream']), 'Only the fixed bounded non-thinking prompt is allowed')
    return value


def usage_from(raw, stream):
    if not stream:
        value = json.loads(raw)
        return value.get('usage') if isinstance(value, dict) else None
    usage = None
    for line in raw.decode().splitlines():
        if line.startswith('data: ') and line != 'data: [DONE]':
            value = json.loads(line[6:])
            if isinstance(value, dict) and isinstance(value.get('usage'), dict):
                usage = value['usage']
    return usage


class PinnedTLS(http.client.HTTPSConnection):
    """Connect only to the validated address, retaining hostname/certificate checks."""
    def __init__(self, address):
        super().__init__(HOST, timeout=20, context=ssl.create_default_context())
        self.address = public_address(address)

    def connect(self):
        raw = socket.create_connection((self.address, 443), timeout=self.timeout)
        try:
            self.sock = self._context.wrap_socket(raw, server_hostname=HOST)
        except BaseException:
            raw.close()
            raise


class Guard(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *_):
        pass

    def refuse(self, code):
        self.send_response(code)
        self.send_header('content-length', '0')
        self.send_header('connection', 'close')
        self.end_headers()
        self.close_connection = True

    def do_POST(self):
        attempt = None
        connection = None
        begun = False
        try:
            require(self.path == '/v1/chat/completions', 'Unknown relay path')
            require(hmac.compare_digest(self.headers.get('authorization', ''), 'Bearer '+self.server.local_token),
                    'Wrong local relay credential')
            require(self.headers.get('transfer-encoding') is None and len(self.headers.get_all('content-length', [])) == 1,
                    'Explicit single content length required')
            size = int(self.headers['content-length'])
            require(0 < size <= 512, 'Invalid request size')
            self.connection.settimeout(20)
            raw = self.rfile.read(size)
            require(len(raw) == size, 'Incomplete request')
            body = validate_payload(raw)
            protocol = 'sse' if body['stream'] else 'whole'
            # The wire protocol is whole/sse; the booked scenario may carry an
            # artifact tag (whole-candidate). The request must match the active
            # protocol, and the reservation books the tagged scenario name.
            active = self.server.active_scenario
            require(isinstance(active, str) and active.split('-', 1)[0] == protocol, 'Scenario is not active')
            encoded = json.dumps(body, separators=(',', ':')).encode()
            attempt = self.server.budget.reserve(active, self.server.run_id, hashlib.sha256(encoded).hexdigest())
            # No socket or credential is sent before the durable reservation returns.
            connection = self.server.transport()
            connection.request('POST', PATH, body=encoded, headers={
                'Content-Type': 'application/json', 'Authorization': 'Bearer '+self.server.provider_key,
                'Accept': 'text/event-stream' if body['stream'] else 'application/json'})
            response = connection.getresponse()
            require(200 <= response.status <= 599, 'Unexpected provider status')
            self.send_response(response.status)
            self.send_header('content-type', 'text/event-stream' if body['stream'] and response.status == 200 else 'application/json')
            self.send_header('connection', 'close')
            # Deliberately omit Retry-After: an external refusal ends this exercise.
            self.end_headers()
            begun = True
            self.close_connection = True
            chunks = bytearray()
            deadline = time.monotonic() + 60
            connected = True
            while True:
                chunk = response.read1(8192)
                if not chunk:
                    break
                require(time.monotonic() <= deadline and len(chunks)+len(chunk) <= MAX_RESPONSE, 'Provider response bound exceeded')
                chunks.extend(chunk)
                if connected:
                    try:
                        self.wfile.write(chunk)
                        self.wfile.flush()
                    except (BrokenPipeError, ConnectionResetError):
                        connected = False  # Finish accounting; never repeat the inference.
            usage = usage_from(bytes(chunks), body['stream']) if response.status == 200 else None
            self.server.budget.finish(attempt, 'http_'+str(response.status), usage)
            attempt = None
        except Exception:
            if attempt:
                # If this write fails, the existing reservation remains charged.
                try:
                    self.server.budget.finish(attempt, 'ambiguous')
                except Exception:
                    pass
            if not begun:
                self.refuse(400)
        finally:
            if connection:
                connection.close()
