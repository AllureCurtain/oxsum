#!/usr/bin/env python3
"""Run examples on an isolated database; never read a checkout's deployment .env."""
from __future__ import annotations

import argparse
import base64
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
from urllib.parse import parse_qsl, urlsplit, urlunsplit
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
# Lines verify.mjs may emit when the committed fixture's shape does not offer a
# check any target: a missing price/sale binding, an empty exported artifact
# scope or a record without rated lines. Each is a declared coverage bound, not
# an essential assertion left unrun. A skipped evidence-bundle block is NOT in
# this set — the v2 fixture is required (asserted present below) and its absence
# would hide the core check. Anything else that is skipped still fails the gate.
CONDITIONAL_SKIPS = (
    re.compile(r"^\[SKIP\] witnesses: no cosigned checkpoint — this deployment pins no witnesses$"),
    re.compile(r"^\[SKIP\] (price|sale) mutation: no such binding in this legacy bill$"),
    re.compile(r"^\[SKIP\] artifact tamper: the exported scope has no committed artifact$"),
    re.compile(r"^\[SKIP\] missing committed row: the exported scope is empty$"),
    re.compile(r"^\[SKIP\] recorded rate mutation: this record has no rated lines$"),
)
EXPORTS = ("OXSUM_CUSTOMER_ID", "OXSUM_CUSTOMER_KEY", "OXSUM_OWNER_EMAIL", "OXSUM_OWNER_PASSWORD")


def require(ok: bool, message: str) -> None:
    if not ok:
        raise RuntimeError(message)


def clean_environment() -> dict[str, str]:
    return {k: v for k, v in os.environ.items()
            if not k.startswith(("OXSUM_", "DATABASE_", "DOUBLEENTRY_", "PG", "SOAK_"))
            and k.upper() not in {"NODE_OPTIONS", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"}}


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def private_file(path: Path, text: str = "") -> None:
    with path.open("w") as stream:
        os.chmod(path, 0o600)
        stream.write(text)


def stop(process: subprocess.Popen) -> None:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def run(command: list[str], *, cwd: Path, env: dict[str, str], log: Path, timeout: int = 180) -> str:
    private_file(log)
    with log.open("a") as output:
        child = subprocess.Popen(command, cwd=cwd, env=env, stdout=output,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            stop(child)
            raise RuntimeError(f"Step timed out; inspect private log {log.name}") from None
        finally:
            stop(child)
    require(child.returncode == 0, f"Step failed; inspect private log {log.name}")
    return log.read_text()


def assertions(text: str, *, optional_witness: bool = False) -> dict[str, int]:
    lines = text.splitlines()
    require(not any("[FAIL]" in line for line in lines), "An example reported a failed assertion")
    skipped = [line for line in lines if "[SKIP]" in line]
    require(all(optional_witness and any(pattern.match(line) for pattern in CONDITIONAL_SKIPS)
                for line in skipped),
            "An essential example assertion was skipped")
    count = sum(line.startswith("[PASS]") for line in lines)
    require(count > 0, "The example produced no passing assertions")
    return {"passed": count, "optionalWitnessAbsent": len(skipped)}


def exported_credentials(text: str) -> dict[str, str]:
    result = {}
    for key in EXPORTS:
        values = re.findall(rf"^{key}=(.+)$", text, re.MULTILINE)
        require(len(values) == 1, "Fresh setup did not return exactly one identity export")
        result[key] = values[0]
    return result


def database_urls(admin_url: str, name: str) -> tuple[str, str]:
    """One explicit endpoint, without libpq host/db/service query overrides."""
    try:
        parsed = urlsplit(admin_url)
        require(parsed.scheme in {"postgres", "postgresql"} and bool(parsed.hostname)
                and bool(parsed.username) and bool(parsed.path.strip("/"))
                and not parsed.fragment and parsed.port != 0,
                "Provide a complete PostgreSQL URL with one host, user and database")
        require("," not in parsed.hostname and "/" not in parsed.hostname,
                "Multiple hosts and socket paths are not supported")
        pairs = parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True)
        require(all(k == "sslmode" and v in {"disable", "allow", "prefer", "require", "verify-ca", "verify-full"}
                    for k, v in pairs) and len(pairs) <= 1,
                "Only sslmode is permitted in the provisioning URL query")
    except ValueError:
        raise RuntimeError("Invalid PostgreSQL provisioning URL") from None
    return admin_url, urlunsplit(parsed._replace(path="/" + name))


@contextmanager
def scratch_database(admin_url: str, evidence: dict):
    import psycopg
    from psycopg import sql

    name = "oxsum_examples_" + secrets.token_hex(12)
    admin_url, scratch_url = database_urls(admin_url, name)
    # libpq otherwise consults PGHOST, PGSERVICE and similar ambient authority.
    removed = {k: os.environ.pop(k) for k in list(os.environ) if k.startswith("PG")}
    try:
        try:
            connection = psycopg.connect(admin_url, autocommit=True, connect_timeout=10)
        except psycopg.Error:
            raise RuntimeError("Cannot connect to provisioning endpoint; no server started") from None
    finally:
        os.environ.update(removed)
    with connection:
        try:
            connection.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
        except psycopg.Error:
            raise RuntimeError("Cannot create isolated database; no fallback is permitted") from None
        evidence["scratchDatabase"] = name
        evidence["databaseRemoved"] = False
        try:
            yield scratch_url
        finally:
            try:
                connection.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(name)))
                evidence["databaseRemoved"] = True
            except psycopg.Error:
                raise RuntimeError(f"Cleanup failed for owned database {name}; do not report success") from None


def copy_example(source: Path, destination: Path) -> None:
    shutil.copytree(source, destination, ignore=shutil.ignore_patterns(
        "node_modules", ".env", ".env.*", "package-lock.json", ".npmrc"))
    private_file(destination / ".env")


def prepare_sdk(args, work: Path, output: Path, env: dict[str, str]) -> Path:
    target = output / "verify-oxsum-bill.tgz"
    if args.sdk_tarball:
        shutil.copyfile(args.sdk_tarball.resolve(), target)
    else:
        sdk = work / "sdk"
        shutil.copytree(ROOT / "web/verify-sdk", sdk,
                        ignore=shutil.ignore_patterns("node_modules", "dist", "wasm", ".env*", ".npmrc"))
        run(["cargo", "build", "-p", "oxsum-verify", "--release", "--features", "wasm", "--target", "wasm32-unknown-unknown"],
            cwd=ROOT, env=env, log=output / "sdk-build.log", timeout=1200)
        metadata = json.loads(run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                             cwd=ROOT, env=env, log=output / "sdk-metadata.log"))
        wasm = Path(metadata["target_directory"]) / "wasm32-unknown-unknown/release/oxsum_verify.wasm"
        run([os.environ.get("WASM_BINDGEN", "wasm-bindgen"), "--target", "web", "--out-name", "verify_wasm",
             "--out-dir", str(sdk / "wasm"), str(wasm)], cwd=work, env=env, log=output / "bindgen.log")
        run(["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund"], cwd=sdk, env=env, log=output / "sdk-install.log")
        run(["npm", "pack", "--pack-destination", str(output)], cwd=sdk, env=env, log=output / "sdk-pack.log")
        packed = list(output.glob("verify-oxsum-bill-*.tgz"))
        require(len(packed) == 1, "SDK packing did not produce exactly one artifact")
        packed[0].rename(target)
    os.chmod(target, 0o600)
    with tarfile.open(target, "r:gz") as archive:
        required = {"package/dist/index.js", "package/dist/index.d.ts", "package/wasm/verify_wasm.js",
                    "package/wasm/verify_wasm_bg.wasm", "package/LICENSE-MIT", "package/LICENSE-APACHE"}
        require(required.issubset(set(archive.getnames())), "SDK artifact is missing required files")
    return target


def install_sdk(path: Path, tarball: Path, output: Path, env: dict[str, str]) -> None:
    # The copied consumer has no workspace link or caller dependency mutation.
    private_file(path / "package.json", json.dumps({"name": "isolated-example", "private": True, "type": "module"}))
    run(["npm", "install", "--ignore-scripts", "--no-audit", "--no-fund", str(tarball)],
        cwd=path, env=env, log=output / (path.name + "-install.log"))


@contextmanager
def listener(binary: Path, work: Path, env: dict[str, str], output: Path, evidence: dict):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    log = output / "server.log"
    private_file(log)
    with log.open("a") as stream:
        process = subprocess.Popen([str(binary)], cwd=work,
            env={**env, "OXSUM_ADDR": f"127.0.0.1:{port}"}, stdout=stream, stderr=stream, start_new_session=True)
    evidence["listenerStopped"] = False
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            require(process.poll() is None, "Server exited; inspect private server.log")
            try:
                with opener.open(base + "/healthz", timeout=2) as response:
                    if response.status == 200:
                        break
            except OSError:
                time.sleep(.2)
        else:
            raise RuntimeError("Server readiness timed out; inspect private server.log")
        yield base
    finally:
        stop(process)
        evidence["listenerStopped"] = process.poll() is not None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database-url", default=os.environ.get("DATABASE_URL", "postgres://postgres:postgres@127.0.0.1:5432/postgres"))
    parser.add_argument("--binary", type=Path, help="Test these exact server bytes instead of building")
    parser.add_argument("--sdk-tarball", type=Path, help="Install these exact SDK bytes instead of packing")
    parser.add_argument("--examples-root", type=Path, default=ROOT, help="Source checkout containing maintained examples")
    parser.add_argument("--output", type=Path, help="New private directory for hashes, counts and private logs")
    args = parser.parse_args()
    source = args.examples_root.resolve() / "examples"
    require((source / "consumption/bootstrap.mjs").is_file(),
            "Maintained consumption bootstrap is required (PR #751); select its checkout with --examples-root")
    require((source / "verify-consumer/fixtures/evidence-bundle.json").is_file(), "Complete-period verification fixture missing")
    for tool in ["node", "npm"] + ([] if args.binary and args.sdk_tarball else ["cargo"]):
        require(shutil.which(tool) is not None, f"Missing prerequisite: {tool}")
    output = args.output.resolve() if args.output else Path(tempfile.mkdtemp(prefix="oxsum-example-evidence-"))
    if args.output:
        output.mkdir(parents=True, exist_ok=False, mode=0o700)
    os.chmod(output, 0o700)
    env = clean_environment()
    evidence = {"tier": "local deterministic examples; no real provider/payment certification", "status": "failed", "assertions": {},
                "sourceFiles": {str(p.relative_to(source)): digest(p) for directory in
                    ["consumption", "scoped-management", "webhook-tools", "verify-consumer"]
                    for p in sorted((source / directory).rglob("*")) if p.is_file()
                    and not any(part.startswith(".") or part == "node_modules" for part in p.relative_to(source).parts)},
                "runnerSha256": digest(Path(__file__)),
                "setupSha256": digest(ROOT / "scripts/verify-examples-setup.mjs")}
    print(f"Private evidence: {output}", flush=True)
    try:
        with tempfile.TemporaryDirectory(prefix="oxsum-examples-") as temp:
            work = Path(temp)
            private_file(work / ".env")
            if args.binary:
                binary_source = args.binary.resolve()
            else:
                run(["cargo", "build", "-p", "oxsum-server", "--bin", "oxsum"], cwd=ROOT, env=env,
                    log=output / "build.log", timeout=1200)
                metadata = json.loads(run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                                         cwd=ROOT, env=env, log=output / "metadata.log"))
                binary_source = Path(metadata["target_directory"]) / "debug/oxsum"
            binary = work / "oxsum"
            shutil.copyfile(binary_source, binary)
            binary.chmod(0o700)
            evidence["binarySha256"] = digest(binary)
            tarball = prepare_sdk(args, work, output, env)
            evidence["sdkSha256"] = digest(tarball)
            for name in ["consumption", "scoped-management", "webhook-tools", "verify-consumer"]:
                copy_example(source / name, work / name)
            install_sdk(work / "consumption", tarball, output, env)
            install_sdk(work / "verify-consumer", tarball, output, env)
            runtime = {**env, "OXSUM_SECRET_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
                       "OXSUM_HEAD_SIGNING_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
                       "OXSUM_ADMIN_TOKEN": secrets.token_urlsafe(32), "OXSUM_SESSION_COOKIE_SECURE": "false",
                       "OXSUM_DISPLAY_CURRENCY": "USD", "OXSUM_DISPLAY_RATE_PER_CREDIT": "1"}
            with scratch_database(args.database_url, evidence) as database:
                runtime["DATABASE_URL"] = database
                run([str(binary), "--migrate-only"], cwd=work, env=runtime, log=output / "migrate.log")
                with listener(binary, work, runtime, output, evidence) as base:
                    example_env = {**env, "OXSUM_BASE_URL": base, "OXSUM_ADMIN_TOKEN": runtime["OXSUM_ADMIN_TOKEN"]}
                    text = run(["node", "consumption.mjs"], cwd=work / "consumption", env=example_env,
                               log=output / "consumption.log")
                    evidence["assertions"]["consumption"] = assertions(text)
                    example_env.update(exported_credentials(text))
                    example_env.update({"OXSUM_EVENT_CHANNEL": "isolated-events", "OXSUM_EVENT_CODE": "local-event"})
                    shutil.copyfile(ROOT / "scripts/verify-examples-setup.mjs", work / "setup.mjs")
                    setup = run(["node", "setup.mjs"], cwd=work, env=example_env, log=output / "setup.log")
                    evidence["assertions"]["event-setup"] = assertions(setup)
                    for name, directory, script, options in [
                        ("management", "scoped-management", "scoped-management.mjs", []),
                        ("signed-events", "webhook-tools", "sign-and-replay.mjs", []),
                        ("dedup", "webhook-tools", "dedup-check.mjs", []),
                        ("storm", "webhook-tools", "retry-storm.mjs", ["--n=12"]),
                    ]:
                        text = run(["node", script, *options], cwd=work / directory,
                                   env=example_env, log=output / (name + ".log"))
                        evidence["assertions"][name] = assertions(text)
                        print(f"[PASS] {name}: {evidence['assertions'][name]['passed']} assertions", flush=True)
            # The billing server and its database are gone before this independent check.
            text = run(["node", "verify.mjs"], cwd=work / "verify-consumer", env=env, log=output / "offline.log")
            evidence["assertions"]["offline"] = assertions(text, optional_witness=True)
            evidence["status"] = "passed"
    finally:
        private_file(output / "result.json", json.dumps(evidence, indent=2) + "\n")
    print("[PASS] All essential examples; owned server/database removed; offline verifier passed", flush=True)


if __name__ == "__main__":
    def interrupted(_signal, _frame):
        raise InterruptedError("Interrupted; cleaning resources owned by this run")
    signal.signal(signal.SIGTERM, interrupted)
    try:
        main()
    except (RuntimeError, OSError, ImportError, tarfile.TarError) as error:
        # Database connection exceptions are deliberately translated above.
        print(f"[FAIL] {error}", file=sys.stderr)
        sys.exit(1)
