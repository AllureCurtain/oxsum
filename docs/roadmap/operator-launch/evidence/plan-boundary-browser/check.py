"""Verify retained domain evidence, without opening a database or network."""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parent

def read(name):
    return json.loads((root / (name + ".json")).read_text())

manifest = read("manifest")
for name, digest in manifest["artifacts"].items():
    assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digest, name
initial = read("initial")
for stage in ("failed", "expired"):
    current = read(stage)
    for field in ("periods", "accounting", "effects", "books", "journals"):
        assert current["renewal"][field] == initial["renewal"][field], (stage, field)
paid, replayed, final = read("paid"), read("replayed"), read("changed-paid")
for field in ("periods", "accounting", "effects", "books", "journals"):
    assert paid["renewal"][field] == replayed["renewal"][field], field
for label, original in initial.items():
    current = final[label]
    assert all(row in current["accounting"] for row in original["accounting"])
    assert sum(row["net_credit"] for row in current["books"]) == 0
    assert {row["path"] for row in current["books"]} <= {
        "Assets:Cash", "Income:PlanFees", "Liabilities:UnearnedPlanFees"
    }
    for period, accounting in zip(current["periods"], current["accounting"]):
        assert period["period_start"] == accounting["service_start"]
        assert period["period_end"] == accounting["service_end"]
        assert accounting["accounting_policy_version"] == 2
for label, fee, quota, overflow in (("renewal", 1000001, 7000000, False), ("change", 2000003, 9000000, True)):
    current = final[label]
    assert len(current["periods"]) == len(current["accounting"]) == 2
    assert current["periods"][0]["period_end"] == current["periods"][1]["period_start"]
    assert current["accounting"][1]["fee_minor"] == fee
    assert current["subscription"][0]["quota_minor"] == quota
    assert current["subscription"][0]["allow_wallet_overflow"] == overflow
assert final["cancel"]["subscription"][0]["state"] == "cancelled"
assert final["expiry"]["plan"][0]["state"] == "past_due"
for label in ("cancel", "expiry"):
    assert len(final[label]["periods"]) == 1
for row in read("quota-probe"):
    assert row["drawnMinor"] == (0 if row["customer"] == "cancel" else 1)
print("All retained plan-boundary evidence assertions passed.")
