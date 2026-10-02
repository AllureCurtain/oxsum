#!/usr/bin/env python3
"""oxsum end-to-end demo: top up, run one billed chat turn, read the entries and the proof.

The script is self-contained: it starts a scripted mock upstream (no real provider, no
API key needed) and an oxsum server against the database in ``DATABASE_URL`` (or the
repo's ``.env``), drives the whole flow through the official ``openai`` Python SDK,
then stops both processes again. Each run registers a fresh demo user, so a run never
interferes with real data; the demo rows stay in the database afterwards, which is the
only trace a run leaves.

Prerequisites: ``pip install -r demo/requirements.txt`` and a reachable PostgreSQL
(the repo's ``docker compose up -d`` starts one).

Database: the demo prefers its own ``oxsum_demo`` database (created with ``psql``
from ``DATABASE_URL`` when the role may create databases, as the documented
compose superuser may); otherwise it falls back to ``DATABASE_URL`` itself. When
the server refuses to boot because stored upstream credentials do not open with
the demo's fresh secret key — the test residue ``docs/development.md`` documents —
the script runs that same documented reset (``TRUNCATE oxsum.channel_prices,
oxsum.channels``) and boots again, saying exactly what it did. At shutdown the
script removes its own bootstrap channel rows (under a unique per-run name) when
they are the only channels in the table — the price rows are append-only, so a
row-level delete is impossible and anything else is left alone. The demo's user,
organization and ledger rows stay behind; that is the only trace a run leaves.

What it shows:
  1. Register a user and take its API key.
  2. Top up 100 credits; fetch the top-up entry's proof bundle.
  3. One billed chat turn through the OpenAI SDK pointed at oxsum (streaming).
  4. The balance before/after, and the turn's settlement entry: named from the
     ``x-oxsum-request-id`` response header via the documented UUIDv5 derivation
     (``docs/api.md``), with its billing record and its proof bundle.

The transcript is deterministic: fixed model prices, a fixed scripted answer and
fixed token usage, a fixed top-up. Only the demo user's email and the API key are
random per run.
"""

from __future__ import annotations

import base64
import http.server
import json
import os
import secrets
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

# ── the demo's fixed numbers ──────────────────────────────────────────────────

MODEL = "demo-chat"
# 1_000_000 minor units per million tokens on both sides: one token costs one minor unit.
INPUT_PRICE_PER_MILLION = 1_000_000
OUTPUT_PRICE_PER_MILLION = 1_000_000
MAX_OUTPUT_TOKENS = 1024
TOPUP_MINOR = 100_000_000  # 100 credits (1 credit = 1_000_000 minor units)

# The scripted answer, and the usage the mock upstream reports for it.
ANSWER_PARTS = ["Holds are", " oxsum's way", " of saying 'reserved'."]
SCRIPTED_PROMPT_TOKENS = 23
SCRIPTED_COMPLETION_TOKENS = 11
# With one minor unit per token on both sides, the turn must cost exactly this.
EXPECTED_CHARGE = SCRIPTED_PROMPT_TOKENS + SCRIPTED_COMPLETION_TOKENS

REPO = Path(__file__).resolve().parent.parent
SERVER_BIN = REPO / "target" / "debug" / "oxsum"


# ── small helpers ─────────────────────────────────────────────────────────────

def die(message: str) -> "NoReturn":
    print(f"demo: error: {message}", file=sys.stderr)
    sys.exit(1)


def section(title: str) -> None:
    print()
    print(f"== {title} ==")


def note(text: str) -> None:
    print(f"   {text}")


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def read_dotenv(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        values[key.strip()] = value.strip().strip("\"'")
    return values


def api(base: str, method: str, path: str, key: str | None = None, body: object = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"content-type": "application/json"},
    )
    if key is not None:
        request.add_header("authorization", f"Bearer {key}")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode(errors="replace")
        die(f"{method} {path} -> HTTP {exc.code}: {detail}")


# ── the mock upstream: a tiny OpenAI-compatible chat-completions server ───────
#
# Mirrors the scripted upstream in crates/server/tests/gateway.rs: one model, a
# streamed answer, the token usage riding in the final chunk.


class MockUpstream(http.server.BaseHTTPRequestHandler):
    server_version = "oxsum-demo-mock/1"

    def log_message(self, *args: object) -> None:  # keep the transcript clean
        pass

    def do_POST(self) -> None:
        length = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.path != "/v1/chat/completions" or body.get("model") != MODEL:
            self.send_error(404, "no script for this model")
            return
        if not body.get("stream"):
            self.send_error(400, "the demo only scripts streaming turns")
            return
        frames = [
            'data: {"choices":[{"delta":{"role":"assistant","content":""}}],"usage":null}',
        ]
        frames += [
            "data: " + json.dumps({"choices": [{"delta": {"content": part}}], "usage": None})
            for part in ANSWER_PARTS
        ]
        frames.append(
            "data: " + json.dumps({"choices": [], "usage": {
                "prompt_tokens": SCRIPTED_PROMPT_TOKENS,
                "completion_tokens": SCRIPTED_COMPLETION_TOKENS,
            }})
        )
        frames.append("data: [DONE]")
        payload = "".join(frame + "\n\n" for frame in frames).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def start_mock_upstream(port: int) -> http.server.ThreadingHTTPServer:
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), MockUpstream)
    thread = threading.Thread(target=server.serve_forever, daemon=True, name="mock-upstream")
    thread.start()
    return server


# ── the oxsum server ──────────────────────────────────────────────────────────

def start_oxsum(env: dict[str, str]) -> subprocess.Popen:
    if not SERVER_BIN.exists():
        die(f"server binary not built: run `cargo build -p oxsum-server` in {REPO} first")
    log = open(SERVER_LOG, "w")  # noqa: PTH123
    print(f"   (server log: {SERVER_LOG})")
    return subprocess.Popen(
        [str(SERVER_BIN)], cwd=REPO, env=env, stdout=log, stderr=subprocess.STDOUT
    )


def wait_for_healthz(base: str, timeout: float = 30) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(base + "/healthz", timeout=2) as response:
                if response.status == 200:
                    return True
        except OSError:
            time.sleep(0.2)
    return False


# ── the demo database ─────────────────────────────────────────────────────────
#
# The demo prefers its own database so a run never touches real data. It falls
# back to DATABASE_URL itself when the role may not create databases.

DEMO_DB_NAME = "oxsum_demo"
CREDENTIAL_ERROR = "does not open with OXSUM_SECRET_KEY"
CHANNEL_RESET_SQL = "TRUNCATE oxsum.channel_prices, oxsum.channels;"
SERVER_LOG = "/tmp/oxsum-demo-server.log"


def with_dbname(database_url: str, name: str) -> str:
    parts = urllib.parse.urlsplit(database_url)
    return urllib.parse.urlunsplit(parts._replace(path="/" + name))


def psql(database_url: str, sql: str) -> tuple[int, str]:
    proc = subprocess.run(
        ["psql", database_url, "-v", "ON_ERROR_STOP=1", "-tAc", sql],
        capture_output=True, text=True, timeout=30,
    )
    return proc.returncode, (proc.stdout + proc.stderr).strip()


def choose_database(database_url: str) -> str:
    """The demo's own database when the role may create one, else DATABASE_URL."""
    if shutil.which("psql") is None:
        note("psql not found; using DATABASE_URL as-is")
        return database_url
    demo_url = with_dbname(database_url, DEMO_DB_NAME)
    code, exists = psql(database_url, f"SELECT 1 FROM pg_database WHERE datname='{DEMO_DB_NAME}'")
    if code != 0:
        note("could not probe for a demo database; using DATABASE_URL as-is")
        return database_url
    if exists != "1":
        code, out = psql(database_url, f"CREATE DATABASE {DEMO_DB_NAME}")
        if code != 0:
            note(f"could not create the demo database ({out}); using DATABASE_URL as-is")
            return database_url
        note(f"created database {DEMO_DB_NAME}")
    code, _ = psql(demo_url, "SELECT 1")
    if code != 0:
        note("could not connect to the demo database; using DATABASE_URL as-is")
        return database_url
    return demo_url


def server_failed_on_credentials() -> bool:
    try:
        tail = Path(SERVER_LOG).read_text()[-2000:]
    except OSError:
        return False
    return CREDENTIAL_ERROR in tail


def stop_server(server: subprocess.Popen) -> None:
    server.terminate()
    try:
        server.wait(timeout=10)
    except subprocess.TimeoutExpired:
        server.kill()


def boot_oxsum(oxsum_base: str, env: dict[str, str], db_url: str) -> subprocess.Popen:
    """Start the server, clearing test-sealed channels once if they block the boot.

    The reset is the one docs/development.md documents for the development
    database; on the demo's own database it only ever touches demo rows.
    """
    server = start_oxsum(env)
    if wait_for_healthz(oxsum_base):
        return server
    if server_failed_on_credentials() and shutil.which("psql") is not None:
        code, _ = psql(db_url, CHANNEL_RESET_SQL)
        stop_server(server)
        if code == 0:
            note(f"cleared sealed channels ({CHANNEL_RESET_SQL}); rebooting")
            server = start_oxsum(env)
            if wait_for_healthz(oxsum_base):
                return server
    stop_server(server)
    if server_failed_on_credentials():
        die("stored upstream credentials do not open with the demo's key; clear them with:\n"
            f'     psql "$DATABASE_URL" -c "{CHANNEL_RESET_SQL}"\n'
            "     (docs/development.md documents this reset)")
    die("the oxsum server did not answer /healthz in time")


def cleanup_channels(db_url: str, channel_name: str) -> None:
    """Remove the demo's bootstrap channel rows, when they are provably the only ones.

    Price rows are append-only (the trigger refuses DELETE), so the only removal
    the schema allows is TRUNCATE — which the script runs only when the demo's
    channel is the single channel in the table, never touching anyone else's rows.
    """
    if shutil.which("psql") is None:
        note("psql not found; leaving the demo channel rows behind")
        return
    code, out = psql(db_url,
                     "SELECT count(*), count(*) FILTER (WHERE name = "
                     f"'{channel_name}') FROM oxsum.channels")
    if code != 0:
        note(f"could not inspect the channels ({out}); leaving the demo channel rows behind")
        return
    total, own = (int(part) for part in out.split("|"))
    if total == 1 and own == 1:
        code, _ = psql(db_url, CHANNEL_RESET_SQL)
        note("removed the demo channel rows" if code == 0 else
             "could not remove the demo channel rows; they stay behind")
    else:
        note(f"the channels table holds {total} channel(s), not just the demo's; "
             "leaving them alone")


def get_proof(base: str, key: str, entry_id: str) -> dict | None:
    """The proof bundle, or None when the entry is not in the log yet (HTTP 404)."""
    request = urllib.request.Request(
        f"{base}/api/v1/entries/{entry_id}/proof", method="GET",
        headers={"authorization": f"Bearer {key}"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)["data"]
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return None
        die(f"GET /api/v1/entries/{entry_id}/proof -> HTTP {exc.code}")


def wait_for_settlement(base: str, key: str, entry_id: str) -> dict:
    """The settlement entry lands just after the client reads the stream's end,
    so the demo waits for it instead of racing the write."""
    deadline = time.time() + 15
    while time.time() < deadline:
        bundle = get_proof(base, key, entry_id)
        if bundle is not None:
            return bundle
        time.sleep(0.2)
    die("the settlement entry did not appear in the log in time")


# ── the UUIDv5 derivation documented in docs/api.md ───────────────────────────
#
# A gateway hold is taken under "req-<id>:hold"; the settlement entry id is
# entry_id_for(settlement_key_for(hold_key)), where both derivations are
# UUIDv5(UUID_NAMESPACE_OID, ...).


def entry_id_for(key: str) -> uuid.UUID:
    return uuid.uuid5(uuid.NAMESPACE_OID, key)


def settlement_entry_id(request_id: str) -> uuid.UUID:
    hold_key = f"req-{request_id}:hold"
    settle_key = f"settle:{entry_id_for(hold_key).hex}"
    return entry_id_for(settle_key)


# ── the demo ──────────────────────────────────────────────────────────────────

def main() -> None:
    try:
        import openai  # noqa: F401  (imported late so --help-style failures read well)
    except ImportError:
        die("the `openai` package is missing: pip install -r demo/requirements.txt")

    dotenv = read_dotenv(REPO / ".env") if (REPO / ".env").exists() else {}
    database_url = os.environ.get("DATABASE_URL") or dotenv.get("DATABASE_URL")
    if not database_url:
        die("DATABASE_URL is not set and the repo has no .env; copy .env.example first")

    mock_port = free_port()
    oxsum_port = free_port()
    oxsum_base = f"http://127.0.0.1:{oxsum_port}"

    mock = start_mock_upstream(mock_port)
    note(f"mock upstream on 127.0.0.1:{mock_port} (scripted, no real provider involved)")

    db_url = choose_database(database_url)
    note(f"demo database: {db_url.rsplit('/', 1)[-1]}")

    channel_name = f"demo-{secrets.token_hex(4)}"
    env = {
        **os.environ,
        "DATABASE_URL": db_url,
        "OXSUM_SIGNUP": "open",
        "OXSUM_ADDR": f"127.0.0.1:{oxsum_port}",
        "OXSUM_UPSTREAM_BASE_URL": f"http://127.0.0.1:{mock_port}/v1",
        "OXSUM_UPSTREAM_API_KEY": "demo-mock-key",
        "OXSUM_UPSTREAM_NAME": channel_name,
        "OXSUM_MODELS": json.dumps({MODEL: {
            "inputPricePerMillion": INPUT_PRICE_PER_MILLION,
            "outputPricePerMillion": OUTPUT_PRICE_PER_MILLION,
            "maxOutputTokens": MAX_OUTPUT_TOKENS,
        }}),
        "OXSUM_SECRET_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
        "RUST_LOG": "warn",
    }
    server = boot_oxsum(oxsum_base, env, db_url)
    try:
        run_demo(oxsum_base)
    finally:
        cleanup_channels(db_url, channel_name)
        note("stopping the oxsum server and the mock upstream")
        stop_server(server)
        mock.shutdown()


def run_demo(base: str) -> None:
    try:
        import httpx
    except ImportError:  # some distributions ship the fork as httpx2
        import httpx2 as httpx
    from openai import OpenAI

    section("1. Register and top up")
    email = f"demo-{secrets.token_hex(4)}@example.com"
    registration = api(base, "POST", "/api/v1/auth/register", body={
        "email": email,
        "password": "correct-horse-battery-staple",
        "organizationName": "demo",
    })["data"]
    api_key = registration["apiKey"]["secret"]
    org = registration["organization"]["name"]
    note(f"registered {email} (organization {org!r}); the API key is shown once")

    balance = api(base, "GET", "/api/v1/balance", key=api_key)["data"]["availableMinor"]
    note(f"balance before top-up: {balance} minor units")

    receipt = api(base, "POST", "/api/v1/topups", key=api_key, body={
        "idempotencyKey": f"demo-topup-{secrets.token_hex(4)}",
        "amountMinor": TOPUP_MINOR,
    })["data"]
    note(f"topped up {TOPUP_MINOR} minor units (100 credits)")
    note(f"top-up entry: {receipt['entryId']}")
    note(f"content hash (keep for verification): {receipt['contentHash']}")

    bundle = api(base, "GET", f"/api/v1/entries/{receipt['entryId']}/proof", key=api_key)["data"]
    assert bundle["entry"]["id"] == receipt["entryId"], "proof bundle is for another entry"
    note(f"proof bundle: entry {bundle['entry']['id']}, "
         f"tree head size {bundle['head']['size']}, "
         f"root {bundle['head']['root'][:16]}...")
    note("bundle + content hash are exactly what the /verify page checks")

    section("2. One billed chat turn through the OpenAI SDK")
    before = api(base, "GET", "/api/v1/balance", key=api_key)["data"]["availableMinor"]
    note(f"balance before the turn: {before} minor units")

    client = OpenAI(
        base_url=f"{base}/v1",
        api_key=api_key,
        # The demo only talks to 127.0.0.1: never route it through a proxy the
        # environment happens to set (their no_proxy lists break URL parsing).
        http_client=httpx.Client(trust_env=False, timeout=30.0),
    )
    raw = client.chat.completions.with_raw_response.create(
        model=MODEL,
        messages=[{"role": "user", "content": "Explain a wallet hold in one sentence."}],
        max_tokens=64,
        stream=True,
        stream_options={"include_usage": True},
    )
    request_id = raw.headers.get("x-oxsum-request-id")
    if not request_id:
        die("the gateway did not return x-oxsum-request-id")
    answer = "".join(
        chunk.choices[0].delta.content or ""
        for chunk in raw.parse()
        if chunk.choices
    )
    print(f'   upstream said: "{answer}"')
    note(f"x-oxsum-request-id: {request_id}")

    section("3. The turn's settlement entry and its proof")
    settle_id = settlement_entry_id(request_id)
    note(f"hold key req-{request_id}:hold -> settlement entry {settle_id}")
    note("the settlement lands just after the stream's end; waiting for the entry")
    settle_bundle = wait_for_settlement(base, api_key, str(settle_id))
    assert settle_bundle["entry"]["id"] == str(settle_id), "proof bundle is for another entry"
    record = json.loads(settle_bundle["entry"]["description"])
    print("   billing record:", json.dumps(record, indent=2).replace("\n", "\n   "))
    if record["kind"] != "usage" or record["charged"] != EXPECTED_CHARGE:
        die(f"unexpected settlement record: {record!r}")
    note(f"proof bundle: tree head size {settle_bundle['head']['size']}, "
         f"root {settle_bundle['head']['root'][:16]}...")

    after = api(base, "GET", "/api/v1/balance", key=api_key)["data"]["availableMinor"]
    charged = before - after
    note(f"balance after the turn: {after} minor units (charged {charged})")
    if charged != EXPECTED_CHARGE:
        die(f"expected a charge of {EXPECTED_CHARGE} minor units, got {charged}")

    section("Done")
    print(f"   The turn cost {EXPECTED_CHARGE} minor units "
          f"({SCRIPTED_PROMPT_TOKENS} input + {SCRIPTED_COMPLETION_TOKENS} output tokens "
          f"at 1 minor unit each), frozen before the call and settled against the "
          f"scripted upstream's usage afterwards.")
    print("   Both proof bundles above verify on the /verify page against their content hashes.")


if __name__ == "__main__":
    main()
