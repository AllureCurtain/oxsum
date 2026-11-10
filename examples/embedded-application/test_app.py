"""Durable operation tests; fake transport never claims real provider certification."""
import json
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest
import uuid
from app import Application, RequestFailed, canonical, data_bytes


class FakeTransport:
    def __init__(self, application, organization):
        self.app = application
        self.org = organization
        self.calls = []
        self.fail = False
        self.key = str(uuid.uuid4())
        self.null_key = False

    def request(self, method, path, credential, body=None, headers=None):
        self.calls.append((method, path, credential, body, headers))
        if self.fail:
            raise OSError("lost response")
        if path.endswith("/customers"):
            request = json.loads(body)
            data = {"applicationId": self.app, "externalId": request["externalId"], "customer": {"organizationId": self.org}}
        elif path.endswith("/keys"):
            data = {"key": {"id": self.key}, "secret": None if self.null_key else "oxs-fixture-private"}
        elif path.endswith("/checkout-quote"):
            data = {"quoted": json.loads(body), "revision": len(self.calls)}
        elif path.endswith("/orders"):
            data = {"order": {"orderId": str(uuid.uuid4()), "status": "created"}, "clientSecret": "private-merchant-secret", "checkoutUrl": "https://pay.example/start"}
        elif path.endswith("/chat/completions"):
            return b'{"choices": [{"message": {"content": "ok"}}]}', {"x-oxsum-request-id": "request-fixture"}
        else:
            data = {"ok": True}
        return canonical({"data": data}).encode(), {}


class AppTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.path = Path(self.folder.name) / "app.sqlite"
        self.app_id = str(uuid.uuid4())
        self.org = str(uuid.uuid4())
        self.http = FakeTransport(self.app_id, self.org)
        self.clock = [1000]
        self.app = self.open()

    def open(self):
        return Application(self.path, "http://127.0.0.1:3000", self.app_id, "oxs-int-test", self.http, lambda: self.clock[0])

    def tearDown(self):
        self.app.close()
        self.folder.cleanup()

    def signup(self):
        return self.app.execute({"action": "signup", "customer": "alice", "operation": "signup-alice", "name": "Alice"})

    def key(self):
        return self.app.execute({"action": "key", "customer": "alice", "operation": "key-alice", "model": "fixture", "spendLimitMinor": 1000, "expiresAt": "2030-01-01T00:00:00Z"})

    def ask(self):
        return {"action": "ask", "customer": "alice", "operation": "question-one", "keyOperation": "key-alice", "model": "fixture", "prompt": "hello"}

    def test_restart_retains_exact_operation_and_mapping(self):
        self.http.fail = True
        with self.assertRaises(OSError): self.signup()
        self.assertEqual(self.app.db.execute("SELECT attempts FROM operations").fetchone()[0], 1)
        original = self.http.calls[-1][3]
        self.app.close(); self.app = self.open(); self.http.fail = False
        result = self.signup()
        self.assertEqual(result["organizationId"], self.org)
        self.assertEqual(self.http.calls[-1][3], original)
        before = len(self.http.calls)
        self.signup()
        self.assertEqual(len(self.http.calls), before)
        with self.assertRaises(ValueError): self.app.execute({"action": "signup", "customer": "bob", "operation": "signup-alice", "name": "Bob"})
        with self.assertRaises(ValueError): self.app.execute({"action": "signup", "customer": "alice", "operation": "signup-alice", "name": "Different"})

    def test_attempt_budget_and_gateway_window_survive_restart(self):
        self.signup(); self.key(); self.http.fail = True
        for _ in range(3):
            with self.assertRaises(OSError): self.app.execute(self.ask())
        self.app.close(); self.app = self.open()
        before = len(self.http.calls)
        with self.assertRaisesRegex(ValueError, "exhausted"): self.app.execute(self.ask())
        self.assertEqual(len(self.http.calls), before)
        other = {**self.ask(), "operation": "unknown-second"}
        with self.assertRaises(OSError): self.app.execute(other)
        self.clock[0] += 23 * 3600
        with self.assertRaisesRegex(ValueError, "replay window"): self.app.execute(other)
        self.assertEqual(self.app.db.execute("SELECT attempts FROM operations WHERE id='unknown-second'").fetchone()[0], 1)

    def test_key_secret_is_private_and_lost_response_cannot_mint_replacement(self):
        self.signup()
        result = self.key()
        self.assertNotIn("secret", canonical(result))
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.app.key("key-alice", "alice")[0], "oxs-fixture-private")
        with self.assertRaises(ValueError): self.app.key("key-alice", "bob")
        self.http.null_key = True
        r = {"action": "key", "customer": "alice", "operation": "lost-key", "model": "fixture", "spendLimitMinor": 1000, "expiresAt": "2030-01-01T00:00:00Z"}
        with self.assertRaisesRegex(ValueError, "one-time"): self.app.execute(r)
        before = len(self.http.calls)
        with self.assertRaisesRegex(ValueError, "one-time"): self.app.execute(r)
        self.assertEqual(len(self.http.calls), before)

    def test_checkout_keeps_frozen_quote_and_never_claims_pending_paid(self):
        self.signup()
        r = {"action": "checkout", "customer": "alice", "operation": "wallet", "selection": {"purpose": "walletTopup", "rail": "hyperswitch", "displayAmountMinor": 100}, "returnUrl": "https://app.example/ok", "cancelUrl": "https://app.example/cancel"}
        result = self.app.execute(r)
        self.assertFalse(result["paid"])
        self.assertNotIn("private-merchant-secret", canonical(result))
        before = len(self.http.calls)
        self.app.close(); self.app = self.open()
        self.assertEqual(self.app.execute(r), result)
        self.assertEqual(len(self.http.calls), before)
        with self.assertRaises(ValueError): self.app.execute({**r, "returnUrl": "https://app.example/changed"})

    def test_competing_processes_send_one_durable_operation(self):
        counter = Path(self.folder.name) / "sends.txt"
        code = r"""
import sys,time
sys.path.insert(0,sys.argv[1])
from app import Application,canonical
class Transport:
 def request(self,*args,**kwargs):
  with open(sys.argv[5],'a') as f:f.write('sent\n')
  time.sleep(.1)
  return canonical({'data':{'applicationId':sys.argv[3],'externalId':'racer','customer':{'organizationId':sys.argv[4]}}}).encode(),{}
a=Application(sys.argv[2],'http://127.0.0.1:3000',sys.argv[3],'oxs-int-test',Transport())
try:a.execute({'action':'signup','customer':'racer','operation':'racer-signup','name':'Racer'})
finally:a.close()
"""
        command = [sys.executable, "-c", code, str(Path(__file__).parent), str(self.path), self.app_id, self.org, str(counter)]
        children = [subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(2)]
        for child in children:
            stdout, stderr = child.communicate(timeout=10)
            self.assertEqual(child.returncode, 0, stderr.decode())
        self.assertEqual(counter.read_text(), "sent\n")

    def test_store_binding_trust_and_lossless_envelope(self):
        self.signup()
        r = {"action": "trust", "customer": "alice", "organizationId": self.org, "origin": "oxsum/ledgers/fixture", "key": {"keyName": "fixture", "publicKey": "a"*64, "keyHash": "a"*8}}
        self.app.execute(r)
        with self.assertRaises(ValueError): self.app.execute({**r, "origin": "oxsum/ledgers/other"})
        with self.assertRaises(ValueError): self.app.execute({**r, "organizationId": str(uuid.uuid4())})
        self.assertEqual(data_bytes(b'{ "data" : {"big": 9223372036854775807} }'), b'{"big": 9223372036854775807}')
        with self.assertRaises(ValueError): Application(self.path, "http://127.0.0.1:3001", self.app_id, "oxs-int-test")



class CheckoutTests(unittest.TestCase):
    def test_checkout_markup_escapes_provider_values_and_refuses_unsafe_urls(self):
        from checkout import document
        value = {"order": {"orderId": str(uuid.uuid4()), "status": "provider_pending"}, "sdkUrl": "https://sdk.example", "clientSecret": "</script><script>bad()</script>", "publishableKey": "pk-test"}
        rendered = document(value, "fixture").decode()
        self.assertNotIn("<script>bad()", rendered)
        self.assertIn("\\u003c/script", rendered)
        with self.assertRaises(ValueError): document({**value, "sdkUrl": "http://private.example"}, "fixture")
        with self.assertRaises(ValueError): document({**value, "checkoutUrl": "javascript:alert(1)"}, "fixture")
        rendered = document({"order": {"orderId": "fixture", "status": "fulfilled"}}, "fixture")
        self.assertIn(b"Payment fulfilled", rendered)


if __name__ == "__main__": unittest.main()
