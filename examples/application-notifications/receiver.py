#!/usr/bin/env python3
"""Signed lifecycle receiver with a separate durable SQLite inbox and scoped reads."""
import argparse
import hashlib
import hmac
import http.server
import json
import os
import sqlite3
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

EVENTS = {
    "customer.changed", "key.changed", "entitlement.changed", "order.changed",
    "statement.changed", "evidence.published", "balance.changed",
}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("redirect refused")


def validate_base(base):
    parsed = urllib.parse.urlsplit(base)
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("base URL may not contain credentials, query or fragment")
    if parsed.scheme != "https" and not (
        parsed.scheme == "http" and parsed.hostname in ("localhost", "127.0.0.1", "::1")
    ):
        raise ValueError("use HTTPS or explicitly local loopback HTTP")
    if not parsed.hostname:
        raise ValueError("missing host")
    return base.rstrip("/")


def verify(secret, header, body, now):
    parts = header.split(",")
    if len(parts) != 2 or not parts[0].startswith("t=") or not parts[1].startswith("v1="):
        raise ValueError("invalid signature")
    stamp = parts[0][2:]
    if not stamp.isdecimal() or abs(now - int(stamp)) > 300:
        raise ValueError("signature timestamp outside five-minute window")
    expected = hmac.new(secret.encode(), stamp.encode() + b"." + body, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, parts[1][3:]):
        raise ValueError("invalid signature")


class Receiver:
    def __init__(self, path, application, signing_secret, base, credential):
        self.application = str(uuid.UUID(application))
        self.signing_secret = signing_secret
        self.base = validate_base(base)
        self.credential = credential
        self.db = sqlite3.connect(path)
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS inbox (
            delivery_id TEXT PRIMARY KEY, body BLOB NOT NULL,
            received_at INTEGER NOT NULL, processed_at INTEGER,
            attempts INTEGER NOT NULL DEFAULT 0,
            retry_at INTEGER NOT NULL DEFAULT 0
          );
          CREATE TABLE IF NOT EXISTS snapshots (
            organization_id TEXT NOT NULL, resource TEXT NOT NULL,
            document TEXT NOT NULL, fetched_at INTEGER NOT NULL,
            PRIMARY KEY(organization_id,resource)
          );
        """)
        # Upgrade retained inboxes in place; never discard pending signed bytes.
        columns = {row[1] for row in self.db.execute("PRAGMA table_info(inbox)")}
        with self.db:
            for name in ("attempts", "retry_at"):
                if name not in columns:
                    self.db.execute(f"ALTER TABLE inbox ADD COLUMN {name} INTEGER NOT NULL DEFAULT 0")
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect)

    def accept(self, header, delivery, event, body, now=None):
        now = int(time.time()) if now is None else now
        if len(body) > 4096:
            raise ValueError("notification exceeds bound")
        verify(self.signing_secret, header, body, now)
        value = json.loads(body)
        required = {"version", "id", "type", "applicationId", "organizationId", "entityId", "entityKind", "action", "createdAt"}
        if not isinstance(value, dict) or set(value) != required:
            raise ValueError("invalid signal envelope")
        if value["version"] != 1 or value["applicationId"] != self.application or value["action"] != "readCurrentState":
            raise ValueError("wrong application or signal version")
        if value["type"] not in EVENTS or event != value["type"] or delivery != value["id"]:
            raise ValueError("signal header differs")
        for name in ("id", "organizationId", "entityId"):
            if str(uuid.UUID(value[name])) != value[name]:
                raise ValueError("invalid signal identity")
        previous = self.db.execute("SELECT body FROM inbox WHERE delivery_id=?", (delivery,)).fetchone()
        if previous is not None:
            if previous[0] != body:
                raise ValueError("delivery identity reused with different bytes")
            return
        with self.db:
            self.db.execute("INSERT INTO inbox(delivery_id,body,received_at) VALUES(?,?,?)", (delivery, body, now))

    def fetch(self, path):
        request = urllib.request.Request(self.base + "/api/v1" + path, headers={"Authorization": "Bearer " + self.credential})
        try:
            response = self.http.open(request, timeout=20)
        except urllib.error.HTTPError as error:
            error.close()
            raise OSError("current resource unavailable") from None
        with response:
            raw = response.read(16 * 1024 * 1024 + 1)
        if len(raw) > 16 * 1024 * 1024:
            raise ValueError("current state exceeds bound")
        return json.loads(raw)

    def reconcile(self, limit=20, now=None):
        # One worker serializes per-customer reads. Notification timestamps never
        # overwrite newer state: every queued signal fetches the server's current facts.
        now = int(time.time()) if now is None else now
        rows = self.db.execute("SELECT delivery_id,body,attempts FROM inbox WHERE processed_at IS NULL AND retry_at<=? ORDER BY retry_at,received_at,delivery_id LIMIT ?", (now, limit)).fetchall()
        completed = 0
        for delivery, body, attempts in rows:
            signal = json.loads(body)
            org = signal["organizationId"]
            root = "/integration/customers/" + org
            kind = signal["type"]
            if kind in ("customer.changed", "entitlement.changed"):
                resource, path = "entitlement", root + "/entitlement"
            elif kind == "order.changed":
                resource = "order/" + signal["entityId"]
                path = root + "/orders/" + signal["entityId"]
            elif kind == "statement.changed":
                resource = "statement/" + signal["entityId"]
                path = root + "/statements/" + signal["entityId"]
            elif kind == "key.changed":
                resource, path = "keys", root + "/keys?limit=50"
            elif kind == "evidence.published":
                resource = "evidence/" + signal["entityId"]
                path = root + "/evidence-bundles/" + signal["entityId"]
            else:
                resource, path = "billing", "/business/customers/" + org + "/billing?limit=20"
            try:
                document = self.fetch(path)
            except (ValueError, OSError):
                # An open statement, revoked grant or unavailable server must not
                # prevent unrelated later signals from reconciling. Retry reads
                # with bounded backoff; never mark a failed read as processed.
                with self.db:
                    self.db.execute("UPDATE inbox SET attempts=attempts+1,retry_at=? WHERE delivery_id=?", (now + min(300, 5 * 2 ** min(attempts, 6)), delivery))
                continue
            with self.db:
                self.db.execute("INSERT INTO snapshots VALUES(?,?,?,?) ON CONFLICT(organization_id,resource) DO UPDATE SET document=excluded.document,fetched_at=excluded.fetched_at", (org, resource, json.dumps(document), now))
                self.db.execute("UPDATE inbox SET processed_at=? WHERE delivery_id=?", (now, delivery))
            completed += 1
        return completed


def handler(receiver):
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass  # Never log headers, signatures, credentials or receiver bodies.

        def do_POST(self):
            if self.path != "/events":
                self.send_error(404)
                return
            try:
                length = int(self.headers.get("content-length", "-1"))
                if not 0 <= length <= 4096 or self.headers.get("transfer-encoding"):
                    raise ValueError("invalid length")
                self.connection.settimeout(5)
                body = self.rfile.read(length)
                if len(body) != length:
                    raise ValueError("incomplete body")
                receiver.accept(self.headers.get("x-oxsum-signature", ""), self.headers.get("x-oxsum-delivery", ""), self.headers.get("x-oxsum-event", ""), body)
            except (ValueError, TypeError, OSError, KeyError, UnicodeError):
                self.send_error(401, "invalid notification")
                return
            except sqlite3.Error:
                self.send_error(503, "inbox unavailable")
                return
            # Acknowledge durable intake; state fetch can retry independently.
            self.send_response(204)
            self.end_headers()
    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True)
    parser.add_argument("--application-id", required=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:3000")
    parser.add_argument("--port", type=int, default=8081)
    args = parser.parse_args()
    secret = os.environ.get("OXSUM_NOTIFICATION_SECRET", "")
    credential = os.environ.get("OXSUM_INTEGRATION_SECRET", "")
    if not secret.startswith("whsec-") or not credential.startswith("oxs-int-"):
        parser.error("supply notification and scoped integration secrets in the environment")
    os.umask(0o077)
    receiver = Receiver(args.database, args.application_id, secret, args.base_url, credential)
    server = http.server.HTTPServer(("127.0.0.1", args.port), handler(receiver))
    server.timeout = 1
    print("Local notification receiver ready", flush=True)
    try:
        while True:
            server.handle_request()
            try:
                receiver.reconcile()
            except (ValueError, OSError, sqlite3.Error):
                pass  # Inbox is retained; never acknowledge failed reconciliation as complete.
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        receiver.db.close()


if __name__ == "__main__":
    main()
