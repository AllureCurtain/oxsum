# Product specification

Status: finalized (2026-10-01). The four open questions are all decided; the conclusions are folded into the body and the former "open questions" section is gone. Any behavior change starts with this document, then the code.

This document records product behavior: who can do what, and how every situation is billed. Technical implementation lives in architecture.md, technology rationale in decisions.md, progress in TODO.md.

## One-liner

An OpenAI-compatible AI gateway. Users point base_url at oxsum and keep using any OpenAI client; every request freezes credit first, then settles against real usage. Every bill can be verified in the browser.

## Demo path

The piece must let a first-time visitor walk this path in minutes:

1. `docker compose up`, create the platform admin from the command line.
2. The admin configures one upstream channel (say DeepSeek) in the dashboard and tops up their own organization.
3. Create an API key, change base_url in any OpenAI SDK or ChatBox, and start chatting.
4. The dashboard shows freezes, settlements and balance changes in real time.
5. Open the bill page, click any entry — browser-local verification passes; change one number in the bundle and it fails.

## Roles

| Role | Who | Can do |
| --- | --- | --- |
| Platform admin | Whoever deploys oxsum | Manage channels and prices, top up or adjust any organization, view all anomalous requests, run monthly closings |
| Organization owner | Organization creator | Everything inside the organization, including transferring ownership |
| Organization admin | Appointed by the owner | Invite and remove members, manage all keys, view the organization's full bill history |
| Organization member | Invited people | Create and revoke their own keys, view organization balance and their own request records |
| API caller | Programs holding a key | Call the gateway, spending the key's organization balance |

The platform admin is not an organization role but a deployment-level identity; users cannot apply for it.

## Accounts and organizations

### Registration and login

- Login is email plus password; passwords are stored with argon2. v1 sends no email, so there is no email verification and no password recovery; the platform admin resets forgotten passwords.
- Platform admins are created only from the server command line: `oxsum admin create --email ...`. Not "the first registrant becomes admin" — on a public instance, whoever registers first would own it.
- Registration mode is deployment-configured (`OXSUM_SIGNUP`):
  - `invite` (default): registration only through invitation links
  - `open`: anyone can register
  - Until invitation links exist, `invite` refuses self-registration outright (`403 FORBIDDEN`) and the operator creates accounts; the mode is not a half-built flow, it is the switch the flow will hang off.
- Registration takes a password of at least 12 characters. Length is the only rule: with no email recovery, composition classes and expiry would cost users more than they buy.
- Passwords are hashed before the database transaction opens, so a slow argon2 hash never holds a connection.
- GitHub login is post-v1. It suits a GitHub-hosted piece, but deployers would have to configure an OAuth app; v1 gets email login solid first.

### Organizations

- Signup creates the user, the personal organization, the owner membership and the first API key in one transaction, and returns that key's secret once.
- The organization's ledger is not created at signup: it is created on first use, so an account that never spends costs nothing and a failed ledger migration cannot leave a half-registered user (docs/decisions.md).
- The tenant id of an organization is its own UUID without dashes, so a ledger schema is `ledger_<32 hex characters>` and nothing has to be chosen, probed or made unique.
- Personal organizations can invite members too. When the first person joins, the organization flips from `personal` to `team` automatically; the ledger does not migrate.
- Users can create team organizations and join several. The current organization switches from the top-right corner.
- Invitations: one link, valid 7 days, usable once. v1 sends no email; the inviter passes the link along themselves.
- An owner cannot leave directly; ownership must transfer first.
- v1 does not support deleting organizations. The ledger is append-only; deleting an organization would orphan its billing history.

## Where credit comes from

v1 has no payment integration; credit has exactly three sources, each recording its reason in the ledger:

| Source | Who | Notes |
| --- | --- | --- |
| Top-up | Platform admin | Reason required, e.g. "offline transfer, 2026-10, #3" |
| Signup bonus | Automatic | Amount configured by the deployer, default 0 |
| Adjustment | Platform admin | Can add or subtract; reason required. A deduction cannot push available balance negative |

- The unit is credit, 1 credit = 1_000_000 minor. What a credit maps to in real money is the deployer's choice; oxsum does not care.
- Adjustments never modify history; they book a new entry. The original entry and its proof stay valid.

## API keys

- Keys belong to organizations and hold no money themselves. The creator is recorded for permissions and audit; a key minted through the API records no creator, because no person is acting there yet. Until roles are enforced, any active key of an organization may read it and mint or revoke its keys; role checks arrive with web login.
- The plaintext shows once at creation. The database stores only the SHA-256 hash and a prefix; the dashboard uses the prefix to help users recognize keys.
- Format: `oxs-` followed by 32 random bytes, so secret-scanning tools can recognize them.
- Optional name and expiry. Revocation is permanent.
- Per-key spend limits and model whitelists are post-v1 (TODO.md's "per-key sub-limits").

## Gateway

### Supported endpoints

- `POST /v1/chat/completions`, streaming and non-streaming
- `GET /v1/models`: lists only models with configured prices

v1 has no embeddings, images, audio, Responses API or Anthropic Messages format. Request content is text-only; messages containing images get a 400. This is decided: the input token upper bound must be computable for the freeze promise to hold (see "How much to freeze" below). Image support is re-evaluated post-v1.

### Channels and prices

- Channels are configured by the platform admin: name, upstream base_url, upstream API key, served models.
- In v1 one model maps to exactly one channel; no load balancing, no failover. The gateway never retries upstream automatically, because a retry might charge upstream twice.
- Upstream API keys are stored encrypted; the dashboard shows only the last 4 characters.
- Each model's price: input price, output price (credit per million tokens), plus a max-output-token count.
- Changing a price never overwrites the old one; it creates a new version. The version in force when a request starts is the version used to settle it. So a price change only affects later requests; in-flight requests and old bills are untouched.

### How much to freeze

When a request arrives, compute the most it could possibly cost, freeze that much, then contact upstream.

- Input upper bound = UTF-8 byte count of all text content in the request, plus a fixed per-message format overhead. Mainstream tokenizers are byte-level BPE, so one token spans at least one byte and the byte count never undercounts tokens. The cost is overshooting — roughly 4x for English, 2x for Chinese — and the excess is refunded in full at settlement.
- Output upper bound = the request's `max_tokens` or `max_completion_tokens`. If absent, the model's configured maximum output; if larger than that maximum, the maximum applies.
- Freeze = input upper bound × input price + output upper bound × output price, rounded up.
- When relaying, the gateway always writes the output upper bound into the request. Upstream output can then never exceed the estimate the freeze was based on.
- If available balance cannot cover the freeze, return 402 without contacting upstream. The error states how much the freeze needs, the current available balance, and suggests lowering `max_tokens`.

No tokenizer-based freeze estimation: tokenizers differ per model, and an underestimate means actual spend can exceed the freeze. new-api's #7554 is the reverse failure — token estimation inflated tens of times, pre-holding 4M at once. Byte estimation overshoots, but the bound is guaranteed and the excess is always refunded.

### How settlement works

When relaying a streaming request, the gateway always writes `stream_options.include_usage = true` so upstream returns usage in the final chunk.

| Situation | Charge | Settlement type |
| --- | --- | --- |
| Completed normally, usage received | usage × the price version from request start | `usage` |
| Upstream returned an error before emitting anything | 0, full release of the hold | `upstream_error` |
| Could not connect or timed out, no response received | 0, full release | `upstream_unreachable` |
| Partial output received, upstream dropped mid-stream, no usage | Local estimation, see below | `estimated` |
| Client disconnected mid-stream | Cancel the upstream call immediately; estimate locally from what was forwarded | `client_cancelled` |
| Usage-priced cost exceeds the freeze | Charge the freeze only; record the excess as an anomaly for admin review | `capped` |
| Hold timed out with no settlement (e.g. the gateway crashed) | 0, full release, recorded as an anomaly | `swept` |

- Local estimation: count input and forwarded output with tiktoken's `o200k_base`; the result never exceeds the freeze. The bill is explicitly marked "estimated".
- The user never pays more than the freeze — that is the gateway's promise. Underestimates, missing usage and upstream overcharges are the platform's cost.
- The hold timeout defaults to 30 minutes and must exceed the longest possible single request. A background job sweeps timed-out holds. The sweeper and normal settlement share the same idempotency key, so both cannot succeed; whichever lands first wins.
- Every response carries an `x-oxsum-request-id` header; users use it to find the matching entry on the bill page.

Two settled trade-offs:

- On client disconnect, cancel the upstream call immediately. Rationale: users must not pay for content they never received; letting upstream keep running means the platform pays upstream for nothing. The cost is settling on local estimation, marked on the bill.
- On mid-stream interruption with no usage, settle on local estimation — do not copy sandbase's free-transaction approach. Free is friendliest to users but rewards deliberately cutting the connection; the platform's cost becomes an exploitable loophole. An explicitly-marked "estimated" bill is the honest middle ground. The never-pay-more-than-the-freeze promise still holds.

### How one request is booked

- One request maps to two entries: the freeze and the settlement. Idempotency keys: `req-<id>:hold`, and the settlement's key derived from it (`oxsum_core::settlement_key_for`).
- The settlement entry's description carries a compact JSON: request id, model, input and output token counts, both prices, settlement type. The description is hashed into the entry, so what the user verifies is not just "how much was charged" but "by how many tokens at what price". The description caps at 512 characters — enough.
- The full request state (in flight, settled, anomalous) lives in oxsum's own `requests` table; only what needs proving goes into the ledger.

## Bills and verification

### Bill page

- Lists the organization's every transaction newest-first: top-ups, bonuses, adjustments, requests. A request row shows model, token counts, freeze, actual charge and settlement type.
- Members see only requests from their own keys plus organization-level top-ups. Owners and admins see everything.
- CSV and JSON export. Exports carry each entry's contentHash so users can archive them.

### Browser verification

- Every entry verifies locally in the browser. The verification logic is the WASM build of `verify_bundle` — the same code the server runs.
- Each visit to the bill page stores the tree heads seen into browser-local storage. On the next visit, consistency proofs are fetched automatically to confirm the log only ever appended — history was not rewritten. The user does nothing.
- Witness-signed tree heads follow later, so multiple users see the same log.

Verification proves: this record was not altered after being written, and history was not rewritten. It does not prove: upstream really returned that many tokens. The page states this sentence verbatim, without inflating it.

## Pages

### Organization view (logged-in users)

| Page | Content | Who sees it |
| --- | --- | --- |
| Overview | Available balance, frozen amount, this month's spend; in-flight requests show live token progress | All members |
| Requests | Each request's status, usage and cost, filterable by key and model | All members; members see only their own |
| Bills | See the section above | Same as above |
| API keys | Create, revoke | Members manage their own; admins manage all |
| Members | Invite, remove, change roles, transfer ownership | Owners and admins |

### Platform admin

| Page | Content |
| --- | --- |
| Organizations | Every organization's balance; top up, adjust |
| Channels & prices | Configure upstreams and model prices; view price version history |
| In-flight requests | Every unsettled hold, globally |
| Anomalies | Requests with settlement type `capped`, `estimated`, `client_cancelled` or `swept`, summarizable per channel to see where the platform loses money upstream |
| Closing | Monthly closing; after closing, that month accepts no new entries and a closing record is produced |

## Phase C: chat page

- Chat directly under the login session, no API key needed. Spends the current organization's balance; requests are marked "web chat" in the records.
- Conversations live only in browser-local storage; the server keeps nothing. Simple to build, and no duty to store users' conversations.
- Each AI reply shows that turn's charge underneath; clicking it opens the verification for that entry.

## Not in v1

- Payments and self-service top-ups
- Email: verification, password recovery, invitation emails
- Multi-currency
- Multi-channel load balancing and failover
- Anything beyond text chat
- Request-level deduplication: retrying with the same Idempotency-Key charges once. Streaming responses cannot be replayed verbatim, so this waits; instead, every request is individually traceable on the bill.
- Rate limiting: v1's only gate is balance. The sum of concurrent freezes cannot exceed the balance, which itself bounds concurrency.
