#!/usr/bin/env python3
"""Rehearse real development-artifact drain, upgrade, separate restore and rollback.

Inputs are explicit native binaries and an original verifier tarball. This never
builds/publishes, touches existing databases, or claims final-candidate provenance.
"""
import argparse
import base64
import concurrent.futures
from datetime import datetime, timezone
import http.server
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

import backup_restore as backup

REPO = Path(__file__).resolve().parent.parent
PG_IMAGE = "postgres@sha256:266d3c257594e195b309ae9d1805e54c788f1d7b375f56bf6c367aac6c917e7b"
MODEL = "recovery-chat"
ANSWER = "Retained state survives the recovery rehearsal."
FUNDING = 100_000_000
CHARGE = 34


def require(condition, message):
    backup.require(condition, message)


def clean_environment():
    return {key: value for key, value in os.environ.items()
            if not key.startswith(("OXSUM_", "DATABASE_", "PG", "NPM_", "npm_"))
            and key not in {"NODE_OPTIONS", "NODE_PATH", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"}}


def run(command, *, env, cwd=None, log=None, timeout=120):
    with (log.open("ab") if log else tempfile.TemporaryFile()) as output:
        result = subprocess.run(command, env=env, cwd=cwd, stdout=output, stderr=output, timeout=timeout)
    require(result.returncode == 0, "rehearsal subprocess failed; its stage is retained, transient credential-bearing logs are removed")


def docker(*args, env=None):
    result = subprocess.run(["docker", *args], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    require(result.returncode == 0, "isolated Docker command failed")
    return result.stdout.decode().strip()


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_):
        return None


def request(base, path, *, token=None, session=None, body=None, headers=None):
    require(re.fullmatch(r"http://127\.0\.0\.1:[0-9]{1,5}", base), "runtime must use explicit numeric loopback")
    values = {"accept": "application/json", **(headers or {})}
    if token:
        values["authorization"] = "Bearer " + token
    if session:
        values["cookie"] = "oxsum_session=" + session
    if body is not None:
        values["content-type"] = "application/json"
    opener = urllib.request.build_opener(NoRedirect(), urllib.request.ProxyHandler({}))
    req = urllib.request.Request(base+path, headers=values, data=json.dumps(body).encode() if body is not None else None)
    try:
        with opener.open(req, timeout=30) as response:
            raw = response.read(2*1024*1024+1)
            require(len(raw) <= 2*1024*1024, "runtime response exceeds fixture limit")
            return raw, dict(response.headers)
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
        raise ValueError("runtime fixture request refused with HTTP " + str(status)) from None


def api(base, path, **kwargs):
    return json.loads(request(base, path, **kwargs)[0])["data"]


class Upstream(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
        if self.path != "/v1/chat/completions" or body.get("model") != MODEL or not body.get("stream"):
            self.send_error(400)
            return
        with self.server.guard:
            self.server.attempts += 1
        controlled = body.get("messages", [{}])[0].get("content") == "drain"
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("connection", "close")
        self.end_headers()
        first = {"choices": [{"index": 0, "delta": {"role": "assistant", "content": ANSWER}}]}
        self.wfile.write(("data: " + json.dumps(first) + "\n\n").encode())
        self.wfile.flush()
        if controlled:
            self.server.started.set()
            require(self.server.release.wait(6), "drain fixture was not released")
        for frame in [{"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
                      {"choices": [], "usage": {"prompt_tokens": 23, "completion_tokens": 11, "total_tokens": 34}}]:
            self.wfile.write(("data: " + json.dumps(frame) + "\n\n").encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


def gateway(base, customer, label):
    raw, headers = request(base, "/v1/chat/completions", token=customer["registration"]["apiKey"]["secret"],
        body={"model": MODEL, "messages": [{"role": "user", "content": label}], "max_tokens": 32,
              "stream": True, "stream_options": {"include_usage": True}}, headers={"idempotency-key": "recovery-"+uuid.uuid4().hex})
    answer = ""
    for line in raw.decode().splitlines():
        if line.startswith("data: ") and line != "data: [DONE]":
            frame = json.loads(line[6:])
            answer += "".join(choice.get("delta", {}).get("content", "") for choice in frame.get("choices", []))
    require(answer == ANSWER and b"data: [DONE]" in raw, "gateway stream did not finish with exact fixture content")
    request_id = next((value for key, value in headers.items() if key.lower() == "x-oxsum-request-id"), None)
    require(request_id, "gateway omitted request identity")
    return request_id


def settlement(base, customer, request_id, expected_calls):
    session = customer["customerSession"]
    record = None
    for _ in range(100):
        rows = api(base, "/api/v1/billing-records?limit=100", session=session)["rows"]
        record = next((row for row in rows if row["requestId"] == request_id), None)
        if record:
            break
        time.sleep(0.1)
    require(record and record["kind"] == "usage" and record["chargedMinor"] == CHARGE and record["upstreamAttempts"] == 1, "exact single-attempt settlement missing")
    balance = api(base, "/api/v1/balance", session=session)
    require(balance["availableMinor"] == FUNDING - CHARGE*expected_calls and balance["reservedMinor"] == 0, "balance/reservation conservation failed")
    return record, balance


def capture(base, customer, record, held, directory):
    directory.mkdir(mode=0o700)
    session = customer["customerSession"]
    entries = api(base, "/api/v1/entries?limit=100", session=session)["rows"]
    entry = next(row for row in entries if row["id"] == record["entryId"])
    bundle = api(base, "/api/v1/entries/"+record["entryId"]+"/proof", session=session)
    head = api(base, "/api/v1/log/head", session=session)
    key = api(base, "/api/v1/log/key")
    require(bundle.get("price") and bundle.get("sale"), "signed supplier/customer terms missing")
    require(bundle["head"] == {"size": head["size"], "root": head["root"]}, "proof head does not match captured head")
    values = {"settlement-bundle.json": bundle, "signed-head.json": head, "operator-key.json": key,
              "held-head.json": {"size": held["size"], "root": held["root"]},
              "consistency.json": api(base, "/api/v1/log/consistency?from="+str(held["size"]), session=session),
              "checkpoint.json": {"data": api(base, "/api/v1/log/checkpoint", session=session)},
              "manifest.json": {"entryId": record["entryId"], "contentHash": entry["contentHash"],
                  "origin": "oxsum/ledgers/"+customer["registration"]["organization"]["tenantId"],
                  "headSize": head["size"], "headRoot": head["root"], "requestId": record["requestId"],
                  "chargedMinor": CHARGE, "keyTrust": "Retained private fixture key; not independent operator identity attestation"}}
    for name, value in values.items():
        backup.private_json(directory/name, value)
    return key


class Runtime:
    def __init__(self, artifact, environment, directory, log):
        self.artifact, self.environment, self.directory, self.log = artifact, environment, directory, log
        self.base = "http://127.0.0.1:"+str(free_port())
        self.process = None

    def migrate(self):
        started = time.monotonic()
        run([str(self.artifact["path"]), "--migrate-only"], env=self.environment, cwd=self.directory, log=self.log)
        return int((time.monotonic()-started)*1000)

    def start(self):
        output = self.log.open("ab")
        self.process = subprocess.Popen([str(self.artifact["path"])], cwd=self.directory,
            env={**self.environment, "OXSUM_ADDR": self.base.removeprefix("http://")}, stdout=output, stderr=output)
        output.close()
        for _ in range(200):
            require(self.process.poll() is None, "selected artifact exited before readiness")
            try:
                identity = api(self.base, "/api/v1/version")
                ready = request(self.base, "/readyz")[0].decode().strip()
                require(identity == {"version": self.artifact["version"], "gitSha": self.artifact["sourceSha"], "schemaVersion": self.artifact["schemaVersion"]}, "runtime identity differs from explicit artifact")
                require(ready == "ready ("+identity["schemaVersion"]+")", "runtime schema not ready")
                return identity
            except (OSError, urllib.error.URLError):
                time.sleep(.1)
        raise ValueError("runtime did not become ready")

    def stop(self, strict=False):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                require(not strict, "runtime exceeded graceful shutdown deadline")
        if strict and self.process:
            require(self.process.returncode == 0, "runtime shutdown was unsuccessful")


def freeze_artifact(path, sha, expected_hash, schema, version, destination):
    require(re.fullmatch(r"[0-9a-f]{40}", sha) and re.fullmatch(r"[0-9a-f]{64}", expected_hash), "artifact requires exact source SHA and content hash")
    require(backup.SCHEMA.fullmatch(schema) and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version), "invalid expected artifact identity")
    require(path.is_file() and not path.is_symlink(), "artifact must be a regular nonsymlink file")
    with path.open("rb") as source, destination.open("xb") as output:
        shutil.copyfileobj(source, output)
    destination.chmod(0o700)
    require(backup.digest(destination) == expected_hash, "frozen binary hash differs from explicit input")
    return {"path": destination, "sourceSha": sha, "sha256": expected_hash, "schemaVersion": schema, "version": version}


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    for role in ["old", "current", "rollback"]:
        result.add_argument("--"+role+"-binary", type=Path, required=True)
        result.add_argument("--"+role+"-sha", required=True)
        result.add_argument("--"+role+"-sha256", required=True)
        result.add_argument("--"+role+"-schema", required=True)
    result.add_argument("--version", default="0.1.0")
    result.add_argument("--sdk", type=Path, required=True)
    result.add_argument("--sdk-sha256", required=True)
    result.add_argument("--confirm-compatible-rollback", action="store_true")
    result.add_argument("--output", type=Path, required=True)
    return result


def main():
    args = parser().parse_args()
    os.umask(0o077)
    require(args.confirm_compatible_rollback and args.rollback_schema == args.current_schema, "rollback requires explicit behavior compatibility and identical schema")
    require(args.old_schema < args.current_schema, "upgrade must advance the schema in this rehearsal")
    require(len({args.old_sha,args.current_sha,args.rollback_sha}) == 3, "rehearsal requires distinct actual artifacts")
    args.output.mkdir(mode=0o700, exist_ok=False)
    output = args.output.resolve()
    artifacts = output/"artifacts"
    artifacts.mkdir(mode=0o700)
    frozen = {}
    for role in ["old", "current", "rollback"]:
        frozen[role] = freeze_artifact(getattr(args,role+"_binary"),getattr(args,role+"_sha"),getattr(args,role+"_sha256"),getattr(args,role+"_schema"),args.version,artifacts/("oxsum-"+role))
    require(args.sdk.is_file() and not args.sdk.is_symlink() and re.fullmatch(r"[0-9a-f]{64}",args.sdk_sha256), "explicit verifier tar/hash required")
    sdk = artifacts/"verify-oxsum-bill.tgz"
    shutil.copyfile(args.sdk,sdk)
    sdk.chmod(0o600)
    require(backup.digest(sdk)==args.sdk_sha256, "verifier bytes differ from explicit input")
    env = clean_environment()
    slug = uuid.uuid4().hex[:12]
    containers = []
    runtimes = []
    stage = "initializing"
    mock = http.server.ThreadingHTTPServer(("127.0.0.1",0),Upstream)
    mock.attempts = 0
    mock.guard = threading.Lock()
    mock.started = threading.Event()
    mock.release = threading.Event()
    threading.Thread(target=mock.serve_forever,daemon=True).start()
    report = {"formatVersion":1,"evidenceTier":"development-artifact-rehearsal","candidateAcceptance":False,
              "startedAt":datetime.now(timezone.utc).isoformat(), "artifacts":{role:{k:v for k,v in artifact.items() if k!="path"} for role,artifact in frozen.items()},
              "sdkSha256":args.sdk_sha256,"postgresImage":PG_IMAGE,"stages":{}}
    try:
        with tempfile.TemporaryDirectory(prefix="oxsum-recovery-private-") as private:
            private = Path(private)
            (private/".env").write_text("")
            postgres = []
            for role in ["source","restore"]:
                name="oxsum-recovery-"+slug+"-"+role
                password=secrets.token_hex(24)
                docker("run","--detach","--name",name,"--label","org.oxsum.task=artifact-recovery","--publish","127.0.0.1::5432",
                       "--env","POSTGRES_PASSWORD","--env","POSTGRES_DB=fixture",PG_IMAGE,env={**env,"POSTGRES_PASSWORD":password})
                containers.append(name)
                port=docker("port",name,"5432/tcp").rsplit(":",1)[1]
                for _ in range(100):
                    checked=subprocess.run(["docker","exec",name,"pg_isready","-U","postgres","-d","fixture"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                    if checked.returncode==0:break
                    time.sleep(.1)
                else:raise ValueError("isolated PostgreSQL unavailable")
                postgres.append({"name":name,"url":"postgresql://postgres:"+password+"@127.0.0.1:"+port+"/fixture"})
            runtime_env={**env,"DATABASE_URL":postgres[0]["url"],"OXSUM_SIGNUP":"invite","OXSUM_ADMIN_TOKEN":secrets.token_hex(24),
                         "OXSUM_SECRET_KEY":base64.b64encode(secrets.token_bytes(32)).decode(),"OXSUM_HEAD_SIGNING_KEY":base64.b64encode(secrets.token_bytes(32)).decode(),
                         "OXSUM_DISPLAY_CURRENCY":"USD","OXSUM_DISPLAY_RATE_PER_CREDIT":"1","OXSUM_DISPLAY_DECIMALS":"2","OXSUM_SESSION_COOKIE_SECURE":"false","RUST_LOG":"warn"}
            def runtime(role,environment):
                instance=Runtime(frozen[role],environment,private,private/(role+"-runtime.log"))
                runtimes.append(instance)
                return instance
            stage="old-runtime"
            old=runtime("old",runtime_env)
            old.migrate();old.start()
            state_path=private/"customers.json"
            run(["node",str(REPO/"scripts/recovery-provision.mjs")],env={**runtime_env,"OXSUM_DEMO_BASE":old.base,"OXSUM_DEMO_UPSTREAM":"http://127.0.0.1:"+str(mock.server_port)+"/v1","OXSUM_DEMO_STATE":str(state_path)},cwd=private,log=private/"provision.log")
            customers=json.loads(state_path.read_text())
            first=customers[0]
            held=api(old.base,"/api/v1/log/head",session=first["customerSession"])
            request_id=gateway(old.base,first,"before-upgrade")
            record,balance=settlement(old.base,first,request_id,1)
            original_key=capture(old.base,first,record,held,output/"old-proof")
            second_id=gateway(old.base,customers[1],"second-customer")
            settlement(old.base,customers[1],second_id,1)
            report["stages"][stage]={"status":"passed","customerCount":2,"chargedMinor":CHARGE,"availableMinor":balance["availableMinor"]}
            stage="graceful-drain"
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
                future=executor.submit(gateway,old.base,first,"drain")
                require(mock.started.wait(5),"controlled stream never reached upstream")
                require(api(old.base,"/api/v1/balance",session=first["customerSession"])["reservedMinor"]>0,"controlled request did not reserve funds")
                started=time.monotonic();old.process.terminate();time.sleep(.25)
                require(old.process.poll() is None,"old process exited before the held stream was released")
                mock.release.set()
                drain_id=future.result(timeout=10)
                old.process.wait(timeout=10)
                require(old.process.returncode==0,"old artifact did not drain cleanly")
                report["stages"][stage]={"status":"passed","durationMs":int((time.monotonic()-started)*1000)}
            stage="upgrade"
            current=runtime("current",runtime_env)
            duration=current.migrate();current.start()
            settlement(current.base,first,drain_id,2)
            require(api(current.base,"/api/v1/log/key")==original_key,"signing identity changed during upgrade")
            require(api(current.base,"/api/v1/balance",session=customers[1]["customerSession"])["availableMinor"]==FUNDING-CHARGE,"other customer balance changed")
            upgraded_id=gateway(current.base,first,"after-upgrade")
            settlement(current.base,first,upgraded_id,3)
            report["stages"][stage]={"status":"passed","migrationDurationMs":duration,"oldCredentialReused":True,"retainedSigningKey":True}
            stage="backup-restore"
            current.stop(strict=True)
            source=backup.Connection(container=postgres[0]["name"],database="fixture",environ=env)
            target=backup.Connection(container=postgres[1]["name"],database="fixture",environ=env)
            manifest=backup.backup(source,output/"backup")
            receipt=backup.restore(target,manifest,"restored")
            restore_env={**runtime_env,"DATABASE_URL":postgres[1]["url"].rsplit("/",1)[0]+"/restored"}
            restored=runtime("current",restore_env)
            restored.migrate();restored.start()
            require(api(restored.base,"/api/v1/log/key")==original_key,"restored signing identity differs")
            capture(restored.base,first,record,held,output/"restored-old-proof")
            restored_id=gateway(restored.base,first,"after-restore")
            restored_record,restored_balance=settlement(restored.base,first,restored_id,4)
            capture(restored.base,first,restored_record,held,output/"restored-proof")
            require(api(restored.base,"/api/v1/balance",session=customers[1]["customerSession"])["availableMinor"]==FUNDING-CHARGE,"restored second customer changed")
            report["stages"][stage]={"status":"passed","manifest":str(manifest.relative_to(output)),"receipt":str(receipt.relative_to(output)),"availableMinor":restored_balance["availableMinor"]}
            stage="compatible-rollback"
            restored.stop(strict=True)
            rollback=runtime("rollback",restore_env)
            rollback.migrate();rollback.start()
            require(api(rollback.base,"/api/v1/log/key")==original_key,"rollback signing identity differs")
            rollback_id=gateway(rollback.base,first,"after-compatible-rollback")
            settlement(rollback.base,first,rollback_id,5)
            rollback.stop(strict=True)
            require(mock.attempts==6,"unexpected duplicate provider calls")
            report["stages"][stage]={"status":"passed","sameSchema":args.current_schema,"availableMinor":FUNDING-CHARGE*5,"reservedMinor":0}
            stage="offline-old-and-restored-proof"
            consumer=private/"consumer";consumer.mkdir()
            backup.private_json(consumer/"package.json",{"name":"oxsum-recovery-consumer","private":True,"type":"module"})
            shutil.copyfile(REPO/"examples/verify-consumer/verify.mjs",consumer/"verify.mjs")
            user_config=private/"user.npmrc";user_config.write_text("")
            global_config=private/"global.npmrc";global_config.write_text("")
            run(["npm","install","--ignore-scripts","--no-audit","--no-fund","--userconfig",str(user_config),"--globalconfig",str(global_config),"--cache",str(private/"npm-cache"),str(sdk)],env=env,cwd=consumer,log=private/"npm.log")
            for role in ["old-proof","restored-old-proof","restored-proof"]:
                run(["node","verify.mjs","--dir",str(output/role)],env=env,cwd=consumer,log=output/(role+"-verification.log"))
            report["stages"][stage]={"status":"passed","networkFetches":0,"tamperedInputsRejected":True}
            report["status"]="passed"
    except BaseException:
        report["status"]="failed";report["failedStage"]=stage
        raise
    finally:
        for instance in reversed(runtimes):
            try:instance.stop()
            except (OSError,subprocess.SubprocessError):pass
        mock.release.set();mock.shutdown();mock.server_close()
        for name in reversed(containers):
            subprocess.run(["docker","rm","--force","--volumes",name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=30)
        backup.private_json(output/"rehearsal.json",report)
    files={str(path.relative_to(output)):backup.digest(path) for path in sorted(output.rglob("*")) if path.is_file()}
    backup.private_json(output/"checksums.json",files)
    print("Development artifact recovery rehearsal passed: "+str(output/"rehearsal.json"))


if __name__=="__main__":
    try:main()
    except (ValueError,OSError,UnicodeError,subprocess.SubprocessError,TimeoutError,KeyError,StopIteration,RecursionError):
        raise SystemExit("Artifact recovery rehearsal failed; inspect its private stage report. Final-candidate acceptance remains unperformed.")
