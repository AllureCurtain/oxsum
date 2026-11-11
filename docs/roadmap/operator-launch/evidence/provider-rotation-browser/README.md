# Provider rotation and pinned streaming request acceptance

Issue #1023, N04/J03. Actual owner and staff browser exercise on isolated current
main `2ee706019a02f24b4b8ed4b949b8c5254b27a178`, November 11, 2026 (Asia/Shanghai).
This records development evidence and an observed defect; it is not final
candidate, real-provider, payment-provider or complete N04 acceptance.

## Browser observations

The owner created `rotation-local` and `rotation-chat` through the Business UI,
with the exact authorized loopback IP, reloaded, and observed only masked key
tails. An explicit connection check sent exactly one HEAD and showed HTTP 200
with the reachability-only warning. Configuration saves generated no inference.
The owner set a billing credential, then attempted to remove the sole active
credential. Removal was refused and the draft/reason remained available.

Five customer-key requests were sent by a separate bounded HTTP consumer, each
requesting SSE and at most 32 output tokens. They were not Chat-page submissions.
The deterministic upstream reported 10 input and 2 output tokens per successful
request. Rates were 1/2 credits per million; each call charged exactly 14 minor
units. Explicit customer sale terms were provisioned through the maintained API.

| Request | Observed upstream | Credential tail | Result |
| --- | --- | --- | --- |
| Before rotation | `/v1/chat/completions` | OLD1 | HTTP 200, 14 minor |
| Paused while owner edits | `/v1/chat/completions` | OLD1 | Finished after removal/repoint, 14 minor |
| After old key disabled | `/v1/chat/completions` | NEW2 | HTTP 200, 14 minor |
| After connection repointed | `/v2/chat/completions` | NEW3 | HTTP 200, 14 minor |
| All keys explicitly disabled | No upstream request | None | HTTP 502, zero-charge error settlement |
| New key enabled again | `/v2/chat/completions` | NEW3 | HTTP 200, 14 minor |

While the original stream waited, the owner added NEW2, disabled and removed
OLD1, then changed the endpoint to `/v2` with fresh NEW3. Reload showed only NEW3
and no billing credential. The original stream completed on its original
connection. The owner separately set/cleared the billing credential and explicitly
disabled/enabled the sole key. These actions are retained in twelve owner audit
rows. `provider-audit.json` is a read-only database export of `admin_audit`;
`/business/audit` is the separate financial-action log and is not this source.

The non-owner Business admin had model/routing controls but no secret/connection
write controls. Its direct browser credential-add request returned 403. Owner
reads before and after the denial, plus the admin's masked read, are identical.
No raw credential is returned. Desktop and 390px screenshots were inspected;
mobile document width is 390, with local credential/model table scrolling
(325px visible, 400/495px content). Long identifiers wrap within those tables.
This exercise does not claim additional alias, fallback or cache interaction.

## Accounting, verification and defect

The synthetic wallet started with 10,000,000 minor units. Five charges total 70;
final available balance is 9,999,930, reserved balance is zero and no hold remains.
The disabled-key refusal retains a sixth settlement with zero charge and zero
upstream attempts. Raw API/stream bytes and request IDs are retained beside this
file. The original stream's timing spans the removal and connection audit events.

A fresh verifier WASM and npm tarball were built from the recorded source and
installed into an isolated consumer. All six original proof bodies passed
inclusion/arithmetic, separate price/sale bindings, signed head and retained
entry/hash/origin/version/key checks. Each rejected six mutations: wrong hash,
changed charge, wrong origin, wrong sale version, missing sale and wrong key.
The consumer blocked fetch; network attempts were zero. This is internal
consistency against separately retained local API responses, not independent
operator-key trust or independent measurement of provider token usage. No
supplier cost was configured; absent supplier cost is not certified zero cost.

**Observed defect #1024:** the disabled-key client receives HTTP 502, while its
request record says `httpStatus: 200` with `status: errored`. Original evidence
preserves that mismatch. This report does not declare the error path fully
accepted; its correction and regression belong to the separate repair issue.

Run `python3 check.py` for retained conservation, temporal, authority and hash
checks. To rerun cryptographic checks, install `verify-oxsum-bill` built from the
manifest source into a temporary project, copy `scope.mjs` and
`verify-offline.mjs` there, and run `node verify-offline.mjs <evidence-directory>`.
The manifest retains the actual tarball/WASM/source hashes; a rebuilt tarball
must be identified separately if its hash differs. Do not use an older published
package as equivalent source evidence.

TaskSpace 8 was finished once with no retained tabs. Both task listeners
18821/18822 were stopped and checked absent. Secrets remain outside the checkout;
exported textual files were scanned against actual fixture credentials before
commit. No external email/model/payment call occurred. The shared provider
allowance remains 2/5 attempts, 16,896 CNY millionths reserved and 52 estimated
usage. Final candidate rerun, independent approval and protected integration
remain required.
