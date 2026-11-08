#!/usr/bin/env python3
"""Promote a verified candidate's saved image; never build, publish an SDK or deploy."""
import argparse
import base64
from datetime import datetime, timezone
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile

import candidate_bundle as gate

DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
VERSION = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?")
REPOSITORY_PATH = re.compile(r"[a-z0-9]+(?:[._-][a-z0-9]+)*(?:/[a-z0-9]+(?:[._-][a-z0-9]+)*)+")
MANIFEST_TYPES = {"application/vnd.oci.image.manifest.v1+json",
                  "application/vnd.docker.distribution.manifest.v2+json"}
INDEX_TYPES = {"application/vnd.oci.image.index.v1+json",
               "application/vnd.docker.distribution.manifest.list.v2+json"}
ACCEPT = ", ".join(sorted(MANIFEST_TYPES | INDEX_TYPES))
MEDIA_EQUIVALENTS = {
    "application/vnd.docker.container.image.v1+json": "application/vnd.oci.image.config.v1+json",
    "application/vnd.docker.image.rootfs.diff.tar.gzip": "application/vnd.oci.image.layer.v1.tar+gzip",
}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """Never forward registry credentials to a redirect destination."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def destination(value, fixture):
    """Accept an explicit version tag, with a loopback-only fixture exception."""
    host, slash, path = value.partition("/")
    repository, colon, version = path.rpartition(":")
    gate.require(slash and colon and REPOSITORY_PATH.fullmatch(repository), "invalid destination repository")
    gate.require(VERSION.fullmatch(version) and len(version) <= 128, "destination must name an explicit vX.Y.Z[-suffix] version")
    if fixture:
        match = re.fullmatch(r"127\.0\.0\.1:([0-9]{1,5})", host)
        gate.require(match and 0 < int(match[1]) <= 65535, "local fixtures require a numeric 127.0.0.1 registry port")
    else:
        gate.require(host == "ghcr.io", "production promotion supports the authorized GHCR registry")
    return host, repository, version


def archive_manifest(path):
    """Describe original archive member bytes without rebuilding or recompressing."""
    descriptors = []
    with tarfile.open(path, "r:*") as archive:
        members = gate.archive_members(archive)
        saved = json.loads(gate.archive_json(archive, members, "manifest.json"))[0]
        for name in [saved["Config"], *saved["Layers"]]:
            stream = archive.extractfile(members[name])
            compressed = stream.read(2) == b"\x1f\x8b"
            stream.seek(0)
            hashed = hashlib.sha256()
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                hashed.update(block)
            media_type = "application/vnd.oci.image.layer.v1.tar" + ("+gzip" if compressed else "")
            descriptors.append({"mediaType": media_type, "size": members[name].size, "digest": "sha256:" + hashed.hexdigest()})
    descriptors[0]["mediaType"] = "application/vnd.oci.image.config.v1+json"
    return {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": descriptors[0], "layers": descriptors[1:]}


class Registry:
    """Distribution API client; only a recognized missing manifest permits a write."""

    def __init__(self, host, repository, fixture=False, write=True):
        self.host = host
        self.repository = repository
        self.base = ("http://" if fixture else "https://") + host
        self.fixture = fixture
        self.write = write
        self.authorization = None
        self.opener = urllib.request.build_opener(NoRedirect())

    def authenticate(self, challenge):
        gate.require(not self.fixture, "local fixture registry unexpectedly requires authentication")
        # Challenge order is not significant. Never send a credential to an
        # arbitrary token realm supplied by a registry/proxy response.
        gate.require(challenge.lower().startswith("bearer "), "unexpected registry authentication challenge")
        fields = dict(re.findall(r'([a-z]+)="([^"\r\n]+)"', challenge[7:]))
        actions = fields.get("scope", "").removeprefix(f"repository:{self.repository}:")
        gate.require(fields.get("realm") == "https://ghcr.io/token" and fields.get("service") == "ghcr.io"
                     and fields.get("scope", "").startswith(f"repository:{self.repository}:")
                     and set(actions.split(",")) <= {"pull", "push"} and "pull" in actions.split(","),
                     "unexpected registry authentication challenge")
        user, token = os.environ.get("OXSUM_REGISTRY_USERNAME", ""), os.environ.get("OXSUM_REGISTRY_TOKEN", "")
        gate.require(user and token, "registry username and token are required for promotion")
        headers = {"Authorization": "Basic " + base64.b64encode(f"{user}:{token}".encode()).decode()}
        actions = "pull,push" if self.write else "pull"
        query = urllib.parse.urlencode({"service": "ghcr.io", "scope": f"repository:{self.repository}:{actions}"})
        try:
            with self.opener.open(urllib.request.Request(fields["realm"] + "?" + query, headers=headers), timeout=60) as response:
                value = json.loads(response.read(2 * 1024 * 1024 + 1))
        except (urllib.error.URLError, ValueError) as error:
            raise ValueError("registry authentication failed") from error
        bearer = value.get("token", value.get("access_token"))
        gate.require(isinstance(bearer, str) and bearer and "\n" not in bearer and "\r" not in bearer,
                     "registry authentication returned no valid token")
        self.authorization = "Bearer " + bearer

    def open(self, method, url, *, data=None, headers=None, missing=False, blob_redirect=False):
        headers = dict(headers or {})
        for attempt in range(2):
            if self.authorization:
                headers["Authorization"] = self.authorization
            if hasattr(data, "seek"):
                data.seek(0)
            try:
                return self.opener.open(urllib.request.Request(url, data=data, headers=headers, method=method), timeout=60)
            except urllib.error.HTTPError as error:
                try:
                    if error.code == 401 and attempt == 0:
                        self.authenticate(error.headers.get("WWW-Authenticate", ""))
                        continue
                    if error.code == 404 and missing:
                        try:
                            errors = json.loads(error.read(65536)).get("errors", [])
                        except ValueError:
                            errors = []
                        if errors and all(row.get("code") in {"MANIFEST_UNKNOWN", "NAME_UNKNOWN"} for row in errors):
                            return None
                    if error.code in {301, 302, 303, 307, 308} and blob_redirect:
                        location = error.headers.get("Location", "")
                        parsed = urllib.parse.urlsplit(location)
                        gate.require(not self.fixture and parsed.scheme == "https"
                                     and parsed.hostname == "pkg-containers.githubusercontent.com"
                                     and not parsed.username and not parsed.password and parsed.port in {None, 443},
                                     "unexpected registry blob redirect")
                        # Signed GitHub blob URL, deliberately without Authorization.
                        return self.opener.open(urllib.request.Request(location), timeout=60)
                    raise ValueError(f"registry {method} failed with HTTP {error.code}; refusing publication") from error
                finally:
                    error.close()
            except urllib.error.URLError as error:
                raise ValueError(f"registry {method} failed; refusing publication") from error
        raise ValueError("registry authentication did not complete")

    def manifest(self, reference, missing=False):
        response = self.open("GET", f"{self.base}/v2/{self.repository}/manifests/{reference}",
                             headers={"Accept": ACCEPT}, missing=missing)
        if response is None:
            return None
        with response:
            raw = response.read(2 * 1024 * 1024 + 1)
            gate.require(len(raw) <= 2 * 1024 * 1024, "oversized registry metadata")
            observed = "sha256:" + hashlib.sha256(raw).hexdigest()
            expected = reference if DIGEST.fullmatch(reference) else response.headers.get("Docker-Content-Digest")
            gate.require(observed == expected, "registry response digest differs from its bytes")
            return observed, json.loads(raw)

    def identity(self, reference, image, expected_manifest, missing=False):
        result = self.manifest(reference, missing=missing)
        if result is None:
            return None
        top_digest, manifest = result
        platform_digest = top_digest
        if manifest.get("mediaType") in INDEX_TYPES:
            gate.require(manifest.get("schemaVersion") == 2, "unsupported registry index schema")
            selected = []
            for child in manifest.get("manifests", []):
                platform = child.get("platform", {})
                if platform.get("os") == "linux" and platform.get("architecture") == "amd64" and not platform.get("variant"):
                    selected.append(child)
                else:
                    gate.require(platform == {"os": "unknown", "architecture": "unknown"}
                                 and child.get("annotations", {}).get("vnd.docker.reference.type") == "attestation-manifest",
                                 "registry index has an unexpected platform")
            gate.require(len(selected) == 1 and DIGEST.fullmatch(selected[0].get("digest", "")),
                         "registry index must select exactly one linux/amd64 manifest")
            platform_digest, manifest = self.manifest(selected[0]["digest"])
        gate.require(manifest.get("schemaVersion") == 2 and manifest.get("mediaType") in MANIFEST_TYPES,
                     "unsupported registry image manifest")
        gate.require(manifest.get("config", {}).get("digest") == image["configId"],
                     "destination version contains a different image config")
        layers = manifest.get("layers", [])
        gate.require(layers and all(DIGEST.fullmatch(row.get("digest", "")) for row in layers),
                     "invalid registry layer descriptors")
        actual = [(row.get("digest"), row.get("size"), MEDIA_EQUIVALENTS.get(row.get("mediaType"), row.get("mediaType")))
                  for row in [manifest["config"], *layers]]
        expected = [(row["digest"], row["size"], row["mediaType"])
                    for row in [expected_manifest["config"], *expected_manifest["layers"]]]
        gate.require(actual == expected, "destination config/layer bytes differ from candidate archive")
        return {"registryDigest": top_digest, "platformManifestDigest": platform_digest}

    def upload_blob(self, stream, size, checksum):
        gate.require(self.write, "read-only registry client cannot upload")
        with self.open("POST", f"{self.base}/v2/{self.repository}/blobs/uploads/", data=b"") as response:
            location = urllib.parse.urljoin(self.base, response.headers.get("Location", ""))
            gate.require(response.status == 202, "registry did not start a blob upload")
        parsed = urllib.parse.urlsplit(location)
        gate.require(parsed.scheme + "://" + parsed.netloc == self.base and not parsed.fragment
                     and parsed.path.startswith(f"/v2/{self.repository}/blobs/uploads/"), "unsafe registry upload location")
        query = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)
        gate.require(not any(key == "digest" for key, _ in query), "registry supplied an upload digest")
        location = urllib.parse.urlunsplit(parsed._replace(query=urllib.parse.urlencode(query + [("digest", checksum)])))
        with self.open("PUT", location, data=stream,
                       headers={"Content-Type": "application/octet-stream", "Content-Length": str(size)}) as response:
            gate.require(response.status == 201 and response.headers.get("Docker-Content-Digest") == checksum,
                         "registry did not retain the uploaded blob digest")

    def publish(self, archive_path, manifest):
        gate.require(self.write, "read-only registry client cannot publish")
        # Transfer the original config and layer member bytes. Docker save does
        # not retain a registry manifest, so create only that transport metadata.
        # No image build, layer recompression or application execution occurs.
        with tarfile.open(archive_path, "r:*") as archive:
            members = gate.archive_members(archive)
            saved = json.loads(gate.archive_json(archive, members, "manifest.json"))[0]
            descriptors = [manifest["config"], *manifest["layers"]]
            for name, descriptor in zip([saved["Config"], *saved["Layers"]], descriptors):
                member = members[name]
                stream = archive.extractfile(member)
                self.upload_blob(stream, member.size, descriptor["digest"])
        raw = json.dumps(manifest, separators=(",", ":"), sort_keys=True).encode()
        checksum = "sha256:" + hashlib.sha256(raw).hexdigest()
        # Upload by immutable digest first. The version tag is the sole mutable
        # write, performed only after a second conflict check by the caller.
        with self.open("PUT", f"{self.base}/v2/{self.repository}/manifests/{checksum}", data=raw,
                       headers={"Content-Type": manifest["mediaType"]}) as response:
            gate.require(response.status == 201 and response.headers.get("Docker-Content-Digest") == checksum,
                         "registry did not retain the uploaded image manifest")
        return checksum, raw

    def tag(self, version, checksum, raw):
        gate.require(self.write, "read-only registry client cannot tag")
        with self.open("PUT", f"{self.base}/v2/{self.repository}/manifests/{version}", data=raw,
                       headers={"Content-Type": "application/vnd.oci.image.manifest.v1+json"}) as response:
            gate.require(response.status == 201 and response.headers.get("Docker-Content-Digest") == checksum,
                         "registry tag does not identify the uploaded manifest")

    def download_blob(self, descriptor, target):
        checksum = descriptor["digest"]
        gate.require(DIGEST.fullmatch(checksum) and isinstance(descriptor.get("size"), int)
                     and descriptor["size"] > 0, "invalid blob descriptor")
        hashed = hashlib.sha256()
        size = 0
        with self.open("GET", f"{self.base}/v2/{self.repository}/blobs/{checksum}", blob_redirect=True) as response, target.open("xb") as output:
            for block in iter(lambda: response.read(1024 * 1024), b""):
                size += len(block)
                gate.require(size <= descriptor["size"], "registry blob exceeds its descriptor size")
                hashed.update(block)
                output.write(block)
        gate.require(size == descriptor["size"] and "sha256:" + hashed.hexdigest() == checksum,
                     "registry blob differs from its size/digest")

    def verify_pulled(self, identity, work, image):
        _, manifest = self.manifest(identity["platformManifestDigest"])
        config_path = work / "pulled-config.json"
        self.download_blob(manifest["config"], config_path)
        gate.require(config_path.stat().st_size <= 2 * 1024 * 1024, "oversized image config")
        config = json.loads(config_path.read_bytes())
        gate.require(manifest["config"]["digest"] == image["configId"], "pulled config differs")
        gate.require(config.get("os", "") + "/" + config.get("architecture", "") == image["platform"], "pulled platform differs")
        gate.require((config.get("config", {}).get("Labels") or {}).get("org.opencontainers.image.revision") == image["sourceSha"],
                     "pulled source revision differs")
        diffs = config.get("rootfs", {}).get("diff_ids", [])
        gate.require(len(diffs) == len(manifest["layers"]), "pulled layer count differs")
        for index, (layer, expected) in enumerate(zip(manifest["layers"], diffs)):
            path = work / f"pulled-layer-{index}"
            self.download_blob(layer, path)
            with path.open("rb") as raw:
                compressed = raw.read(2) == b"\x1f\x8b"
                raw.seek(0)
                stream = gzip.GzipFile(fileobj=raw) if compressed else raw
                hashed = hashlib.sha256()
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    hashed.update(block)
            gate.require("sha256:" + hashed.hexdigest() == expected, "pulled layer differs from candidate rootfs")
            path.unlink()


def promote(args):
    """Validate a private snapshot before any registry mutation and retain a receipt."""
    root = args.directory.resolve()
    host, repository, version = destination(args.destination, args.local_fixture)
    gate.require(gate.SHA.fullmatch(args.sha), "full lowercase source SHA required")
    gate.require(gate.REPOSITORY.fullmatch(args.repository), "invalid source repository")
    gate.require(re.fullmatch(r"[1-9][0-9]*", args.run) and args.attempt > 0, "positive candidate run/attempt required")
    receipt_path = args.receipt.resolve()
    gate.require(not receipt_path.is_relative_to(root), "receipt must be outside the immutable candidate bundle")
    gate.require(receipt_path.parent.is_dir() and not receipt_path.exists() and not args.receipt.is_symlink(),
                 "receipt parent must exist and receipt must not already exist")
    with tempfile.TemporaryDirectory(prefix="oxsum-image-promotion-") as name:
        work = Path(name)
        frozen = work / "candidate"
        shutil.copytree(root, frozen, symlinks=True)
        manifest = gate.validate(frozen, args.sha)
        gate.require(manifest["repository"] == args.repository and manifest["workflow"]["runId"] == args.run
                     and manifest["workflow"]["attempt"] == args.attempt, "candidate repository/run/attempt differs")
        manifest_hash = gate.digest(frozen / "candidate-manifest.json")
        if not args.local_fixture:
            gate.validate_online(frozen, manifest)
        archive = gate.local_file(frozen, manifest["image"]["archive"])
        archive_hash = manifest["artifacts"][manifest["image"]["archive"]]["sha256"]
        gate.require(gate.digest(archive) == archive_hash and gate.digest(frozen / "candidate-manifest.json") == manifest_hash,
                     "candidate changed after validation")
        image = {key: manifest["image"][key] for key in ["configId", "platform", "sourceSha"]}
        gate.require(gate.image_identity(archive, args.sha) == image, "candidate image changed after validation")
        expected_manifest = archive_manifest(archive)
        registry = Registry(host, repository, args.local_fixture)
        published = registry.identity(version, image, expected_manifest, missing=True)
        result = "already-present"
        if published is None:
            checksum, raw = registry.publish(archive, expected_manifest)
            # Registries have no compare-and-swap tag API. The workflow serializes
            # destination writers; other writers require external serialization.
            published = registry.identity(version, image, expected_manifest, missing=True)
            if published is None:
                registry.tag(version, checksum, raw)
                published = registry.identity(version, image, expected_manifest)
                gate.require(published["registryDigest"] == checksum, "published manifest digest differs")
                result = "published"
        immutable = f"{host}/{repository}@{published['registryDigest']}"
        registry.verify_pulled(published, work, image)
        gate.require(registry.identity(version, image, expected_manifest) == published, "destination tag changed during promotion")
        receipt = {"formatVersion": 1, "evidenceTier": "local-fixture" if args.local_fixture else "verified-candidate",
                   "sourceSha": args.sha, "repository": args.repository, "workflow": manifest["workflow"],
                   "candidateManifestSha256": manifest_hash, "archiveSha256": archive_hash, **image,
                   "destination": args.destination, **published, "immutableReference": immutable,
                   "result": result, "promotedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")}
        with receipt_path.open("x") as output:
            output.write(json.dumps(receipt, indent=2) + "\n")
        return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    for name in ["sha", "repository", "run", "destination"]:
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--attempt", type=int, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--local-fixture", action="store_true", help="loopback-only rehearsal; never release evidence")
    receipt = promote(parser.parse_args())
    print(f"Image {receipt['result']}: {receipt['immutableReference']} ({receipt['evidenceTier']})")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError, tarfile.TarError, zipfile.BadZipFile) as error:
        raise SystemExit("Image promotion refused: " + str(error))
