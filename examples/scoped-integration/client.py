#!/usr/bin/env python3
"""Scoped customer lookup and explicit event metering. Standard library only."""
import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RuntimeError("redirect refused: do not forward an integration secret")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:3000")
    sub = parser.add_subparsers(dest="command", required=True)
    read = sub.add_parser("customer", help="read one selected customer")
    read.add_argument("customer_id")
    meter = sub.add_parser("meter", help="bill an event (moves customer credits)")
    meter.add_argument("customer_id")
    meter.add_argument("--channel", required=True)
    meter.add_argument("--code", required=True)
    meter.add_argument("--key", required=True, help="persist and reuse for the same event")
    meter.add_argument("--units", type=int, required=True)
    meter.add_argument("--event-id", required=True)
    meter.add_argument("--confirm-metering", action="store_true", required=True)
    args = parser.parse_args()
    secret = os.environ.get("OXSUM_INTEGRATION_SECRET", "")
    if not secret.startswith("oxs-int-"):
        parser.error("OXSUM_INTEGRATION_SECRET must contain an owner-issued integration credential")
    parsed = urllib.parse.urlsplit(args.base_url)
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        parser.error("base URL must not contain credentials, query or fragment")
    if parsed.scheme != "https" and not (
        parsed.scheme == "http" and parsed.hostname in ("localhost", "127.0.0.1", "::1")
    ):
        parser.error("use HTTPS, or HTTP on loopback for an isolated test")
    customer = urllib.parse.quote(args.customer_id, safe="")
    if args.command == "customer":
        path = f"/api/v1/business/customers/{customer}"
        body = None
    else:
        if args.units < 1:
            parser.error("units must be positive")
        path = "/api/v1/metering/settlements"
        body = json.dumps({
            "idempotencyKey": args.key,
            "organizationId": args.customer_id,
            "channel": args.channel,
            "billableCode": args.code,
            "eventId": args.event_id,
            "usage": {"inputTokens": args.units, "eventType": "example.event"},
        }).encode()
    request = urllib.request.Request(
        args.base_url.rstrip("/") + path,
        data=body,
        headers={"Authorization": f"Bearer {secret}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.build_opener(NoRedirect).open(request, timeout=20) as response:
            print(json.dumps(json.load(response), indent=2))
    except urllib.error.HTTPError as error:
        payload = json.loads(error.read())
        print(json.dumps({"status": error.code, "error": payload.get("error")}), file=sys.stderr)
        return 1
    except (OSError, RuntimeError) as error:
        print(f"Request failed ({type(error).__name__}); retry only with the same key and body.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
