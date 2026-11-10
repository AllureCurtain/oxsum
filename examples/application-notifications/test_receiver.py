"""Independent exact HMAC, durable inbox and current-state reconciliation checks."""
import hashlib
import hmac
import json
from pathlib import Path
import sqlite3
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

    def test_statement_signal_fetches_exact_current_statement(self):
        value, _, _ = self.signal()
        value["type"] = "statement.changed"
        value["entityKind"] = "statement"
        value["entityId"] = str(uuid.uuid4())
        body = json.dumps(value).encode()
        signature = "t=1000,v1=" + hmac.new(self.secret.encode(), b"1000." + body, hashlib.sha256).hexdigest()
        self.accept(value, body, signature)
        seen = []
        def fetch(path):
            seen.append(path)
            return {"data": {"state": "void", "lines": []}}
        self.receiver.fetch = fetch
        self.assertEqual(self.receiver.reconcile(), 1)
        self.assertEqual(seen, ["/integration/customers/" + self.org + "/statements/" + value["entityId"]])

    def test_old_signal_fetches_current_archived_state_and_failed_fetch_stays_pending(self):
        first = self.signal(created="new")
        self.accept(*first)
        def failed(_):
            raise OSError("unavailable")
        self.receiver.fetch = failed
        self.assertEqual(self.receiver.reconcile(now=1000), 0)
        self.assertIsNone(self.receiver.db.execute("SELECT processed_at FROM inbox").fetchone()[0])
        self.receiver.fetch = lambda _: {"data": {"state": "archived"}}
        self.assertEqual(self.receiver.reconcile(now=1004), 0)
        self.assertEqual(self.receiver.reconcile(now=1005), 1)
        self.accept(*self.signal(created="old"))
        self.assertEqual(self.receiver.reconcile(), 1)
        state = json.loads(self.receiver.db.execute("SELECT document FROM snapshots").fetchone()[0])
        self.assertEqual(state["data"]["state"], "archived")
        self.accept(*first)
        self.assertEqual(self.receiver.reconcile(), 0)

    def test_full_failed_batch_does_not_starve_later_customer(self):
        for _ in range(20):
            self.accept(*self.signal())
        self.receiver.fetch = lambda _: (_ for _ in ()).throw(OSError("temporarily unavailable"))
        self.assertEqual(self.receiver.reconcile(now=1000), 0)
        self.org = str(uuid.uuid4())
        self.accept(*self.signal())
        self.receiver.fetch = lambda _: {"data": {"customerState": "archived"}}
        self.assertEqual(self.receiver.reconcile(now=1000), 1)
        self.assertEqual(self.receiver.db.execute("SELECT organization_id FROM snapshots").fetchone()[0], self.org)
        self.assertEqual(self.receiver.db.execute("SELECT count(*) FROM inbox WHERE processed_at IS NULL").fetchone()[0], 20)
        self.assertEqual(self.receiver.reconcile(now=1005), 20)

    def test_legacy_inbox_upgrade_retains_pending_bytes(self):
        path = str(Path(self.folder.name) / "legacy.db")
        value, body, _ = self.signal()
        with sqlite3.connect(path) as db:
            db.execute("CREATE TABLE inbox(delivery_id TEXT PRIMARY KEY,body BLOB NOT NULL,received_at INTEGER NOT NULL,processed_at INTEGER)")
            db.execute("INSERT INTO inbox VALUES(?,?,?,NULL)", (value["id"], body, 1000))
        db.close()
        receiver = Receiver(path, self.app, self.secret, "http://127.0.0.1:3000", "oxs-int-fixture")
        try:
            self.assertEqual(receiver.db.execute("SELECT body,attempts,retry_at FROM inbox").fetchone(), (body, 0, 0))
            receiver.fetch = lambda _: {"data": {"customerState": "archived"}}
            self.assertEqual(receiver.reconcile(now=1000), 1)
        finally:
            receiver.db.close()


if __name__ == "__main__":
    unittest.main()
