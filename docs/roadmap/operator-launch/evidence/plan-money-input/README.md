# Exact plan amount browser regression (#963)

Date: 2026-11-10. Found during #946 / N10 browser acceptance.

## Baseline and repair

The owner entered price `0.0000001` and quota `1.0000001` in the real
Business Plans form on integrated source `ac7bb191`. Publishing silently
created version `2d16dd34-b7e8-4e54-949f-5417f3a8eac8` with price **0** and
quota **1,000,000** minor. No customer was bound to this regression fixture.

The repaired form uses the existing exact decimal parser before mutation.
Price and quota allow explicit zero; blank quota remains unmetered. Extra
precision, exponent notation, negative values and unsafe integers refuse
publication and retain the draft. Invalid controls refer to a bilingual alert.

## Observed validation

- The same overprecision input displayed English and Chinese refusal, retained
  both original inputs and left the catalogue count at one.
- `1e3`, `-1` and `9007199254.740992` also left the catalogue unchanged.
- Correcting to price `0.000001` and quota `1.000001` published exactly **1**
  and **1,000,001** minor; the server response is retained in `plan.json`.
- Explicit price zero and blank quota published zero and JSON null respectively.
- Two focused monetary tests, five OpenAPI contract checks, frontend production
  build, pinned formatting and whitespace checks passed. No Rust behavior changed;
  a complete workspace rerun is unnecessary for this frontend-only repair.
- All three screenshots were visually inspected. No credential or real payment
  is present. Hashes cover screenshots and the sanitized plan response.

Backend: `ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`, binary SHA-256
`941e9f1776e64ed5559074c637bf73d67de7380a382dc12f56e1b149ca610c53`.
Frontend: rebuilt from `1201d2e9` plus this repair and served by the owned
backend's debug asset path; its byte inventory is `frontend-sha256.json`.
This is a labeled development repair check, not a final candidate verdict.
Independent review and protected integration remain.
