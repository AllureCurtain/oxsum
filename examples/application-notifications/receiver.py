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
            received_at INTEGER NOT NULL, processed_at INTEGER
          );
          CREATE TABLE IF NOT EXISTS snapshots (
            organization_id TEXT NOT NULL, resource TEXT NOT NULL,
            document TEXT NOT NULL, fetched_at INTEGER NOT NULL,
            PRIMARY KEY(organization_id,resource)
          );
        """)
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
            self.db.execute("INSERT INTO inbox VALUES(?,?,?,NULL)", (delivery, body, now))

    def fetch(self, path):
        request = urllib.request.Request(self.base + "/api/v1" + path, headers={"Authorization": "Bearer " + self.credential})
        with self.http.open(request, timeout=20) as response:
            raw = response.read(16 * 1024 * 1024 + 1)
        if len(raw) > 16 * 1024 * 1024:
            raise ValueError("current state exceeds bound")
        return json.loads(raw)

    def reconcile(self, limit=20):
        # One worker serializes per-customer reads. Notification timestamps never
        # overwrite newer state: every queued signal fetches the server's current facts.
        rows = self.db.execute("SELECT delivery_id,body FROM inbox WHERE processed_at IS NULL ORDER BY received_at,delivery_id LIMIT ?", (limit,)).fetchall()
        for delivery, body in rows:
            signal = json.loads(body)
            org = signal["organizationId"]
            root = "/integration/customers/" + org
            kind = signal["type"]
            if kind in ("customer.changed", "entitlement.changed"):
                resource, path = "entitlement", root + "/entitlement"
            elif kind == "order.changed":
                resource = "order/" + signal["entityId"]
                path = root + "/orders/" + signal["entityId"]
            elif kind == "key.changed":
                resource, path = "keys", root + "/keys?limit=50"
            elif kind == "evidence.published":
                resource = "evidence/" + signal["entityId"]
                path = root + "/evidence-bundles/" + signal["entityId"]
            else:
                resource, path = "billing", "/business/customers/" + org + "/billing?limit=20"
            document = self.fetch(path)
            now = int(time.time())
            with self.db:
                self.db.execute("INSERT INTO snapshots VALUES(?,?,?,?) ON CONFLICT(organization_id,resource) DO UPDATE SET document=excluded.document,fetched_at=excluded.fetched_at", (org, resource, json.dumps(document), now))
                self.db.execute("UPDATE inbox SET processed_at=? WHERE delivery_id=?", (now, delivery))
        return len(rows)


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
