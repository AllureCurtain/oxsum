#!/usr/bin/env python3
"""Collect an allowlisted support report without logs, secrets or provider probes."""
import argparse
from datetime import datetime, timezone
import ipaddress
import json
import os
from pathlib import Path
import re
import stat
import urllib.error
import urllib.parse
import urllib.request

CONFIG_FLAGS = {"smtpConfigured", "publicUrlConfigured", "credentialSealConfigured", "headSigningConfigured",
                "stripeConfigured", "airwallexConfigured", "hyperswitchConfigured", "x402Configured"}
COUNTS = {"providerChannels", "providerCredentialsActive", "providerCredentialsCooling", "providerCredentialsDisabled",
          "merchantsEnabled", "merchantsDisabled", "jobsScheduled", "jobsDue", "jobsRunning", "jobClaimsExpired", "jobsDead",
          "operationsOpen", "operationLeasesExpired", "holdsOpen", "holdsDead", "paidOrdersUnfulfilled",
          "paymentOrdersNeedAttention", "merchantEventsPending", "merchantEventsAccepted", "refundsRetryDue",
          "refundsNeedReview", "legacyRefundsRetryDue", "legacyRefundsNeedReview"}
ENV_GROUPS = {
    "database": ["DATABASE_URL"], "operatorToken": ["OXSUM_ADMIN_TOKEN"],
    "smtp": ["OXSUM_SMTP_URL", "OXSUM_MAIL_FROM", "OXSUM_PUBLIC_URL"],
    "publicUrl": ["OXSUM_PUBLIC_URL"], "credentialSeal": ["OXSUM_SECRET_KEY"], "headSigning": ["OXSUM_HEAD_SIGNING_KEY"],
    "stripe": ["OXSUM_STRIPE_SECRET_KEY", "OXSUM_STRIPE_WEBHOOK_SECRET", "OXSUM_PUBLIC_URL"],
    "airwallex": ["OXSUM_AIRWALLEX_CLIENT_ID", "OXSUM_AIRWALLEX_API_KEY", "OXSUM_AIRWALLEX_WEBHOOK_SECRET", "OXSUM_PUBLIC_URL"],
    "hyperswitch": ["OXSUM_HYPERSWITCH_API_KEY", "OXSUM_HYPERSWITCH_WEBHOOK_SECRET", "OXSUM_HYPERSWITCH_PUBLISHABLE_KEY", "OXSUM_PUBLIC_URL"],
    "x402Facilitator": ["OXSUM_X402_FACILITATOR_URL", "OXSUM_X402_NETWORK", "OXSUM_X402_ASSET", "OXSUM_X402_PAY_TO"],
    "x402StandIn": ["OXSUM_X402_VERIFYING_KEY"],
}
SCHEMA = re.compile(r"[0-9]{4}_[a-z0-9_]{1,115}")
VERSION = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?")
SHA = re.compile(r"(?:[0-9a-f]{40}|unknown)")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def require(condition, message):
    if not condition:
        raise ValueError(message)


def target_url(value):
    parsed = urllib.parse.urlsplit(value)
    require(parsed.scheme in {"https", "http"} and parsed.hostname and parsed.username is None and parsed.password is None
            and not parsed.query and not parsed.fragment and parsed.path in {"", "/"}, "target must be an explicit HTTP(S) origin")
    require(parsed.port is None or 0 < parsed.port <= 65535, "invalid target port")
    if parsed.scheme == "http":
        try:
            loopback = ipaddress.ip_address(parsed.hostname).is_loopback
        except ValueError:
            loopback = False
        require(loopback, "unencrypted HTTP is allowed only for numeric loopback targets")
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))


def version(value):
    require(isinstance(value, dict) and set(value) == {"version", "gitSha", "schemaVersion"}, "invalid version fields")
    require(isinstance(value["version"], str) and VERSION.fullmatch(value["version"]), "invalid package version")
    require(isinstance(value["gitSha"], str) and SHA.fullmatch(value["gitSha"]), "invalid source revision")
    require(isinstance(value["schemaVersion"], str) and SCHEMA.fullmatch(value["schemaVersion"]), "invalid schema version")
    return {key: value[key] for key in ["version", "gitSha", "schemaVersion"]}


def diagnostics(value):
    require(isinstance(value, dict) and set(value) == {"version", "gitSha", "schemaVersion", "databaseState",
            "appliedSchemaVersion", "configuration", "counts"}, "invalid diagnostic fields")
    identity = version({key: value[key] for key in ["version", "gitSha", "schemaVersion"]})
    require(value["databaseState"] in {"ready", "schemaMismatch", "unmigrated", "unavailable"}, "invalid database state")
    applied = value["appliedSchemaVersion"]
    require(applied is None or isinstance(applied, str) and SCHEMA.fullmatch(applied), "invalid applied schema")
    config = value["configuration"]
    require(isinstance(config, dict) and set(config) == CONFIG_FLAGS and all(type(v) is bool for v in config.values()),
            "invalid configuration flags")
    counts = value["counts"]
    if value["databaseState"] == "ready":
        require(applied == value["schemaVersion"] and isinstance(counts, dict) and set(counts) == COUNTS
                and all(type(v) is int and 0 <= v <= 2**63 - 1 for v in counts.values()), "invalid ready counts/schema")
    else:
        require(counts is None, "unavailable schema must not report zero counts")
        if value["databaseState"] in {"unmigrated", "unavailable"}:
            require(applied is None, "unavailable schema must not report an applied version")
    return {**identity, "databaseState": value["databaseState"], "appliedSchemaVersion": applied,
            "configuration": {key: config[key] for key in sorted(CONFIG_FLAGS)},
            "counts": None if counts is None else {key: counts[key] for key in sorted(COUNTS)}}


def read_private(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "r") as source:
        metadata = os.fstat(source.fileno())
        require(stat.S_ISREG(metadata.st_mode), "input must be an explicit regular file")
        require(metadata.st_mode & 0o077 == 0, "sensitive input file must be private (mode 0600)")
        require(metadata.st_size <= 1024 * 1024, "input file is too large")
        value = source.read(1024 * 1024 + 1)
        require(len(value) <= 1024 * 1024, "input file is too large")
        return value


def environment_presence(path):
    # Parse fixed names as text only. Never evaluate interpolation, shell
    # substitutions, export statements or unknown keys, and never report values.
    names = {name for group in ENV_GROUPS.values() for name in group} | {"POSTGRES_PASSWORD"}
    values = {}
    for line in read_private(path).splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("export "):
            stripped = stripped[7:]
        key, separator, raw = stripped.partition("=")
        if separator and key in names:
            require(key not in values, "duplicate documented variable in explicit environment file")
            values[key] = bool(raw.strip().strip("\"'"))
    if not values.get("DATABASE_URL") and values.get("POSTGRES_PASSWORD"):
        values["DATABASE_URL"] = True
    result = {}
    for group, fields in ENV_GROUPS.items():
        active = sum(bool(values.get(name)) for name in fields)
        # A shared PUBLIC_URL does not imply that every payment/mail group is
        # partially configured when its own primary fields are all absent.
        own = [name for name in fields if name != "OXSUM_PUBLIC_URL"]
        if own and not any(values.get(name) for name in own):
            active = 0
        result[group] = "present" if active == len(fields) else "partial" if active else "absent"
    return result


def fetch(origin, path, token=None):
    headers = {"Accept": "application/json"}
    if token is not None:
        headers["Authorization"] = "Bearer " + token
    opener = urllib.request.build_opener(NoRedirect(), urllib.request.ProxyHandler({}))
    try:
        with opener.open(urllib.request.Request(origin + path, headers=headers), timeout=8) as response:
            body = response.read(32769)
            require(len(body) <= 32768, "oversized response")
            return {"status": "ok"}, body
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
        return {"status": "refused", "httpStatus": status}, None
    except (urllib.error.URLError, TimeoutError, ConnectionError, OSError):
        return {"status": "unavailable"}, None
    except ValueError:
        return {"status": "invalidResponse"}, None


def probe(origin, path, validator, token=None):
    outcome, raw = fetch(origin, path, token)
    if raw is None:
        return outcome
    try:
        result = validator(raw)
        return {"status": "ok", "data": result}
    except (ValueError, KeyError, TypeError, UnicodeError):
        return {"status": "invalidResponse"}


def wrapped(raw, validator):
    value = json.loads(raw)
    require(isinstance(value, dict) and set(value) == {"data"}, "invalid response envelope")
    return validator(value["data"])


def liveness(raw):
    require(raw.strip() == b"ok", "invalid liveness")
    return {"alive": True}


def readiness(raw):
    match = re.fullmatch(rb"ready \(([0-9]{4}_[a-z0-9_]{1,115})\)", raw.strip())
    require(match, "invalid readiness")
    return {"schemaVersion": match[1].decode()}


def collect(args):
    origin = target_url(args.url)
    require(args.output.parent.is_dir() and not args.output.exists() and not args.output.is_symlink(),
            "output must be a new file in an existing directory")
    token = None
    if args.token_file:
        token = read_private(args.token_file).strip()
        require(16 <= len(token) <= 4096 and all(33 <= ord(char) <= 126 for char in token), "invalid operator token file")
    local = environment_presence(args.env_file) if args.env_file else None
    report = {"formatVersion": 1, "collectedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
              "targetTransport": "https" if origin.startswith("https:") else "loopback-http",
              "localConfigurationPresence": local,
              "liveness": probe(origin, "/healthz", liveness),
              "readiness": probe(origin, "/readyz", readiness),
              "version": probe(origin, "/api/v1/version", lambda raw: wrapped(raw, version)),
              "operator": probe(origin, "/api/v1/admin/diagnostics", lambda raw: wrapped(raw, diagnostics), token)
                  if token else {"status": "notRequested"}}
    descriptor = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as output:
        output.write(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--token-file", type=Path, help="private file containing only the operator token")
    parser.add_argument("--env-file", type=Path, help="explicit private env file; only fixed presence states are retained")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    collect(args)
    print("Sanitized diagnostic report written: " + str(args.output))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, UnicodeError):
        # Neither local paths/values nor network exception bodies enter output.
        raise SystemExit("Diagnostic report refused: check explicit target, private inputs and new output path")
