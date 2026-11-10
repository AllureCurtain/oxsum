#!/usr/bin/env python3
"""A local external AI application with private durable state and scoped runtime."""
import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

MAX_RESPONSE = 16 * 1024 * 1024
MAX_ATTEMPTS = 3
GATEWAY_RETRY_SECONDS = 23 * 3600


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)


def identity(value):
    return str(uuid.UUID(value))


def base_url(base):
    parsed = urllib.parse.urlsplit(base)
    if parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in ("", "/"):
        raise ValueError("use an origin without credentials, path, query or fragment")
    if parsed.scheme != "https" and not (parsed.scheme == "http" and parsed.hostname in ("localhost", "127.0.0.1", "::1")):
        raise ValueError("use HTTPS or explicit loopback HTTP")
    if not parsed.hostname:
        raise ValueError("missing server host")
    return base.rstrip("/")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("redirect refused")


class RequestFailed(Exception):
    def __init__(self, status):
        self.status = status
        super().__init__(f"oxsum request failed (HTTP {status}); exact operation retained")


class Transport:
    def __init__(self, base):
        self.base = base_url(base)
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect)

    def request(self, method, path, credential, body=None, headers=None):
        if not path.startswith("/") or path.startswith("//") or "#" in path:
            raise ValueError("invalid fixed API path")
        request = urllib.request.Request(self.base + path, data=body, method=method,
            headers={"Authorization": "Bearer " + credential, "Content-Type": "application/json", **(headers or {})})
        try:
            response = self.http.open(request, timeout=20)
        except urllib.error.HTTPError as error:
            # Receiver bodies and headers may contain secrets; don't expose them.
            error.close()
            raise RequestFailed(error.code) from None
        with response:
            raw = response.read(MAX_RESPONSE + 1)
            kept = {name: response.headers[name] for name in ("x-oxsum-request-id", "x-oxsum-charged-minor", "idempotent-replayed") if name in response.headers}
        if len(raw) > MAX_RESPONSE:
            raise ValueError("response exceeds bound; operation retained")
        json.loads(raw)
        return raw, kept


def data_bytes(raw):
    """Select the server data envelope without rewriting original JSON numbers."""
    text = raw.decode("utf-8")
    value = json.loads(text)
    if not isinstance(value, dict) or set(value) != {"data"}:
        raise ValueError("expected a data envelope")
    decoder = json.JSONDecoder()
    at = text.index("{") + 1
    while text[at].isspace():
        at += 1
    key, at = decoder.raw_decode(text, at)
    if key != "data":
        raise ValueError("unexpected envelope")
    while text[at].isspace():
        at += 1
    if text[at] != ":":
        raise ValueError("invalid envelope")
    at += 1
    while text[at].isspace():
        at += 1
    _, end = decoder.raw_decode(text, at)
    return text[at:end].encode("utf-8")


class Application:
    def __init__(self, path, base, application, credential, transport=None, clock=time.time):
        self.base = base_url(base)
        self.application = identity(application)
        if not credential.startswith("oxs-int-"):
            raise ValueError("an application credential is required; owner credentials are not accepted")
        self.credential = credential
        self.clock = clock
        self.transport = transport or Transport(base)
        self.path = Path(path).absolute()
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.lock = open(str(self.path) + ".lock", "a+b")
        os.chmod(str(self.path) + ".lock", 0o600)
        self.db = sqlite3.connect(self.path)
        os.chmod(self.path, 0o600)
        self.db.execute("PRAGMA foreign_keys=ON")
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS binding (id INTEGER PRIMARY KEY CHECK(id=1), base TEXT NOT NULL, application TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS customers (external TEXT PRIMARY KEY, organization TEXT NOT NULL UNIQUE, mapping TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, customer TEXT NOT NULL, kind TEXT NOT NULL, arguments TEXT NOT NULL,
            path TEXT NOT NULL, body BLOB NOT NULL, key_ref TEXT, created INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
            response BLOB, headers TEXT, error TEXT);
          CREATE TABLE IF NOT EXISTS keys (operation TEXT PRIMARY KEY REFERENCES operations(id), customer TEXT NOT NULL, key_id TEXT NOT NULL, secret TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS trust (customer TEXT PRIMARY KEY, organization TEXT NOT NULL, origin TEXT NOT NULL, key_json TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS receipts (operation TEXT PRIMARY KEY REFERENCES operations(id), entry TEXT NOT NULL, content_hash TEXT NOT NULL);
        """)
        with self.serialized(), self.db:
            row = self.db.execute("SELECT base,application FROM binding WHERE id=1").fetchone()
            if row is None:
                self.db.execute("INSERT INTO binding VALUES(1,?,?)", (self.base, self.application))
        if row is not None and row != (self.base, self.application):
            self.close()
            raise ValueError("store belongs to another deployment/application")

    @contextlib.contextmanager
    def serialized(self):
        fcntl.flock(self.lock, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(self.lock, fcntl.LOCK_UN)

    def close(self):
        self.db.close()
        self.lock.close()

    def org(self, customer):
        row = self.db.execute("SELECT organization FROM customers WHERE external=?", (customer,)).fetchone()
        if row is None:
            raise ValueError("sign up this local customer first")
        return row[0]

    def key(self, operation, customer):
        row = self.db.execute("SELECT secret,key_id FROM keys WHERE operation=? AND customer=?", (operation, customer)).fetchone()
        if row is None:
            raise ValueError("key secret unavailable; recover metadata and explicitly revoke/replace it")
        return row

    def read(self, path, credential=None):
        raw, _ = self.transport.request("GET", path, credential or self.credential)
        return json.loads(data_bytes(raw))

    def operation(self, op, customer, kind, args, build, key_ref=None):
        if not isinstance(op, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,100}", op):
            raise ValueError("operation must be a stable 1–100 character local identifier")
        encoded = canonical(args)
        row = self.db.execute("SELECT customer,kind,arguments,path,body,key_ref,created,attempts,response,headers FROM operations WHERE id=?", (op,)).fetchone()
        if row is None:
            path, body = build()
            raw = canonical(body).encode()
            # Persist BEFORE any state-changing network request. A quote is a read.
            with self.db:
                self.db.execute("INSERT INTO operations(id,customer,kind,arguments,path,body,key_ref,created) VALUES(?,?,?,?,?,?,?,?)",
                    (op, customer, kind, encoded, path, raw, key_ref, int(self.clock())))
            row = (customer, kind, encoded, path, raw, key_ref, int(self.clock()), 0, None, None)
        if row[:3] != (customer, kind, encoded) or row[5] != key_ref:
            raise ValueError("operation identity already binds different inputs/customer/key")
        if row[8] is not None:
            return row[8], json.loads(row[9])
        if row[7] >= MAX_ATTEMPTS:
            raise ValueError("three durable attempts exhausted; inspect current server state")
        if kind == "ask" and self.clock() - row[6] >= GATEWAY_RETRY_SECONDS:
            raise ValueError("unknown gateway result is past the safe replay window; do not infer again")
        secret = self.key(key_ref, customer)[0] if key_ref else self.credential
        with self.db:
            self.db.execute("UPDATE operations SET attempts=attempts+1,error=NULL WHERE id=?", (op,))
        try:
            raw, headers = self.transport.request("POST", row[3], secret, row[4], {"Idempotency-Key": op} if kind == "ask" else None)
        except (OSError, ValueError, RequestFailed) as error:
            with self.db:
                self.db.execute("UPDATE operations SET error=? WHERE id=?", (f"HTTP {error.status}" if isinstance(error, RequestFailed) else "response unavailable", op))
            raise
        with self.db:
            self.db.execute("UPDATE operations SET response=?,headers=?,error=NULL WHERE id=?", (raw, canonical(headers), op))
        return raw, headers

    def execute(self, request):
        if not isinstance(request, dict):
            raise ValueError("command must be a JSON object")
        with self.serialized():
            return self._execute(request)

    def _execute(self, r):
        action = r["action"]
        customer = r["customer"]
        if not isinstance(customer, str) or not 1 <= len(customer) <= 100 or customer.strip() != customer:
            raise ValueError("invalid local customer identity")
        op = r.get("operation")
        args = {k: v for k, v in r.items() if k not in ("action", "customer", "operation")}
        if action == "signup":
            raw, _ = self.operation(op, customer, action, args, lambda: ("/api/v1/integration/customers",
                {"idempotencyKey": op, "externalId": customer, "name": r["name"], "contactEmail": r.get("email")}))
            mapping = json.loads(data_bytes(raw))
            if mapping["applicationId"] != self.application or mapping["externalId"] != customer:
                raise ValueError("server mapping differs from this application")
            org = identity(mapping["customer"]["organizationId"])
            with self.db:
                existing = self.db.execute("SELECT organization FROM customers WHERE external=?", (customer,)).fetchone()
                if existing and existing[0] != org:
                    raise ValueError("retained customer identity differs")
                self.db.execute("INSERT OR IGNORE INTO customers VALUES(?,?,?)", (customer, org, canonical(mapping)))
            return {"organizationId": org, "applicationId": self.application}
        org = self.org(customer)
        root = "/api/v1/integration/customers/" + org
        if action == "trust":
            if identity(r["organizationId"]) != org or not r["origin"].startswith("oxsum/ledgers/"):
                raise ValueError("trust must name the independently supplied customer/origin")
            key = r["key"]
            if set(key) != {"keyName", "publicKey", "keyHash"}:
                raise ValueError("supply the separately obtained operator key")
            current = (org, r["origin"], canonical(key))
            with self.db:
                old = self.db.execute("SELECT organization,origin,key_json FROM trust WHERE customer=?", (customer,)).fetchone()
                if old is not None and old != current:
                    raise ValueError("retained trust differs; no implicit key/origin replacement")
                self.db.execute("INSERT OR IGNORE INTO trust VALUES(?,?,?,?)", (customer, *current))
            return {"trustRetained": True}
        if action == "key":
            raw, _ = self.operation(op, customer, action, args, lambda: (root + "/keys", {
                "idempotencyKey": op, "name": "External application", "reason": "bounded application operation", "expiresAt": r["expiresAt"],
                "spendLimitMinor": r["spendLimitMinor"], "modelAllowlist": [r["model"]], "requestsPerMinute": 5, "maxConcurrentHolds": 1}))
            result = json.loads(data_bytes(raw))
            if not result.get("secret"):
                raise ValueError("one-time key response was lost; revoke this retained key before explicit replacement")
            with self.db:
                self.db.execute("INSERT OR IGNORE INTO keys VALUES(?,?,?,?)", (op, customer, identity(result["key"]["id"]), result["secret"]))
            return {"keyId": result["key"]["id"], "keyOperation": op}
        if action == "ask":
            if not isinstance(r["prompt"], str) or not 1 <= len(r["prompt"]) <= 512:
                raise ValueError("prompt must have 1–512 characters")
            raw, headers = self.operation(op, customer, action, args, lambda: ("/v1/chat/completions", {
                "model": r["model"], "messages": [{"role": "user", "content": r["prompt"]}], "max_tokens": 32, "stream": False}), r["keyOperation"])
            return {"response": json.loads(raw), "receipt": headers, "operation": op}
        if action == "checkout":
            def build():
                selection = r["selection"]
                quote, _ = self.transport.request("POST", root + "/checkout-quote", self.credential, canonical(selection).encode())
                return root + "/orders", {"idempotencyKey": op, "quote": json.loads(data_bytes(quote)), "returnUrl": r["returnUrl"], "cancelUrl": r["cancelUrl"]}
            raw, _ = self.operation(op, customer, action, args, build)
            value = json.loads(data_bytes(raw))
            # The checkout URL is user-facing; client/sealing secrets remain in the private store.
            return {"orderId": value["order"]["orderId"], "status": value["order"]["status"], "checkoutUrl": value.get("checkoutUrl"), "paid": value["order"]["status"] == "fulfilled"}
        if action == "resume":
            order_id = identity(r["orderId"])
            raw, _ = self.operation(op, customer, action, args, lambda: (root + "/orders/" + order_id + "/resume", {}))
            value = json.loads(data_bytes(raw))
            return {"orderId": value["order"]["orderId"], "status": value["order"]["status"], "checkoutUrl": value.get("checkoutUrl"), "paid": value["order"]["status"] == "fulfilled"}
        if action == "retrieve":
            return self.read("/api/v1/integration/customers?externalId=" + urllib.parse.quote(customer, safe=""))
        if action == "archive":
            raw, _ = self.operation(op, customer, action, args, lambda: (root + "/archive", {"idempotencyKey": op, "reason": r["reason"]}))
            return json.loads(data_bytes(raw))
        if action == "revoke-key":
            key_id = identity(r["keyId"])
            raw, _ = self.operation(op, customer, action, args, lambda: (root + "/keys/" + key_id + "/revoke", {"idempotencyKey": op, "reason": r["reason"]}))
            return json.loads(data_bytes(raw))
        if action == "publish":
            raw, _ = self.operation(op, customer, action, args, lambda: (root + "/evidence-publications", {**r["scope"], "idempotencyKey": op}))
            return json.loads(data_bytes(raw))
        if action == "read":
            resource = r["resource"]
            paths = {"entitlement": root + "/entitlement", "models": root + "/models?limit=50", "keys": root + "/keys?limit=50", "statements": root + "/statements?limit=50", "orders": root + "/orders?limit=50", "evidence": root + "/evidence-bundles?limit=50", "billing": "/api/v1/business/customers/" + org + "/billing?limit=50"}
            if resource in ("statement", "order"):
                path = root + ("/statements/" if resource == "statement" else "/orders/") + identity(r["id"])
            else:
                path = paths[resource]
            return self.read(path)
        if action == "receipt":
            return self.receipt(customer, r["askOperation"], r["directory"])
        if action == "evidence":
            return self.evidence(customer, identity(r["bundleId"]), r["directory"])
        raise ValueError("unknown action")

    def trust(self, customer):
        row = self.db.execute("SELECT organization,origin,key_json FROM trust WHERE customer=?", (customer,)).fetchone()
        if row is None:
            raise ValueError("retain independent organization/origin/operator key before exporting")
        return row

    def write_export(self, directory, files):
        path = Path(directory).absolute()
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
        for name, raw in files.items():
            target = path / name
            if target.exists():
                if target.read_bytes() != raw:
                    raise ValueError("export already retains different bytes; use another directory")
                continue
            with open(target, "xb") as out:
                os.chmod(target, 0o600)
                out.write(raw)
                out.flush()
                os.fsync(out.fileno())
        return {"directory": str(path), "verified": False, "next": "run the offline verifier with the separately installed package"}

    def receipt(self, customer, op, directory):
        org, origin, key_raw = self.trust(customer)
        row = self.db.execute("SELECT key_ref,response,headers FROM operations WHERE id=? AND customer=? AND kind='ask'", (op, customer)).fetchone()
        if row is None or row[1] is None:
            raise ValueError("recover the exact gateway operation first")
        secret = self.key(row[0], customer)[0]
        request_id = json.loads(row[2]).get("x-oxsum-request-id")
        if not request_id:
            raise ValueError("gateway did not retain a request receipt")
        retained = self.db.execute("SELECT entry,content_hash FROM receipts WHERE operation=?", (op,)).fetchone()
        if retained is None:
            log = self.read("/api/v1/requests?limit=200", secret)
            rows = [v for v in log["rows"] if v["requestId"] == request_id and v.get("entryId")]
            if len(rows) != 1:
                raise ValueError("receipt not yet in bounded request window; retry read or request operator recovery")
            entry = identity(rows[0]["entryId"])
            bills = self.read("/api/v1/bills", secret)
            matches = [v for v in bills["rows"] if v["entryId"] == entry]
            if len(matches) != 1:
                raise ValueError("receipt not in bounded bill window")
            retained = (entry, matches[0]["contentHash"])
            with self.db:
                self.db.execute("INSERT INTO receipts VALUES(?,?,?)", (op, *retained))
        # Proof and head can race other writes. Refuse mismatched snapshots; a read retry is safe.
        proof_raw, _ = self.transport.request("GET", "/api/v1/entries/" + retained[0] + "/proof", secret)
        head_raw, _ = self.transport.request("GET", "/api/v1/log/head", secret)
        proof_raw, head_raw = data_bytes(proof_raw), data_bytes(head_raw)
        proof, head = json.loads(proof_raw), json.loads(head_raw)
        if proof["entry"]["id"] != retained[0] or head["origin"] != origin or proof["head"] != {"size": head["size"], "root": head["root"]}:
            raise ValueError("proof/head scope changed; retry read without another inference")
        manifest = {"entryId": retained[0], "contentHash": retained[1], "organizationId": org, "origin": origin, "headSize": head["size"], "headRoot": head["root"]}
        return self.write_export(directory, {"manifest.json": canonical(manifest).encode(), "settlement-bundle.json": proof_raw, "signed-head.json": head_raw, "operator-key.json": key_raw.encode()})

    def evidence(self, customer, bundle, directory):
        org, origin, key_raw = self.trust(customer)
        root = "/api/v1/integration/customers/" + org + "/evidence-bundles"
        page = self.read(root + "?limit=50")
        rows = [v for v in page["bundles"] if v["bundleId"] == bundle]
        if len(rows) != 1:
            raise ValueError("evidence not in bounded descriptor window")
        manifest = {"bundleId": bundle, "organizationId": org, "origin": origin, "manifestDigest": rows[0]["manifestDigest"]}
        # Retain descriptor and trust separately before downloading the original signed bytes.
        self.write_export(directory, {"evidence-manifest.json": canonical(manifest).encode(), "operator-key.json": key_raw.encode()})
        raw, _ = self.transport.request("GET", root + "/" + bundle, self.credential)
        value = json.loads(raw)
        if value["bundleId"] != bundle or value["organizationId"] != org or value["manifestDigest"] != manifest["manifestDigest"]:
            raise ValueError("download differs from retained scope")
        return self.write_export(directory, {"evidence-bundle.json": raw})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True)
    parser.add_argument("--application-id", required=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:3000")
    args = parser.parse_args()
    os.umask(0o077)
    application = None
    try:
        raw = sys.stdin.buffer.read(65537)
        if len(raw) > 65536:
            raise ValueError("command exceeds 64 KiB")
        request = json.loads(raw)
        application = Application(args.database, args.base_url, args.application_id, os.environ.get("OXSUM_APPLICATION_CREDENTIAL", ""))
        print(canonical(application.execute(request)))
    except (ValueError, KeyError, TypeError, OSError, sqlite3.Error, RequestFailed) as error:
        # Inputs, provider error bodies and tokens stay out of logs.
        print(canonical({"error": str(error) if isinstance(error, (ValueError, RequestFailed)) else "local operation failed; state retained"}), file=sys.stderr)
        raise SystemExit(1)
    finally:
        if application is not None:
            application.close()


if __name__ == "__main__":
    main()
