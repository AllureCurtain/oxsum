"""Independent exact HMAC, durable inbox and current-state reconciliation checks."""
import hashlib
import hmac
import json
from pathlib import Path
import tempfile
import unittest
import uuid
from receiver import Receiver, verify


class ReceiverTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.app = str(uuid.uuid4())
        self.org = str(uuid.uuid4())
        self.secret = "whsec-" + "1" * 48
        self.receiver = Receiver(str(Path(self.folder.name) / "inbox.db"), self.app, self.secret, "http://127.0.0.1:3000", "oxs-int-fixture")

    def tearDown(self):
        self.receiver.db.close()
        self.folder.cleanup()

    def signal(self, stamp=1000, created="new"):
        value = dict(version=1, id=str(uuid.uuid4()), type="customer.changed", applicationId=self.app, organizationId=self.org, entityId=self.org, entityKind="customer", action="readCurrentState", createdAt=created)
        body = json.dumps(value).encode()
        signature = hmac.new(self.secret.encode(), str(stamp).encode() + b"." + body, hashlib.sha256).hexdigest()
        return value, body, f"t={stamp},v1={signature}"

    def accept(self, value, body, signature):
        self.receiver.accept(signature, value["id"], value["type"], body, now=1000)

    def test_signature_precedes_parse_and_rejects_tamper_old_wrong_key(self):
        value, body, signature = self.signal()
        for other in (body + b" ", b"not json"):
            with self.assertRaises(ValueError):
                self.accept(value, other, signature)
        with self.assertRaises(ValueError):
            verify("wrong", signature, body, 1000)
        with self.assertRaises(ValueError):
            verify(self.secret, signature, body, 1400)
        self.assertEqual(self.receiver.db.execute("SELECT count(*) FROM inbox").fetchone()[0], 0)

    def test_durable_duplicate_and_identity_conflict(self):
        value, body, signature = self.signal()
        self.accept(value, body, signature)
        self.accept(value, body, signature)
        self.assertEqual(self.receiver.db.execute("SELECT count(*) FROM inbox").fetchone()[0], 1)
        changed = body + b" "
        mac = hmac.new(self.secret.encode(), b"1000." + changed, hashlib.sha256).hexdigest()
        with self.assertRaises(ValueError):
            self.accept(value, changed, "t=1000,v1=" + mac)
        other = Receiver(str(Path(self.folder.name) / "inbox.db"), self.app, self.secret, "http://127.0.0.1:3000", "oxs-int-fixture")
        self.assertEqual(other.db.execute("SELECT count(*) FROM inbox").fetchone()[0], 1)
        other.db.close()

    def test_old_signal_fetches_current_archived_state_and_failed_fetch_stays_pending(self):
        first = self.signal(created="new")
        self.accept(*first)
        def failed(_):
            raise OSError("unavailable")
        self.receiver.fetch = failed
        with self.assertRaises(OSError):
            self.receiver.reconcile()
        self.assertIsNone(self.receiver.db.execute("SELECT processed_at FROM inbox").fetchone()[0])
        self.receiver.fetch = lambda _: {"data": {"state": "archived"}}
        self.assertEqual(self.receiver.reconcile(), 1)
        self.accept(*self.signal(created="old"))
        self.assertEqual(self.receiver.reconcile(), 1)
        state = json.loads(self.receiver.db.execute("SELECT document FROM snapshots").fetchone()[0])
        self.assertEqual(state["data"]["state"], "archived")
        self.accept(*first)
        self.assertEqual(self.receiver.reconcile(), 0)


if __name__ == "__main__":
    unittest.main()
