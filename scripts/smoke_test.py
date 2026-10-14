#!/usr/bin/env python3
"""Lightweight smoke test for a deployed oxsum instance.

No Rust build, no database setup — just HTTP calls against the deployment.
Run with:
    OXSUM_BASE_URL=https://ox.785777.xyz \\
    OXSUM_ADMIN_TOKEN=<admin-token> \\
    OXSUM_ORG_KEY=<org-api-key> \\
    OXSUM_TEST_MODEL=step-3.7-flash \\
    python3 scripts/smoke_test.py

Exits 0 if all checks pass, 1 otherwise. Safe to run repeatedly:
it only spends a few cents of the test org's balance per run.
"""
import json
import os
import sys
import urllib.request
import urllib.error

BASE = os.environ.get("OXSUM_BASE_URL", "").rstrip("/")
ADMIN_TOKEN = os.environ.get("OXSUM_ADMIN_TOKEN", "")
ORG_KEY = os.environ.get("OXSUM_ORG_KEY", "")
MODEL = os.environ.get("OXSUM_TEST_MODEL", "step-3.7-flash")

results = []


def check(name, fn):
    try:
        detail = fn()
        results.append((name, True, detail))
        print(f"  PASS {name}" + (f" — {detail}" if detail else ""))
    except Exception as e:
        results.append((name, False, str(e)[:120]))
        print(f"  FAIL {name} — {e}")


def get(path, token):
    req = urllib.request.Request(
        BASE + path, headers={"Authorization": f"Bearer {token}"}
    )
    return json.load(urllib.request.urlopen(req, timeout=30))


def post(path, token, data):
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(data).encode(),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
    )
    return json.load(urllib.request.urlopen(req, timeout=90))


def main():
    if not (BASE and ADMIN_TOKEN and ORG_KEY):
        print("Set OXSUM_BASE_URL, OXSUM_ADMIN_TOKEN, OXSUM_ORG_KEY")
        sys.exit(2)

    print(f"Smoke test against {BASE}")

    def health():
        req = urllib.request.Request(BASE + "/healthz")
        body = urllib.request.urlopen(req, timeout=10).read().decode()
        assert body.strip() == "ok", f"unexpected body: {body!r}"
        return "ok"

    check("healthz", health)

    state = {}

    def non_streaming():
        body = post(
            "/v1/chat/completions",
            ORG_KEY,
            {
                "model": MODEL,
                "messages": [{"role": "user", "content": "Say hi."}],
                "max_tokens": 20,
            },
        )
        usage = body.get("usage") or {}
        assert usage.get("total_tokens", 0) > 0, "no usage returned"
        state["usage"] = usage
        return f"{usage['prompt_tokens']}in/{usage['completion_tokens']}out"

    check("non-streaming completion", non_streaming)

    def streaming():
        data = json.dumps(
            {
                "model": MODEL,
                "messages": [{"role": "user", "content": "Say hi."}],
                "max_tokens": 20,
                "stream": True,
                "stream_options": {"include_usage": True},
            }
        ).encode()
        req = urllib.request.Request(
            BASE + "/v1/chat/completions",
            data=data,
            headers={
                "Authorization": f"Bearer {ORG_KEY}",
                "Content-Type": "application/json",
            },
        )
        chunks, usage = 0, None
        for line in urllib.request.urlopen(req, timeout=90):
            line = line.decode().strip()
            if line.startswith("data: ") and line != "data: [DONE]":
                chunks += 1
                try:
                    if json.loads(line[6:]).get("usage"):
                        usage = json.loads(line[6:])["usage"]
                except ValueError:
                    pass
        assert chunks > 0, "no SSE chunks"
        assert usage, "no final usage chunk"
        return f"{chunks} chunks"

    check("streaming completion", streaming)

    def balance_settled():
        orgs = get("/api/v1/admin/organizations", ADMIN_TOKEN)["data"][
            "organizations"
        ]
        # find org by matching the key's org — use first org with activity
        for o in orgs:
            if o["reservedMinor"] != 0:
                raise AssertionError(
                    f"dangling freeze: reserved={o['reservedMinor']}"
                )
        return "no dangling freezes"

    check("no dangling freezes", balance_settled)

    def billing_records():
        rows = get("/api/v1/billing-records?limit=3", ORG_KEY)["data"]["rows"]
        assert rows, "no billing records"
        r = rows[0]
        for k in ("model", "inputTokens", "outputTokens", "chargedMinor"):
            assert k in r, f"missing {k}"
        return f"{len(rows)} rows"

    check("billing records", billing_records)

    def usage_stats():
        days = get("/api/v1/usage?days=7", ORG_KEY)["data"]["days"]
        assert days, "no usage days"
        return f"{len(days)} days"

    check("usage stats", usage_stats)

    def merkle_head():
        head = get("/api/v1/log/head", ORG_KEY)["data"]
        assert head.get("note", "").startswith("oxsum/ledgers/"), "bad head"
        return "signed head present"

    check("merkle tree head", merkle_head)

    failed = [n for n, ok, _ in results if not ok]
    print(f"\n{len(results)-len(failed)}/{len(results)} passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
