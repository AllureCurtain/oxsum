#!/usr/bin/env python3
"""Send a synthetic Airwallex webhook to a running oxsum server, signed under
`OXSUM_AIRWALLEX_WEBHOOK_SECRET` with the documented `x-timestamp + body`
HMAC-SHA256. Used to exercise the deposit's `pending → confirmed → credited`
walk against a live oxsum instance when a real Airwallex webhook delivery
would need the hosted-page checkout to be mounted (which the project has
deliberately not built — see docs/decisions.md).

    WEBHOOK_SECRET=whsec_…   # OXSUM_AIRWALLEX_WEBHOOK_SECRET
    INTENT_ID=int_…          # the intent id on the deposit's meta
    MERCHANT_ORDER_ID=…      # the deposit id (meta.merchant_order_id)
    AMOUNT_MAJOR=5.00        # the major-units amount the intent reports
    BASE=http://127.0.0.1:3000  # or the public URL the webhook should reach
    EVENT=payment_intent.succeeded   # or cancelled/expired to test refusal
    python3 scripts/airwallex_webhook.py
"""

import hashlib
import hmac
import json
import os
import sys
import time
import urllib.request
import urllib.error


def env(name: str) -> str:
    return os.environ[name]


def main() -> None:
    secret = env("WEBHOOK_SECRET").encode()
    intent_id = env("INTENT_ID")
    merchant_order_id = env("MERCHANT_ORDER_ID")
    amount_major = float(env("AMOUNT_MAJOR"))
    base = env("BASE").rstrip("/")
    event_name = os.environ.get("EVENT", "payment_intent.succeeded")

    body = json.dumps(
        {
            "name": event_name,
            "data": {
                "object": {
                    "id": intent_id,
                    "amount": amount_major,
                    "currency": "USD",
                    "status": (
                        "SUCCEEDED"
                        if event_name == "payment_intent.succeeded"
                        else "CANCELLED"
                    ),
                    "merchant_order_id": merchant_order_id,
                }
            },
        },
        separators=(",", ":"),
    ).encode()

    timestamp = str(int(time.time() * 1000))
    signature = hmac.new(secret, timestamp.encode() + body, hashlib.sha256).hexdigest()

    req = urllib.request.Request(
        f"{base}/api/v1/webhooks/airwallex",
        data=body,
        headers={
            "Content-Type": "application/json",
            "x-timestamp": timestamp,
            "x-signature": signature,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            print(f"{r.status} {r.read().decode()}")
    except urllib.error.HTTPError as e:
        sys.exit(f"HTTP {e.code}: {e.read().decode()}")


if __name__ == "__main__":
    main()
