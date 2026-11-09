#!/usr/bin/env python3
"""Isolated owner -> customer -> synthetic receipt -> SDK stream -> offline bill check.

Uses a unique disposable database; provisioning failure is fatal. It never connects
oxsum to the database named in DATABASE_URL and never reads a checkout's .env.
Run `python3 demo/demo.py --help` for prerequisites and retained evidence options.
"""
from __future__ import annotations

import argparse
import importlib.util
import base64
from contextlib import contextmanager
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parent.parent
MODEL = "demo-chat"
TOPUP_MINOR = 100_000_000
EXPECTED_CHARGE = 34  # 23 input + 11 output, one integer minor unit per token.
ANSWER = "Holds reserve credit until actual usage settles."


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n")


def run(command: list[str], *, env: dict[str, str], cwd: Path, log: Path) -> None:
    with log.open("a") as stream:
        result = subprocess.run(command, cwd=cwd, env=env, stdout=stream,
                                stderr=subprocess.STDOUT, check=False)
    require(result.returncode == 0, f"{command[0]} step failed; inspect {log}")


def clean_environment() -> dict[str, str]:
    # In particular, do not inherit paid upstream, mail, database or rail config.
    return {k: v for k, v in os.environ.items()
            if not k.startswith(("OXSUM_", "DATABASE_", "PG")) and k != "NODE_OPTIONS"}


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class MockUpstream(http.server.BaseHTTPRequestHandler):
    """Deterministic OpenAI-compatible SSE; no proxy or external destination."""
    def log_message(self, *_args: object) -> None:
        pass

    def do_POST(self) -> None:
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
        if self.path != "/v1/chat/completions" or body.get("model") != MODEL or not body.get("stream"):
            self.send_error(400, "unexpected local fixture request")
            return
        self.server.attempts += 1
        frames = [
            {"choices": [{"index": 0, "delta": {"role": "assistant", "content": ANSWER}}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            {"choices": [], "usage": {"prompt_tokens": 23, "completion_tokens": 11, "total_tokens": 34}},
        ]
        payload = "".join("data: " + json.dumps(frame) + "\n\n" for frame in frames) + "data: [DONE]\n\n"
        data = payload.encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


@contextmanager
def scratch_database(admin_url: str):
    import psycopg
    from psycopg import sql
    from psycopg.conninfo import conninfo_to_dict

    name = "oxsum_demo_" + secrets.token_hex(12)
    try:
        connection = psycopg.connect(admin_url, autocommit=True, connect_timeout=10)
    except psycopg.Error:
        raise RuntimeError("Cannot connect to the database provisioning endpoint; no server was started") from None
    with connection:
        try:
            connection.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
        except psycopg.Error:
            raise RuntimeError("Cannot create an isolated demo database; no fallback is permitted") from None
        try:
            print(f"Disposable database: {name}", flush=True)
            fields = conninfo_to_dict(admin_url)
            fields["dbname"] = name
            # sqlx accepts PostgreSQL URLs, not libpq's keyword connection form.
            from urllib.parse import quote, urlencode
            user = quote(fields.pop("user", "postgres"), safe="")
            password = quote(fields.pop("password", ""), safe="")
            host = fields.pop("host", "127.0.0.1")
            port = fields.pop("port", "5432")
            fields.pop("dbname")
            require("," not in host and "/" not in host and ":" not in host,
                    "Demo provisioning requires a single hostname or IPv4 address")
            url = f"postgresql://{user}:{password}@{host}:{port}/{name}"
            if fields:
                url += "?" + urlencode(fields)
            yield name, url
        finally:
            # Only the random identifier created by this invocation is eligible.
            connection.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(name)))
            print(f"Removed own database: {name}", flush=True)


def api(base: str, path: str, *, session: str | None = None) -> dict:
    headers = {"cookie": f"oxsum_session={session}"} if session else {}
    request = urllib.request.Request(base + path, headers=headers)
    # Local fixture requests ignore the user's global proxy configuration.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=15) as response:
            return json.load(response)["data"]
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"GET {path} returned HTTP {error.code}") from None


def stop_server(server: subprocess.Popen) -> None:
    if server.poll() is None:
        server.terminate()
        try:
            server.wait(timeout=15)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=5)


def serve_and_capture(binary: Path, env: dict[str, str], cwd: Path, evidence: Path, log: Path) -> None:
    import httpx
    from openai import OpenAI

    base = f"http://127.0.0.1:{free_port()}"
    mock = http.server.ThreadingHTTPServer(("127.0.0.1", 0), MockUpstream)
    mock.attempts = 0
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    env = {**env, "OXSUM_ADDR": base.removeprefix("http://")}
    server = None
    try:
        with log.open("a") as output:
            server = subprocess.Popen([str(binary)], cwd=cwd, env=env, stdout=output, stderr=output)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            require(server.poll() is None, f"Server exited; inspect {log}")
            try:
                api(base, "/api/v1/version")
                break
            except (OSError, RuntimeError):
                time.sleep(.2)
        else:
            raise RuntimeError(f"Server readiness timed out; inspect {log}")
        state_path = cwd / "customer.json"
        run(["node", str(REPO / "demo/provision.mjs")], env={**env,
            "OXSUM_DEMO_BASE": base, "OXSUM_DEMO_UPSTREAM": f"http://127.0.0.1:{mock.server_port}/v1",
            "OXSUM_DEMO_STATE": str(state_path)}, cwd=cwd, log=log)
        state = json.loads(state_path.read_text())
        session = state["customerSession"]
        customer = state["registration"]["organization"]
        print("[PASS] Owner consent, invited customer, explicit sale terms and synthetic receipt replay", flush=True)
        print("Synthetic funding: 100 credits; USD display at exactly 1 credit = USD 1. No payment.", flush=True)
        held = api(base, "/api/v1/log/head", session=session)
        operator_key = api(base, "/api/v1/log/key", session=session)
        expected_origin = "oxsum/ledgers/" + customer["tenantId"]
        require(held["origin"] == expected_origin, "Initial head names another ledger")
        write_json(evidence / "held-head.json", {"size": held["size"], "root": held["root"]})
        write_json(evidence / "operator-key.json", operator_key)
        with OpenAI(base_url=base + "/v1", api_key=state["registration"]["apiKey"]["secret"],
                    max_retries=0, http_client=httpx.Client(trust_env=False, timeout=30.0)) as client:
            raw = client.chat.completions.with_raw_response.create(model=MODEL,
                messages=[{"role": "user", "content": "Explain a wallet hold."}], max_tokens=32,
                stream=True, stream_options={"include_usage": True},
                extra_headers={"Idempotency-Key": "demo-" + secrets.token_hex(16)})
            request_id = raw.headers.get("x-oxsum-request-id")
            require(bool(request_id), "Gateway omitted its request identity")
            answer = "".join(c.choices[0].delta.content or "" for c in raw.parse() if c.choices)
            require(answer == ANSWER, "SDK stream content differs from the deterministic fixture")
        print(f"[PASS] OpenAI SDK streamed one local request: {request_id}", flush=True)
        record = None
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            rows = api(base, "/api/v1/billing-records?limit=10", session=session)["rows"]
            record = next((row for row in rows if row["requestId"] == request_id), None)
            if record:
                break
            time.sleep(.1)
        require(record is not None and record["kind"] == "usage" and
                record["chargedMinor"] == EXPECTED_CHARGE and record["upstreamAttempts"] == 1,
                "Exact one-attempt usage settlement did not arrive")
        balance = api(base, "/api/v1/balance", session=session)
        require(balance["availableMinor"] == TOPUP_MINOR - EXPECTED_CHARGE and
                balance["reservedMinor"] == 0, "Balance or reservation conservation failed")
        require(mock.attempts == 1, "Unexpected duplicate upstream call")
        entries = api(base, "/api/v1/entries?limit=100", session=session)["rows"]
        entry = next((row for row in entries if row["id"] == record["entryId"]), None)
        require(entry is not None, "Settlement's retained content hash is missing")
        bundle = api(base, f"/api/v1/entries/{record['entryId']}/proof", session=session)
        require(bool(bundle.get("price")) and bool(bundle.get("sale")), "Signed supply/sale binding missing")
        head = api(base, "/api/v1/log/head", session=session)
        require(bundle["head"] == {"size": head["size"], "root": head["root"]}, "Proof/head changed unexpectedly")
        write_json(evidence / "settlement-bundle.json", bundle)
        write_json(evidence / "billing-record.json", record)
        write_json(evidence / "signed-head.json", head)
        write_json(evidence / "consistency.json", api(base, f"/api/v1/log/consistency?from={held['size']}", session=session))
        write_json(evidence / "checkpoint.json", {"data": api(base, "/api/v1/log/checkpoint", session=session)})
        write_json(evidence / "manifest.json", {"entryId": record["entryId"], "contentHash": entry["contentHash"],
            "origin": expected_origin, "headSize": head["size"], "headRoot": head["root"],
            "requestId": request_id, "chargedMinor": EXPECTED_CHARGE, "sale": state["sale"],
            "keyTrust": "Local fixture key retained before the call; not independent operator identity attestation"})
        write_json(evidence / "runtime.json", api(base, "/api/v1/version"))
        print("[PASS] Exact charge 34 minor units; available 99999966; reserved 0", flush=True)
    finally:
        if server:
            stop_server(server)
        mock.shutdown()
        mock.server_close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database-url", default=os.environ.get("DATABASE_URL", "postgres://postgres:postgres@127.0.0.1:5432/postgres"),
                        help="Provisioning connection with CREATEDB authority; .env is never read")
    parser.add_argument("--output", type=Path, help="New directory for sanitized evidence, tarball and logs; defaults to a unique temp directory")
    args = parser.parse_args()
    require(all(importlib.util.find_spec(name) for name in ["httpx", "openai", "psycopg"]),
            "Install demo/requirements.txt in this Python environment before running the demo")
    for tool in ["cargo", "node", "npm", "bash"]:
        require(shutil.which(tool) is not None, f"Missing prerequisite: {tool}")
    if args.output:
        output = args.output.resolve()
        output.mkdir(parents=True, exist_ok=False)
    else:
        output = Path(tempfile.mkdtemp(prefix="oxsum-demo-evidence-"))
    log = output / "build-and-server.log"
    print(f"Evidence directory: {output}", flush=True)
    env = clean_environment()
    # Build steps do not run the application or read deployment env files.
    run(["cargo", "build", "-p", "oxsum-server", "--bin", "oxsum"], env=env, cwd=REPO, log=log)
    metadata = subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=REPO, env=env)
    binary = Path(json.loads(metadata)["target_directory"]) / "debug" / ("oxsum.exe" if os.name == "nt" else "oxsum")
    run(["bash", "scripts/build-verify-wasm.sh"], env=env, cwd=REPO, log=log)
    run(["npm", "ci", "--no-audit", "--no-fund"], env=env, cwd=REPO / "web/verify-sdk", log=log)
    run(["npm", "pack", "--pack-destination", str(output)], env=env, cwd=REPO / "web/verify-sdk", log=log)
    tarballs = list(output.glob("verify-oxsum-bill-*.tgz"))
    require(len(tarballs) == 1, "Expected one built verifier tarball")
    evidence = output / "evidence"
    evidence.mkdir()
    with tempfile.TemporaryDirectory(prefix="oxsum-demo-runtime-") as private:
        cwd = Path(private)
        (cwd / ".env").write_text("")  # Stops dotenv from climbing into another checkout.
        with scratch_database(args.database_url) as (_name, database_url):
            runtime_env = {**env, "DATABASE_URL": database_url, "OXSUM_SIGNUP": "invite",
                "OXSUM_ADMIN_TOKEN": secrets.token_urlsafe(32), "OXSUM_SECRET_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
                "OXSUM_HEAD_SIGNING_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
                "OXSUM_DISPLAY_CURRENCY": "USD", "OXSUM_DISPLAY_RATE_PER_CREDIT": "1", "OXSUM_DISPLAY_DECIMALS": "2",
                "OXSUM_SESSION_COOKIE_SECURE": "false", "RUST_LOG": "warn"}
            serve_and_capture(binary, runtime_env, cwd, evidence, log)
    # The application is stopped and its entire database gone before verification.
    with tempfile.TemporaryDirectory(prefix="oxsum-demo-consumer-") as consumer_dir:
        consumer = Path(consumer_dir)
        write_json(consumer / "package.json", {"name": "oxsum-demo-consumer", "private": True, "type": "module"})
        shutil.copy(REPO / "examples/verify-consumer/verify.mjs", consumer / "verify.mjs")
        shutil.copy(REPO / "examples/verify-consumer/scope.mjs", consumer / "scope.mjs")
        run(["npm", "install", "--ignore-scripts", "--no-audit", "--no-fund", str(tarballs[0])], env=env, cwd=consumer, log=log)
        with (output / "verification.log").open("w") as verify_log:
            checked = subprocess.run(["node", "verify.mjs", "--dir", str(evidence)], cwd=consumer,
                                     env=env, stdout=verify_log, stderr=subprocess.STDOUT)
        print((output / "verification.log").read_text(), end="")
        require(checked.returncode == 0, "Independent verification or an altered-input rejection failed")
    hashes = {str(p.relative_to(output)): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sorted(output.rglob("*")) if p.is_file()}
    write_json(output / "checksums.json", hashes)
    print(f"Demo complete. Retained evidence and verifier: {output}")
    print("Scope: local stub, synthetic receipt, immediate entry proof. No closed-period, witness or PSP certification.")


if __name__ == "__main__":
    try:
        main()
    except (Exception, KeyboardInterrupt) as error:
        print(f"demo: failed: {error}", file=sys.stderr)
        sys.exit(1)
