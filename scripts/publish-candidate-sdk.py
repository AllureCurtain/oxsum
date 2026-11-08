#!/usr/bin/env python3
"""Publish only the original verified SDK tarball; never build, repack or deploy."""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tarfile
import tempfile
import threading
import urllib.error
import urllib.parse
import urllib.request
import zipfile

import candidate_bundle as gate

PACKAGE = "verify-oxsum-bill"
TAG = re.compile(r"[a-z][a-z0-9-]{0,62}")
VERSION = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def registry_url(value, fixture):
    if fixture:
        match = re.fullmatch(r"http://127\.0\.0\.1:([0-9]{1,5})/", value)
        gate.require(match and 0 < int(match[1]) <= 65535, "fixture registry must be explicit http://127.0.0.1:<port>/")
    else:
        gate.require(value == "https://registry.npmjs.org/", "production publication requires the official HTTPS npm registry")
    return value


class Registry:
    """Bound credentials to one exact origin and never follow registry redirects."""

    def __init__(self, url, token):
        self.url = url
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect(), urllib.request.ProxyHandler({}))

    def request(self, method, path, data=None, missing=False, maximum=16 * 1024 * 1024):
        tarball = re.fullmatch(r"verify-oxsum-bill/-/verify-oxsum-bill-(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)\.tgz", path)
        gate.require(path == PACKAGE or (method == "GET" and tarball),
                     "unexpected npm registry path")
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = "Bearer " + self.token
        if data is not None:
            headers["Content-Type"] = "application/json"
        try:
            with self.opener.open(urllib.request.Request(self.url + path, data=data, headers=headers, method=method), timeout=60) as response:
                raw = response.read(maximum + 1)
                gate.require(len(raw) <= maximum, "oversized npm registry response")
                return raw
        except urllib.error.HTTPError as error:
            try:
                if error.code == 404 and missing:
                    try:
                        value = json.loads(error.read(65536))
                    except ValueError:
                        value = {}
                    if value.get("error") in {"Not found", "not_found", "package not found"}:
                        return None
                raise ValueError(f"npm registry {method} failed with HTTP {error.code}; publication refused") from error
            finally:
                error.close()
        except urllib.error.URLError as error:
            raise ValueError("npm registry unavailable; publication refused") from error

    def metadata(self):
        raw = self.request("GET", PACKAGE, missing=True)
        if raw is None:
            return None
        value = json.loads(raw)
        gate.require(value.get("name", value.get("_id")) == PACKAGE and isinstance(value.get("versions"), dict)
                     and isinstance(value.get("dist-tags"), dict), "invalid npm package metadata")
        return value

    def check(self, sdk, tag, size, checksum):
        metadata = self.metadata()
        if metadata is None:
            return False
        bound = metadata["dist-tags"].get(tag)
        gate.require(bound in {None, sdk["version"]}, "distribution tag already points to another version")
        version = metadata["versions"].get(sdk["version"])
        if version is None:
            gate.require(bound is None, "distribution tag points to missing version metadata")
            return False
        gate.require(version.get("name") == PACKAGE and version.get("version") == sdk["version"]
                     and version.get("dist", {}).get("integrity") == sdk["integrity"], "existing SDK version has different identity or integrity")
        tarball = version["dist"].get("tarball", "")
        expected_url = self.url + f"{PACKAGE}/-/{PACKAGE}-{sdk['version']}.tgz"
        gate.require(tarball == expected_url, "registry tarball URL leaves the selected package origin")
        raw = self.request("GET", tarball[len(self.url):], maximum=size)
        gate.require(len(raw) == size and hashlib.sha256(raw).hexdigest() == checksum,
                     "downloaded SDK tarball bytes differ from candidate")
        gate.require(bound == sdk["version"], "existing SDK version is missing the selected tag; explicit registry repair required")
        return True


class PublishRelay:
    """Let npm form its real publication request without exposing remote credentials.

    npm only sees this bounded loopback endpoint and an ephemeral token. Python
    forwards one validated package PUT to the selected registry with redirects
    refused. Only registry transport URLs change; original gzip bytes do not.
    """

    def __init__(self, registry, sdk, tag, tarball):
        self.registry, self.sdk, self.tag = registry, sdk, tag
        self.bytes = tarball.read_bytes()
        self.token = secrets.token_hex(24)
        self.puts = 0
        self.lock = threading.Lock()
        self.failure = None
        relay = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def answer(self, code, value):
                raw = json.dumps(value).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def authorized(self):
                return self.path == "/" + PACKAGE and self.headers.get("Authorization") == "Bearer " + relay.token

            def do_GET(self):
                if not self.authorized():
                    self.answer(403, {"error": "forbidden"})
                    return
                try:
                    value = relay.registry.metadata()
                    self.answer(200 if value else 404, value or {"error": "Not found"})
                except (ValueError, OSError) as error:
                    relay.failure = str(error)
                    self.answer(502, {"error": "registry refused"})

            def do_PUT(self):
                if not self.authorized():
                    self.answer(403, {"error": "forbidden"})
                    return
                try:
                    size = int(self.headers.get("Content-Length", "0"))
                    gate.require(0 < size <= len(relay.bytes) * 2 + 1024 * 1024, "oversized npm publication body")
                    body = json.loads(self.rfile.read(size))
                    with relay.lock:
                        relay.forward(body)
                    self.answer(201, {"ok": True})
                except (ValueError, KeyError, TypeError, OSError) as error:
                    relay.failure = str(error)
                    self.answer(409, {"error": "candidate publication refused"})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/"

    def forward(self, body):
        gate.require(self.puts == 0, "a second npm publication request is not allowed")
        sdk, version = self.sdk, self.sdk["version"]
        gate.require(body.get("_id") == PACKAGE and body.get("name") == PACKAGE
                     and body.get("dist-tags") == {self.tag: version}
                     and set(body.get("versions", {})) == {version}, "npm publication metadata differs")
        name = f"{PACKAGE}-{version}.tgz"
        attachments = body.get("_attachments", {})
        gate.require(set(attachments) == {name}, "npm publication attachments differ")
        attachment = attachments[name]
        raw = base64.b64decode(attachment["data"], validate=True)
        gate.require(raw == self.bytes and attachment.get("length") == len(raw), "npm changed the original candidate tarball")
        dist = body["versions"][version].get("dist", {})
        gate.require(dist.get("integrity") == sdk["integrity"] and dist.get("shasum") == hashlib.sha1(raw).hexdigest(),
                     "npm publication integrity differs")
        # The npm request targets the private relay, so replace only the
        # transport metadata's registry URL before forwarding its original blob.
        dist["tarball"] = self.registry.url + f"{PACKAGE}/-/{name}"
        if self.registry.check(sdk, self.tag, len(raw), hashlib.sha256(raw).hexdigest()):
            return
        self.puts += 1
        self.registry.request("PUT", PACKAGE, json.dumps(body, separators=(",", ":")).encode())

    def __enter__(self):
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def run_npm(registry, sdk, tag, tarball, work):
    npm = shutil.which("npm")
    gate.require(npm, "npm must be installed explicitly")
    cwd = work / "npm"
    cwd.mkdir()
    # An allowlisted environment removes npm/proxy/Node preload configuration.
    # Explicit config files prevent a user/global/project npmrc from redirecting
    # publication. The remote token never enters npm's process or config.
    environment = {key: os.environ[key] for key in ["PATH", "SYSTEMROOT", "WINDIR", "TMPDIR"] if key in os.environ}
    environment.update({"CI": "true", "NO_COLOR": "1"})
    empty = cwd / "global.npmrc"
    empty.write_text("")
    with PublishRelay(registry, sdk, tag, tarball) as relay:
        config = cwd / "user.npmrc"
        config.write_text(f"registry={relay.url}\n//127.0.0.1:{relay.server.server_address[1]}/:_authToken={relay.token}\n")
        config.chmod(0o600)
        command = [npm, "publish", str(tarball), "--ignore-scripts", "--provenance=false", "--access=public", "--tag=" + tag,
                   "--registry=" + relay.url, "--userconfig=" + str(config), "--globalconfig=" + str(empty),
                   "--cache=" + str(cwd / "cache"), "--fetch-retries=0", "--fetch-timeout=60000", "--update-notifier=false", "--json"]
        result = subprocess.run(command, cwd=cwd, env=environment, capture_output=True, text=True, timeout=180)
        gate.require(result.returncode == 0 and relay.failure is None,
                     relay.failure or "npm publication failed; no successful receipt was written")


def publish(args):
    url = registry_url(args.registry, args.local_fixture)
    gate.require(gate.SHA.fullmatch(args.sha) and re.fullmatch(r"[1-9][0-9]*", args.run) and args.attempt > 0,
                 "explicit source and candidate run/attempt required")
    gate.require(VERSION.fullmatch(args.version) and TAG.fullmatch(args.tag), "explicit valid SDK version and distribution tag required")
    root, receipt_path = args.directory.resolve(), args.receipt.resolve()
    gate.require(not receipt_path.is_relative_to(root) and receipt_path.parent.is_dir()
                 and not receipt_path.exists() and not args.receipt.is_symlink(), "receipt must be a new file outside the candidate")
    token = "" if args.local_fixture else os.environ.get("OXSUM_NPM_TOKEN", "")
    gate.require(args.local_fixture or token, "OXSUM_NPM_TOKEN is required for public registry publication")
    gate.require(not any(char in token for char in "\r\n"), "invalid registry token")
    with tempfile.TemporaryDirectory(prefix="oxsum-sdk-publish-") as name:
        work = Path(name)
        frozen = work / "candidate"
        shutil.copytree(root, frozen, symlinks=True)
        manifest = gate.validate(frozen, args.sha)
        gate.require(manifest["repository"] == "AllureCurtain/oxsum" and manifest["workflow"]["runId"] == args.run
                     and manifest["workflow"]["attempt"] == args.attempt, "candidate repository/run/attempt differs")
        gate.require(manifest["sdk"]["version"] == args.version, "candidate SDK version differs from selector")
        manifest_hash = gate.digest(frozen / "candidate-manifest.json")
        if not args.local_fixture:
            gate.validate_online(frozen, manifest)
        sdk = {key: manifest["sdk"][key] for key in ["name", "version", "integrity"]}
        tarball = gate.local_file(frozen, manifest["sdk"]["tarball"])
        record = manifest["artifacts"][manifest["sdk"]["tarball"]]
        gate.require(gate.digest(frozen / "candidate-manifest.json") == manifest_hash
                     and tarball.stat().st_size == record["size"]
                     and gate.digest(tarball) == record["sha256"] and gate.sdk_identity(tarball) == sdk,
                     "candidate SDK changed after validation")
        with tarfile.open(tarball, "r:gz") as archive:
            package = json.loads(gate.archive_json(archive, gate.archive_members(archive), "package/package.json"))
        gate.require(not package.get("publishConfig"), "candidate publishConfig is unsupported; regenerate without publish overrides")
        registry = Registry(url, token)
        existing = registry.check(sdk, args.tag, record["size"], record["sha256"])
        if not existing:
            run_npm(registry, sdk, args.tag, tarball, work)
        gate.require(registry.check(sdk, args.tag, record["size"], record["sha256"]), "published SDK version is unavailable")
        receipt = {"formatVersion": 1, "evidenceTier": "local-fixture" if args.local_fixture else "verified-candidate",
                   "sourceSha": args.sha, "repository": manifest["repository"], "workflow": manifest["workflow"],
                   "candidateManifestSha256": manifest_hash, "tarballSha256": record["sha256"],
                   **sdk, "registry": url, "tag": args.tag, "result": "already-present" if existing else "published",
                   "publishedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")}
        with receipt_path.open("x") as target:
            target.write(json.dumps(receipt, indent=2) + "\n")
        return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    for field in ["sha", "run", "version", "tag", "registry"]:
        parser.add_argument("--" + field, required=True)
    parser.add_argument("--attempt", type=int, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--local-fixture", action="store_true", help="numeric loopback only; not release evidence")
    receipt = publish(parser.parse_args())
    print(f"SDK {receipt['result']}: {receipt['name']}@{receipt['version']} ({receipt['evidenceTier']})")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile) as error:
        raise SystemExit("SDK publication refused: " + str(error))
