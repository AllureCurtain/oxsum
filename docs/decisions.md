# Technical decisions

## 2026-11-09 — Stable application ownership and exact operation replay (#846)

Application identity is owner-created and separate from replaceable credentials.
A dedicated immutable application/customer mapping preserves business profiles
and ledger ownership while allowing identical external IDs in different apps.
Application grants use an empty explicit allowlist and only their named scopes;
legacy grants never inherit namespace reach. A distinct lifecycle scope permits
archive only. Provision/archive persist normalized request and result in the same
transaction, with business-before-credential lock ordering. Metering fingerprints
use stable application/customer/key and retain original credential provenance,
so rotation cannot create a second charge. Owner-selected lifetime limits remain
consumed after archive. Existing portal customers require a separate verified
consent flow; email matching or broad customer wildcard grants are rejected.

## 2026-11-08 — Separate checkout selection from callback bootstrap (#835)

Environment bootstrap was the only order selector, leaving owner-created merchants
unusable for checkout. An audited, revision-checked per-rail selection now retains
bootstrap compatibility only until explicit adoption. Null or disabled selections
never fall back; legacy callback identity remains unchanged. New order pinning
rechecks route/version under the business lock, while existing keys recover their
original merchant and SDK version. Customer choice snapshots and frozen submitted
payloads prevent background refreshes after a lost response from creating another
order. Only deliberate user edits replace the payment intent.

New decisions go on top. Overturned decisions are never deleted; mark them "Superseded" and name the decision that replaces it.

## 2026-11-04 — The dashboard bundle reads from disk in debug builds; `debug-embed` stays off (issue #569)

- Status: Adopted. Corrects the `debug-embed` claim in "Dashboard: React + Vite, replacing the Topcoat pages" (2026-11-01), which stays as written and is corrected here.
- Background: that entry — and the module comment in `crates/server/src/spa.rs` and docs/development.md — said the `debug-embed` feature is what makes a debug build read `web/dist` from disk. It is the opposite. rust-embed 8 picks its implementation with `#[cfg(any(not(debug_assertions), feature = "debug-embed"))]`: a debug build embeds at compile time *unless* the feature is off, and enabling `debug-embed` forces the embedded branch even in debug builds. The workspace enabled it, so the documented workflow (edit the frontend, rebuild it, see the change without touching Rust) did not hold — an acceptance run served the stale placeholder until a `.rs` file was touched to force a rebuild. The false claim had gone unnoticed because the committed placeholder is what a debug binary usually carries anyway.
- Decision: `rust-embed` is declared without the `debug-embed` feature, restoring the documented split — debug builds read the directory from disk at runtime, release builds embed it. The three comments and docs that described the feature backwards are corrected, and this entry records why the feature is absent so it is not "helpfully" re-added.
- Why not keep the feature and flip the documentation: always-embedding would make every frontend change wait on a Rust rebuild and would keep serving whatever bundle the binary was compiled with, which is exactly the failure the acceptance run hit. The disk-reading behaviour is the more useful development default and is what the docs promise.
- Pinned by a debug-side check: build the server, replace a file under `web/dist`, and the next request serves the replacement with no cargo rebuild.

## 2026-11-01 An experiment is org-side routing config; the pick rewrites `model`, and scoring stays offline (issue #509)

- Status: Adopted. Shares the mutable-config doctrine "A fallback is model→model, its own table" (2026-11-01) settled: `oxsum.experiments` sits beside `model_fallbacks` as a table an owner edits, never a ledger row.
- Background: an organization comparing two models for the same workload — an A/B, a gradual rollout, an eval harness — needs the platform to *split* traffic between them and then read back what each served. The pieces existed independently: `route_order`'s weighted pick, `servings_named`'s model resolution, the v12 `requestedModel`/`servedModel` record. What nothing did was let the *caller* ask for the split per request and record which arm served. Two boundaries the issue had to respect: the pick's landing model is priced like any serve (a variant is just a model name), and oxsum records which arm ran — whether its answers were *better* is the customer's own judgment on the responses they hold, a call a billing wallet is not built to make.
- Decision:
  - `oxsum.experiments (org_id, name, variants, status)` (migration 0069) is mutable config, not ledger: `variants` is a JSONB `[{model, weight}]` set a `PUT /api/v1/org/experiments/{name}` writes atomically (`ON CONFLICT DO UPDATE`), `POST …/end` flips `status` to `ended`. `put_experiment` refuses a variant naming nothing served or aliased — the same name-must-resolve doctrine `set_fallbacks` keeps — and a weight below 1. Writes are owner/admin through `membership_actor`, the role gate `teams` already keeps; the list and results reads answer any member — and the organization's key, which acts with the organization's full authority.
  - The request opts in with `X-Oxsum-Experiment: <name>` on the gateway: an open experiment's `pick_variant` — the same roll convention `route_order` uses — lands a variant and *rewrites `request.model`* to it before admission. A variant is just a model name, so the rewritten request resolves through the same `servings_named`, prices by its own book and freezes the same bound any request of that model would — the served model still carries the whole bill's proof. An unknown or ended name is a no-op, not an error: an experiment is a routing hint, never an admission gate.
  - The caller's own reach still constrains the landing model, never widened: the rewritten `model` flows through `hold_for_key`'s locked allowlist check, the tier's and a presented mandate's `models` the way a directly-named model does — a variant the key may not reach refuses like any disallowed model, which is the honest answer because the caller asked for *the experiment's* pick.
  - The settlement description goes to v13: the optional `experiment` block `{id, variant}` records the experiment's own id and the picked variant's model — recorded, never a pricing input, so `recompute_v13` only validates its shape (`id` a uuid, `variant` a non-empty string) before delegating to `recompute_v12`. The usage row's `usage_details.experiment` carries the same pair — the escape hatch `usage_details.cache` already uses — so `GET /api/v1/org/experiments/{id}/results` groups the experiment's recorded usage by variant (turns, token and charge sums, errored count) without a schema column spent on what is already recorded.
  - Scoring stays offline by design: oxsum reports which variant served and what it cost; whether the responses were *better* is the customer's own eval on the bodies they hold. No promotion, no winner judgment, no per-variant quality score — a billing wallet proves which model answered, it does not grade the answer.
- Rejected:
  - A `experiments` map on the API key or channel: the binding is the *organization's* traffic-splitting decision a request opts into, not a credential's constraint nor a route's property — the org-level table names who configures it.
  - Scoring inside oxsum (a quality verdict per variant, a promoted winner): the wallet's honest boundary is the proof of which model served and what it charged — grading the answer's content is a second product the bill does not cover, and a platform-judged "winner" would be a verdict the proofs cannot establish.
  - Refusing a request naming an ended or unknown experiment: the header is a hint, not a contract — the requested `model` still answers legitimately, so silently resolving it is the honest fallback, the same doctrine "a denied fallback is skipped" keeps.
- Costs and knock-on effects:
  - `DESCRIPTION_VERSION` is now 13; v13 descriptions parse everywhere v12 did plus the `experiment` block, and a reader of any older record simply sees no `experiment`.
  - The pick is a coin flip per request, not a sticky assignment: a customer replaying a request may land a different arm — the recorded `variant` on each settlement is what makes the grouping honest anyway.
  - The results read is `usage_records`-backed, so it reflects what *settled*, which is the only spend that exists — an in-flight turn's variant is known to the relay but not yet grouped.
  - Pinned by `crates/server/tests/experiments.rs` (the header serving the picked variant with the v13 block recorded, an ended/unknown name leaving `model` untouched, the results read grouping the recorded spend, and the owner/admin write gate refusing a member and a key).

## 2026-11-01 Route picks fold observed upstream health in as an effective weight (issue #499)

- Status: Adopted
- Background: `route_order` picked by `weight` first, then cheapest-in-tier — a channel that is currently slow or erroring still led when its configured weight was highest, because the pick had no signal about what upstream was doing *now*. The credential pool already cooled a *dead* credential (`pools.rs` `cool_down`), but a *slow-but-alive* channel shed nothing.
- Decision:
  - A process-local per-channel `RouteStats` — an EMA of time-to-first-byte and error rate — lives on `Db` the way `pools::Affinity` does (`db.route_stats()`, `SharedRouteStats` = `Arc` across `Db`'s clones). In-memory is correct for a routing hint: the bill only names the channel that served, so which candidate a turn *almost* took is not part of its proof, and a process restart simply starts the window cold — the gateway falls back to weight+price until the first samples land, the healthy default.
  - `route_order_stats(servings, roll, stats)` is the stats-aware pick; `route_order` delegates with `None` so the no-signal case is byte-for-byte today's behavior. Effective weight replaces configured weight throughout — the summed-weight roll and the failover ordering both — computed as: a channel dropped on error rate contributes `0` (skipped outright), a slow-but-alive one counts `weight * healthy_ttft / observed_ttft` floored at `weight/8` so latency alone never fully removes it.
  - The constants are deliberate, not tuned: EMA α `0.2` (recent behavior weighs a fifth — one slow turn does not tank a channel, a turned-bad one sheds picks within a few calls), error-rate drop threshold `0.5` (half of recent attempts failing is past retry-worthiness), cool-down `60s` after which the channel is re-admitted at scaled weight — the decaying EMA reads healthier over time, so a recovered upstream regains picks without a probe — and a cold-start healthy baseline of `2000ms` TTFT until the fleet has samples to compare against (`healthy_ttft_ms` is the min of observed channels: "what does good look like" is the fastest observed upstream).
  - The relay feeds samples at the single point every attempt passes through: `call.send()`'s resolution records the per-attempt elapsed as the latency sample and success/5xx+429/unreachable as the outcome, on `state.db.route_stats()` — the same seam `metrics::upstream_response` already counts. The task relay's pick (`tasks.rs`) folds the same window.
- Rejected: *persisted stats* — a routing hint must not become a billing fact; persisting it would invite "what did it cost when the window said X" questions the row cannot answer, and a cold start is one feature not a bug. *Per-credential health in the pick* — the pool's own `cool_down` already skips dead credentials; stats live one level up at the channel the bill names. *Median or p95 latency* — an EMA is the honest running estimate; the pick needs "slower than healthy, roughly how much," not a distribution.
## 2026-11-02 Request groups restrict bearer channel access at admission (issue #508)

- Status: Adopted.
- `oxsum.groups` and `group_channels` (migration 0070) hold operator-managed channel subsets per organization. A nullable `api_keys.group_id` assigns at most one group; a composite foreign key enforces same-organization membership. Ungrouped keys retain all channels. An empty group denies all.
- The gateway reads assignment and channels in one SQL snapshot and filters candidates before routing, cache access, retries, and cross-model fallback. Task submission uses the same restriction. New requests see saved configuration; admitted requests keep their snapshot. Groups restrict channels alongside model allowlists, budgets, tiers and guardrails.
- Creating a key and assigning its group commit together. Constraint replacement and optional reassignment also commit together. PATCH omission preserves the group; explicit null removes it. Invalid groups produce no partial key or constraint writes.
- A grouped bearer cannot create or patch keys. The acting key is locked and re-read in the write transaction so concurrent assignment cannot race key creation. Owner/admin sessions and ungrouped organization keys may assign groups; member sessions may manage their own ordinary constraints but cannot change `groupId`. Groups restrict bearer credentials, not trusted member sessions: a member account is not an isolated channel principal and may still mint ungrouped keys.
- Deleting an assigned group returns 409, including assignments from revoked keys. Reassignment or explicit removal must precede deletion. Rejected: `ON DELETE SET NULL`, which would silently broaden access.
- `GET /api/v1/org/groups` is the organization-scoped read; operator CRUD stays under `/api/v1/admin`. Admin channel responses include stable UUIDs for membership configuration. Console management follows separately.
- Historical usage/report filtering by group is deferred. Joining historical usage to current key assignments would misrepresent past admission. Settlement evidence continues to name the channel that actually served.
- Integration coverage exercises atomic writes, null/omission, cross-organization misuse, bearer escalation refusal, deletion conflicts, normal routing/retries/fallback/cache, and compatibility with existing constraints.

## 2026-11-01 A fallback is model→model, its own table, and the caller's reach is re-judged per hop (issue #500)

- Status: Adopted. Extends "failover between channels under one hold" (issue #168) upward: the attempt loop now walks a chain of *models*, not only the routes one model prices.
- Background: a caller asks for a capability, and the deployment knows which models can serve it — "gpt-4o unavailable → try gpt-4o-mini". The channel-level failover of #168 already rotates routes and credentials under one hold; what it could not do is leave the requested name. The issue sketched a `fallbacks` column on `oxsum.prices`; there is no such table — the price book is `oxsum.channel_prices`, **append-only** (a trigger forbids UPDATE/DELETE so a version's figures never change after write, and a bill always proves the price it charged under). A chain that edits with configuration cannot ride a row that may never change, and a fallback is a *model*, not a price version.
- Decision:
  - `oxsum.model_fallbacks (model, position, fallback_model)` (migration 0067) is mutable config, not ledger: a `PUT` replaces one model's whole chain atomically (`DELETE` + re-insert in one transaction), and the admin surface `GET`/`PUT`/`DELETE /api/v1/admin/model-fallbacks/{model}` drives it under `require_admin`. `set_fallbacks` refuses a name that resolves to nothing served or aliased — the same name-must-resolve doctrine the guardrail bindings keep — and refuses the requested model inside its own chain, the loop that would serve nobody.
  - The attempt loop flattens the chain into `(model_index, route_index, credential)` triples: the requested model's routes and their credential pools first, then each fallback's own `servings_named` resolution (its own channels, its own alias `upstream_model`, its own `route_order`). The freeze is the dearest bound across the *whole* chain — a failover may land on a fallback's dearest route, so the hold must cover it — and `max_tokens` forwards at the tightest output ceiling any route could bill. The forwarded body's `model` is rebuilt per attempt to the serving model's own upstream resolution.
  - The caller's reach is re-judged per hop, never inherited: a fallback is a new model, so the key's `modelAllowlist`, a presented mandate's `models` subset and the tier's allowlist are each checked again — and a denied fallback is *skipped* (the loop moves to the next model), not a refusal of the turn, because the requested model was legitimately attempted and a constraint-denied fallback is simply not a route.
  - The chain is capped at 8 and de-duplicated at resolve time: `set_fallbacks` stops a self-loop at write, but a multi-hop `a→b→c→a` only closes when the chain is read, so a name already in the chain is dropped and a misconfigured loop cannot cycle.
  - The settlement description goes to v12: `servedModel` always (what answered, rewritten by `Turn::reroute` on a cross-model failover) and `requestedModel` (the caller's ask, fixed at admission) only when it differs — emitting the redundant name on every clean serve would push a rule-priced record past the ledger's 512-char description cap. `model` stays the served name for back-compat readers. `recompute_v12` prices against the served model's binding (the price that priced the turn, already inside `lines`); `requestedModel` is recorded, never a pricing input.
- Rejected:
  - A `fallbacks` column on `oxsum.channel_prices`: the table is append-only by trigger, and the chain is model→model channel-agnostic — a model may be priced on several channels, so a per-row column could not name a chain anyway.
  - Refusing the whole turn when a fallback is denied: the requested model already failed legitimately; a mandate-denied fallback is not a route, it is simply absent — skipping is the honest answer.
  - Inheriting the requested model's permission onto fallbacks: a key that may not call `gpt-4o-mini` directly must not reach it by naming `gpt-4o` and waiting for a failure — the re-check is the whole point of the constraint.
- Costs and knock-on effects:
  - `DESCRIPTION_VERSION` is now 12; v12 descriptions parse everywhere v11 did plus the model fields (`servedModel` always, `requestedModel` when it differs), and older records read `requestedModel`/`servedModel` as their `model`.
  - The freeze over the whole chain reserves more than a single-model bound when a fallback is pricier — the conservative bound a failover needs, at the cost of a larger reservation for chains that span differently-priced models.
  - The key's `modelAllowlist` is read fresh at admission for the fallback check (`Db::key_model_allowlist`); the requested name's authoritative gate stays inside `hold_for_key`'s locked read, so a PATCH landing mid-request is still caught there.
  - Pinned by `crates/server/tests/fallbacks.rs` (a failed model falling to its fallback, a mandate-denied fallback skipped not served, a chain walking req→a→b in order, an unserved name refused 400, and v12's served-model record on a clean bill).

## 2026-11-01 The guardrail pipeline judges a turn in two phases, and only the pre-hold one fails closed (issue #502)

- Status: Adopted. **Narrows** "The productization roadmap is settled" (2026-10-05), whose product-boundary line put "guardrails/content safety" out of scope: the boundary that stays out is the *content-moderation product* — a registry, an evaluation harness, a safety model of oxsum's own — while the pipeline that runs an org's own rules around a turn is billing-adjacent protection, the same layer `requiredPolicies` already belongs to. The mandate-side requirement lands on the mandate *row* rather than the signed blob — a reversal of where the issue's sketch first put it, for the reason below.
- Background: a commercial gateway needs org-written rules around a turn — a keyword list a prompt must not match, a PII scan over the answer, an external moderation endpoint a security team already runs. The issue sketched the mandate's own `requiredPolicies` inside the signed `MandateWire`; that would have made the delegate's blob carry server-side state the signature then covers, so a policy added or renamed after minting would either fail signature or silently unguard. The requirement is the server's, so it stores on `oxsum.mandates` beside `models` — the other narrowing field that lives outside the signature.
- Decision:
  - `oxsum.guardrail_policies` holds named rules of three kinds: `keyword` (a regex list, each pattern `flag` or `block`, most-restrictive wins), `pii` (the built-in scan over the response's text, always a flag — a `block` action on PII is a keyword pattern's job, since rewriting a serialized body risks corrupting it), and `moderation` (an HTTP callout whose bearer rides the row as `secret_sealed` under the same `SecretKey` an upstream key seals with — it cannot live in `channel_credentials`, whose `channel_id` is `NOT NULL` routing state a moderation endpoint has no part of).
  - `oxsum.guardrail_bindings` attaches a policy set to a `channel` or `key` scope; `api_keys.required_policies` is the key's own requirement and `mandates.required_policies` a subset-checked narrowing of it (minting a mandate that names a policy its key does not is a 400).
  - Resolution is per request: `GuardrailSet::for_turn` folds channel binding, key binding, the key's requirement and the presented mandate's into one deduplicated set — two indexed reads inside `run`, so a policy written or bound takes effect on the next call, never on a restart. A dangling name (a deleted policy a binding still lists) is skipped, never an error — the same fail-open the verdicts keep.
  - `pre_call` runs after the route is chosen, before the hold: a `block` answers 403 and costs nothing (only the `refused` marker is written); a `flag` is collected onto the settlement. `post_call` runs on the completed answer: a `block` settles `Charge::Nothing` (the hold fully refunded — the upstream answer already arrived, so the exposure is the org's, not the caller's) and still answers 403; a streamed turn's post-call verdict can only be *recorded*, which the settlement's `guardrail` list marks `streamed: true` — the honest answer for bytes already sent.
  - Fail doctrine splits by phase: `pre_call` fails **closed** — a moderation endpoint down must not admit an unscreened prompt, and refusing before the hold costs the caller nothing. `post_call` fails **open** — the answer is paid for on upstream's side regardless, so an outage must not eat a settled turn. Every error logs; the verdict is never silent.
  - The settlement description goes to v11: the optional `guardrail` array carries `{policy, verdict, reason, streamed?}` per acted policy — the audit a flag exists to leave, and the reason a blocked turn's refunded bill reads. The verifier recomputes v11 as v10's arithmetic plus the array's shape; the verdicts never change the charge.
- Rejected:
  - Policies in the signed mandate blob: a delegation's signature covers the delegate's constraints, not the server's rules; a policy renamed between mint and use would break the signature or unguard quietly. The row subset-check keeps the mandate's narrowing server-side where it is verified at mint.
  - Building the `GuardrailSet` once at startup: `app`/`app_with_billing` are synchronous and called by ~20 test files; a per-request build costs two indexed SELECTs and makes policy changes live without restart — the same seam the price history already uses.
  - Rewriting a flagged response's text (masking the PII in place): the answer is a serialized body whose strings a regex rewrite could corrupt mid-JSON; the flag's honest shape is the record, and a caller who wants the content blocked writes a keyword `block`.
  - Fail-closed `post_call`: the upstream answer already arrived and the upstream cost is real either way; an endpoint outage eating a settled turn punishes the org for its own monitoring's downtime.
  - A `moderation` bearer in `channel_credentials`: the table's `channel_id NOT NULL` makes a row routing state — a pool pick, a position, a cooldown — and a screening endpoint is none of that; sealing on the policy row itself keeps the same AES-256-GCM story without bending the pool's invariants.
- Costs and knock-on effects:
  - A streamed post-call `block` cannot un-send bytes: it settles as a flagged turn whose `streamed` marker says so — the verdict a dashboard shows for a finished stream.
  - `DESCRIPTION_VERSION` is now 11; v11 descriptions parse everywhere v10 did plus the `guardrail` shape.
  - Pinned by `crates/core/src/guardrail.rs` unit tests (compile, resolve, most-restrictive) and `crates/server/tests/guardrails.rs` (pre-call block before the hold, a flag settling on the record, a moderation endpoint's own verdict refusing, a response-side block refunding the hold, the PII flag, the mandate subset refusal, a dangling `requiredPolicies` and an uncompilable regex refused at write).

- Status: Adopted. Extends "Per-key rate limits consume at admission; the holds cap counts the ledger" (issue #130) — the seam that entry reserved ("a shared backend slots behind when the deployment goes multi-instance") now has its second implementation, and `memory` stays the default the deferred list already anticipated.
- Background: a single binary gets the whole `requestsPerMinute` story from `SlidingWindow`, but a deployment behind a load balancer running several oxsum instances let each process keep its own window — `per_minute` was really `per_minute × instances`, and a busy key double-admitted in proportion to the replica count.
- Decision:
  - `PostgresRateLimiter` in `crates/core/src/ratelimit.rs` implements the existing `RateLimiter` trait against `oxsum.rate_windows` (migration 0063): one row per limited key holding a `timestamptz[]` of request hits, bounded by the cap and the 60s window. `admit` runs `INSERT ... ON CONFLICT DO NOTHING` → `SELECT ... FOR UPDATE` → a prune-and-count `UPDATE ... RETURNING` → a conditional `array_append(now())` in one transaction — the row lock is the serialization two instances cannot get from a mutex, and the database's `now()` is the one timeline every instance shares. `peek` is a lock-free `COUNT`/`MIN` over the still-fresh hits: it consumes nothing, matching `SlidingWindow::peek`'s snapshot contract.
  - `OXSUM_RATE_LIMIT_BACKEND` (`memory` default, `postgres` shared) selects the impl at startup in `app_with_billing`; anything else refuses to boot. The env var is the whole contract change — no endpoint, field or error shape moves.
  - The trait stays synchronous: `PostgresRateLimiter` runs its pool work through `block_in_place` + `Handle::block_on`, valid because every call site sits inside axum's runtime. Making `admit`/`peek` async was rejected — the change ripples through every caller to buy a signature the in-memory impl never needed.
  - Fail-open on storage error, same doctrine `per_minute == 0` already encodes: a protection knob must never take the gateway down — the error is logged and the request admitted, because the wallet's `POOL_BUSY` is the real saturation refusal. This also means a misconfigured or missing table admits everything rather than refusing everything; the tests verify the window by reading the row back.
- Rejected:
  - One row per hit (`rate_windows (key_id, hit)`): unbounded growth under a busy key plus a `DELETE` sweep on every admit; the array keeps the row to the cap's own bound and the prune is one `UPDATE`.
  - Redis: still the deferred-list answer for a *third* backend; Postgres is already the deployment's shared store, so the feature costs no new infrastructure.
  - An async `RateLimiter` trait: see above.
- Costs and knock-on effects:
  - The `at: Instant` parameter is ignored by the Postgres impl — documented on it — because a process clock is exactly what a shared window cannot use; `SlidingWindow` keeps honouring it for tests.
  - `block_in_place` panics on a current-thread runtime, so every `rate_windows`/`rate_limit_backend` test is `multi_thread`.
  - Pinned by `crates/core/tests/rate_windows.rs` (two handles sharing one window, peek consuming nothing, a hit ageing out on the server clock, the row staying bounded) and `crates/server/tests/rate_limit_backend.rs` (the HTTP layer under `postgres` refusing the second request in the minute, and two backend handles over the app's pool admitting one window).

## 2026-10-31 Queued holds wait off the pool: an in-process per-key semaphore before `begin()` (issue #486)

- Status: Adopted. Complements "All tenants share one connection pool" (2026-10-01) — the pool stays the deployment's whole connection budget; what changes is where one key's waiting happens.
- Background: the 2026-10-31 load probe against the live demo saturated the shared pool from one unconstrained API key. `hold_for_key` serializes a key's holds on a `pg_advisory_xact_lock`, and every queued hold waited for that lock *inside a transaction it had already begun* — pinned a pooled connection for the whole wait. ~30 concurrent holds on one key checked out all 20 connections; every tenant's unrelated reads then timed out at the 5s `ACQUIRE_TIMEOUT` and answered `429 POOL_BUSY`. The blast radius was deployment-wide and the threshold to trip it was one busy key. The same probe surfaced a stray `401`: `resolve_key` folded a `PoolTimedOut` on the credential lookup into "no such credential", misreporting the service's own saturation as the caller's failure.
- Decision:
  - `hold_for_key` queues in-process *before* `pool().begin()` checks a connection out: `Wallet::key_queue` maps each key's `limit_lock_key` hash to a `tokio::sync::Semaphore` of width `KEY_QUEUE_WIDTH = 1`, and only the permit holder enters the transaction that waits on the advisory lock. The rest of the key's queue sits on async permits — waiting costs no connection, so a pile-up cannot check out the pool. The database lock stays the real arbiter (multi-instance correctness is unchanged); the semaphore only moves the in-process half of the queue off the shared resource.
  - A queue wait that outlasts `KEY_QUEUE_WAIT` (5s, matching `ACQUIRE_TIMEOUT`) refuses `WalletError::PoolBusy` — the same fast-fail a saturated checkout answers, so the backpressure contract `POOL_BUSY` already defines is the refusal both paths share.
  - The credential lookups propagate their storage errors instead of folding them into `None`: `resolve_key`, `resolve` and `authenticate_service` now return their `Result`, and a saturated pool answers `429 POOL_BUSY` on `/api/v1` and `/v1` alike rather than `401 UNAUTHORIZED`. `Ok(None)` still means "this credential authenticates nothing" — only a lookup that ran may say that.
- Rejected:
  - `pg_try_advisory_lock` in a retry loop: keeps the queued hold on a pooled connection while it polls — the same pinning, plus a poll interval.
  - A dedicated lock-taking connection per hold: pins one connection per queued hold anyway; the budget is the same.
  - A server-side per-key concurrency cap as a billing constraint (`maxConcurrentHolds` exists but is opt-in per key): the default-unconstrained key is exactly the case the probe broke; the fix has to live below the constraint layer, at the resource the queue consumes.
  - Raising `OXSUM_DB_MAX_CONNECTIONS`: moves the cliff, doesn't remove it — a busier key or a second concurrent pile-up finds the new ceiling the same way.
- Costs and knock-on effects:
  - The queue is per-process: multi-instance deployments each carry their own `KEY_QUEUE_WIDTH` depth inside the transaction plus their own queued waiters — the semaphore is a resource-protection layer on top of the database lock, not a distributed queue. Two instances still cap the in-transaction pile-up at 2 (one per instance), never at the request count.
  - `limit_lock_key` is now `pub` so the test can hold the same lock the wallet waits on.
  - Pinned by `queued_holds_do_not_pin_the_pool` (key_limits.rs — the lock held on a side connection, five holds queue on a 3-connection pool, `pool.size() - pool.num_idle()` never exceeds one) and `a_saturated_pool_is_busy_not_unauthorized` (api.rs — every pooled connection held, a valid key answers `POOL_BUSY` not `UNAUTHORIZED`).

## 2026-10-17 Test scratch cleanup is self-healing on the next run (issue #271)

- Status: Adopted. Complements #226 (per-suite scratch databases) and #241 (doubleentry on `DATABASE_URL`).
- Decision: drop-at-exit stays the last line, but the *first* line is a sweep run at setup time — a suite removes predecessors' residue before creating its own. `oxsum-testkit` drops `oxsum_test_*` older than 24 h inside `create_suite_db`; doubleentry's External path drops `de_*` databases whose v7-uuid timestamp is older than 24 h once per process. `scripts/drop-test-scratch.sql` offers the same pattern for immediate/manual cleanup.
- Why not the alternatives that were tried first:
  - `#[ctor::dtor]` in testkit never runs when the process is killed, times out, or panics through an unwind nextest cannot drive to completion — which is exactly when residue appears.
  - `tokio::runtime::Handle::try_current().spawn(...)` inside `Drop` posts a task that the teardown-bound runtime never executes, so doubleentry's harness drop was structurally incapable of running at process exit.
  - A janitor test or cron would still depend on something else choosing to run; making the sweep part of suite setup means every run pays the cleanup cost only when residue is actually present.
- Why 24 hours: it is far longer than any test run, so a colleague's live suite on a shared cluster is never matched by the stale-name pattern; and it bounds accumulation to one day of crashed runs.
- The `de_*` sweep keys on the name's own timestamp — a v7 uuid's first 48 bits are the unix-epoch millisecond the id was minted at — so a live neighbour's database is never selected, and no separate bookkeeping table is needed.

## 2026-10-12 Pages move back to Topcoat, replacing Leptos

- Status: Adopted for the development phase. Supersedes "2026-10-01 Pages move to Leptos, replacing Topcoat" (marked Superseded below).
- Decision: the dashboard pages move from Leptos 0.8 to Topcoat 0.10 (pinned). Server-side rendering only — no WASM bundle, no hydration, no client build step. The Topcoat router mounts as an axum `fallback_service` on the same port; API routes keep priority. Sessions stay self-built (the `oxsum_session` cookie mechanism is unchanged; Topcoat's built-in session module is not used).
- Why now (what changed since 2026-10-01):
  - Compile time proved to be the dominant cost of Leptos: touching one file in `crates/web` cost 2m52s of `cargo check` on the dev machine (Leptos's ~25 related crates plus heavy proc macros, compiled twice — SSR natively and hydrate to wasm32). The Topcoat spike measured ~7s for the same edit (~25x), with Cargo.lock shrinking from 624 to 561 packages.
  - Topcoat matured from 0.1 to 0.10; the "no frontend build step" advantage the 2026-10-01 decision dismissed as moot turned out to be exactly the compile-time win that matters.
  - The verification trade-off the 2026-10-01 decision centered on is accepted as a development-phase cost (see below), not a permanent verdict — the decision is revisit-able if the dashboard outgrows server rendering.
- Costs and knock-on effects:
  - `/verify` trust-model downgrade: the proof check ran in the browser (WASM), so a user verified a bill without trusting the server. Topcoat has no browser target; the check now runs server-side via `oxsum-verify` (native). The proof math is unchanged, but the "don't trust the server" property is gone. The page says so plainly ("The check runs on this server."). If the public narrative needs the trustless property back, the page can regain it with a standalone WASM verifier later — the pure `oxsum-verify` crate is already wasm-clean.
  - Interactivity steps back to server-rendered form posts plus vanilla JS for the chat/admin live parts; the `/ws/billing` socket is untouched.
  - Topcoat 0.10 is still pre-1.0 and self-described experimental: macro error messages are poor, docs are thin, community support is near zero. Mitigation: the version is pinned, and page/component helpers are kept behind small local wrappers to isolate API churn.
- Rejected:
  - Staying on Leptos: paying the ~25x compile tax every day for a dashboard whose pages are overwhelmingly server-rendered admin screens.
  - askama + htmx: the safest compile-time story, but it discards the component model the pages already use and still leaves the heaviest interactivity to hand-written JS; Topcoat keeps one Rust technology for pages.
  - Dioxus: same proc-macro weight class as Leptos, no compile-time gain.

## 2026-10-05 The productization roadmap is settled (issue #100)

- Status: Adopted as direction; nothing implemented yet. **Narrowed** by "The guardrail pipeline judges a turn in two phases" (2026-11-01): the product-boundary line below put "guardrails/content safety" out of scope, and what stays out is the moderation *product* — oxsum does not write the rules or score the content. The org-configured pipeline that runs an operator's own policies around a turn (issue #502) is the billing-adjacent protection layer the boundary always kept, and it landed under that later entry. It names the work backlog (P5-1 through P8-4 plus the items below); each item lands as its own issue and PR, and the behavioral docs (product.md, architecture.md, api.md) are updated inside those PRs, not here.
- Background: two studies settled this — a capability comparison against a production billing monorepo (sandbase), then targeted evidence from LiteLLM, new-api, Portkey, AIProxy, Bifrost, TensorZero and Lago. The full discussion record lives in local notes outside the repository; this entry archives what was decided.
- Decision — product boundary: oxsum is a verifiable AI credit wallet and billing gateway. A general metering core accepts arbitrary billable events, but sandbox execution, agent runtimes, skills marketplaces, content generation, the LLMOps surface (prompt registry, evaluation, experiments, optimization) and guardrails/content safety are all out of scope.
- Functional form, five settled groups:
  - **Pricing.** Conditional price rules ride on versioned price rows; a matched condition swaps the whole price set, never a field-level diff. Selection is most-specific-wins, and ambiguous same-specificity overlaps are rejected when the rule is written, not when it bills. Price units are explicit `{price, unit}` pairs. The stacking order is: a condition picks the price, gross line items are computed, then the single most favorable applicable discount applies — discounts never stack. An organization's tier is a capability package (limits, allowlists, operational defaults), never an input to the pricing formula.
  - **Credentials and credit.** Lago-lite statements with a lifecycle (draft, finalized, payment pending, paid, overdue, suspended), statement lines, an attached proof reference, payment terms, due date and overdue timestamp. A credit limit lets an organization's balance go negative down to a configured bound. PDF invoicing is deferred. Deletion means pseudonymization: recognizable fields (name, email, IP, endUser) are blanked while the ledger history stays physically intact — the books keep the UUID, not the identity, so proofs keep verifying.
  - **Attribution.** `endUser` and `tags` write freely (bounded length and count, no allowlist — they name the customer's own users). Analytics never aggregates per end user; it shows top-N plus an "other" bucket, and a specific user is reached through detail filtering. Rate limits take tag scopes in three forms — Each (a bucket per tag value, expiring with the window so cardinality stays bounded), Concrete (one named value), Total (the whole tag).
  - **Protection.** Admission refuses by distinct status code: `402` for insufficient credit (the body carries `requiredMinor`, `availableMinor` and a hint), `429` with `Retry-After` and `X-RateLimit-*` for rate limits, `403` for allowlist/IP/disabled refusals. RPM and concurrency consume their allowance at hold creation; TPM windows count actual settled usage, with admission checking window-actual plus this estimate. Holds never occupy TPM allowance; hold-spam is bounded by a per-key concurrent-hold cap. Hold estimation is honest rather than clever: a client-declared `max_tokens` (or the model ceiling when undeclared) priced at the highest applicable tier — no clamping of the client's request and no guessing, and a balance short of the ceiling reservation gets the actionable 402 instead of a silently truncated response. Admission valuation failures are fail-closed; money admission never fails open.
  - **Metering.** A thin service-credential API (`holds`, `settle`, `release`, one-shot `settlements`) reuses the existing hold/settle ledger primitives; event pricing resolves a `billableCode` into the PriceBook; usage rides the same UsageRecord shape; every call carries an idempotency key; external settlements enter the ledger and its proofs like any other. Organization keys can never self-report usage — only the deployer's own services may meter on an org's behalf. There is no metric-definition DSL.
- Monitoring is split in two. User-facing analytics lives in Leptos pages (hand-drawn SVG charts, no JS chart library, to keep the single-binary pure-Rust stack); operator observability is a Prometheus `/metrics` endpoint plus a lightweight built-in ops page (open holds, job queue, anomalies, channel health). Grafana is never bundled; alerting rides the outbound webhook system rather than a second notification channel.
- Outbound webhooks: a lean event catalog — `deposit.credited` / `deposit.failed` / `deposit.reversed`, `balance.low` (the first event to ship), `statement.finalized` / `statement.payment_overdue` / `statement.payment_status_updated`, `org.suspended`, `org.credit_limit_exceeded`, `anomaly.detected`, and an opt-in `request.settled` (per-request events stay off by default — they are too noisy). The envelope follows the Stripe shape — `{id, type, created_at, org_id, data}` — with an HMAC signature header and a timestamp against replay, exponential-backoff retries, delivery logs, and manual redelivery from the admin surface.
- External dependencies are optional by environment: SMTP and the anti-bot check are env-gated so a deployment runs fully without them; GitHub is the first OAuth provider.
- Frontend stays Leptos-only: SSR plus hydration plus in-browser WASM verification already work, and single-binary deployment is preserved. The decision is revisited only if a future UI proves beyond it — an embedded SPA inside the same binary remains the fallback, a separately deployed frontend is rejected.
- Technical form (T1–T4), all settled:
  - **UsageRecord** carries the full dimensions: input/output tokens, cache-read, cache-write 5m and cache-write 1h (two distinct billing dimensions, not a duration parameter), reasoning output, tool calls, media token fields, `serviceTier`, `eventType`, `endUser`, `tags`, and a `usageDetails` escape hatch holding `provider_raw`. Non-token units (per image, per second, per request) live in `usageDetails` units — the structured columns hold token-metered quantities only.
  - **Versioned settlement descriptions.** Descriptions gain an explicit `v` field. v1 is the current shape; v2 is a self-contained bill: `priceVersion`, matched rule, full usage, itemized lines, applied discount, charged, freeze, upstream cost — everything recompute needs, inside the hashed bytes. Inclusion proofs are schema-agnostic by construction; recompute verification dispatches on `v` and degrades to "inclusion verified, recompute skipped" on versions newer than the verifier. No v1 backfill exists or is needed (no production data); the version field exists so future evolution is possible without breaking verifiability.
  - **PriceBook** extends `channel_prices` in place (append-only versions preserved), adding the new usage dimensions, conditional rule rows, upstream-cost prices and a `mode` field; changing a price or a rule inserts a new version. Discounts are a separate table (org/model scope, percent, validity window) so price hit and discount hit are two independent snapshots. The wallet splits into `Bonus` and `Purchased` sub-accounts; a hold spanning pools splits into per-pool reservations so each stays funded, and settlement draws Bonus first. A rail-to-pool map decides which pool a deposit credits.
  - **New tables.** `deposits` (unique `rail:payment_ref` idempotency, amount vs received, pending → confirmed → credited | reversed | expired), `redemption_codes`, `statements` + `statement_lines`, `metering_holds` + `metering_settlements`, `pricing_discounts`, `jobs`, idempotency records, `tier_profiles`; `orgs` gains `tier`, `credit_limit`, `payment_terms_days`, `rpm_limit`, `enabled`.
  - **Interfaces.** A `UsageAdapter` trait lives in crates/core (usage normalization is billing semantics), keyed by protocol through a registry — SSE scanning and transport stay in the server gateway; OpenAI chat is the first implementation, Anthropic Messages the second. `Idempotency-Key` follows the Stripe shape: same key and same request fingerprint replays the stored response, a different fingerprint is `422`, in-flight is `409`, records live 24h. Streaming duplicates return the settled receipt rather than replaying a stream.
  - **Runtime.** An in-process sliding-window rate limiter behind a `RateLimiter` trait (a Redis backend is a drop-in later); a Postgres `jobs` table with `FOR UPDATE SKIP LOCKED` workers, exponential backoff and a dead state surfaced as anomalies — SKIP LOCKED makes the queue multi-instance safe already; Prometheus metrics including refusal-reason counters; graceful shutdown drains in-flight holds under a bounded timeout and the startup sweeper collects orphans.
  - **Migrations and contracts.** Migrations extend existing tables additively with no backfill; `/v1` accepts additive changes only, breaking changes go to `/v2` with deprecation headers and dates; new errors carry actionable hints.
- Data retention: idempotency records 24h; the ledger, deposits, statements, normalized usage records and daily aggregates are permanent; `usageDetails.provider_raw` is nulled after 7 days (the reconciliation window suffices and raw upstream payloads may carry prompt fragments — a privacy liability); jobs 90 days; webhook delivery logs 30 days. Cleanup is a periodic batched job on the jobs table; durations are env-configurable.
- Testing policy applies to every issue: completion criteria must include tests — unit tests for core domain logic, an integration test under `crates/server/tests/` per endpoint, settlement golden fixtures, ledger-invariant property tests, proof round-trips, and wasm-side verification checks on schema changes. Red tests block merge; the full local gate runs before every push.
- Footnotes: budget and statement windows are UTC; a suspended organization's in-flight holds settle while new holds refuse (P7-3 acceptance criterion); a bill proof attests "charged correctly given the stated price" — price authenticity rests on the append-only catalog plus the recorded `priceVersion`, and a signed catalog snapshot is listed as future hardening.
- Rejected, highlights: fail-open admission valuation (the reference gateway does it; money admission must fail closed); a metric-definition DSL (that is Lago's product core, not ours); per-field conditional price diffs; stackable discounts; tier as a formula input; bundling Grafana; a second frontend framework today; a separately deployed frontend ever, while single-binary is the product's shape; silently clamping a client's `max_tokens` to keep reservations small.
- Deferred scope — recorded here so it is not re-debated; the architecture already holds interface positions for each: further payment rails (manual, redemption, Stripe, x402 and Airwallex are live — the seam proved; what remains is *more* gateways, not the missing rail), crypto on-chain deposits and ledger anchoring (a watcher reads the chain directly, N confirmations before crediting, reorg produces a reversing entry), Redis-backed rate limiting and job backends, request failover with mid-stream cutoff as low-balance graceful degradation, batch API, playground, embed dashboards, multimodal metering beyond token dimensions, first-deposit bonuses, finer member budgets, embedding-similarity semantic caching, affiliate attribution. (Model aliases left this list with issue #286 — a name is a rename on one channel, not a routing layer, so it sits beside the deferred *weighted* alias→several-models feature, which stays out. Exact-digest caching left this list with issue #497 — reuse keyed on the normalized prompt needs no embedding infrastructure; the similarity-search variant stays here, since it is the version that needs a vector store and an honest threshold.)
- Implementation notes — the planned issue map, filed as issues just-in-time per batch:
  - **P1 billing core (the correctness spine):** P1-1 UsageRecord schema expansion; P1-2 UsageAdapter layer plus the OpenAI implementation; P1-3 versioned settlement descriptions plus WASM verify v2; P1-4 PriceBook (new dimensions, conditions, upstream prices, mode) plus itemized pricing; P1-5 fail-closed pricing; P1-6 upstream-cost tracking; P1-7 billing-correctness test suite (golden fixtures, ledger-invariant property tests, proof round-trips).
  - **P2 funds:** P2-1 Bonus/Purchased pools plus migration; P2-2 deposit abstraction plus the redemption rail.
  - **P3 controls:** P3-1 key constraints (budgetDuration, modelAllowlist, expiresAt); P3-2 org creditLimit plus statements (split statements into their own issue if the review surface gets large); P3-3 usage_daily aggregation plus the usage dashboard; P3-4 rate limiting; P3-5 request-level idempotency.
  - **P4 reliability:** P4-1 reconciliation layer (ledger invariants, our settlements vs upstream bills, deposits vs rail); P4-2 Prometheus metrics; P4-3 response-cost header plus balance runway; P4-4 settlement dead-letter state.
  - **P5 usability:** P5-1 Anthropic Messages adapter; P5-2 outbound webhooks; P5-3 programmatic APIs (usage, billing-records, estimate-price, public pricing catalog); P5-4 analytics pages.
  - **P6 accounts:** P6-1 email verification plus password reset (SMTP); P6-2 OAuth (GitHub first); P6-3 anti-bot on signup and redemption; P6-4 CLI device authorization.
  - **P7 enterprise:** P7-1 org tiers plus discount admin endpoints; P7-2 admin audit log; P7-3 org suspend plus member budgets; P7-4 the demo `seed` command.
  - **P8 scale:** P8-1 the PG jobs layer (webhook delivery, reconciliation, aggregation, retention cleanup); P8-2 failover (one model to N channels, weights, upstream_attempts, and the upstream-429 retry/backoff policy); P8-3 embeddings/rerank metering; P8-4 the metering API (service credentials only, depends on P3-5 and P8-1).
  - `TODO.md` tracks status per the development process; documentation for each capability lands in the same PR as its code. The deployment runbook (backup, restore, migration ordering, upgrade/rollback, key rotation) lands in `docs/development.md` during P8.

## 2026-10-05 The billing-core roadmap is settled (issue #100)

- Status: Adopted as direction; implementation pending. P1 through P4 is the correctness spine and lands before the productization batches.
- Background: a dissection of sandbase's metering/billing/prediction stack plus LiteLLM spend tracking, new-api quota semantics and Portkey normalization supplied the evidence. The design's six layers (UsageRecord, PriceBook, Deposit, Controls, Observability, account structure) and the full threat table live in the local notes; the settled decisions are archived here.
- Decisions D1–D13:
  - **D1 — balances split into two pools.** `Equity:Bonus` (granted credit) vs `Liabilities:Wallet` (purchased credit); settlement draws Bonus first. Refund, expiry and reconciliation semantics stay clean, and a hold spanning pools splits into per-pool reservations.
  - **D2 — top-up is a multi-rail deposit abstraction.** First rail is redemption codes (smallest, reversible, zero payment integration); Stripe joined as the second rail (#201) and crypto is a later rail on the same `deposits` table.
  - **D3 — usage expansion is structured columns plus a JSON escape hatch.** cached, cache-creation (5m and 1h separately), reasoning and friends are real columns; `usageDetails` JSON carries provider-specific extras.
  - **D4 — credit limits are in scope.** "Borrow first, settle monthly" for enterprise orgs; lands in P3-2 only after the rest of P1–P3 is done, because it changes FundedReservations semantics.
  - **D5 — free quotas are out** for now.
  - **D6 — fine-grained pricing is itemized, not difference-based.** Each charge component is computed and summed independently and lands in the settlement record's breakdown, so every line is independently recomputable from the proof.
  - **D7 — `serviceTier` gets a schema slot now**, implementation later when a channel actually prices tiers.
  - **D8 — no per-key sub-quotas.** The existing cumulative `spendLimit` covers the cases that matter; sub-quotas split the unit of account for little gain.
  - **D9 — the usage-normalization adapter layer is built now**, not deferred: protocol-keyed, OpenAI first, Anthropic next.
  - **D10 — absolute unit prices plus an explicit multiplier layer**, not a compressed-ratio model: org discounts, promotions and reseller markups are a `price × multiplier` snapshot recorded in the settlement (discountPercent/markupRatio), so an audit can always answer "why this number".
  - **D11 — 1 credit = $1** (working definition, revisable; the OpenRouter-shaped intuition).
  - **D12 — upstream cost is tracked per request** alongside the customer charge: gross margin is visible and upstream misbilling is detectable (P1-6).
  - **D13 — request-level idempotency and per-key rate limiting are both built** — a client retry must never produce a second hold or charge.
- Three rules written into schema and settlement behavior: rounding goes up to the minor unit in the customer's direction on every billing multiplication; freeze estimates always take the most expensive plausible path (cached priced as full, output at the declared bound) and settle down against actuals; crediting trusts first-party sources only — a redemption row in our database, a signed payment webhook, or a chain watcher reading the chain itself — never a client's claim.
- The threat-model posture behind them, for the record: duplicate crediting is refused by the `rail:payment_ref` idempotency key and atomic `FOR UPDATE` code redemption; lost billing cannot happen because the settlement entry lands inside the request transaction; overspend is refused at the ledger layer by the hold before the upstream call; mispriced usage fails closed to anomalies instead of silently billing zero; payment under/over-payment records expected vs received and reconciles by hand, never auto-patches; stolen keys are bounded by hard spend caps, model allowlists and revocation; upstream channel credentials are AES-GCM sealed at rest; operator tampering is impossible without trace because adjustments require a reason and an operator identity inside proof coverage; a forged parallel history is caught by signed tree heads, consistency proofs and the browser's own archive.
- Rejected, highlights: new-api's compressed price-ratio model (expressiveness and float pitfalls); LiteLLM-style best-effort spend logging (our settlement entry lands inside the request transaction — billing is a correctness system, not a log); fail-open on missing prices (fail-closed to anomalies and alerting); mutable balances anywhere in the money path.
- Implementation notes: P1 was re-split after review because the original P1-1 bundled three concerns — the map is now P1-1 UsageRecord schema, P1-2 adapter layer with the OpenAI implementation, P1-3 versioned settlement descriptions and WASM verification, P1-4 PriceBook, P1-5 fail-closed pricing, P1-6 upstream cost, P1-7 the billing-correctness test suite; then P2-1 balance pools, P2-2 deposits plus redemption, P3-1…P3-5 controls, P4-1…P4-4 reliability. Contract-first per the hard rules: `openapi.yaml` changes before implementation on every API item.

## 2026-10-06 The credit limit is a third pool, not a relaxed overdraft rule (issue #122)

- Status: Adopted. Implemented 2026-10-06, first half of issue #122 (roadmap P3-2 — statements follow as their own issue, the split the roadmap already allowed).
- Decision — D4 lands without touching `FundedReservations` or the vendored engine: credit is a third pool. `Liabilities:CreditLine` carries the same `FundedReservations` limit as Bonus and Wallet, so a hold on credit reserves against what the line carries and a release can never invent room; `Assets:CreditFacility` (`NoCreditBalance`) is the commitment memo whose debit balance is the granted limit. Granting or resizing books the delta `debit CreditFacility, credit CreditLine` — the whole credit history sits inside the verifiable log, and a shrink below the outstanding draw is refused by the credit line's own funding rule inside the append, not by a check beside it. The organization owes `limit − line balance`; draws order bonus → wallet → credit line.
- A repaying deposit is two-legged: `top_up` credits the drawn part to `CreditLine` first and the rest to `Wallet`, so paying in shrinks the debt before it adds own funds — the "org 充值 → 余额回升" half of the monthly-billing shape without waiting for statements. Because the repay split depends on what is drawn, `top_up` answers a stored entry under the derived id before sealing a retry's recomputed draft, and a same-key different-amount call is the engine's key-reuse conflict. Both `set_credit_limit` and the repaying leg serialize on a per-tenant advisory lock (`oxsum/credit-limit/v1`): limit writes re-read the committed figure under it, repayments re-read the draw under it, and draws — which only ever push the line's balance down — stay off the lock entirely.
- Reads: `available` is now the pools' sum including undrawn credit (what a hold can actually reserve); `credit_limit`/`credit_used` report the facility and `limit − line headroom`. `settled_spend_since` counts credit-line charges but excludes credit-limit entries by their facility leg — shrinking a line is not spend. A grant or shrink classifies as an `Adjustment` in the transaction list, since the signed spendable effect is the honest figure to show.
- One latent defect the generative suite surfaced once a third pool made replay shapes diverge: a retried `hold` recomputes its pool split against balances that moved since the original landed, so the retry's draft need not equal the committed entry — and the engine's key gate then read a faithful retry as key reuse. `hold`, `hold_for_key`, `set_credit_limit` (whose delta likewise depends on when it lands) and the deducting `adjust` (whose split depends on the bonus pool at retry time) now answer a stored entry by what the request *is* — amount, description, actor, and the leg only that operation carries (a top-up always debits `Cash`, a deduction always credits `Adjustments`) — not by the postings it happened to compute; the same rule `top_up` already applied to its repaying shape.
- Rejected: a parameterized `BalanceLimit` letting the wallet settle negative to a bound (changes the vendored engine's canonical account encoding and the per-account CHECK for what the three-pool model already expresses; and commingling borrowed money into `Liabilities:Wallet` makes the same "balance" conflate own funds with the facility); auto-writing an `organizations.credit_limit` column as the source of truth (the ledger is the source of truth — a cached column can drift from the entries that moved the line).
- Pinned by `crates/core/tests/wallet.rs` (draw order, release-to-line, repay-first top-up including the recomputed-shape replay, shrink-below-draw refusal, concurrent holds bounded by the grant, credit entries classifying without counting as spend) and `crates/server/tests/admin.rs` (grant → draw → shrink refuse → replay → repay end to end).

## 2026-10-05 Deposits are one table for every rail; redemption is the first live rail (issue #118)

- Status: Adopted. Implemented 2026-10-05, closing issue #118 (roadmap P2-2), per settled decision D2.
- Decision — migration 0011 adds `oxsum.deposits` and `oxsum.redemption_codes`. Every credit into the purchased pool is a deposit: the existing `/topups` writes a `manual` row beside the ledger credit, and redemption codes mint through `POST /api/v1/admin/redemption-codes` and redeem through `POST /api/v1/redemptions`. Stripe and chain watchers join the same table later; the rail column is the seam, and `amount_minor` versus `received_minor` is where an under- or over-payment is recorded for a human, never auto-patched.
- The idempotency anchor is `(rail, organization_id, payment_ref)` — not `(rail, payment_ref)` alone, because a top-up's reference is the caller's idempotency key, which is only scoped inside its organization; two organizations answering the same key stay distinct. For the redemption rail the reference is the code's id.
- A deposit walks `pending → confirmed → credited` with `reversed` and `expired` as the ways out; `entry_id` names the ledger entry once credited. The redeem sequence is claim-the-code → insert the `confirmed` deposit in the same transaction → ledger credit under the fixed key `redemption:<code_id>` → flip the deposit to `credited`. A crash anywhere after the claim leaves a `confirmed` row a retry resumes: the code is already this organization's, the ledger key replays the same credit, and the entry_id lands on the row it belongs to. Two redeems of one code cannot both win because the claim holds the row `FOR UPDATE`.
- Codes are `oxr-` plus 32 random bytes, stored only as SHA-256 — the same posture as invitation tokens and API-key secrets: a dump redeems nothing and the mint answer is the only readable form. Unknown, spent-by-another-organization and expired codes all answer `NOT_FOUND`; a code this same organization already redeemed replays its own credit so a retry after a lost answer still gets a receipt.
- Pinned by the integration suite: mint validates its batch bounds, the hash-only storage is asserted against the table, a redeem credits the purchased pool once across the caller's own replays, the same code is `NOT_FOUND` to another organization, an expired code is `NOT_FOUND`, and a top-up's `manual` deposit row carries the entry its receipt names.

## 2026-10-05 Key constraints: periodic budgets and model allowlists ride the key row (issue #120)

- Status: Adopted. Implemented 2026-10-05, closing issue #120 (roadmap P3-1).
- Decision: the key's constraint set is one row on `oxsum.api_keys` — `spend_limit_minor` (existing), `budget_duration` (migration 0012, `daily|weekly|monthly`, null) and `model_allowlist` (`text[]`, null for all). `expiresAt` already existed; nothing was reimplemented for it. A `KeyConstraints` value passes through `create_key` and `update_key_constraints`, validated in core — a window without a limit is refused, as is an empty or blank allowlist entry — and `PATCH /api/v1/org/keys/{keyId}` replaces the whole set, each field cleared by `null`.
- Enforcement stays in `Wallet::hold_for_key`, inside the same per-key advisory-lock transaction that already serialized the limit check: the locked row now projects all three columns, so a PATCH landing between authentication and the hold cannot be missed for any of them. `budget_duration` scopes only the settled half of the committed read (`p.layer = 'pending' OR e.booking_date >= period_start`): an outstanding hold binds money now whatever period it began in, so it always counts; a charge settled before the window does not. `period_start` is the UTC calendar boundary — the day itself, the ISO week's Monday, the first of the month.
- `model_allowlist` gates only callers that name a model: the gateway passes the requested model and a miss answers 403 (`permission_error` in the OpenAI shape) before a freeze is written or upstream is contacted; `POST /api/v1/holds` names no model and is never gated. The check sits after the replay test, like the limit's: a retried hold answers its own receipt even if the list changed underneath it. Exact string match — no globs, no prefixes: a pattern language would make the contract ambiguous for one column's worth of value.
- Rejected: a separate constraints or quota table (one constraint set per key, same lifetime — a 1:1 join for no reason); checking the allowlist at authentication (the row is already locked and re-read in the hold, and session holds name no model anyway).
- Pinned by `crates/core/tests/key_limits.rs` (daily/weekly/monthly boundaries, outstanding holds counting across a reset, racing holds under a period budget, allowlist gating and replay-past-list-change), `crates/server/tests/api.rs` (round-trip, PATCH replace-all and clearing, malformed-input refusals) and `crates/server/tests/gateway.rs` (403 for an unlisted model with nothing frozen and upstream never contacted, the listed model passing, `null` restoring all).

## 2026-10-05 Balance pools: Bonus beside Wallet, with lazy grant reclassification (issue #116)

- Status: Adopted. Implemented 2026-10-05, closing issue #116 (roadmap P2-1).
- Decision — D1 landed as written: every tenant ledger gains `Equity:Bonus` beside `Liabilities:Wallet`, both carrying `FundedReservations`. Grants — the signup bonus and every positive admin adjustment — credit Bonus; top-ups still credit Wallet. A hold splits its reservation across the pools bonus-first, posting a pending debit on each pool it draws so `FundedReservations` keeps each side funded inside the append; a settlement releases exactly the split the hold recorded and charges `min(actual, bonus_held)` from Bonus before the Wallet. A deduction draws Bonus first too: the platform takes back what it granted before touching money the user paid for. Nothing in the contract changes — the balance endpoints answer the pools' sum, and the reclassification entry classifies as no transaction kind, so bills never show it.
- The split is a read-then-write choice: two racing holds can read the same bonus headroom and both over-allocate it, so the write retries (bounded) with balances re-read — the append's limit is still the only thing that can truly refuse. A settled charge never exceeds what its own hold reserved per pool, so a charge can never eat another hold's reservation; the aggregate argument that held for the single pool holds per pool.
- The migration is one reclassification entry per existing ledger — `debit Wallet, credit Bonus` for `clamp(grants - settled wallet outflow, 0, wallet balance)`, where `grants` sums the settled Wallet credits that pair with an Adjustments debit and `outflow` is everything the wallet ever paid out. The formula is draw-bonus-first applied retroactively, bounded by what the wallet still holds. The entry writes under the fixed idempotency key `pool-reclassification` from `Wallet::open` — lazy per-ledger rather than a sweep of every tenant inside `Db::migrate`'s global lock (a sweep serializes every boot behind the busiest tenant's schema work and holds the lock across work done on other pooled connections), self-marking so a failed pass retries cleanly on the next open, and excluded from `settled_spend_since` by that key because a rebalancing between pools is not spend.
- Pinned by the wallet suite: a hold spanning both pools reserves each side and charges bonus-first on settle, a deduction spills only after the bonus side is empty, the reclassification moves exactly the unspent remainder once, and the tightened-limit test now weakens and re-tightens both pool accounts.

## 2026-10-05 Upstream cost rides the usage row; the margin view is admin-only (issue #112)

- Status: Adopted. Implemented 2026-10-05, closing issue #112 (roadmap P1-6).
- Decision: `oxsum.usage_records` gains `upstream_cost_minor` (migration 0010), written beside `charged_minor` by the path that settles the turn. `Price::upstream_cost` resolves the same set the customer priced under — a matched rule's `upstream` block comes with its whole set — and `PriceSet::upstream_minor` prices the same usage at upstream's rates with the same subset-fold and ceiling-over-numerator arithmetic as the customer lines. `GET /api/v1/admin/margin` sums the two columns per `(channel, model)` plus an `untrackedTurns` count of the NULL rows.
- NULL is a state, not a number: a price with no `upstream` block, a swept turn (its watch row carries no price), and every row predating the column are untracked — where zero would hide a coverage gap inside a real cost, `untrackedTurns` names it. The customer side changes nothing: upstream cost is never an input to the charge or the freeze, never enters a settlement description, and reaches no organization-facing endpoint — the margin is the deployer's data.
- The swept-turn NULL is deliberate scope: the watch row holds only what its own settlement needs (the base rates for the anomaly list), not the resolved `Price` — snapshotting the whole price for a row the reconciler will compare against upstream's actual bill anyway adds a second copy of the wrong thing.
- Contract `openapi.yaml` 0.22.0 first; pinned by the billing unit tests (upstream rates, rule resolution, fold semantics, flat fee, overflow), a gateway test whose usage row carries the tracked cost beside an unchanged charge and NULL without an `upstream` block, a swept-row NULL assertion, and the margin endpoint's integration test.

## 2026-10-05 Pricing fails closed: a dimension the book cannot bill settles `unpriced` (issue #110)

A usage report that names a metered dimension no `PriceSet` covers — `toolCalls`, the media token columns, a foreign `eventType` — settles `unpriced`, a new `SettlementKind`. `Price::unpriced_dimensions` names what no set can bill; the relay flags the turn and charges exactly the computable part, capped by the freeze. The unpriced rest is the platform's recorded loss — never a silent zero, never folded into a rate the dimension does not belong to. The distinction the P1-4 note drew still stands: a dimension *inside* a side's totals (cache, reasoning) folds into that side's base line when unpriced; what is flagged is what no line can claim. The kind joins the anomalies the admin endpoint lists, `verify_charge` treats it as a billed turn (the flat fee still counts once), and the OpenAI adapter maps `prompt/completion_tokens_details.audio_tokens` into the named audio columns so the check has a real dimension to catch today. Contract `openapi.yaml` 0.21.0 first; pinned by the billing unit tests, the adapter audio test, the verifier's unpriced fixture, the anomalies-list integration test, and an end-to-end audio turn settling `unpriced` in the gateway suite.

## 2026-10-05 The price book is itemized and conditional, and settlement descriptions are v3 (issue #108)

- Status: Adopted. Implemented 2026-10-05, closing issue #108 (roadmap P1-4).
- Background: the roadmap's pricing decisions settled the semantics — D6 itemized pricing, the "Pricing" bullet's whole-set conditional rules with most-specific-wins, D7's `serviceTier` slot, D12's upstream costs — and P1-3's v2 description was deliberately written before the itemized book existed ("`matchedRule` … arrive with P1-4"). The open questions were the row's shape and the v3 wire's spelling.
- Decision: `channel_prices` extends in place (migration 0009, versions still append-only) with one JSONB `price` column carrying the whole `Price`: the base set (input/output rates, the output ceiling, `cacheRead`/`cacheWrite5m`/`cacheWrite1h`/`reasoning` rates, `costPerRequest`, `upstream`), a `mode` (only `chat` today; the field reserves the others), and `rules` — conditional sets whose `match` (`serviceTier`, `minInputTokens`, `maxInputTokens`) picks the price, exactly as the Pricing decision requires: a match swaps the *whole* set, selection is most-specific-wins by count of present conditions, and `Price::validate` refuses two rules that could match the same request at the same specificity when the price is written. `Price::itemize` decomposes a turn into `BillLine`s — the cached part on a `cache_read` line, reasoning on its own line, unpriced dimensions folded into their side's base line, and the flat fee as a `request` line whose rate is the fee × 1e6 so every line obeys the same `units·pricePerM` arithmetic — with one ceiling over the combined numerator. The freeze prices the dearest set that could match (a rule's rates join the estimate whenever the request could hit them, so a request's declared `serviceTier` picks that lane), plus the flat fee.
- The description is `"v":3`: `matchedRule` records the match itself so the bill names its rule without the price history, and `lines` spells each line as the tuple `[item, units, pricePerMillion]` — the ledger's 512-character limit cannot afford an object's repeated keys, and the item name keeps the tuple self-describing. `usage` stays `MeteredUsage`: subsets still *include* their side's totals (`input` = fresh + cached + cache-writes; `output` = completion + reasoning), so `verify_charge`'s v3 rule binds the *sum* of each side's bound lines to the side's total, each bound item once, `request` to one per billed turn — an unpriced or future item is unbound and still joins the sum, the same open-extension posture v2 took.
- Implementation notes: contract `openapi.yaml` 0.20.0 first (`PriceSet`, `UpstreamPrices`, `PriceMatch`, `PriceRule`; `ModelPrice`/`PriceRequest` flatten the set). `BillLine` owns its tuple serde so writer and reader cannot drift; `oxsum_verify` keeps its own wire interpretation and v2 descriptions still recompute under the old rule. `upstream` rides the stored price for the P1-6 cost view but stays out of the settlement description — it is admin data, not a billing input. Pinned by the billing unit tests (itemize, folding, rule match and specificity, ambiguous-book refusal, freeze lanes, round trips), `charge.rs`'s v3 tests (partition binding, flat fee, tamper), the admin suite's write-and-read-back of a full price plus its ambiguity rejection, and a gateway end-to-end rule-priced turn whose description recomputes.

## 2026-10-05 Settlement descriptions are versioned, and the verifier recomputes them (issue #106)

- Status: Adopted. Implemented 2026-10-05, closing issue #106 (roadmap P1-3).
- Background: T1-2 settled the mechanism — `"v"` on the description, dispatch by version, degrade on an unknown one — and the open question was what v2 contains before the itemized price book (P1-4) exists.
- Decision: `Settlement::description` writes `{"v":2, request, channel, model, priceVersion, kind, usage, lines, charged, freeze}`. `usage` is `MeteredUsage`: the record's metered dimensions only — `endUser`, `tags` and `provider_raw` stay out of the immutable ledger exactly as the retention decision demands, while `serviceTier` and `eventType` stay *in* because prices may match on them and a verifier needs every pricing input under the hash. `lines` decomposes today's two-component price into `[{item:"input",units,pricePerM},{item:"output",...}]`, so the recompute rule is the one the roadmap's example already spells: `charged == min(ceil(Σ units·pricePerM / 1e6), freeze)`, a single ceiling over the combined numerator — matching `Price::minor_for` bit for bit. Zero-valued usage fields are not written, so the common text-only record stays small inside the ledger's 512-character description limit. `oxsum_verify::verify_charge` dispatches on `v` (older → `OlderSchema`, newer → `NewerSchema{v}`, v2 → `Recomputed`/`Mismatch`, non-settlement → `NotASettlement`) and the `/verify` page reports the result beside the inclusion verdict. `SettlementRecord::parse` reads v2 only — the T1-2 revision declined compat parsing, so unversioned dev-ledger rows stop listing on the requests and anomalies pages.
- Implementation notes: the wire shape is pinned inside `oxsum-verify` rather than imported — versioned schemas mean today's verifier reads what today's writer wrote after the writer has moved on. `matchedRule`, `discount`, `upstreamCost` and `actualModel` arrive with P1-4, P7-1 and P1-6 as additive fields. Pinned by `charge.rs`'s unit tests (recompute, single-ceil, cap, zero-charge kinds, tampering, schema dispatch), the billing round-trip tests, and real settled and swept descriptions passing `verify_charge` in the gateway and sweeper suites.

## 2026-10-05 The channel names its protocol, and the adapter registry resolves it (issue #104)

- Status: Adopted. Implemented 2026-10-05, closing issue #104 (roadmap P1-2).
- Background: D9 built the usage-normalization adapter layer — protocol-keyed, OpenAI first, Anthropic next as an implementation only. The question left open was *where the protocol is named*: a field on the request, a deployment-wide assumption, or a property of the channel.
- Decision: `oxsum.channels.protocol` (migration 0008) is the name — a channel's wire protocol is part of its connection identity, alongside its address and credential. `adapter_for` in `crates/core/src/adapters.rs` resolves the name to a `UsageAdapter` implementation; `Db::set_channel` refuses a protocol the registry does not know at write time, because a channel that cannot normalize its own usage reports cannot serve — the same fail-closed posture as missing prices. The gateway resolves the adapter once per request when it resolves the serving, and a stored protocol with no adapter is a deployment error (`Misconfigured` → 500), not a caller's 4xx. Every existing channel defaults to `openai`, which is the truth for all of them. `usage_of` left `relay.rs` for `OpenAiChat::usage` unchanged in behavior; the `Frames` scanner takes the adapter as a parameter so the SSE loop stays protocol-agnostic.
- Implementation notes: contract `openapi.yaml` 0.19.0 first — `ChannelRequest` gained an optional `protocol`, `Channel` answers it. `seed_channel` writes `openai` explicitly. Pinned by `adapters.rs`'s unit tests (registry lookup, the OpenAI shape end to end, clamping, refusals), the channels suite's protocol read/write checks, the admin suite's `VALIDATION_ERROR` on an unknown protocol, and the unchanged gateway usage-row test proving the delegation is behavior-preserving. Anthropic lands in P5-1 as a second `UsageAdapter` implementation plus a `messages` surface, not another channel flag.

## 2026-10-05 The usage record lives in mutable store beside the ledger (issue #102)

- Status: Adopted. Implemented 2026-10-05, closing issue #102 (roadmap P1-1).
- Background: P1-1 expands the two-count `Usage` into the normalized `UsageRecord` of D3 — the full dimensions pricing looks at, plus the `usageDetails` escape hatch — and the record has to persist somewhere. The settlement description is hashed and immutable, and two of the record's fields are mutable-store data by design: `end_user` must be pseudonymizable when an organization is deleted, and `provider_raw` must be nulled after the retention window because it may carry prompt fragments. Neither can live inside a hashed entry.
- Decision: `oxsum.usage_records` (migration 0007) is the record's home, written by whichever settlement path landed — the turn itself, or the sweeper for a timed-out hold — keyed on `request_id` with `ON CONFLICT DO NOTHING`, so a replay writes nothing, like the settlement it describes. The entry stays the source of truth for money; a missing row is drift for the reconciler (P4-1), never a reason to retry a charge. The sweeper's watch row carries the caller's attribution, so a swept settlement's row still says whose turn it was. Two smaller rules land with it: the request-side attribution fields (`user`, `metadata`, `service_tier`) are bounded and refused past the bound rather than truncated, because a silently truncated id would bill under the wrong name; and a reported subset count past its total is clamped rather than rejected, because upstream's counts are still better than an estimate.
- Implementation notes: `Usage` became `UsageRecord` (`crates/core/src/usage.rs`), priced through the same `Price::cost_minor`; the OpenAI wire shape is read into it in `crates/server/src/gateway/relay.rs`, where `prompt_tokens` already includes the cached subset — the Anthropic adapter's additions are P5-1's. Contract `openapi.yaml` 0.18.0 first: `ChatCompletionRequest` gained `user`, `metadata` and `service_tier`. Pinned by `usage.rs`'s own tests, the request- and relay-parsing tests, `crates/server/tests/gateway.rs`'s settled-row check, and `sweep.rs`'s swept-row and replay checks.

## 2026-10-03 The admin pages carry the operator token, not a session (issue #57)

- Status: Adopted. First page shipped 2026-10-03 (channels and prices); the remaining pages land under #57.
- Background: docs/product.md lists five platform-admin pages. The dashboard's pages are session pages — a cookie, server functions, role checks — and none of that fits the platform admin, whose credential is `OXSUM_ADMIN_TOKEN`: an operator token, not a person and not a member of anything. A page behind the session would invent a login the role does not have.
- Decision: `/admin` is its own surface. The token is typed into a gate form once, proven against a real call (`GET /api/v1/admin/channels`) before it is remembered, kept in `localStorage` beside the chat page's key, and sent as `Authorization: Bearer` on direct browser `fetch`es to the admin endpoints — the same calls a `curl` makes. A `401` forgets the token and reopens the gate, which is also what a deployment with no `OXSUM_ADMIN_TOKEN` looks like from the page: every call refused. SSR renders the gate and the page skeletons; the data always arrives with the browser, because the server never holds the token.
- Why: the endpoints already exist and were built to be driven ("the endpoints are the ones the dashboard will drive"); a parallel set of server functions taking `token` as an argument would re-implement the admin middleware's check a second time and put the credential through the server-function codec for no gain. `localStorage` rather than a cookie keeps the token out of every unrelated request the browser makes — including the dashboard's — and out of the server's reach entirely.
- Rejected:
  - **A server function per endpoint taking the token as a parameter**: the functions would do nothing but forward to the same checks, and every call would put the token through an extra hop it does not need. The pages it adds are duplicates of routes that already answer.
  - **A session or a cookie for the operator**: the platform admin is whoever deploys oxsum, and modelling them as a user was rejected when the token was chosen — a cookie would smuggle that identity system back in, HttpOnly or not.
  - **Reading the token from `?token=`**: a credential in a URL lands in browser history and any referer that follows it; the form keeps it out of both.
- Implementation notes: `crates/web/src/admin.rs` holds the layout (nav plus the gate), the channels and prices page, and the hydrate-only fetch module that attaches the token; `crates/server/tests/admin.rs` pins the served gate. The endpoints themselves are unchanged — no contract entry, because the page is a client of the contract, not a new term in it.

## 2026-10-03 Membership management is a person's action (issue #56)

- Status: Adopted. Implemented 2026-10-03, closing issue #56.
- Background: docs/product.md promised that owners and admins invite, remove, change roles and transfer ownership from the members page; what shipped was a read-only email/role/joined table with no role check and no membership write endpoint at all. #61 deliberately kept the role rule out of the read because there was nothing to manage yet. Closing #56 means both halves: the four actions, and the role checks that keep them to owners and admins.
- Decision:
  - **The four actions, contract first.** `crates/server/openapi.yaml` 0.8.0 defines `POST /api/v1/org/members` (add an existing account by email), `PATCH`/`DELETE /api/v1/org/members/{userId}` (change a role, remove the membership) and `POST /api/v1/org/ownership` (transfer ownership), with the rules and statuses written out in docs/api.md, and the implementation follows in `crates/core/src/orgs.rs` and `crates/server/src/routes.rs`.
  - **A session is required, in the owner or admin role.** This is the one place a key is refused although #17 has keys acting as the whole organization: a key is not a person and names no role, so a key managing memberships would let a credential escalate the organization's own privileges — add an accomplice, promote it to admin, hand ownership away — with no person accountable for any step. An owner who wants to manage memberships logs in. Reading the list stays open to every member: who is in the organization is not a secret from its own members.
  - **Ownership is transferred, not assigned.** The role-change body takes only `admin` or `member` (an enum in the contract), so promotion to owner is not a request the API can express; `POST /api/v1/org/ownership` is the only way there, and it does both halves in one transaction — the target becomes the owner and the acting owner an admin — so exactly one owner exists before, during and after.
  - **The privilege gaps are closed in the rules, and the page does not offer what the rules refuse.** An admin may not remove or re-role an owner (`403 FORBIDDEN`: the request is refused for who is asking), and the organization's last owner can be neither removed nor demoted (`409 CONFLICT`, "transfer ownership first") so the owner seat is never empty. The last-owner guard is count-based and in practice reachable only by the sole owner acting on themselves, since an admin cannot touch an owner at all. The members page computes the same two conditions from the role and the member list and renders no control that would fail.
  - **"Invite" means adding an account that already exists.** #56 was scoped down rather than growing a pending-invitation flow: an email nobody has registered is `404 NOT_FOUND` naming the address, and nothing is stored, so the page can say what was wrong instead of pretending a stranger was invited. Pending invitations, tokens, email and the accept flow stay in #59, and the response, docs/api.md and docs/product.md all say so.
  - **Adding the second membership flips the organization from `personal` to `team`**, one way and automatically — the migration's own comment promised it — and the ledger does not migrate, because it belongs to the organization rather than to the person.
  - **Naming someone who is not a member is 404** for remove, re-role and transfer, and a role change to the role already held is an idempotent 200: a second removal cannot look like it worked, and a no-op need not be an error.
- Rejected:
  - **Letting an API key manage memberships**, the alternative that stays consistent with #17: it would make the strongest credential the least accountable one, and the escalation path (a key adds an account, promotes it to admin, transfers ownership) completes without a single confirmation from a person.
  - **404 instead of 403 for a caller below the role bar**: hiding the surface from a member would report authorization as absence. The endpoints are documented; the refusal states the rule it broke.
  - **409 instead of 403 for an admin acting on an owner**: an owner may do exactly that to another owner, so the state does not forbid it — the caller does.
  - **Accepting `owner` in the role-change body and refusing it in a rule**: assigning ownership would then be representable, and the contract would have to say when it is allowed. An enum of two has no such case.
  - **Building invitations under #56** (a pending-invitation table, tokens, an accept flow): most of it cannot ship in v1 at all, since v1 sends no email; it is #59's feature, and this change would have hidden it behind a half-built flow.
- Implementation notes: the rules live in `crates/core/src/orgs.rs` (`MembershipActor`, `Db::add_member`, `remove_member`, `change_member_role`, `transfer_ownership`, and the `add_member` helper the signup path shares), with `WalletError::NotFound` added in `crates/core/src/error.rs` so a refusal can carry a message a page may show, and `Principal::membership_actor` in `crates/core/src/sessions.rs` turning a session into the acting person. The server maps the new variant to `404 NOT_FOUND` (`crates/server/src/routes.rs`, `crates/server/src/error.rs`). The page (`crates/web/src/api.rs`, `crates/web/src/app.rs`) got one server function per action plus the controls, in the existing visual language. Pinned by `crates/server/tests/members.rs`, twelve tests over the whole matrix — owner, admin, member, another organization's people, an API key — each action's happy path and every refusal, including the page's own server functions and the rendered controls. No ledger change and no migration.
## 2026-10-03 The requests page reads the ledger (issue #55)

- Status: Adopted. Implemented 2026-10-03, closing issue #55.
- Background: docs/product.md promised a requests page — each request's status, usage and cost, filterable by key and by model — and described the table it would read: "The full request state (in flight, settled, anomalous) will live in oxsum's own `requests` table — planned, not built". That table does not exist, and `crates/core/migrations/0003_open_holds.sql` already says the watch table is superseded by it "when it arrives". The issue's completion criteria ask for the page "built on existing endpoints" instead, so the choice was between adding the promised table (a migration, a write path in the gateway and the sweeper, and a backfill for requests already settled) and reading what a settled request already leaves behind.
- Decision: the requests page reads the organization's ledger through a new `Wallet::recent_requests`, and adds no table. A settled gateway turn already *is* a settlement entry: its description carries the request id, the model, the token counts and both prices, and the entry's content hash covers all of it, so a row on the page is exactly what its bill proves. `SettlementRecord` reads that JSON back in the module that writes it; `recent_requests` walks the log backwards until it has the page's hundred records, and resolves the key that paid through the hold's provenance actor, which `settle` copies onto the settlement. In-flight turns keep their existing home: the `oxsum.open_holds` watch list, shown live on the overview.
- Why: the numbers on the page are then the numbers the user verifies, with no second store to keep in step and no way for the page and the bill to disagree — the failure mode a hand-maintained request row has by construction. It also needs no backfill and no gateway or sweeper write, so the page lists requests that settled before it existed. The ledger is already the source of truth for holds and settlements (product.md: only what needs proving goes into it), and every field this page shows is a provable fact.
- What the page does not show, by the same reasoning: a turn still in flight (it has no settlement entry yet; the overview lists those live), and a settlement written through the wallet API (it carries no description, so there is nothing to name a model or a token count).
- Rejected:
  - Building the `requests` table and writing a row per gateway turn: a migration, a write on the hold path and one on every settlement path, plus the sweeper's own settlements, and a backfill for the requests already in the log — all to hold facts the ledger already holds, with a new chance for the table and the bill to disagree.
  - Parsing the entry descriptions in the page's server function over `Wallet::recent_entries` as it stands: that read caps at a hundred *entries*, and a gateway turn costs two (the hold and the settlement) before top-ups and hand-driven holds, so the page would list a few dozen requests and would re-implement the log walk that belongs in the read. The record's shape belongs next to its writer either way.
  - A second SQL query over the entries' descriptions: descriptions are `text` inside the ledger's own tables, which `crates/server/AGENTS.md` puts out of bounds for hand-written SQL, and the ledger's paging API already answers the question honestly.
- Implementation notes: `SettlementRecord` (`crates/core/src/billing.rs`) is the reader of `Settlement::description`, with `a_settlement_description_reads_back` and `only_a_settlement_description_parses_as_a_record` pinning the round trip and the refusals; `Wallet::recent_requests` (`crates/core/src/wallet.rs`) scans 512-entry pages until it has a hundred requests; `RequestView` and `RequestFilters` (`crates/web/src/requests.rs`) are the page's row shape and its URL query string; `get_requests` (`crates/web/src/api.rs`) resolves the key prefix through `list_keys` under the session's `key_scope`, so the member rule holds without a second role check; `crates/server/tests/requests.rs` pins the payload, the filters and the scoping. docs/product.md records the page as shipped.

## 2026-10-03 The wasm name is set for plain cargo builds too (issue #65)
## 2026-10-03 The bills export is two page GET routes, and a settled entry is what the ledger released (issue #54)

- Status: Adopted. Implemented 2026-10-03, closing issue #54.
- Background: product.md promised a bill page with CSV and JSON export carrying each entry's `contentHash`; nothing existed — no `/dashboard/bills` route, no export, and no read of the ledger that carried a settled entry's charge and date together. The transaction log page (`Wallet::recent_entries`) lists every entry with its content hash, but says neither when an entry was booked nor what it charged, and it lists holds and top-ups beside settlements.
- Decision:
  - **A settled entry is the entry that released a hold**, and the ledger itself says which those are: `Wallet::settle` always credits the wallet account in the pending layer — the mirror of the pending debit a hold takes — and no other write does (`crates/core/src/wallet.rs`, `is_settlement`). The charge is that entry's net debit on the wallet account in the settled layer, so the number comes from the books, and it is 0 exactly when the settlement charged nothing (an upstream error, an unreachable upstream, a swept hold).
  - **Identifying settlements by their description would have been wrong**, not merely clever: `POST /api/v1/settlements` takes a hold key and an amount and records no description at all (`crates/server/src/routes.rs`), so a page that parsed the gateway's settlement record would have listed gateway turns and silently omitted every settlement made through the wallet API. For the same reason the charge is not read from the description's `charged` field: an archive must not depend on a memo agreeing with the postings.
  - **`Wallet::settled_entries(limit)` is the one read** behind the page and both exports: the newest settled entries, newest first. It walks back one page of the log at a time and stops as soon as it has `limit` bills, so a busy log costs no more than the entries above the last bill it returns. Each entry carries the id, the booking date — the only time the ledger records; there is no recorded-at timestamp — the charge in minor units and the content hash.
  - **One row type, `oxsum_web::bills::BillView`**, is the page's table, the CSV row and the JSON object: `bookedOn`, `entryId`, `chargedMinor`, `contentHash`, in that order. The two exports cannot drift from each other or from the page because there is one declaration of the fields, and the page's server function and the export routes use that same type.
  - **The export is two GET page routes** — `GET /dashboard/bills/export.csv` and `GET /dashboard/bills/export.json` — answering with `Content-Disposition: attachment` and a fixed filename (`crates/server/src/bills.rs`). That is the browser's own download: the page's two links need no script of ours, and a command-line client fetches the same bytes with the same session cookie. They sit on the dashboard's own surface, like `/ws/billing`, and not under `/api/v1`, so `openapi.yaml` and docs/api.md are untouched and the credential is the session cookie alone. A missing or dead session answers 401 rather than a redirect: this is a download, and the browser has nowhere to send a reader next. The filename is ours, never the organization's name — a caller-supplied value in a header is header injection.
  - **The exports carry money as an integer in minor units**, and the page renders that same integer in credits: `BillView::charged_minor` is never formatted in the payload, because the archive has to be checkable against the ledger as it stands, while the table's **Cost (credits)** column goes through the dashboard's `credits()` like every other amount in the product, so the page does not read as a second currency. One amount, two renderings. The CSV is RFC 4180, quoting a field only where a value needs it — which, for the four fields that exist today (a UUID, an ISO date, an integer, a hex hash), is never; the rule is pinned by a unit test with the values that would need it rather than by a row that happens to be simple.
  - **The page lists a hundred transactions at a time and the exports list them all** (`oxsum_web::bills::BILLS_LIMIT`, issue #93): the page paginates on the log index — a slice of history — while an export is an archive, so it walks the same read to the log's start.
- Rejected:
  - **A `/_pages` server function returning the payload**: the browser would receive the string, build a blob and click itself, which is the same download with more moving parts, requires the wasm bundle to be loaded, and leaves the export unreachable without it.
  - **A new `/api/v1` endpoint per format**: it would put a session-only, file-shaped route inside the REST contract, and its field definitions would have to be mirrored in `openapi.yaml` for a client that is a browser saving a file.
  - **Turning the transaction log page into the bill page**: the log is the append-only record of everything (holds included) and is the page a user reads to follow a turn; the bills page is the settled, archivable subset. Merging them would have made the log's content-hash column mean two things.
  - **Quoting every CSV field**: valid CSV, but it makes every row harder to read for no gain, and it hides the escaping rule instead of pinning it.
  - **Filtering the list by member role now**: product.md wants members to see only their own requests, but a session's hold carries no key attribution, so a role filter would empty the page for exactly the sessions the page belongs to. It stays planned with the rest of #54.
- Implementation notes: `crates/core/src/wallet.rs` (`SettledEntry`, `settled_entries`, `is_settlement`, `settled_charge`), `crates/web/src/bills.rs` (the row type, both exports and their unit tests), `crates/web/src/api.rs` (`get_bills`), `crates/web/src/app.rs` (the `/dashboard/bills` page and its nav entry), `crates/server/src/bills.rs` (the two download routes) wired in `crates/server/src/routes.rs`, and `crates/server/tests/bills.rs`: the page lists both settlements and neither the hold nor the top-up, the CSV's header and rows (money an integer, newest first, nothing quoted), the JSON's field names and integer money, the two exports row for row, the 401s without a session, and one organization never seeing another's bills.

- Status: Adopted. Implemented 2026-10-03, closing issue #65.
- Background: `cargo run -p oxsum-server` — the command AGENTS.md and docs/development.md tell a developer to use — served pages that never hydrated. leptos decides the wasm file name when *leptos* is compiled: `HydrationScripts` appends `_bg` unless `LEPTOS_OUTPUT_NAME` is set (`leptos-0.8.21/src/hydration/mod.rs`), and only `cargo leptos build`/`serve` set that variable — for the wasm half and the server half alike. The site, in turn, is written by cargo-leptos, which renames wasm-bindgen's `oxsum_bg.wasm` to `oxsum.wasm` "for backward compatibility with leptos' `HydrationScripts`". So a server built by cargo-leptos (and the release image, which builds that way) is consistent, and a server built by plain cargo is not: its markup asked for `/pkg/oxsum_bg.wasm`, the site holds `pkg/oxsum.wasm`, the module import 404'd and every page stayed exactly as the server had rendered it. No router-level test could see it, because every route the tests asked answered 200 — the same shape as issues #44 and #46.
- Decision: `.cargo/config.toml` sets `LEPTOS_OUTPUT_NAME` for every cargo invocation in the workspace. Only the variable's *presence* matters to leptos — the name in the URL still comes from `output-name` in `[[workspace.metadata.leptos]]` — so the value only has to say that this workspace is built the way cargo-leptos builds it. A server built by plain `cargo run` then asks for the file cargo-leptos writes, and hydration works on the documented development path.
- Why the build configuration rather than the site service: the mismatch is a *compile-time* name, so it is fixed where the name is decided. Cargo tracks `option_env!` reads, so the leptos crate is rebuilt when the value changes (verified: setting the value, changing it, and removing it each changed what `option_env!` saw, with a rebuild each time). The workspace value matches the one cargo-leptos sets, so the name no longer changes with the way the server was built (the two paths still compile different unit graphs for their own reasons: cargo-leptos passes `--no-default-features` and its own rustflags). The one case this does not cover is a site built by hand instead of by cargo-leptos: `cargo leptos build` is the documented producer of `target/site`, and it always writes `oxsum.wasm`.
- Rejected:
  - Resolving `pkg/oxsum_bg.wasm` to the built `pkg/oxsum.wasm` in the `/pkg` service: it would leave the server rendering a URL that no file on disk has, for a name no build writes, and every plain-cargo build would keep asking for the `_bg` module — the mismatch would be papered over at serve time instead of removed. A rename of `output-name` would leave the alias behind.
  - The variable on the documented command instead (`LEPTOS_OUTPUT_NAME=oxsum cargo run -p oxsum-server`): it fixes that one line and nothing else — `cargo test` and every other cargo invocation keep the `_bg` name, so the test that pins the contract would not see what a developer sees, and the build becomes environment-dependent in a way a developer has to remember.
  - Documenting `cargo leptos serve` as the only way to browse a hydrated site and dropping the `cargo run` instructions: honest, but it makes the fastest development loop serve a page that cannot hydrate when the fix is one variable; the release image and `cargo leptos serve` were never broken, so the documentation was describing the intent correctly.
- Implementation notes: `.cargo/config.toml` (new file, `[env] LEPTOS_OUTPUT_NAME`), pinned by `the_shell_asks_for_the_wasm_file_the_built_site_holds` in `crates/server/tests/dashboard.rs`: it renders `/login` through the real app over a pool that never connects, asserts the markup names `pkg/oxsum.js` and `pkg/oxsum.wasm` and never a `_bg` module, and, when a site is on disk, fetches the module the markup names and requires 200. The test binary is itself a plain-cargo build, which is what makes the assertion cover the command a developer actually runs. Verified against a built site, before and after: the pre-change server's `/login` named `/pkg/oxsum_bg.wasm`, which answered 404, and the post-change server names `/pkg/oxsum.wasm`, which answers 200 with the module's bytes. docs/development.md documents the variable and the contract. No interface, contract or ledger change.

## 2026-10-03 product.md honesty: the shipped behaviour wins in three places (issue #61)

- Status: Adopted. Implemented 2026-10-03, closing issue #61.
- Background: an audit of docs/product.md against the code found the document describing a larger product than the one that exists — five platform-admin pages, a bills page with exports, a requests page, membership management, team organizations, invitations, the signup bonus and a browser-local tree-head archive, none of which is built. Each of those now has its own issue (#54 the bills page, #55 the requests page, #56 membership management, #57 the platform-admin pages, #58 team organizations and switching, #59 invitations, #60 adjustments and the signup bonus), except the tree-head archive, which has none yet; docs/product.md marks them all as planned. This entry records the three places where the document contradicted behaviour that *is* built; those statements now describe the code.
- Decision:
  - **Self-service top-up on the session is the v1 behaviour.** `POST /api/v1/topups` sits in the `authenticated` router (`crates/server/src/routes.rs`), behind the same credential check as every other organization endpoint: a session cookie or an API key may credit its own organization's balance, and the handler takes an amount and an idempotency key with no description, so no reason is recorded. The chat page's top-up form (`crates/web/src/chat.rs`) is exactly this call, which is why it works with no operator in the loop.
  - **The member list is readable by every member; management is planned.** `get_members` (`crates/web/src/api.rs`) authenticates the session and returns the organization's members with no role check, and the page renders a read-only table (`crates/web/src/app.rs`): reading who is in the organization is not a management action. Invite, remove, change roles and transfer ownership are planned under #56, and those actions will carry their own role checks.
  - **The overview shows money and characters, not tokens.** It shows the available balance, each in-flight request's frozen upper bound and live progress in forwarded characters (`crates/web/src/app.rs`), and there is no live token count on it; the frozen total and the month-to-date spend this entry then called planned ship with issue #53, as ledger reads beside the balance. Tokens cannot be a live number: upstream reports usage only in the final chunk of a streaming response, so before settlement the page would be presenting an estimate as a measurement. Characters are what has actually arrived.
- Rejected:
  - **Admin-gated top-ups with a required reason** (the operator token plus a description on every credit, product.md's original credit-source table): it would make the chat page's top-up button depend on an operator, and it demands a reason where there is none — the organization is moving its own money in. A reason belongs on an operator's change, which is the shape the planned adjustments (#60) will take.
  - **Gating the member list to owners and admins**: it would add a role rule to a read-only page whose management actions do not exist yet (#56), and the shipped transaction log is already readable by every member.
  - **Printing live token progress on the overview**: upstream sends usage only with the final chunk, so any live token count would be an estimate shown as a measurement; if the overview ever shows tokens, it will be at settlement.
- Implementation notes: documentation only — `docs/product.md` (every page and flow marked shipped or planned, and the three statements corrected), `TODO.md` (the planned items as phase D under Next; this change under Recently completed). No code, no `openapi.yaml`.

## 2026-10-03 Chat page: the browser chats with an API key, bills through a page server function (issue #39)

- Status: Adopted. Implemented 2026-10-03, closing issue #39 (TODO C-11).
- Decision: `/dashboard/chat` drives the existing API only — no new REST endpoints, no `openapi.yaml` change.
  - **Top-up** goes to `POST /api/v1/topups` from the browser with the session cookie (the login page's pattern — the cookie is `HttpOnly`, so only a real endpoint call works). The amount is parsed to minor units without floating point, the same integer-only rule as the rest of the money path.
  - **Chat goes through the gateway with an API key.** The cookie is never accepted on `/v1` (#17), so the page mints a key named `chat` through the existing `create_key` server function and keeps it in the browser's `localStorage` — the user never handles it by hand. Pasting a key from the keys page works too. This revises product.md's "no API key needed": what the user experiences is still "log in and chat", but the credential on the wire is a key, not the session.
  - **Chat is streaming `POST /v1/chat/completions`**, parsed as SSE in the browser; the `x-oxsum-request-id` header names the turn before the first frame.
  - **Billing is the existing `/ws/billing` socket**, now behind a shared client module (`crates/web/src/billing_socket.rs`) that delivers typed events to a caller-supplied handler — the dashboard's holds section and the chat page share it. Freeze on `turnStarted`, streaming progress on `turnProgress`, and on `turnSettled` the new `get_turn_bill` `/_pages` server function returns the settlement entry's proof bundle plus the parsed charge and the content hash.
  - **Each settled turn links to `/verify?bundle=…&contentHash=…`**, which prefills both fields and runs the check on load (a page-level change, not an endpoint).
  - **Conversations persist in `localStorage`**; the server keeps nothing (product.md). Restored turns re-fetch their bills — the entry ids are derivable from the request ids, so a reload loses nothing but the live socket state.
- Why a server function for the bill, not a browser fetch of the proof endpoint: the bundle does not carry the entry's content hash (the entry serializes without it), and the browser cannot recompute it — the hash runs over the canonical entry bytes inside doubleentry (`digest` is crate-internal), and `crates/doubleentry` is untouched by rule. `get_turn_bill` (session-authenticated, page API under `/_pages` like the rest of the dashboard) derives the settlement entry id with `oxsum_core::settlement_key_for`/`entry_id_for`, builds the proof bundle server-side, and returns the parsed charge with the bundle JSON and the content hash for the `/verify` link.
- Rejected:
  - A session-authenticated chat relay endpoint: would punch a hole in #17's gateway auth model ("the cookie is never accepted on `/v1`") for page convenience.
  - Deriving the settlement entry id in the browser and fetching the proof over REST: workable for the bundle, but leaves the page with no content hash for the verify link.
  - Storing conversations server-side: product.md says the server keeps nothing, and there is no duty to store users' conversations.

## 2026-10-03 Per-key spend limits: committed spend, serialized per key (issue #32)

- Status: Adopted. Implemented 2026-10-03, closing issue #32 (TODO B-8).
- Background: API keys have full authority (#17: roles constrain sessions, not keys), so one key could spend an organization's whole balance. product.md deferred per-key spend limits to post-v1, and the gateway's freeze discipline made them the natural next step: a limit that only bound settled spend would be after-the-fact interception — the exact failure this file criticizes in OpenAI's limits.
- Decision:
  - **Storage: `oxsum.api_keys.spend_limit_minor`** — nullable bigint, `CHECK (spend_limit_minor >= 0)`, migration `0005_key_spend_limits`. NULL is unlimited, so every existing key is unaffected; 0 means the key can never hold. A limits table would be a 1:1 join for no reason: exactly one limit per key, with the key row's lifetime.
  - **What it limits: committed spend** — settled charges attributed to the key **plus** outstanding holds attributed to the key, in minor units. A hold of `m` is refused when `committed + m > limit`. A pure hold cap lets cumulative spend blow past the limit; a pure spend cap lets in-flight freezes exceed it, and the freeze-before-spend discipline is the product's whole pitch.
  - **Attribution: `provenance.actor`** — the key id in uuid simple form. Hold entries carry it; `Wallet::settle` copies the hold's actor onto the settlement entry (it already reads the hold entry for the held amount), so the release side and the settled charge side attribute to the key with no extra plumbing: the sweeper, the gateway and the wallet API all attribute for free. Keys with no limit attribute too, so a limit added later counts history.
  - **Enforcement: `Wallet::hold_for_key`**, serialized per key around the engine append, with no engine change. It begins a transaction, takes `pg_advisory_xact_lock` on a stable hash of (tenant, key id), re-reads the limit from the key row `FOR UPDATE` (the value in force now, pinned against a concurrent limit change), reads committed spend from the ledger — postings joined to entries on the actor, wallet account, both layers — and only then appends through the engine's normal path. The lock is held until commit, i.e. until after the engine's append committed, so a racing second hold for the same key can only read usage that already includes the first hold. Two racing holds cannot both pass the check.
  - **Lock order is always key-lock, then the engine's per-tenant append lock**, and the engine never takes the key lock: no deadlock. Per-key serialization adds no tenant-level contention beyond the append lock that already serializes the tenant.
  - **Refusal: `WalletError::KeyLimitExceeded`** — 429 `KEY_LIMIT_EXCEEDED` on `/api/v1`, and on the gateway as `insufficient_quota` in the OpenAI shape. A quota refusal, not a balance one: 402 stays the wallet's.
  - **Management: `spendLimitMinor`** on key creation, and `PATCH /api/v1/org/keys/{keyId}` to set or clear it, under the revoke scope rules (members: keys they created; owners, admins and keys acting as the organization: all; anything else 404). Sessions carry no limit — the wallet hold route enforces only for key principals.
- Rejected:
  - **An engine-side limit** (a `BalanceLimit` variant carrying an amount): it would change the account record's canonical encoding and invalidate the content hash of every stored account, for what oxsum can do outside the engine.
  - **Read-then-write in `Wallet::hold` without the lock**: the exact race the `FundedReservations` decision documents — two requests both read pre-append usage and both commit.
  - **An oxsum-side holds table**: duplicates ledger state that can drift, and would need the same lock to be honest.
  - **A DB CHECK on postings**: the per-key total is an aggregate over attributed postings, not a column.
- Implementation notes:
  - `crates/core/src/keys.rs` (`ActingKey`, `update_key_limit`), `crates/core/src/wallet.rs` (`hold_for_key`, `key_committed`, the settle actor flow, the lock-key hash), `Principal::Key(KeyPrincipal)` carrying the acting key, `crates/server/src/auth.rs` resolving it on both middlewares.
  - The concurrency test races ten 300-minor holds against a 1000 limit: exactly three succeed and 900 is committed — deterministic under the serialization.
  - One in-flight `hold_for_key` transiently needs two pool connections (the outer transaction plus the engine's append inside it); `OXSUM_DB_MAX_CONNECTIONS` should stay comfortably above peak concurrent key-holds.

## 2026-10-03 Trustworthy tree heads: operator-signed heads via doubleentry's witness module (issue #30)

- Status: Adopted. Implemented 2026-10-03, closing issue #30 (TODO B-7).
- Background: inclusion proofs answer everything about the history a user was *shown*; they cannot answer whether it is the history everybody else was shown. Each guarantee in the ledger is relative to a head, and a user who archived an old head had no way to check that today's head extends it — the server could serve two heads to two users, each with perfectly verifying proofs.
- Decision:
  - **The operator signs each tenant's ledger head** as a C2SP `signed-note` (a `tlog-checkpoint` body) with an Ed25519 operator note signature (algorithm `0x01`, `SigningKey::sign`) under the fixed key name `oxsum/tree-heads`, using doubleentry's witness module as a dependency. Each tenant's ledger is its own Merkle log, so the origin names the tenant: `oxsum/ledgers/<tenant_id>`.
  - **Signing is on demand and stateless.** The note attests the head at signing time; nothing is stored, so rotating the key rotates every head with no migration. A write landing mid-request leaves the answer one entry behind the absolute latest — still a true statement about the head it attests.
  - **No server-side `Witness`.** The state machine is for independent parties, and the operator witnessing its own log would prove nothing. The C2SP wire format is the interop point: third-party witnesses can cosign these notes later, with software that is not this crate's.
  - **Key management: `OXSUM_HEAD_SIGNING_KEY`**, 32 bytes base64 like `OXSUM_SECRET_KEY` (generate with `openssl rand -base64 32`). Unset means the `/api/v1/log` endpoints answer 503; the wallet serves without it. The verifying key is published at the public `GET /api/v1/log/key` — key name, base64 public key, and the 4-byte signature selector. Fetching it from the server is convenience: a verifier must have chosen the key through a channel the operator does not control, or the signature proves only that the server agrees with itself.
  - **Endpoints:** `GET /api/v1/log/head` (org credential: the current head, signed), `GET /api/v1/log/consistency?from=<size>` (org credential: the signed new head, the old head recomputed from the log, and the proof between them), `GET /api/v1/log/key` (public). `from=0` is 400 — every log extends the empty tree, so such a proof would verify against any history; `from` beyond the log is 400; `from == size` answers the trivial empty proof.
- Rejected:
  - **A `tree_heads` table of stored signed heads.** The ledger is the record; signing on demand is one Ed25519 sign per request and cannot drift from the log it attests.
  - **Cosignatures (algorithm `0x04`) from the operator.** A cosignature says "an independent party had seen this history by time T"; the operator cannot be its own witness, and a self-cosignature would be theater.
  - **Refusing to boot without the key.** The wallet is useful without signed heads; the admin-token precedent (unset means the surface stays closed) fits — 503 names the missing variable instead.
- Implementation notes:
  - `crates/core/src/heads.rs` (origin, signing, key publication), `Wallet::{signed_head, consistency}`, thin routes in `crates/server/src/routes.rs`, `ApiError::ServiceUnavailable` → 503 `SERVICE_UNAVAILABLE`.
  - `crates/doubleentry` is untouched; the `witness` cargo feature is enabled on the workspace dependency.
  - Contract first per the hard rules: openapi.yaml is at 0.6.0 with the three endpoints and the new schemas.

## 2026-10-03 Bill verification in the browser via a shared `oxsum-verify` crate (issue #28)

- Status: Adopted. Implemented 2026-10-03, closing issue #28 (TODO B-6).
- Background: the verification page must run `verify_bundle` in the browser (WASM) — the component calls the same code the server runs, directly, with no round-trip. But `oxsum-core` cannot target `wasm32-unknown-unknown`: it depends on sqlx-postgres with the tokio `net` runtime, which needs OS sockets.
- Decision:
  - **New crate `oxsum-verify` for the pure verification half.** `ProofBundle`, `verify_bundle`, and the money `SCALE` the entry encoding depends on move into `crates/verify`, whose only dependencies are doubleentry (a path dep with the `serde` feature only — the workspace entry's `postgres` feature would pull sqlx and tokio-net back in), serde and serde_json. All three are verified wasm32-clean.
  - **`oxsum-core` re-exports, nothing else changes.** `proof.rs` becomes a re-export shim and `wallet::SCALE` re-exports `oxsum_verify::SCALE`, so `oxsum_core::{verify_bundle, ProofBundle, SCALE}` keep resolving for the server and every existing caller. One definition of the scale: the writer and every verifier cannot drift.
  - **A public `/verify` route, no login.** Verification is the trust surface for anyone holding a bill; a session gate would defeat the point. SSR renders the inert form; the check runs on submit in the browser. The dashboard sidenav links it next to the transaction log.
  - **The verdict is icon plus words, never color alone** (DESIGN.md); a bundle that does not parse and a hash that is not 64 hex characters get their own error states, distinct from verification failure. The page states product.md's honesty sentence verbatim.
- Rejected:
  - **Feature-gating `oxsum-core` for wasm.** Making sqlx, tokio and the rest optional behind a feature would thread `#[cfg]` through the whole domain layer for one pure function; the extraction is smaller, and the boundary (I/O-free) is honest.
  - **Duplicating the check in the web crate.** Two implementations of the verification would drift; the issue requires the same code the server runs.
  - **Verifying through a server function.** The check would then depend on trusting the server — the party being checked.
- Implementation notes:
  - `crates/verify/src/lib.rs` carries a tamper unit test over a golden bundle fixture captured from a real ledger: the fixture verifies, the same fixture with one amount digit changed does not.
  - `docs/development.md` ("Web dashboard") and `docs/user-guide.md` ("Verifying a bill") describe the page; TODO.md B-6 is closed.

## 2026-10-03 The dashboard is a Leptos 0.8 app in `crates/web`, served by the same binary

- Status: Adopted. Implemented 2026-10-03, closing issue #26 (TODO B-5).
- Background: the pages were decided for Leptos 0.8 earlier ("pages move to Leptos, replacing Topcoat"), and the session endpoints were built for pages that did not exist yet ("the Leptos login/logout pages stay with TODO 5"). This item builds them: `/login`, `/logout`, and `/dashboard` with the organization surface — members, keys, balances, in-flight holds and the transaction log — plus live billing progress over WebSocket.
- Decision:
  - **Versions: Leptos 0.8.21, `leptos_axum` 0.8.10, `leptos_router` 0.8.16** — the latest non-yanked 0.8.x line, staying on the major the project chose. `cargo-leptos` 0.3.11 builds both halves; it is installed on the build machine and touches nothing in the repo. Rust stays 1.98 (Leptos 0.8's MSRV is below it).
  - **One binary.** `crates/web` (package `oxsum-web`) is a standard cargo-leptos crate — `ssr` feature for the server side, `hydrate` for the browser side, code separated by feature — and the `oxsum` binary depends on it with the `ssr` feature, mounting it through `leptos_axum::LeptosRoutes` into the same axum router that serves `/api/v1` and `/v1`. The `[[workspace.metadata.leptos]]` section lives in the workspace root (a virtual manifest cannot carry `[package.metadata]`, and cargo-leptos 0.3 only honours `bin-package`/`lib-package` on the workspace section); plain `cargo build` and `cargo test` are unaffected because `ssr` is the web crate's default feature.
  - **SSR calls `oxsum-core` directly; the browser calls server functions.** There is no hand-written page API: the pages' data functions are `#[server]` functions under the `/_pages` prefix, and the database reaches them through Leptos context provided at mount time (`Db` and `Tenants`; the request's own parts, for the session cookie, are provided by `leptos_axum` itself). The `/_pages` prefix keeps the page API off the REST contract's `/api/v1` namespace, so `crates/server/openapi.yaml` stays the REST contract and gains nothing here.
  - **The login/logout pages call the session endpoints from the browser.** `POST /api/v1/auth/login`, `POST /api/v1/auth/logout` and `GET /api/v1/session` are called with `fetch` from the login/logout components: the session cookie is `HttpOnly`, so only a real call to those endpoints sets or clears it on the browser. The dashboard's session guard uses `GET /api/v1/session` the same way; the server functions independently refuse a missing or dead session.
  - **Live billing progress is a process-wide broadcast.** The gateway publishes `BillingEvent`s — turn started, streaming progress (every 1 KiB of forwarded answer text), turn settled with the charge and the kind — onto a `tokio::sync::broadcast` channel on `AppState`, keyed by tenant id. `/ws/billing` (session-cookie auth, like the pages) forwards the caller's organization events as JSON, starting with a snapshot of the holds currently in flight. No database table, no REST endpoint: the watch table stays the durable record, and a missed event is a missed live update, not lost state. The sweeper's own settlements do not publish — the row disappearing is what the dashboard shows.
  - **Three small reads in core, nothing in the engine.** `Db::open_holds_for_tenant` (one organization's in-flight holds), `Db::members` (memberships with emails and roles), `Wallet::recent_entries` (newest log entries with index, id, description and content hash). `crates/doubleentry` is untouched.
- Why:
  - **The `/_pages` prefix rather than the default `/api`.** Server functions at the default prefix would live beside `/api/v1` without colliding today, but the contract file claims the `/api` namespace and a future endpoint could take a name a page function already uses. A separate prefix makes the page API visibly not-the-contract.
  - **A broadcast channel rather than polling or a table.** Polling the watch table from the browser is a second read path with its own staleness; a table of events is durable state for information nobody replays. The events the gateway already knows — it takes the hold, watches the chunks, writes the settlement — are published where they happen, which is the smallest hook that satisfies "pushed in real time".
  - **Session-only WebSocket and server functions.** The dashboard is the session's surface: an API key names no session, and the key's authority (acting as the whole organization) does not map onto pages built around a user and a role. Keys keep using the REST API directly.
  - **The snapshot on connect.** A page opened mid-turn would otherwise show an empty holds list until the next turn starts; the snapshot is one indexed query on the watch table.
- Rejected:
  - **Server functions under `/api`.** The namespace argument above; the REST contract stays exactly the API's.
  - **Progress events per chunk.** A push per upstream packet is a push per few dozen bytes; every 1 KiB of forwarded text is live enough for a dashboard and bounded in rate.
  - **Publishing sweeper settlements.** The sweeper lives in core and has no access to the server's broadcast; threading it through would couple the background job to a page concern for an event the dashboard already shows as the row disappearing.
  - **Leptos 0.9.** A beta line mid-project, against the recorded 0.8 decision; 0.8.21 is the maintained line the decision named.
- Implementation notes:
  - `crates/web/src/app.rs` holds the pages, `crates/web/src/api.rs` the server functions and their view types; `crates/server/src/billing.rs` the event enum, `crates/server/src/ws.rs` the socket route, `crates/server/src/web.rs` the mount. The gateway hook is three publishes: `TurnStarted` in `gateway::run`, progress in `Turn::observe`, `TurnSettled` in `Plan::write` (which covers the stream, the whole-body and the `Drop` paths).
  - The dashboard is an admin/organization surface only: organizations, members, keys, balances, in-flight holds, the transaction log. Chat is TODO C-11 and is not built here.
  - `docs/development.md` gains the frontend commands (`cargo leptos build`, `cargo leptos serve`); `docs/user-guide.md` gains the dashboard section; DESIGN.md's token location is corrected to `crates/web/style/main.css`.

## 2026-10-03 Web login: a session is a second credential, and roles constrain sessions, not keys

- Status: Adopted. Implemented 2026-10-03, closing issue #17 (TODO B-4).
- Background: an API key names an organization and nothing else — a machine credential with no person behind it, which is why `api_keys.created_by` was always null and product.md's role table had a schema but no enforcement. This item makes a *user* a possible principal: a session table, a login, a logout, and role checks on the organization and key endpoints.
- Decision:
  - **Two credentials, one principal.** A request may carry `Authorization: Bearer oxs-…` or the session cookie; both resolve to a `Principal` — `Key` (the organization the key spends for) or `Session` (the user, the organization they act as, and their role in it). The middleware resolves once and hands the principal to the handler; nothing else in the request can name an organization.
  - **Roles constrain sessions, not keys.** A key keeps acting as the whole organization — full authority, pre-session behaviour deliberately unchanged — while a user acts with the authority their role grants them: members list and revoke only the keys they created, owners and admins see and revoke all. The reason is what the credential *is*: a key is a machine credential the organization issued to itself, so there is no person to hold to a role; a session is a person, and a person can be a member. Constraining keys by role would require inventing a person behind a credential that has none.
  - **The session table holds no usable credential.** The cookie value is `oxsess-` plus 32 random bytes; the database keeps only its SHA-256 hash (unique, so the lookup is one index probe), for the same reason `api_keys` keeps only key hashes: a key is 256 bits of machine randomness, so there is nothing to slow down with argon2, and every authenticated request pays for the lookup.
  - **Resolution joins `memberships` on `(user_id, organization_id)`.** A session stops authenticating the moment its membership is gone — the row cannot outlive the authorization it names. No second invalidation mechanism, no session list to sweep.
  - **Login is email + password, one failure for both.** An unknown email and a wrong password answer the same 401 with the same message ("invalid email or password"), and the unknown-email path still pays for one argon2 verification against a process-wide dummy hash, so neither timing nor wording reveals whether an account exists. The hash runs before any session row is written and outside any transaction, so a slow hash never holds a connection — the same rule registration follows.
  - **The session acts as the user's oldest membership.** Invitation flows that create a second membership are a later item, so today this is always the personal organization from signup; switching between several arrives with the dashboard rather than as a login parameter nobody can yet use.
  - **Thirty days, absolute.** `expires_at` is login plus thirty days, no sliding renewal in v1: a session that never expires is a credential that never dies, and sliding renewal is a second expiry policy to get right. `last_used_at` is touched on authentication at most once per five minutes, so the column stays meaningful without a write on every request.
  - **The cookie is `HttpOnly`, `SameSite=Lax`, `Path=/`, and `Secure` when the deployment says so.** Lax, because the dashboard is same-site and Lax is what keeps the cookie off cross-site requests without breaking top-level navigation to it; `OXSUM_SESSION_COOKIE_SECURE` (default `false` for local http development) is what a deployment behind TLS must set. The endpoints stay JSON — the cookie is only the credential, not a page contract — so the same API serves the dashboard and any script holding a session.
  - **An explicit bearer token wins over the cookie.** With no bearer credential at all the cookie is tried; a presented bearer is resolved as a key and never falls through. A wrong credential fails rather than getting a second chance, and the ambient cookie cannot override what the caller explicitly sent.
  - **The cookie is never accepted on `/v1`.** The gateway's credential stays the API key alone: a logged-in browser must not be able to spend through the gateway by ambient authority, and the gateway's error shape stays OpenAI's.
  - **Logout is an open route and always 200.** It revokes the session the cookie names (idempotent — twice is not an error) and clears the cookie with `Max-Age=0`. It sits outside the auth middleware precisely so that logging out twice answers 200 with no session at all.
  - **A key that exists but is not the caller's answers 404, never 403.** The member naming a key they did not create gets the same answer as a key that does not exist — and the key stays live — because the scope is part of the lookup, not a check after it. The organization boundary already works this way, so ids cannot be probed.
  - **`created_by` starts being set, and is exposed.** Keys minted through a session record the acting user; keys minted with an API key keep it null. The field is on the API response (nullable) so the rule is observable, not just a database column.
- Why:
  - **A hand-written session table rather than a token library.** The project already decided this for users and keys (docs/decisions.md, "users and login"): the table is four queries, the semantics (membership-bound, revocable, expiring) are the product's own, and a JWT would need a revocation list to support logout — at which point it is a session table with extra steps.
  - **SHA-256 for session tokens, argon2 for passwords.** The same split as API keys versus passwords, for the same reason: what is being guessed decides the hash, not which hash is "better".
  - **No session choice at login.** A `organizationId` parameter would be honest for multi-org users, but no flow creates a second membership yet, so it would be a parameter nothing can meaningfully vary today. The oldest-membership rule is recorded here so the dashboard item knows what it replaces.
- Rejected:
  - **Sliding expiry:** a second policy (idle timeout) beside the absolute one, with its own edge cases; the absolute expiry is the whole v1 story.
  - **Roles on API keys:** there is no person behind a key to assign a role to; the pre-session "any active key may manage keys" stays exactly as it was.
  - **A 403 for cross-member key access:** 404 keeps ids unprobeable, matching the organization boundary.
  - **Session auth on `/v1`:** non-goal per the issue; the chat page (TODO C-11) is what will need it, and it gets its own contract then.
  - **The Leptos login/logout pages:** `crates/web` does not exist until TODO B-5; a page written in the server crate now would be deleted there. This item delivers the endpoints those pages call.
- Implementation notes:
  - `crates/core/src/sessions.rs` holds the store, the token scheme and the `Principal`/`KeyScope` types; `crates/server/src/auth.rs` the two middlewares; `crates/server/src/routes.rs` the three endpoints and the cookie attributes; migration `0004_sessions` (the issue's `0003` was taken by #13's `0003_open_holds`).
  - `crates/core/tests/sessions.rs` and `crates/server/tests/sessions.rs` cover the completion criteria: the cookie attributes, the indistinguishable 401s, session/membership/expiry/revocation invalidation, the member/owner key rules, `createdBy` on both mint paths, the gateway refusing the cookie, and the table dump authenticating nothing.
  - `GET /api/v1/session` for a key principal is 404: a key names no session.

## 2026-10-02 Test-sealed channels are cleared with TRUNCATE, documented in the dev loop

- Status: Adopted. Implemented 2026-10-02, closing issue #19.
- Background: the gateway and admin integration tests write channels sealed with the tests' own key (`[7; 32]`) into the shared `oxsum` schema. A `cargo run -p oxsum-server` afterwards refuses to boot: `prepare` opens every stored credential at startup and names `OXSUM_SECRET_KEY` when one does not open, by design. The tests therefore leave the development database in a state the server rejects.
- Decision:
  - The dev loop documents the reset: `psql "$DATABASE_URL" -c "TRUNCATE oxsum.channel_prices, oxsum.channels;"` before `cargo run`, in docs/development.md.
  - `TRUNCATE` is what the append-only trigger leaves open: the trigger fires on row-level `UPDATE`/`DELETE`, not on `TRUNCATE`, so the reset does not weaken the "a price version cannot be rewritten or deleted" guarantee — it clears the tables, it does not rewrite history.
  - Users, organizations, keys and ledgers are untouched; only channels and their prices are cleared.
- Why:
  - Test-side cleanup is the infeasible alternative, not the smaller one: `DELETE` is blocked by the trigger plus `ON DELETE RESTRICT` (both deliberate product invariants), `TRUNCATE` in per-test teardown races the parallel tests sharing the schema, libtest has no after-all hook, and a separate test database is a bigger architectural change than the problem warrants.
  - Keeping the startup refusal (rather than teaching the server to skip unopenable channels) is the point of the check: a deployment that cannot open its credentials must fail before serving, not per request.
- Rejected:
  - Deleting test channels in teardown: blocked by the append-only trigger by design; working around it would punch a hole in a product invariant for test tidiness.
  - Sealing test channels with the dev `.env` key: makes tests depend on ambient configuration, and breaks the missing-key startup test, which needs channels the config cannot open.
  - A server flag to clear channels: a new interface for what is a documented one-liner, and a footgun on a real deployment.
- Implementation notes:
  - Verified end-to-end: a gateway test leaves a `mock-*` channel, `cargo run` refuses with "a stored upstream credential does not open with OXSUM_SECRET_KEY", the documented `TRUNCATE` clears both tables, and the server boots with `/healthz` answering ok.

## 2026-10-02 The generative suite loads `.env` before reading `DATABASE_URL`

- Status: Adopted. Implemented 2026-10-02, closing issue #18.
- Background: `tenants()` in `crates/core/tests/generative.rs` read `DATABASE_URL` from the process environment before `runtime()` loaded `.env` via dotenvy. Under the documented setup (`cp .env.example .env`, nothing exported) every one of the 1000 proptest cases hit `let Some(tenants) = tenants() else { return Ok(()); }` and the suite passed in ~0.2s, having tested nothing. The other suites (`wallet.rs`, `channels.rs`, `identity.rs`, `admin.rs`, `api.rs`, `gateway.rs`) all load `.env` first; the generative suite was the only one with the inverted order.
- Decision:
  - The suite reads the variable through a `database_url()` helper that calls `dotenvy::dotenv()` first, mirroring the `url()` helper in the other suites.
  - The no-database skip stays: docs/development.md documents that database tests skip without `DATABASE_URL`. But it is loud now — a warning naming the skipped case count, printed once, so a vacuous run cannot look like a real one.
  - A subprocess probe pins the ordering: it runs one proptest case in a child whose environment has no `DATABASE_URL` and whose working directory holds a `.env` with an unparseable URL, and asserts the child fails (it tried the database). Hermetic — no real database, no repo `.env` — and fast, since the URL fails at parse time before any I/O.
- Why:
  - Keeping the skip (rather than failing hard without a database) is what the docs promise, and a bare `cargo test` on a machine without PostgreSQL must stay green.
  - Loading `.env` in the helper rather than requiring an exported variable is what makes the documented setup honest: `cp .env.example .env` followed by `cargo test` now really tests.
  - A subprocess probe rather than a unit test: the ordering is a process-global property (environment plus working directory), so only a subprocess can pin it without racing the suite's own parallel cases.
- Rejected:
  - Making the skip a hard failure: contradicts the documented contract and would break `cargo test` on machines without a database.
  - Requiring `DATABASE_URL` to be exported: contradicts the documented setup and the other suites' behaviour.
- Implementation notes:
  - Verified both directions: the probe passes with the fix and fails with the exact assertion message when the lookup is reverted to the environment-before-`.env` order.
  - `runtime()` keeps its own `dotenvy::dotenv()` call; loading twice is harmless (dotenvy never overrides an existing variable).

## 2026-10-02 Channels and versioned prices: the database is the configuration, and a request is priced by the version it starts on

- Status: Adopted. Implemented 2026-10-02, closing TODO 3 / issue #15. Replaces one line of "A gateway turn freezes first": prices still come from a `PriceBook` behind a seam, and what fills the seam is now rows rather than the environment.
- Background: item 2 left the deployment described by `OXSUM_MODELS` and `OXSUM_UPSTREAM_*` — one channel, one price per model, read once at startup. product.md asks for more than that from the start: a price change appends a version instead of overwriting one, and the version in force when a request starts is the version that settles it. None of that fits an environment variable, which has no history. The request path already had the right shape — the price is resolved before the hold and carried by the turn — so what was missing was a source with a history, and recording which version was used.
- Decision:
  - **Channels and prices are oxsum's own tables** (`oxsum.channels`, `oxsum.channel_prices`, migration `0002_channels`), in the same schema as identity. A channel is a name, a base URL, a sealed credential, and the credential's last four characters; a price is one row per `(channel, model, version)`.
  - **Prices are append-only, and a trigger enforces it.** A change inserts `version = max + 1` under the channel's own row lock; an `UPDATE` or a `DELETE` on `oxsum.channel_prices` raises, for any client, psql included. "Changing a price never overwrites the old one" is a property of the database rather than a convention the code observes.
  - **A request resolves its channel and its version once, before the freeze**, carries them through the turn, and writes them into the settlement's description (`channel`, `priceVersion`) beside the prices and the token counts. A bill therefore says which version priced it, and that statement stays checkable after any number of later price changes.
  - **Upstream credentials are sealed with AES-256-GCM under `OXSUM_SECRET_KEY`** (32 bytes, base64), with a fresh nonce per value and only the last four characters kept in the clear. A dump of `oxsum.channels` cannot call upstream, and the key stays deployment configuration rather than data.
  - **The environment is the bootstrap, not the configuration.** At startup a database with no channels is seeded with one channel and one price version per `OXSUM_MODELS` entry; a database that has channels is left alone, whatever the environment says.
  - **The platform admin is an operator token** (`OXSUM_ADMIN_TOKEN`, at least 16 characters, compared in constant time) on `/api/v1/admin`: create or repoint a channel, append a price version, list the channels with their current versions, and read one channel's whole price history. It is not an API key and belongs to no organization — pointing the gateway at another upstream is not something an organization does to itself.
  - **A model belongs to exactly one channel, enforced where it can be**: pricing a model that another channel already serves is `CONFLICT`. That is what keeps the gateway's model lookup unambiguous — one model, one upstream — which is product.md's v1 rule rather than an implementation detail.
  - **A deployment that cannot open its channels refuses to start.** `oxsum_server::prepare` opens every stored credential once and names `OXSUM_SECRET_KEY` when it cannot, so a lost or wrong key is a startup failure instead of a 500 on a request that has already been relayed.
  - **A settlement's arithmetic does not change.** The record still carries the token counts, both prices, the charge and the freeze, plus the channel and version; nothing re-prices anything already written.
- Why:
  - **History in the table, not in a column that moves.** A price version is what a bill references, and a row that can be overwritten cannot be referenced. The trigger is eight lines of SQL and removes the whole class of "someone updated a price and an old bill no longer adds up".
  - **The version is recorded, not only the prices.** The prices alone let a bill be recomputed; the version lets it be *checked against the configuration* — a reader can ask the API for version 3 of that model and compare. That is the difference between a record and evidence.
  - **An operator token rather than a session.** Organization roles and sessions arrive with TODO 4, and the dashboard with TODO 5. What this item needed was a way for whoever deploys oxsum to change a price today, and an environment token is one a deployment can rotate without a login system. The endpoints are the ones the dashboard will drive.
  - **Encryption at rest with the key outside the database.** product.md requires stored credentials to be unreadable, and a key stored beside its ciphertext would satisfy the letter and none of the point. AES-GCM authenticates as well as it encrypts, so a record that does not open under the configured key is found at startup rather than becoming a plausible wrong credential.
  - **The environment seeds an empty database.** A first deployment still needs no HTTP call before it can serve, the tests keep describing a deployment the way an operator would, and the way off the environment is "start once, then change prices over the API".
  - **One model, one channel, checked when the price is written.** The alternative — letting two channels price one model and picking at request time — makes the answer depend on row order, which is a routing policy nobody asked for and which v1's "no load balancing, no failover" forbids.
- Rejected:
  - **A channel id in the ledger descriptions instead of a name and a version**: the description is capped at 512 characters and is meant to be read by a person verifying a bill; a uuid plus a lookup is neither readable nor stable.
  - **Keeping prices in the environment and versioning them in a file**: a file has no history either, and two deployments of one file can disagree about what is in force.
  - **Deleting or deactivating a channel or a price**: with no delete there is no way to make an old bill unverifiable; a channel that should stop serving can be repointed, or left without a current price.
  - **A platform-admin flag in `oxsum.users`**: the platform admin is whoever deploys oxsum and belongs to no organization. Modelling them as a user would put a second identity system in the same PR as the first one.
  - **Encrypting with `pgcrypto`**: the key would live in the same database as the ciphertext, and every relayed request would hand the credential to Postgres to decrypt.
- Implementation notes:
  - `crates/core/src/channels.rs` holds the store and the sealing, `crates/server/src/admin.rs` the surface and its middleware, and `crates/core/migrations/0002_channels.sql` the schema and the trigger.
  - The bootstrap is `oxsum_server::prepare`, which `main` and the integration tests both call. `app` stays a pure router builder, so a test can still build one over a pool that never connects for the requests that are answered without the database.
  - `crates/core/tests/channels.rs` covers version allocation, the trigger refusing an update and a delete, the one-channel-per-model conflict, credential sealing (including a record tampered with in the database), and the bootstrap's "seed an empty database, leave a populated one alone".
  - `crates/server/tests/admin.rs` covers the surface: create, repoint, append, list, history, both token refusals, and the input refusals in oxsum's error shape. `crates/server/tests/gateway.rs` covers the promise itself: a price change while a streamed turn is in flight, the in-flight turn settling at version 1 and the next one at version 2.
  - The gateway tests share one database and a model belongs to one channel, so every test world names its channel and its models with a random suffix. Without that, two worlds pricing `ok` would conflict, which is the rule working rather than a test problem.
  - `OXSUM_SECRET_KEY` and `OXSUM_ADMIN_TOKEN` are documented in `.env.example` and `docs/development.md`. The first is required as soon as a channel exists, including one seeded from the environment — which is why a deployment that sets `OXSUM_MODELS` must set it too.

## 2026-10-02 A gateway turn freezes first, relays through a generator, and settles however it ends

- Status: Adopted. Implemented 2026-10-02, closing TODO 2 / issue #12. Carries out the shape "Gateway HTTP client and token estimation" chose, and corrects two of its lines: what the disconnect signal is, and how the stream is relayed.
- Background: the gateway is the component that owns both halves of a hold/settle pair (issue #6 named it as such), so it is where product.md's billing table becomes real: freeze an upper bound before the call, settle against upstream's usage afterwards, and account for every way a turn can end. Three of those endings are not "upstream answered normally": upstream refuses, upstream is unreachable, and the client leaves mid-stream. The last one is the interesting one, because the code that would settle it is the code the client's departure destroys.
- Decision:
  - **The order is the promise.** The hold is taken before upstream is contacted, upstream's answer decides the charge, and the settlement is the last thing that happens to a turn. A refusal at the hold is answered 402 before any network call, and the message states the freeze, the balance, and that `max_tokens` lowers the price — a payment error that does not name its price is a support ticket.
  - **Prices come from a `PriceBook` behind a seam.** `OXSUM_MODELS` describes the channel in the environment for now; TODO 3 replaces the source with versioned rows managed from the admin dashboard, which is a change of what fills the book rather than a change of the request path.
  - **All money arithmetic lives in `crates/core/src/billing.rs`**: the input upper bound (UTF-8 bytes plus a fixed per-message overhead), the freeze, the priced usage, the local estimate, and the settlement record. The server decides *when* to price; core decides *what it costs*. The freeze rounds up, so a fraction of a minor unit is never charged at zero.
  - **The settlement kinds are product.md's table, and which one applies is decided by where the turn ended, not by configuration**: `usage` (upstream reported usage), `estimated` (it reported none), `client_cancelled` (the client left mid-stream), `upstream_error` and `upstream_unreachable` (nothing was received, so nothing is charged), `capped` (upstream's usage priced above the freeze — the freeze is charged and the excess is an anomaly for the admin page rather than a silent platform loss).
  - **The relay is an `async-stream` generator over `reqwest::Response::chunk()`**, and it awaits the settlement *after* the last forwarded chunk and before the stream ends, so a client that reads a stream to its end reads a settled bill. This corrects the earlier entry's `bytes_stream()`: `chunk()` is reqwest's own accessor, and the generator is what lets the settlement be awaited at the end of a stream at all — a hand-written `Stream` has nowhere to await.
  - **The turn owns its own settlement, and a begun settlement cannot be cancelled.** hyper drops the response body when the client goes away, which drops the generator, which drops the upstream response — reqwest's only cancel. The write itself runs in a task of its own and is awaited, so the client's departure cannot cancel an append that has started. What a departure can do is end the turn before the settlement starts, and then `Drop` settles from what had been forwarded, spawned onto the runtime because a destructor cannot await. A client that leaves after upstream has already ended has not cut the turn short: that turn is billed as the finished turn it is, from upstream's own counts, as `usage`. This corrects the earlier entry's "axum's connection notification and a `tokio::select!`": the body is the notification, and there is no second signal to keep in sync with it.
  - **The scanner reads the SSE stream for two things only**: the `usage` object upstream's final chunk carries (the stream is asked for it with `stream_options.include_usage`), and the answer text of the deltas, capped at 256 KiB, for the estimate when usage never arrives. Frames split across chunks are reassembled by holding the trailing partial line.
  - **`/v1/*` answers in OpenAI's error shape**, with oxsum's own code in `error.code`, and every response carries `x-oxsum-request-id`. The two ledger entries are `req-<id>:hold` and `req-<id>:settle`, derived by the public `oxsum_core::entry_id_for`, so a caller holding a request id can name the entries it caused without asking the server which ones they were.
  - **`/v1/models` lists exactly the models with a price**, and a request for any other model is refused 400. A model the gateway cannot price cannot be frozen, so it is not served at all.
- Why:
  - **A generator, because the settlement is the last step of a stream.** The three alternatives all move the ledger write off the request: settling in a spawned task (the client's stream closes before its bill exists, and a test has to poll), settling in a `Drop` (a destructor cannot await, so the write races the process), or settling before the last chunk is forwarded (it would charge for output it had not sent). Awaiting inside the generator is the only shape where the stream's end *is* the settlement, and the `Drop` path remains for the case the generator never reaches its end.
  - **The body as the disconnect signal.** Cancelling the upstream call is the whole point (product.md: a disconnect cancels the call and settles on estimation), and dropping the response is how reqwest cancels. Anything else — a watchdog, a `select!` on a connection notification — is a second thing that has to be right about the same event.
  - **A spawn inside the turn, because a destructor cannot retry what a cancelled write leaves behind.** The first version settled inline and gave the plan up as the write began. An OpenAI SDK client closes the connection the moment it reads the terminator, which over a real socket arrives while the append is in flight, so the append was cancelled with the plan already spent: no entry, and a freeze reserved until the sweeper. Settling through a task that is still awaited keeps the property the relay was built for — the stream's end is the settlement — and makes a begun write uncancellable. The `Drop` path cannot retry it instead: the cancelled append still holds its transaction's locks, so the retry waits behind it.
  - **A disconnect after upstream ended is not a cancelled turn.** Billing it as `client_cancelled` would put the mainstream SDK's every streamed call in the fallback column and hide a real cancellation among them. The turn records that upstream has ended, so the two are distinguishable without a second signal, and the estimate is only what a turn cut short is charged from.
  - **Freeze before, never after.** A reservation taken after the call would be a claim on a balance that may already be gone, and the whole product promise is that credit is committed before it is spent.
  - **Capped as its own kind, not a bigger estimate.** A usage report above the freeze means the output bound did not hold upstream, which is a bug somewhere, and an anomaly the operator needs to see rather than a charge nobody can explain. An *estimate* above the freeze is not that: it is the platform under-measuring, and it keeps its own kind.
  - **No retries, no second upstream attempt.** product.md forbids retrying a streamed call that has already billed; a retry would need the idempotency of a turn, which is TODO 5's `requests` table, not this loop.
- Rejected:
  - **A `requests` table in this change**: it is TODO 5, and the ledger already records both halves of a turn. Adding it here would put two sources of truth for "is this turn in flight" in one PR.
  - **A hold sweeper in this change**: issue #13. Without the `requests` table there is no way to tell a turn that is still running from one whose process died, so a sweeper would have to guess.
  - **Price channels and versioning**: TODO 3. One channel and one price per model is what a deployment needs to bill honestly today, and the `PriceBook` seam is where a second channel attaches.
  - **Passing upstream's error status through unchanged**: a refusal from the provider is not the caller's fault and not evidence that oxsum is broken, so it answers 502 with upstream's own error object, which the SDK shows verbatim.
  - **Tokenizing the input to compute the freeze**: already rejected in product.md; a tokenizer that undercounts breaks the freeze, and byte length cannot.
  - **Holding the whole answer in memory to price it exactly on the cancel path**: the estimate window bounds what a stalled stream can cost the server, and an undercount is the platform's cost rather than the user's.
- Implementation notes:
  - `crates/server/tests/gateway.rs` runs a second axum server as a scripted upstream on a free port, selected by model name, so refusals, a non-JSON 200, a stream without its terminator, and a stream that stalls forever are all ordinary test cases. Seventeen tests cover the loop: usage charging, the estimate fallback, the cap, both upstream failure kinds, the 402 that never touches the network, the image refusal, the unknown model, the disconnect that settles off the request path, the client that hangs up at the terminator and is still billed from upstream's counts, and two concurrent turns that cannot overdraw one wallet. The disconnect tests run the gateway on a real socket, because the failure they cover only happens when a client hangs up for real.
  - The loop was also driven by the official OpenAI Python SDK (3.23) against a mock provider: `models.list()`, one plain completion and one streamed completion, both billed from the ledger with upstream's token counts. The streamed call is what exposed the cancelled append; the integration test above is that case, kept.
  - A settlement that fails after the response has already been sent is logged and left: the hold stays outstanding for the sweeper (issue #13) instead of turning into a 500 after upstream has answered. Its two reachable refusals — a release beyond the reservation and a reused key — cannot happen for a turn with a freshly minted request id, which is why the gateway mints one per request.
  - Graceful shutdown drops in-flight bodies, so a turn that was mid-stream at shutdown settles as `client_cancelled` even though the client was still there. It is the same path, and the alternative is a shutdown that waits for every stream.
  - The three environment variables that describe the channel are documented in `.env.example` and `docs/development.md`; a deployment that sets none of them serves the wallet and answers `/v1/models` with an empty list.

## 2026-10-02 A reservation may not be released beyond what is reserved: `BalanceLimit::FundedReservations`

- Status: Adopted. Implemented 2026-10-02, closing issue #6. Found by the generative harness, which is where the hole came from.
- Background: `Wallet::settle` releases the `held` the caller hands it and never looks the hold up, which is fine as long as the ledger refuses a release nothing covers. It did not. The wallet carried `NoDebitBalance`, which folds the two layers asymmetrically — a reservation consumes room, a pending credit grants none — but a pending credit also *costs* none, so releasing a reservation that was never made raised the available balance by exactly that amount: free, spendable credit from a credit entry. The generator's first 1000-case run reported it in minutes (a settlement of 1 credit against a wallet that had never held anything).
- Decision:
  - The invariant is stated in the engine as a fourth `BalanceLimit`: **`FundedReservations`** is `NoDebitBalance` **and** "the pending layer may not carry a credit of its own", so a release can only give back room a reservation first took. oxsum's wallet account carries it.
  - It is enforced where every other limit is: inside the append transaction, against the balance the entry would leave behind, with the constrained account's row locked — so a limit checked before the write cannot be raced.
  - The rule is **aggregate**: it bounds the total released by the total reserved, not one settlement by one hold. The gap that leaves is issue #10.
  - The refusal is `INSUFFICIENT_FUNDS`, 402, like an unfunded hold. The contract changed first (`crates/server/openapi.yaml`), then `docs/api.md`, `docs/user-guide.md` and `docs/architecture.md`.
  - `Wallet::open` applies the limit on **every** open and upserts it, so a ledger written under the older rule is tightened rather than keeping the weaker rule for the rest of its life.
  - The generative harness dropped the restriction it was written with: it now settles any earlier op's amount, and its model predicts settlement refusals from the reserved total, read through the new `Wallet::settled()` / `Wallet::reserved()` accessors.
- Why:
  - **The engine, because that is where the check can be atomic.** Both alternatives in the issue leave it outside the append. A `holds` table in oxsum's schema duplicates state the ledger already keeps — the pending layer — so it can drift from the books, and it is only race-free if it is written in the same transaction as the append, which means the same lock, in a second place. Reading the pending layer in `Wallet::settle` is a read-then-write: two concurrent settlements both see the reservation and both release it.
  - **One variant, not a second limit column.** An account carries one limit, and this is the rule a funded reservation account needs; two of the three existing variants would have to be set on the wallet to get both halves. A per-layer column would also change the account record's canonical encoding, which would invalidate the content hash of every stored account, for a rule only this account uses.
  - **The headroom is the tighter of the two rules.** They constrain opposite directions — a reservation is bounded by the funds on hand, a release by the reservations outstanding — so the *sign* is what the check needs, and the magnitude is the binding one. `headroom_minor` documents this.
  - **The change is marked where it lands** (`oxsum change (not upstream)` at the variant, the headroom arm, both store code mappings, the DDL and the README), and the engine's own conformance suite gained a check, so the in-memory journal, SQLite and PostgreSQL all prove the rule rather than only the one oxsum runs.
- Rejected:
  - An oxsum-side `holds` table: the same fact kept twice, with a lock and a transaction to keep it honest, and a second thing that can be wrong.
  - Reading the pending layer in `Wallet::settle` before appending: a read-then-write races two settlements into both releasing one reservation. This was the mechanism the issue called "reading the pending layer inside the append transaction" — it is the right shape, but doing it from the domain layer means adding a hook to the engine's append, and a hook is a bigger engine change than a limit the engine already knows how to enforce.
  - Redefining `NoDebitBalance` to include it: it would change behaviour for accounts that use the variant today, and its asymmetric fold is documented and tested as it is.
  - Making a settlement name the hold's idempotency key: it pairs a settlement with exactly one hold, but it changes the request contract and needs per-hold release state the ledger does not keep. That is issue #10, and the gateway (TODO 2) is the component that owns both halves of a hold/settle pair.
  - A database `CHECK` on the pending layer: the layer's total is the sum of postings, not a column, and the rule has to hold against the balance the entry would leave behind.
- Implementation notes:
  - **What the rule does not do** is pair a settlement with the hold it names: releasing more than the named hold is accepted while other holds cover the total. No value can be fabricated that way — the total released still cannot exceed the total reserved — but the pairing is loose, which issue #10 tracks and which `docs/api.md` and the `settle` doc comment state plainly rather than hiding.
  - The generative model was rewritten to the aggregate: it tracks `settled` and `reserved` absolutely, read from the ledger at the start of a case, instead of per-hold amounts and deltas. That the new path is actually exercised is not assumed: with the refusal prediction removed, 60 cases fail within two seconds on a settlement of an amount that was never held.
  - Deploying over an existing database needs the constraint widened, and `CREATE TABLE IF NOT EXISTS` does not rewrite one, so the PostgreSQL DDL drops and re-adds `accounts_balance_limit` (idempotent, and it runs inside the migration's transaction with the schema pinned). Verified against a ledger in the dev database that predates the variant: the widened constraint accepts `funded_reservations` and still refuses an unknown code. SQLite has no `ALTER TABLE ... DROP CONSTRAINT`, so an existing SQLite file keeps the old constraint — noted in its schema, and oxsum uses PostgreSQL only.
  - `Wallet::open` sets the limit on every open, so an existing ledger is upgraded on next use; a test weakens a ledger's stored limit to `no_debit` and asserts that opening it restores `funded_reservations` and refuses an unheld settlement again.

## 2026-10-02 A reused idempotency key is a conflict, mapped by the domain layer from the engine's refusal

- Status: Adopted. Implemented 2026-10-02, closing issue #7. Corrects one line of the generative-test entry below, which left the mapping to this issue.
- Background: the ledger refuses a key that an entry with different content already holds (`PostgresError::IdempotencyConflict`). The domain layer's `From<PostgresError>` mapped only `LimitBreached`, so this one fell into `Storage`, which the API deliberately answers as 500 `INTERNAL_ERROR` with the details kept in the logs. A caller that reused a key by mistake therefore got a server error, for a request it could fix itself — and every such mistake was logged as an incident.
- Decision:
  - `PostgresError::IdempotencyConflict` maps to `WalletError::Conflict`, which the HTTP layer answers 409 `CONFLICT`.
  - The refusal classes each pick their own code: a breach of a balance limit is `INSUFFICIENT_FUNDS` (402), a reused key is `CONFLICT` (409), a bad argument is `VALIDATION_ERROR` (400). Everything else stays `Storage` and 500, which is the honest answer for a ledger that cannot be read or written.
  - The message names the cause and not the ledger's internals: "idempotency key already used for a different request". The existing entry's id is not echoed back, and the caller does not need it — the same key with the original content still replays successfully.
- Why:
  - The distinction being made is *whose mistake it is*, and that is what decides the code: a caller who reused a key gets a 4xx it can act on, while a failing ledger gets a 500 that pages someone. Collapsing them makes both worse — the caller retries a request that will never succeed, and the logs fill with incidents that are not incidents.
  - Mapping it in the domain layer rather than in the route layer keeps the HTTP layer's job what it is: `WalletError` in, code out. The engine's error type does not appear above `oxsum_core`.
- Rejected:
  - Answering 400 `VALIDATION_ERROR`: the request is well formed and the key is valid; it is the *combination* of key and content that conflicts, which is what 409 means.
  - Answering 422 or a new code: no code in docs/api.md fits better than `CONFLICT`, and a code used once is a code every client has to special-case.
  - Checking the key against stored entries before appending: a read-then-write races, two concurrent requests with the same key could both pass, and the engine already makes the check atomic inside the append.
- Implementation notes:
  - `crates/core/tests/wallet.rs` asserts the refusal is `Conflict`, that the refused attempts leave the balance, the log and the key's original entry untouched, and that a top-up and a hold under one key collide with each other. `crates/server/tests/api.rs` asserts 409, `CONFLICT`, a message that does not mention storage, and that the original request still replays with `isNew: false`.
  - The generative model (`crates/core/tests/generative.rs`) reads a reused key as a refusal class rather than a `WalletError` variant, so it needed only its stale comment removed: the arm that accepted a storage-shaped refusal was deleted, which makes the oracle fail if the mapping ever regresses.

## 2026-10-02 Generative wallet sequences: proptest against an in-memory model, on eight shared ledgers

- Status: Adopted. Implemented 2026-10-02 (`crates/core/tests/generative.rs`), closing TODO 1 / issue #5.
- Background: the unit and integration tests cover the cases somebody thought of, while the wallet's promises are about *sequences*: a hold settled twice, a top-up replayed after a failure, a key reused with different content, a hold that runs into the balance after an earlier settlement released part of it. The project already has this style of test in the engine (`crates/doubleentry/tests/simulation.rs` generates seeded sequences and checks the engine's invariants after every step), so the tool and the shape were not up for debate: proptest, an explicit operation enum, a model, assertions after each step.
- Decision:
  - `Vec<Raw>` of `TopUp`, `Hold`, `Settle { back, mode }`, `Replay { back }`, `Collide { back, up }` is generated, then resolved **positionally** into ops, so a back-reference can only ever name an earlier op and every generated sequence is well formed. The first op has no earlier op, so a back-reference there becomes the write it would otherwise have referred to.
  - The oracle is the wallet account's two layers (`settled`, `pending`) plus the map of keys that have written an entry. After every step it asserts the outcome class the model predicted (writes / replays / conflict / insufficient funds / invalid input), `available() == settled + pending`, `available() >= 0`, and the log size. At the end of a case it asserts a proof for every entry the case wrote, and that the proof stops verifying when the amount it records changes.
  - The 1000 cases **share eight ledgers** (`generative_0` … `generative_7`), dropped and recreated once per run, and each case's keys carry its own number. A case reads its tenant's balance and log size at the start and works in deltas from there.
  - `PROPTEST_CASES` sets the case count; the default is 1000, which is what TODO asks CI for.
- Why:
  - Sharing ledgers is what makes 1000 cases affordable and repeatable. Creating a ledger is a DDL migration (an extension plus eleven tables); one per case would spend the whole run on schema creation and leave a thousand schemas behind. The delta is not a weaker assertion: the state at the start of a case cancels out of both sides of it, and the invariants (never negative, log size, proofs, idempotency) do not depend on where the case began.
  - The oracle comes from the *intended* contract, not from what the implementation happens to do, which is the only way such a test can find anything. Where the implementation deviates the deviation is named in the module docs and left to its issue, rather than written into the oracle as if it were correct: at the time it was written the generator settled only holds the ledger actually took, a restriction issue #6 lifted once the rule it needs existed.
  - Keys are derived from the op index rather than generated as random strings, because the interesting operations reference earlier ones by name; a per-case prefix keeps two cases on one ledger from colliding in the ledger's idempotency space.
- Rejected:
  - One ledger per case: the DDL cost above, and a test database that grows by a thousand schemas per run.
  - Asserting on `WalletError` variants for the reused-key case: when this harness was written the engine's refusal reached the domain layer wrapped as a storage failure, and pinning that shape would have frozen a bug into the oracle. The model asserts the class instead — refused, nothing changed — so mapping the refusal to `CONFLICT` (issue #7) did not rewrite it.
  - A tamper check that flips a byte: it can land in a field name, and the verifier deliberately ignores fields it does not know so that a newer server can add one without breaking browsers that already shipped. The tamper check moves an amount instead, which the content hash covers.
- Implementation notes:
  - The generator found two things while it was being written, both filed rather than papered over: a settlement of an amount that was never held was accepted and fabricated available balance (issue #6, fixed by the `FundedReservations` entry above), and a key reused with different content reached the API as a 500 rather than a 409 (issue #7, since fixed: a reused key is mapped to `CONFLICT` and answers 409).
  - The first version shared ledgers without a per-case key prefix and failed immediately: case two's `op-0` collided with case one's entry. The seed, the shrunken input and the message came out of proptest and made it a two-minute fix.
  - A settlement cannot fail for lack of funds, which the oracle relied on until issue #6 changed the rule: a settlement charges `actual` out of settled money and releases `held >= actual` of reservation, so the available balance it leaves is at least the one it found. That is still true, and it is now the reason the only two refusals a settlement has are an argument outside `0..=held` and a release beyond the reserved total — see the `FundedReservations` entry above for the second one.

## 2026-10-02 oxsum's own tables live in one `oxsum` schema, and an organization's ledger is created on first use

- Status: Adopted. Implemented 2026-10-02. Refines "a tenant is an organization" (which already put these tables outside the ledger schemas) and corrects one line of "users and login": registration no longer creates the ledger inside its transaction.
- Background: the user, organization, membership and API key tables needed a home. The ledger tables already have a mechanism — one schema per organization, targeted by `SET LOCAL search_path` at the start of every transaction (see "all tenants share one connection pool") — which is exactly what oxsum's own tables must *not* share: `users` is global, an email is unique across the deployment, and a key resolves the organization rather than being scoped by it.
- Decision:
  - `users`, `organizations`, `memberships` and `api_keys` live in the single `oxsum` schema, created and versioned by oxsum's own runner: `Db::migrate` creates the schema, then applies the files under `crates/core/migrations/` that have no row in `oxsum._migrations`, each migration in one transaction together with that row, under a database-wide advisory lock.
  - Every statement names the schema (`INSERT INTO oxsum.users …`). There is no second `search_path` pinning mechanism, and identity statements are not wrapped in the ledger's transaction-scoped pin.
  - Registration writes user, personal organization, owner membership and first API key in one transaction. It does **not** create the ledger: `Tenants::get` creates `ledger_<tenant_id>` on first use, idempotently, inside the ledger's own advisory lock.
  - `tenant_id` is the organization's UUID without dashes — 32 lowercase hex characters, which the ledger's existing tenant-id rule accepts unchanged, so nothing has to be chosen, probed for uniqueness or sanitized.
  - API keys: `oxs-` plus 32 random bytes, hex-encoded. The database stores a SHA-256 hash (unique, so a lookup is one index probe) plus the first 12 characters as a display prefix, and never the plaintext, which is returned once at creation.
- Why:
  - One schema for identity, schema-qualified, is the smallest thing that works: the tables are few and always the same, and `oxsum.` in the SQL is explicit and greppable, unlike a pin that has to be issued correctly on every path. It also keeps one place to migrate, and no per-organization identity DDL.
  - Cross-organization isolation is unaffected: balances, entries and proofs stay in per-organization ledger schemas, and identity rows are reached by the key lookup that produced the organization in the first place.
  - Lazy ledger creation keeps registration inside tables that exist before any organization does. A ledger migration is a large DDL (an extension plus eleven tables); running it inside the registration transaction holds that transaction open and makes a retry depend on the ledger migration being idempotent. Created on first use, it is idempotent by construction, and a user who never tops up never costs a schema.
  - SHA-256 for API keys, argon2 for passwords: a key is 256 bits of machine randomness, so there is nothing to brute-force, while every authenticated request pays for the lookup; a password is chosen by a human and short, so it gets the slow hash. Both choices are about what is being guessed, not about which hash is "better".
- Rejected:
  - Identity tables inside each ledger schema: N copies of `users`, and cross-organization email uniqueness becomes impossible.
  - A separate identity database: a second pool and a two-phase commit to join what is one foreign key today.
  - Pinning `search_path` per identity transaction as well: a second mechanism to get right, for tables that never vary by organization.
  - Plaintext or argon2-hashed API keys: plaintext leaks on any dump; argon2 costs ~100 ms per API request for no gain over SHA-256 on a 256-bit random secret.
  - Letting the caller pass a tenant id or organization slug in the path: the credential already names the organization, and a path segment the caller controls is one more thing to authorize. This is why the ledger endpoints lost `/tenants/{tenant}`.
- Implementation notes:
  - The migration runner clears `search_path` for the transaction it applies a migration in. The first version of `0001_identity.sql` used unqualified names and created its tables in `public` while recording the migration as applied — found by the suite, not by reading. With `search_path` empty, that mistake fails loudly instead, and a test asserts nothing named `users`/`organizations`/`memberships`/`api_keys` exists in `public`.
  - A duplicate email surfaces as 409 `CONFLICT` by mapping the unique-constraint violation of `users_email_normalized_key`, not by checking first: two simultaneous registrations cannot both pass a check, and the loser would otherwise be a 500.
  - Every way a key can fail — missing header, malformed, unknown, revoked, expired — answers the same 401 `UNAUTHORIZED` with the same message, so key state cannot be probed. A key id belonging to another organization answers 404, not 403, for the same reason.
  - `Db::migrate` unlocks the advisory lock before returning the migration's own result, so a failed migration hands its connection back to the pool unlocked.
  - Signup mode is deployment configuration, not domain state: `Config::from_env` reads `OXSUM_SIGNUP` (default `invite`) and the route refuses registration with 403 `FORBIDDEN` when it is not `open`.

## 2026-10-01 All tenants share one connection pool: `SET LOCAL search_path` at the start of every transaction

- Status: Adopted. Implemented 2026-10-02; the two points the plan below left open were settled while implementing, see "Implementation notes".
- Background: `Tenants` currently opens one `PgPool` per tenant (`PostgresStore::connect_with` pins `search_path` to the tenant schema via connection options), so connection count grows linearly with tenant count. Each PostgreSQL connection is a backend process; a few hundred tenants would exhaust the database.
- Decision:
  - One `PgPool` for the whole database, fixed size (`max_connections` set per deployment); `Tenants` degrades from "open a pool" to "build a lightweight ledger facade per tenant id" and holds no connection resources.
  - Tenant targeting uses a transaction-scoped session variable: **the first statement of every transaction is `SET LOCAL search_path = 'ledger_<tenant>'`, followed by doubleentry's queries.** `SET LOCAL` only lives inside its transaction and vanishes on COMMIT or ROLLBACK (verified empirically on local PostgreSQL 17), so a connection returning to the pool cannot leak one tenant's search_path to the next user — that is the entire reason it beats plain `SET` (plain `SET` survives COMMIT on the session, confirmed empirically; on a pooled connection that is a cross-tenant landmine).
  - Storage-layer changes in doubleentry (`crates/doubleentry`, marked `oxsum change (not upstream)`):
    - A new constructor path: `PostgresStore::new(pool, ledger).in_schema(schema)` accepts an externally owned shared pool and no longer requires search_path pinned in connection options.
    - All 30 query/transaction sites that currently run directly on the pool (`execute/fetch_*(&self.pool)`, `pool.begin()`) funnel through one internal entry point: transaction paths prepend `SET LOCAL`, read-only paths wrap in a tiny `BEGIN; SET LOCAL ...; <query>; ROLLBACK`. This is where the real work of this change is.
    - `migrate` is the exception: it already runs `CREATE SCHEMA`/`execute_schema` on a dedicated connection (the schema SQL carries its own BEGIN/COMMIT, see the migrate-lock decision); a transaction-scoped `SET LOCAL` at the start of that dedicated connection suffices, mechanism unchanged.
    - `migrate`'s existing `current_schema()` check upgrades in meaning: from "the pool's default search_path must be the tenant schema" to "the check runs inside the transaction, after SET LOCAL". Failure still reports `WrongSearchPath`; the guard stays.
  - oxsum-core side: `Wallet::open` takes the shared pool plus a schema name and never connects itself; `Tenants` only caches `Wallet` facades.
  - Cross-tenant defenses (two layers):
    1. `SET LOCAL`'s transaction-scoped lifetime guarantees a returned connection cannot carry the previous tenant's search_path.
    2. Tenant schema names are always assembled from validated tenant ids (`validate_tenant_id`, existing), with double quotes escaped when spliced into SQL (the same approach `migrate` already uses).
  - Test baseline: the existing isolation tests (`tenants_are_isolated` and the other 7 wallet integration tests) stay untouched, plus two targeted cases — two tenants on the shared pool cannot see each other's reads or writes; one connection reused by tenants A then B then A does not leak.
- Rejected:
  - Keep one pool per tenant with a smaller `max_connections`: only delays the problem; with enough tenants every pool starves and total connections still grow linearly.
  - Prefix every SQL with `ledger_<tenant>.`: doubleentry has 55 `sqlx::query` sites; a full rewrite is error-prone, would make the upstream diff unrecognizable, and `search_path` is exactly the mechanism PostgreSQL provides for this.
  - Plain `SET` plus an `after_connect` reset hook: correctness depends on remembering to reset everywhere; one missed path cross-tenant-leaks. `SET LOCAL` does not depend on discipline.
- Why the append lock is unaffected: all three write paths use `pg_advisory_xact_lock` (transaction-scoped, bound to the transaction by definition); the migrate-lock decision already switched `MIGRATE_LOCK` to per-ledger derived keys, so two ledgers on the shared pool contend on different locks and do not block each other.
- Implementation notes (2026-10-02), where the plan met the code:
  - One primitive, `PostgresStore::begin`, opens a transaction and issues `SET LOCAL search_path` as its first statement. Every statement the store makes opens its transaction through it, so read paths, write paths and `migrate` cannot drift apart, and the pin is a property of the code path rather than of remembering to reset. Read-only paths roll back, which is free and leaves nothing behind.
  - The reference DDL (`crates/doubleentry/schema/postgres.sql`) carries its own `BEGIN;`/`COMMIT;`. A `SET LOCAL` issued before it would therefore land outside any transaction block and be **silently ignored** — PostgreSQL only warns (verified on local PostgreSQL 17) — leaving the tables to land wherever the pooled connection resolved. `execute_schema` opens the transaction itself, runs the DDL inside it, and closes it with `COMMIT`/`ROLLBACK` whichever way the DDL ends. Trusting the DDL's own wrapper instead would leave an open transaction on a pooled connection the day upstream drops it, which is the one failure mode a shared pool cannot absorb.
  - `WrongSearchPath` keeps its name and stays in `migrate`, but no longer means "the pool is configured wrong": with a schema name the store quotes itself, it cannot fire. It is now an assertion that the pin took effect before any unqualified name was used. The test that used to provoke it (`a_misconfigured_search_path_is_refused`) was replaced by two tests of the new behaviour — a store on a pool that resolves elsewhere writes into its own schema and nothing into `public`, and two ledgers on one pool of one connection never see each other's rows.
  - `connect_with` is kept and still works for a caller that wants a pool of its own; an externally owned pool goes through `new(pool, ledger).in_schema(schema)`, which no longer requires `search_path` in the connection options.
  - The pool's size is the process's whole connection budget, not a per-tenant allowance: `OXSUM_DB_MAX_CONNECTIONS`, default 10.

## 2026-10-01 Gateway HTTP client and token estimation: reqwest + tiktoken-rs

- Status: Adopted. Executed when phase B starts; settled now so no choice is made mid-implementation.
- Decision:
  - The gateway relays with `reqwest` (0.12, default rustls): stream upstream via `bytes_stream()`, forwarding to the client while accumulating forwarded bytes for the estimation-on-interrupt path; disconnect detection uses axum's connection notification and a `tokio::select!` branch that drops the upstream future (reqwest has no bare cancel API; dropping the future is the disconnect). Cancelling the upstream means cancelling the charge — that is product.md's settled "client disconnect cancels the upstream call".
  - Freeze-time byte estimation needs no library: `str::len()` is the UTF-8 byte count, plus a fixed per-message overhead.
  - Local token estimation on stream interruption/disconnect uses `tiktoken-rs` (0.6, `o200k_base` vocabulary built in, pure Rust): this is where product.md's "estimate with tiktoken's o200k_base" lands. Fallback path only — normal settlement always uses upstream usage, so the two pricing paths never mix.
  - Upstream timeouts: connect timeout 10 seconds; no read timeout (stream inter-arrival is the upstream's guarantee), the 30-minute hold timeout is the backstop, see product.md.
- Rejected:
  - Hand-rolling on `hyper`: reqwest is the default answer in the axum ecosystem; the saved dependency is not worth the showcase.
  - Maintaining our own BPE vocabulary: `tiktoken-rs` already embeds o200k_base; a self-maintained vocabulary is pure burden.
  - Estimating the freeze with a tokenizer: already rejected in product.md — tokenizers differ per model, an underestimate breaks the freeze promise; byte estimation overshoots but has a guaranteed upper bound.

## 2026-10-01 Pages move to Leptos, replacing Topcoat

- Status: **Superseded** by "2026-10-12 Pages move back to Topcoat, replacing Leptos" (compile time proved to be Leptos's dominant cost; the verify trust-model downgrade is accepted for the development phase). Replaces the Topcoat part of "full-stack Rust: axum + Topcoat" below; the axum, doubleentry and WASM-verification conclusions stand.
- Decision: pages use Leptos 0.8; the admin dashboard, bill page, chat page and verification page all live in one Leptos app. Server-side rendering plus browser-side WASM hydration, mounted through the official `leptos_axum`, one binary with the API. `verify_bundle` is called directly inside a Leptos component; no separate wasm-bindgen crate.
- Verified premise: `cargo build -p doubleentry --features serde --target wasm32-unknown-unknown` passes on this machine. The only obstacle was uuid 1.26's `v7` feature force-requiring a randomness source on wasm32; solved with an `oxsum change (not upstream)` target dependency in `crates/doubleentry/Cargo.toml`: `[target.'cfg(target_arch = "wasm32")'.dependencies] uuid = { version = "1", features = ["js"] }`. Browser-side randomness goes through wasm-bindgen; native builds are unaffected.
- Why:
  - Verification is oxsum's core selling point. Leptos's browser side is already WASM, so `verify_bundle` is called straight from a component and the frontend is one technology. Topcoat's design translates Rust expressions to JS, so the verification page would need a separate wasm-bindgen crate with hand-written JS glue — two technologies side by side, and Topcoat's "no frontend build step" advantage disappears anyway.
  - Maturity: Leptos has been maintained since 2022 with about 1.32M downloads in 90 days; Topcoat 0.1 shipped in 2026-04 and its README says "early-stage and experimental, expect breaking changes". Mid-project migration risk is far smaller with Leptos.
  - Server functions let pages call server code directly, no hand-written page API layer.
- Rejected:
  - Staying on Topcoat: accepting a separate verification crate and framework instability for the "newer and shinier" optics.
  - Dioxus: equally capable, but its energy is mostly on desktop and mobile.
  - askama + htmx: the templating is very mature, but it hardly showcases Rust on the frontend and the verification page still needs a separate WASM crate.
- Costs and knock-on constraints:
  - The build goes from "just cargo" to also needing `cargo-leptos`; server and WASM code are separated by feature.
  - Toolchain: Leptos 0.8's MSRV is below 1.98; rust-toolchain.toml keeps 1.98, with its comment rewritten to no longer attribute the pin to Topcoat.
  - Sessions change with it: Topcoat's built-in `topcoat::session` is gone, see the "users and login" decision below.

## 2026-10-01 Users and login: self-built sessions, password-auth, hand-written user/org layer

- Status: Adopted
- Decision:
  - Login sessions are self-built: one `sessions` table (token hash, user id, expiry); token generation, new-token-on-login, logout and renewal are implemented by oxsum. Cookie carries the usual safety attributes (HttpOnly, SameSite=Lax, Secure).
  - Password hashing uses `password-auth` (RustCrypto, 1.0, argon2 inside): two functions, hash and verify, with no knobs to get wrong.
  - Users, organizations, memberships, roles and API keys are all hand-written. Four roles (platform admin, owner, admin, member); permission checks are one `match`.
  - CSRF: mutating operations never use GET, plus an Origin-check middleware.
  - Email later via `lettre` (SMTP); GitHub login later via `oauth2` or `openidconnect`. Neither in v1.
- Why:
  - The user/org layer is tightly coupled to the ledger: registration creates user, personal organization and owner membership in one transaction (the ledger itself is created on first use, see "oxsum's own tables live in one `oxsum` schema" above; the tenant model is "a tenant is an organization"); removing a member has to deal with their keys. Off-the-shelf user libraries know nothing about "a ledger hanging under an organization".
  - Authorization engines like casbin or cedar are built for many, constantly-changing rules; here the configuration would outgrow the business code. This layer is exactly the business code worth showing in a portfolio piece.
  - The demo path is "docker compose up, one binary, and you can play"; adding a standalone identity service (Rauthy, Kanidm, Keycloak, Zitadel, etc.) lengthens the path and adds a deployment unit. Enterprise SSO later plugs in via `openidconnect` without redesign.
- Rejected:
  - axum-login + tower-sessions: the common axum-ecosystem pairing, but its last release was 2025-07 and it buys nothing over a self-built session table. Note: the tower-sessions crate itself remains evaluable where a default implementation is wanted; this decision means "we write the session table and renewal logic", not "that crate is banned".
  - Topcoat's built-in `topcoat::session`: retired together with the page layer moving to Leptos.
- Known cost: argon2 verification is slow (~100ms), so the login endpoint responds noticeably; acceptable for a portfolio's login frequency — slow hashing is exactly the protection wanted if the credential database leaks.

## 2026-10-01 doubleentry's APPEND_LOCK derived per ledger id

- Status: Adopted
- Decision: in `crates/doubleentry/src/storage/postgres.rs`, the constant `APPEND_LOCK` is replaced by `append_lock_key(ledger_id)`: blake3 over a domain-separated string plus the ledger id, first 8 bytes as the advisory lock key. The key is computed when the `PostgresStore` is constructed and exposed via `append_lock()` for testing. Marked `oxsum change (not upstream)`.
- Why: an advisory lock's scope is the whole database. Upstream assumes one ledger per database, so a constant is fine there. oxsum puts many tenant schemas in one database; with a constant, every tenant's writes serialize against every other's.
- Why derived from the ledger id, not the schema name: the same ledger opened through different pools must still contend on the same lock, or log positions would collide.
- Hash collision: if two ledgers' keys collide on the 64-bit key, the consequence is only that those two ledgers serialize with each other — slower, not wrong. This lock only orders writes; it carries no data.
- Test: `ledgers_in_one_database_do_not_share_an_append_lock` in `crates/doubleentry/tests/postgres.rs`. While one ledger's lock is held, another ledger writes through; the same ledger keeps waiting.

## 2026-10-01 A tenant is an organization; every user gets a personal organization at signup

- Status: Adopted. Refines "one tenant, one schema": the "tenant" in that entry means the organization.
- Decision:
  - Balances and ledgers hang on organizations; one organization, one ledger (schema `ledger_<org>`).
  - Signup creates the user, the personal organization (`personal`) and the owner membership in one transaction. A personal user is just "an organization with one member" — the UI never surfaces the organization layer.
  - Users can create team organizations and join several. Roles: owner, admin, member.
  - API keys belong to organizations and hold no balance themselves; they are credentials. Web login and API keys are two auth channels resolving to the same "current organization".
  - Users, organizations, memberships and API keys are oxsum's own tables, outside the ledger schemas, added via migrations.
- Why: every comparable product does this; adding people later means adding members, not migrating ledgers.
  - sandbase: personal organization at signup, balance on the organization, API keys as organization-scoped credentials.
  - OpenAI: prepaid balance on the organization; projects only divide spend within it.
  - OpenRouter: personal accounts and organizations are separate; organizations share a credit pool with admin and member roles.
- Rejected:
  - One ledger per user plus a separate organization-ledger scheme: two models coexisting; moving from personal to team means migrating the ledger.
  - Balance on the user (new-api's approach): team-shared credit becomes impossible later.
- Deferred: per-key sub-limits (sandbase and LiteLLM have them). The approach waits until the gateway works, see TODO.md.

## 2026-10-01 Wallet service first, then AI gateway, then chat UI

- Status: Adopted
- Decision: three phases, each independently demoable.
  - A. Wallet service: make the existing multi-tenant wallet API solid.
  - B. AI gateway: an OpenAI-compatible proxy; point base_url at oxsum and any OpenAI client works. Freeze an upper bound on entry, settle on upstream usage. The admin dashboard, WASM verification and witness all live in this phase.
  - C. Chat UI: on top of B, a chat page (framework per "pages move to Leptos") — top up and chat in the browser.
- Why:
  - With only the wallet API, someone has to write a client before they can try it. B gives oxsum real callers, and streaming relays, mid-stream interruptions and client retries all actually happen.
  - A is B's core; make A solid and B stands. C is a bonus, saved for last.
  - Comparable products all leave billing gaps, which is exactly B's selling point:
    - sandbase is postpaid with no freezing; its own comments admit balances can go negative under concurrency.
    - OpenAI's spend limits are after-the-fact interception; the docs admit actual spend may slightly exceed the cap.
    - LiteLLM added budget reservation in 2026-04, but the freeze lives in a Redis counter kept alive by TTL, not in one transaction with the ledger, and with no proofs.
  - oxsum's freeze and balance check complete inside one database transaction, and every bill carries a Merkle proof.
- Rejected:
  - A only: nobody can try it without writing their own client; not persuasive as a portfolio.
  - C directly: the largest workload, and a chat page is meaningless before the gateway works.
- Known cost: B overlaps with new-api and LiteLLM. The goal is not to replace them but to build a small gateway whose billing cannot go wrong and whose bills prove themselves.

## 2026-10-01 Fix doubleentry's concurrent migrate colliding on the extension's unique constraint

- Status: Adopted
- Decision: in `crates/doubleentry/src/storage/postgres.rs`'s `execute_schema`, a database-level session advisory lock serializes migrations. Marked `oxsum change (not upstream)`.
- Why: multiple tenants migrating an empty database concurrently both pass the `CREATE EXTENSION IF NOT EXISTS btree_gist` existence check and the loser hits `pg_extension`'s unique constraint; reproduced in the spike. A session lock rather than a transaction lock, because the schema SQL carries its own BEGIN/COMMIT which would end the enclosing transaction early and drop a transaction-level lock halfway through.
- Rejected: wrapping a lock in oxsum-core (what the spike did): it only routes around the problem; any other caller hits it again.

## 2026-10-01 One tenant, one schema

- Status: Adopted
- Decision: each tenant uses its own PostgreSQL schema `ledger_<tenant>`, holding one doubleentry ledger.
- Why: doubleentry is designed assuming "one database holds one ledger". Ledgers are physically isolated; their Merkle trees do not leak entry counts to each other, and there is no "forgot the filter column" data-leak risk.
- Rejected:
  - One ledger plus a tenant column: all tenants share one Merkle tree; closing a month would commit other tenants into the seal, and proofs would leak log sizes.
  - One tenant, one database: connection count and operational cost are too high.
- Known costs:
  - doubleentry's `APPEND_LOCK` was a global constant, so all tenants' writes in one database serialized with each other. Solved, see "doubleentry's APPEND_LOCK derived per ledger id".
  - Each tenant opened its own connection pool. To be changed in a later task, see TODO.md.

## 2026-10-01 Vendor doubleentry's source; no crates.io dependency

- Status: Adopted
- Decision: copy hupe1980/doubleentry at commit `58b8739` (0.7.0) into `crates/doubleentry`, keeping LICENSE-MIT and LICENSE-APACHE. Every change is marked `oxsum change (not upstream)`.
- Why: we need to change the engine for our needs (multi-tenant locks, connection pooling, migrate) without being bound to upstream's release cadence. The markers keep future diffs and merges of upstream fixes possible.
- Rejected:
  - crates.io dependency plus a patch: once the changes pile up, unmaintainable.
  - PRs upstream only: uncontrollable cycle, and some changes only make sense in the multi-tenant setting; upstream may not take them.

## 2026-10-01 Full-stack Rust: axum + Topcoat

- Status: Superseded. The page part is replaced by "pages move to Leptos, replacing Topcoat"; axum as the API layer stands.
- Decision:
  - API in axum 0.8
  - Pages in Topcoat 0.9: server-side rendering, interactions translated from Rust expressions to JS
  - Topcoat mounted onto the axum Router via `TowerRoute`, one binary
  - Verification page in WASM
- Why: the project is a Rust portfolio piece meant to show full-stack Rust. Verification logic sharing one code base with the server shows Rust's edge directly.
- Rejected at the time:
  - React frontend: not full-stack Rust.
  - Leptos/Dioxus: equally viable, but Topcoat was newer and tokio-team maintained.
- Underestimated then: the verification WASM needs its own crate under Topcoat; the framework is very new (0.1 in 2026-04, self-described "early-stage and experimental"), so a breaking release mid-project was likely. Both became reasons to move to Leptos.

## 2026-10-01 Positioning: portfolio demo, not a commercial product

- Status: Adopted
- Decision: build a complete, verifiable AI credit wallet demo as a GitHub portfolio piece.
- Why: every commercial direction investigated fails to hold up. Ledger services have Formance, Midaz and Blnk; hold/settle has Blnk and TigerBeetle; AI-usage reconciliation demand is weak. The piece's value is being complete, demonstrable and showing Rust craft — none of that depends on a market gap.
- Rejected:
  - Selling verifiable bills to relay operators: a Merkle proof only shows records were not altered after the fact; it cannot prove the record was honest at write time.
  - A reconciliation service for AI spend against upstream bills: the recoverable amounts on the enterprise side are small, and relay operators' willingness to pay is low.
  - The research lives in three documents under `D:\Study\project`.

## 2026-10-03 Settlement-hold pairing: contract change, not a holds table (issue #10)

- Status: Adopted
- Decision: a settlement names the hold it releases (`holdKey`); the server reads the hold's amount from the hold entry in the ledger, so the request carries no amount to assert. One hold settles at most once: the settlement entry's idempotency key is derived from the hold's key (`oxsum_core::settlement_key_for`), and the ledger's idempotency gate refuses a second, different settlement of the same hold with `Conflict` ("hold already settled"), inside the append. Retrying the identical settlement replays it. Naming a hold that is not outstanding is `WalletError::HoldNotFound`, answered 404; the gateway maps it to 500 because it settles the hold it just took, so a missing one is an internal inconsistency.
- Why: the gateway already owns both halves of a turn — it takes the hold under `req-<id>:hold` and settles under a key it chose — so asking the client to re-supply the hold key costs nothing and removes the aggregate hole: a settlement can no longer release more than its own hold reserved while other holds cover the total. The ledger stays the source of truth (no `oxsum.holds` table, no second store to keep consistent), and the pairing rides the existing idempotency machinery instead of a new lock.
- Rejected:
  - An `oxsum.holds` table: a second source of truth for what the ledger already records; every write would need the table and the ledger to agree.
  - A pre-check for "already settled" before the append: racy by construction, and unnecessary — the derived-key collision in the append is the check, and it is atomic.
- Kept as backstop: the `funded_reservations` limit still refuses a pending credit the outstanding reservations cannot cover, behind the pairing.

## 2026-10-03 A small watch table for the hold sweeper (issue #13)

- Status: Adopted
- Decision: the sweeper finds stale holds in `oxsum.open_holds`, one row per unsettled gateway hold (hold key, tenant, request, model, channel, price version, prices, freeze, opened at). The gateway writes the row *before* taking the hold and deletes it when the turn settles; the background job settles rows older than `OXSUM_HOLD_TIMEOUT` at 0 with kind `swept` and deletes the row. The timeout defaults to 30 minutes, is validated at startup (parseable, at least 60 seconds), and must exceed the longest possible single request.
- Why: product.md's `requests` table (item 5) does not exist yet, so it cannot be the source. The ledger's pending layer says how much is held but not by which request or since when, and paging every tenant's log on every pass would cost O(the log) per tenant with no index to aim at — the issue already judged that scan too expensive, and the store's only enumeration is a full-log `page()`. The table is a finding aid only, which is what keeps #10's decision intact: the ledger stays the source of truth for the hold's amount (read from the hold entry, re-read inside the settle) and for whether it is settled, and the derived settlement key (`oxsum_core::settlement_key_for`) is the atomic guard — the sweeper and a late settlement name the same entry, so exactly one of them takes effect and the other sees `Conflict("hold already settled")`. A row whose hold is gone (the process died between noting the row and taking the hold) is deleted without a ledger write, so a disagreement always resolves toward the ledger. "A hold that is old but whose request is still streaming is not swept" is handled by the timeout contract rather than liveness tracking: anything older than the timeout is abandoned by definition, and the gateway already caps the output side of every turn by writing the output upper bound into the relayed request.
- Rejected:
  - Waiting for the `requests` table: would drag this issue into item 5's (Leptos dashboard) scope; the watch list is superseded by that table when it arrives.
  - A ledger scan per tenant: O(log) per tenant per sweep pass, opening every tenant's ledger, no index.
  - A per-chunk heartbeat for liveness: a write per chunk for a case the timeout contract already excludes.

## 2026-10-03 Demo materials: a self-driving Python script, an honestly-manual GIF (issue #35)

- Status: Adopted
- Decision: `demo/demo.py` (stdlib plus the official `openai` SDK) starts everything it needs: a scripted mock upstream in-process — mirroring `crates/server/tests/gateway.rs`' upstream (one scripted model, SSE answer frames, the token usage in the final chunk) — and the oxsum server on a free port with bootstrap env (`OXSUM_SIGNUP=open`, the mock as `OXSUM_UPSTREAM_BASE_URL`, one demo model with fixed prices, a fresh `OXSUM_SECRET_KEY`). It registers a fresh demo user (a fresh org per run, so a run never touches real data), tops up a fixed amount, runs one streaming chat turn through the SDK with `base_url` pointed at oxsum, and prints the balance delta, the turn's settlement entry — named from the `x-oxsum-request-id` header via the UUIDv5 derivations `docs/api.md` documents as caller-computable (`req-<id>:hold`, `oxsum_core::settlement_key_for`, `entry_id_for`) — with its billing record, and both proof bundles. No real provider, no API key; no new Rust code and no new HTTP endpoints: the demo uses only what the contract already promises.
- Why Python: the demo's point is the OpenAI SDK path a real user takes, and the SDK's reference implementation is Python — a demo in any other language would demo a different client. The transcript is deterministic (fixed prices, fixed scripted usage, fixed top-up); only the demo email and the API key vary per run.
- The GIF is deliberately not faked: this environment cannot record one, so the script prints a deterministic terminal-friendly transcript and the recording (the terminal running `python3 demo/demo.py`) is a manual follow-up for the repo owner, noted in the issue and the PR. No placeholder GIF is committed.
- Rejected:
  - Driving the demo against a real provider: needs a real key, is non-deterministic, and costs money — the scripted upstream answers the same wire format.
  - A new `/api/v1/entries` listing endpoint for the demo: the request-id-to-entry derivation is already public interface; adding a listing endpoint would widen the contract for a demo's convenience.

## 2026-10-03 Release: Dockerfile and GitHub Actions CI (issue #37)

- Status: Adopted
- Decision: the release is a multi-stage `Dockerfile` plus a `.github/workflows/ci.yml` workflow. The builder is `rust:1.98-bookworm` (the tag verified to exist; matches `rust-toolchain.toml`), installing `cargo-leptos` 0.3.11 (the version already used for the dashboard; verified not yanked) and the `wasm32-unknown-unknown` target, then `cargo leptos build --release` — the server binary lands in `target/release/oxsum`, the site (WASM bundle, CSS, `index.html`) in `target/site`. The runtime is `debian:bookworm-slim` plus `ca-certificates` only: reqwest uses rustls, so no OpenSSL is needed. It carries the binary and the site, sets `OXSUM_ADDR=0.0.0.0:3000` and `LEPTOS_SITE_ROOT=/app/site`, and runs against an external PostgreSQL through `DATABASE_URL` supplied at `docker run`; the schema migrates at startup, so there is no migrate step. CI runs on every PR and push to `main`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets` with `-D warnings`, and `cargo test --workspace` against a `postgres:17-alpine` service container with `DATABASE_URL` set — followed by doubleentry's testcontainers Postgres conformance suite (`cargo test -p doubleentry --features postgres --test postgres`). The toolchain comes from `dtolnay/rust-toolchain@stable` with `toolchain: "1.98"` and the `rustfmt, clippy` components, matching the pinned `rust-toolchain.toml`.
- Why: the dashboard's style is plain CSS (no tailwind, no npm), so the Docker build needs only cargo-leptos and the wasm target — no Node.js. The doubleentry conformance suite is included in CI because GitHub-hosted runners have Docker while developer machines may not; CI is the one place the full gate is green. The tree uses no `sqlx::query!` macros, so compilation needs no live database and no `SQLX_OFFLINE`. `.dockerignore` excludes `.env` so secrets can never be baked into the image.
- Rejected:
  - Bundling PostgreSQL into the image: the deployment contract is one binary plus an external database (AGENTS.md), and compose already covers local development.
  - Skipping the testcontainers suite in CI: it is doubleentry's own Postgres conformance gate; leaving it out would make CI weaker than the documented release bar, and the runners provide Docker.
  - A non-root user or distroless base: kept out to keep the image minimal; the surface is one binary behind the operator's own TLS.
- Found while verifying the image: the server never served `/pkg/*` — the shell referenced assets that answered 404, so the pages rendered but never hydrated, in the Docker image and in local development alike. `web::mount` now serves the site pkg dir through `leptos_axum`'s `site_pkg_dir_service` (a targeted route, not a fallback, so API 404s are unchanged), covered by `the_pkg_bundle_is_served_from_the_site_root` in `crates/server/tests/dashboard.rs`.

## 2026-10-03 Browser hydration: the wasm entry point (issue #46)

- Status: Adopted
- Decision: `crates/web` exports exactly one browser entry point, `hydrate()`, under `#[cfg(feature = "hydrate")]`, calling `leptos::mount::hydrate_body(App)`. The name and the shape are leptos's own — `leptos_meta`'s generated module script awaits the module's default initialiser and then calls `mod.hydrate()` — so the export is the contract between the shell and the bundle, not a name this project is free to choose. The login form also carries `method="post"`, so a browser that never hydrates posts instead of putting the credentials in the URL.
- Why: the export was missing, and wasm-bindgen only exports `#[wasm_bindgen]` items, so the bundle had no `hydrate` at all. Every page loaded the wasm, threw `TypeError: mod.hydrate is not a function` and stayed as inert as the server had rendered it: no login handler, no top-up, no chat, no billing socket, no proof check. The 2026-10-03 release entry above saw the symptom through the image and fixed the pkg *route*; the site root (issue #44) and this entry point were the two remaining reasons no browser had ever run the app — and, like the site root, no router-level test can see either, because they pin what the server answers rather than what the browser does with it. The gate that does run the wasm is C-11's manual walkthrough.
- Rejected:
  - A hand-written JS bootstrap in the shell (import the wasm ourselves and call an entry point of our own naming): it replaces leptos's generated script, which is one more thing to keep in step with the framework, and buys nothing.
  - `console_error_panic_hook`: not a dependency today and not needed by this fix; a panic still surfaces as a failed wasm call.

## 2026-10-03 The anomalies page reads the ledgers, not a new table (issue #57)

- Status: Adopted
- Decision: `GET /api/v1/admin/anomalies` and `/admin/anomalies` list the settled turns that did not price cleanly — `capped`, `estimated`, `client_cancelled`, `swept` — by reading each organization's own ledger back through `Wallet::recent_settlements` (`crates/core/src/wallet.rs`), which walks the log newest-first and parses every settlement description into the full `SettlementRecord`. `recent_requests` was refactored onto the same reader: the requests page and the anomalies page are two filters over one walk, so neither can drift from what the ledger holds. The per-channel summary the page shows is computed in the browser from the same rows.
- Why: the anomaly records already exist — every gateway settlement description carries the channel, the price version, the prices, the token counts, the charge and the freeze, and that description is what the bill's proof covers. A second store of "anomalies" would have to be kept in step with facts the ledger already pins; reading the ledger means a row on the page is exactly what verification proves, which is the same choice as "the requests page reads the ledger" above. The `upstream_error` and `upstream_unreachable` kinds are not listed: they charged nothing, so no money is at risk, and the product question the page answers is where the platform loses money upstream.
- Rejected:
  - An `oxsum.anomalies` table written at settle time: a parallel store of ledger facts, one more thing to disagree with the books.
  - Flagging `upstream_error`/`upstream_unreachable` rows too: they are failures, not anomalies in the sense this page exists for — nothing was charged, so nothing was lost.

## 2026-10-03 Monthly closing is doubleentry's period seal (issue #57)

- Status: Adopted
- Decision: `POST /api/v1/admin/closings` closes a `YYYY-MM` month in every organization's ledger via `Wallet::close_month` (`crates/core/src/wallet.rs`), which defines the period if needed, transitions it to closing and calls the store's `seal_period` — the vendored engine's own machinery, unused until now. The seal it appends *is* the closing record product.md asks for: it commits to the log's tree head, the period's entry count and the closing trial balance, and chains onto the seal before it, so the closing history is itself tamper-evident. "The month accepts no new entries" is enforced by the engine's sealed watermark, checked in `Entry::seal`; for that check to see a seal made after the wallet opened, `Wallet::seal` now reads the calendar from storage on every write instead of holding the empty one from `open`.
- Why: the promise "after closing, that month accepts no new entries" has to be structural, not a route-layer convention — every write path (`top_up`, `hold`, `hold_for_key`, `settle`, the sweeper) already funnels through the one `seal`, so the watermark check there covers all of them at once. The alternative, an oxsum-side `closed_months` table checked in routes, would both duplicate the engine's state and leave the wallet API able to write into a closed month.
- Rejected:
  - Year-end closing entries (`doubleentry::closing_postings`): they zero single-period accounts into equity; oxsum's months are review checkpoints, not balance resets — the wallet's balances must carry across a close.
  - Sealing only months with entries: an empty ledger's month still gets a record, and the watermark over its dates is what makes "closed" mean anything.
  - An `oxsum.closings` table beside the seals: the seals table already is the record; a second store can only disagree with it.

## 2026-10-04 The acting organization lives on the session row (issue #58)

- Status: Adopted
- Decision: a session acts as exactly one organization, the one `oxsum.sessions.organization_id` names. Login sets it to the user's oldest membership (the behaviour that already existed); `POST /api/v1/session/organization` updates the same column after proving a membership in the target, and authentication resolves user, organization and role from the stored pair on every request. The dashboard switcher calls the same switch and then reloads: a reload re-reads every page through the session, so nothing on the page has to know the organization changed.
- Why: the column was already there — session authentication already joins `sessions` to `memberships` through it — so the switch needed no new state, only a write. Storing the choice anywhere else (a cookie the server cannot trust, a per-request header, a per-page parameter) would split "which organization acts" across two places; one row on the session is the single answer, and it is what makes a switch survive reloads and new tabs without being askable cross-organization. The membership check is done at switch time, not per request beyond the join authentication already runs — a removed member's session still stops authenticating, because the join finds nothing.
- Rejected:
  - A second session-per-organization or a re-login: heavier machinery for a field the session already carries, and it would leave stale sessions naming organizations the user left.
  - Acting organization in the request (header or query parameter): every caller would carry it, the dashboard's server functions would all grow an argument, and a hand-made request could name any organization — the check would have to run everywhere anyway.
  - Client-side state (a cookie the server reads): the server must not trust a client-writable organization id; the choice would have to be re-validated per request against memberships regardless, which is the session row's join with extra steps.

## Invitation links carry the credential, not an email address

An invitation is a link, not a pending row keyed by the invitee's email: `POST /api/v1/org/invitations` mints an `oxi-` token, the inviter passes it on out-of-band, and `POST /api/v1/invitations/redeem` trades it for an account. The alternatives were rejected: emailing invites needs a mailer v1 does not have, and email-keyed pending rows would have to guess at identity — anyone who knew an address could claim its invite, or the accept step would need a second proof. A bearer-style link needs neither: whoever holds the link is the invitee by definition.

The token is stored only as a SHA-256 hash, for the same reason `api_keys` and `sessions` keep only hashes: a dump of `oxsum.invitations` redeems nothing. Redemption takes the invitation row `FOR UPDATE`, so concurrent registration attempts on one link cannot both win, and spends it in the same transaction that creates the user, the membership and the first key — a link is live, spent or expired, never half-used. An invited account gets no personal organization: the inviting organization is its first membership, so the member list the inviter reads is exactly the people the links brought in. Links only ever make `member`s — granting a role is membership management's business, done after the person is in.

## Adjustments are signed ledger entries, not a second top-up

`POST /api/v1/admin/organizations/{organizationId}/adjustments` books one signed amount with a required reason: a grant debits `Equity:Adjustments` and credits `Liabilities:Wallet`; a deduction runs the other way. The alternatives were rejected: a negative top-up would have put grants and clawbacks on `Assets:Cash` — money that was never received — and a reason column off the ledger would have split "why" from the proof. As an entry, the adjustment's reason is its description, covered by the content hash like every entry's, and the wallet's `FundedReservations` limit is what refuses a deduction the balance cannot carry — under concurrency too, not as a read-then-write check.

The signup bonus is the same mechanism turned on by configuration: `OXSUM_SIGNUP_BONUS_MINOR` (default 0) books "signup bonus" into a brand-new organization's wallet at self-registration, idempotent under the organization's id. An invited account joins an existing organization and is granted nothing — a bonus per redemption would reward the inviter's organization, which is not what the setting is for. At zero, registration still writes no ledger entry: the laziness the ledger was designed for is kept.

## A ledger list's cursor is the log index; an oxsum table's is its keyset

Lists page by position, never page/offset (issue #93). For the ledger — the bills page, the transaction log and the requests page — the cursor is the log's own index: monotone, already the natural order of the reads, and append-only so a held cursor cannot be invalidated by new writes — they always land above it. `Wallet::scan_back` (crates/core/src/wallet.rs) walks backward one store page at a time and returns the index the next page resumes below, so a filtered list (settlements among holds, requests among entries) resumes inside a window rather than skipping what it did not visit. The pages expose it as the URL's own `?before=` — a plain link, the way the filter form's `GET` is, so a browser that never hydrates turns pages too — and the platform admin's organizations list encodes its keyset `(created_at, organization_id)` as an opaque `nextCursor` string, since `created_at` alone cannot order two organizations registered in the same instant.

The bounded lists — in-flight holds, anomalies, closings — stay unpaginated on purpose: a hold list is capped by what a balance can freeze, anomalies and closings are bounded review lists, and none is a table that grows without bound. The exports walk `transactions_page` to the log's start instead of taking one page, because an archive means the whole history.

## First opens of one tenant tolerate the schema-creation race

`Wallet::open` migrates the tenant's ledger schema, and `CREATE SCHEMA IF NOT EXISTS` is not atomic in Postgres — two processes opening the same brand-new tenant at once (a second `AppState`; the integration tests share one database, and admin endpoints open every organization's wallet) can both pass the existence check, and the loser fails `pg_namespace_nspname_index` with `23505` (a race against an in-flight insert reports a unique violation, not `42P06`). `Wallet::open` therefore issues a plain `CREATE SCHEMA` first and treats either "already exists" answer as the other opener having won; everything else inside `migrate` already runs under the store's own locks, and its `IF NOT EXISTS` then sees the committed schema. An advisory lock held across `migrate` was rejected: it pins one pooled connection while the store asks the same pool for more, a starvation deadlock on a small pool. `crates/core/tests/wallet.rs` `one_tenants_first_open_converges_under_concurrency` reproduces the race through separate pools.

## Password reset revokes the user's sessions with it

`POST /api/v1/admin/users/{userId}/password-reset` (issue #91) changes the hash and revokes every live session in the same transaction. The alternative — let sessions ride until expiry — leaves a possibly-compromised credential valid for up to thirty days, which is exactly what a recovery path exists to end. The endpoint answers the revoked count so the operator sees what the reset killed, and an unknown user is 404 rather than a silent no-op.

## The tree-head archive lives in the browser, checked by WASM

The bills page keeps each organization's signed tree head in the browser's `localStorage` as `size:root` under a key derived from the log's origin (`oxsum.tree-head.oxsum/ledgers/<tenant>`) — issue #92. The alternatives were rejected: server-side storage would make the server the witness of its own log, which is precisely what the archive exists to catch; and asking users to save heads by hand (the flow `docs/user-guide.md` still documents as the manual path) is a duty no one performs. `localStorage` survives restarts, is origin-scoped and needs nothing installed; its loss is graceful — a missing or unreadable archive reads as a first visit, recording a fresh signed head rather than alarming.

The check itself never trusts a stored or served value: the head's signature verifies before it is archived, the consistency proof must anchor the archived head exactly (a proof from anywhere else proves nothing), and a failed check never overwrites the archive. That pushed the signature verification into `oxsum-verify` — the crate that must compile to `wasm32-unknown-unknown` — so its doubleentry dependency gained the `witness` feature: Ed25519 verification is pure Rust and wasm-clean, and the feature pulls no network or OS dependencies in.

## A statement's paid figure is derived from the ledger, not allocated (issue #124)

The second half of roadmap P3-2 — monthly statements for organizations on a credit limit. One statement per organization and UTC `YYYY-MM` period, in `oxsum.statements` + `oxsum.statement_lines` (migration 0013), lifecycle `draft → finalized` with payment standing `pending | paid | overdue | suspended`. A draft is the platform's working document — regenerated on demand, invisible to the organization — and finalization locks the lines, snapshots `organizations.payment_terms_days` into the due date, and pins `log_from_index`/`log_to_index`: the ledger window the statement's lines prove, so the document anchors to the verifiable log.

The design decision is that **payments are never allocated**: a statement's `paid_minor` is derived. Credit-line draws are the debt and settled credits on the line are the repayments, and the queue is oldest-first by definition — for a statement whose period ended `last`, the draws ahead of it are everything the line drew before the period began, so what repayments have reached it is `repaid − prior_draws` clamped to the statement's own due. `reconcile_statements` rewrites that bookkeeping on every read and every money-in (top-up, redemption, recorded payment), which makes the figure self-healing and removes the drift class an allocation table would create: a top-up repays the line, and the oldest open statement settles without ever being named. An overdue flip is lazy for the same reason — a standing read is the only place a due date can matter, so `flip_overdue` runs inside those reads rather than in a cron.

Two consequences of deriving from the ledger rather than summing billed rows: the payment cap on `…/statements/{id}/payments` is the credit-line debt outstanding *through* the statement's period (`credit_drawn_through(last) − repaid`), because an earlier unbilled draw still stands ahead in the FIFO queue — capping at billed statements only would have let a payment land that the ledger could not apply to the statement it named. And the usage→ledger join carries the tenant filter explicitly: an entry id is derived from the hold key, so the same key names the same id in every tenant's ledger, and `usage_records` unfiltered would have joined one tenant's rows onto another's entries — cross-tenant leakage on the billing path.

Statement periods read the settlement entry's `booking_date`, not `usage_records.settled_at`: the latter is the write's instant, which can land beside-midnight into the next day, and billing a turn by when its row was written rather than when it was booked would split one logical month at the seam.

## The daily rollup is written transactionally, keyed by the ledger's day (issue #126)

Roadmap P3-3 — `oxsum.usage_daily` (migration 0014) plus the `/dashboard/usage` page. One row per `(tenant_id, day, key_id, channel, model)` sums a day's turns, the four token counts and the charged minor, and `record_usage` upserts it in the same transaction as the usage insert: a replayed write lands no usage row and rolls nothing, so the aggregate can never double-count. The alternative — a dashboard-time `GROUP BY` over `usage_records` — would scan a table that only grows; the rollup keeps the read proportional to the window, and its inputs are all in the ledger already, so nothing materialized can disagree silently with what was billed: the day key is the settlement entry's `booking_date`, not the usage row's write instant, the same seam statements close on.

`key_id` stays on the rollup row rather than being pre-summed away: the members' scope rule needs it lossless — a member reads their own keys' days plus the unattributed (`NULL`, session settles) shared usage, filtered at read like the bills page. `NULLS NOT DISTINCT` on the unique index keeps the unattributed rows aggregating into one; a plain unique index would have stored every unattributed write as its own row.

The migration backfills existing `usage_records` per tenant ledger schema, skipping a tenant whose `ledger_*` schema is absent — test fixtures write usage rows for tenants that never opened a wallet. Reads are scoped to a `[from, to]` day window by the caller (`get_usage` hands the page the trailing 30 UTC days plus the day labels, so a quiet day draws a zero-height bar, not a gap).

## Per-key rate limits consume at admission; the holds cap counts the ledger (issue #130)

Roadmap P3-4 — `requestsPerMinute` and `maxConcurrentHolds` on `oxsum.api_keys` (migration 0015), beside the P3-1 constraint set, set at creation and replaced wholesale by `PATCH /api/v1/org/keys/{keyId}` like the rest.

`requestsPerMinute` is protection, not billing, and it is deliberately process state: a `RateLimiter` trait with an in-process sliding-window implementation (`crates/core/src/ratelimit.rs`), one slot consumed at admission — the gateway's `run` and `POST /api/v1/holds` consult it before the wallet is touched. A refused request records nothing: no entry, no usage row, no upstream call. The trait is the seam a shared backend (Redis) slots behind when the deployment goes multi-instance, which is why the roadmap's deferred list names it there rather than here. A restart forgiving in-flight allowance is acceptable for a protection limit; it would not be for a billing one. `Instant` is passed into `admit` rather than read inside, so tests steer the clock.

`maxConcurrentHolds` is different in kind: it bounds ledger state, so it is counted from the ledger — pending postings on the pool accounts attributed to the key's provenance actor, an entry debiting a pool counting as one open hold and an entry crediting one (a settlement's release leg) cancelling it. The count runs inside `hold_for_key`'s existing per-key advisory-lock transaction, beside the spend-limit check, so two racing holds cannot both fit. No separate watch table: `oxsum.open_holds` tracks sweeps, not ownership, and a count derived from the ledger cannot drift from it.

Refusals map by surface, matching how the spend limit already splits: `RATE_LIMITED`/`TOO_MANY_HOLDS` as 429 on `/api/v1` (the request refusal carries `Retry-After` and `X-RateLimit-Limit`/`Remaining`/`Reset`), and on `/v1` the same requests arrive 429 with OpenAI's `rate_limit_exceeded` type carrying oxsum's own code — `insufficient_quota` stays reserved for money (spend limit and balance). Replay order is preserved: a committed entry answers its own receipt before any limiter is consulted, so a client retry never burns the key's windows twice.

## Request-level idempotency: the Stripe shape, and the receipt stands in for the stream (issue #132)

Roadmap P3-5 — `oxsum.idempotency_records` (migration 0016), claimed by the `Idempotency-Key` header on `POST /v1/chat/completions`. The claim is `(organization_id, key)` — organization-scoped, like the deposit anchor — and the fingerprint is a domain-separated SHA-256 of the raw request bytes: byte-exact, because a re-encoded body is a different request, not a silent dedup.

The answer stored is the answer replayed: a finished non-streamed turn stores its status and body verbatim (post-execution upstream errors included — running again would freeze and charge again); a streamed turn cannot replay bytes, so its record is completed by the write that already proves the turn — `Db::record_usage` — with a settled receipt (`oxsum.receipt`) naming the request id, the charge and the usage. That one hook covers every way a turn can end: the turn's own settle, the `Drop` a disconnect triggers, and the sweeper's settlement of a hold whose process died. A retry mid-turn is `409 IDEMPOTENCY_IN_FLIGHT`; the same key under a different body is `422 IDEMPOTENCY_MISMATCH`; replays carry `Idempotent-Replayed: true` and the original `x-oxsum-request-id`.

Two deliberate exceptions. A refusal that never reached the wallet — validation, rate limit, a freeze the balance cannot cover — releases the claim rather than recording it, because nothing was billed and a corrected retry should run, not replay an old refusal. And the claim is consulted before the rate limiter, so a replay never burns the key's window: the same "receipt before limiter" order the ledger's own idempotency gate already keeps. Records live 24 hours, matching the roadmap's retention line; expiry is lazy on the next claim until the jobs layer (P8-1) sweeps them.

## 2026-10-07 — Reconciliation is a report, not a repair (roadmap P4-1, issue #134)

`GET /api/v1/admin/reconciliation` scans every organization's ledger against the oxsum-owned projections and answers one entry per drift class with a count and a bounded sample. The classes: `usage_orphans` (a usage row whose settlement entry is absent, including rows naming a ledger that does not exist), `settlements_unrecorded` (a `{"v":…,"request":…}` entry with no usage row), `deposits_unbooked` (credited, no entry), `deposits_mismatched` (`received_minor ≠ amount_minor` — reconciles by hand per the deposits decision), `deposits_stuck` (confirmed past a short grace, never credited or reversed), `watches_orphaned`, `holds_unwatched` and `log_gaps` (positions not dense from zero, or entries stale-unsequenced past an hour).

Three judgment calls, recorded:

- **Read-only, always.** A reconciler that patches the books is a second writer nobody asked for, and a silent repair hides the bug that caused the divergence. The report is evidence for an operator; repair stays a human act, whether a manual adjustment or a code fix. The scheduled run arrives with the P8-1 jobs layer; today the scan is on-demand.
- **Hold liveness is an existence question, not a postings question.** A hold's postings stay pending-layer after settlement (the release legs live on the settlement entry), so neither watch-check can net postings out. The scan derives both entry ids from the hold key — `entry_id_for` and `settlement_key_for` — and reads them back batched: a watch row is stale when its hold is absent or its settlement exists; a `req-*:hold` entry is unwatched when it is live (no settlement) and no watch row names it. Manual API holds carry caller-chosen keys and stay intentionally unwatched.
- **Upstream-bill comparison is deferred, honestly.** The roadmap line reads "our settlements vs upstream bills", but no upstream rail exposes a bill to check against — `upstream_cost_minor` remains the estimate, and the margin view's `untrackedTurns` is where a coverage gap shows. If a rail ever reports its own invoices, its rows join the scan as a ninth class. *Superseded 2026-10-19 by issue #283 (upstream bill sync via admin cost APIs), extended 2026-10-25 by #367 (Anthropic impl).*

The request-side extraction reads `"request"` out of the description with a regex rather than a `::jsonb` cast: the planner may evaluate a cast before the `LIKE` guard, and one non-JSON description would fail the whole scan. Pinned by `crates/core/tests/reconcile.rs` (a clean cycle flags nothing, each planted class is found, a ledgerless tenant's rows are orphans) and `crates/server/tests/admin.rs` (the endpoint's shape and its operator-only gate).

## 2026-10-07 — Metrics are per-app, at the call site, and behind the operator token (roadmap P4-2, issue #136)

`GET /metrics` serves the Prometheus exposition format, gated by `OXSUM_ADMIN_TOKEN` like the rest of the admin surface — a scrape config carries it as `bearer_token`. The recorder is metrics-exporter-prometheus's `PrometheusRecorder` with its default features off: the exporter's own HTTP listener and push gateway would pull a second HTTP stack into a binary that already serves, so only the recorder and its render handle are used.

Three judgment calls, recorded:

- **A registry per `app()`, not the `metrics` facade's global slot.** The recorder lives in `AppState` and is reached through it everywhere. A global recorder would be the standard shape, but it installs once per process — every test's app in a binary would record into the first-installed registry, and a scrape could never be attributed to the traffic that caused it. Per-app, a test drives a turn and reads back the exact counts it produced; `app_with_billing` hands the registry out so `main`'s sweeper task records into the same one the scrape renders.
- **Call-site instrumentation, not the billing broadcast.** The `BillingEvent` broadcast is already a stream of turn lifecycle events, but it is explicitly best-effort — a lagged subscriber loses events — and the sweeper publishes none at all. Counters belong where the event happens: `Turn::settle` (the one place every kind of settle passes, including the disconnect `Drop`), the claim and rate-limit decisions in `chat`, and the router-edge middleware that labels by `MatchedPath` — never the concrete path, so a request id can never become a label.
- **Gauges are scraped, not tracked.** `oxsum_open_holds` and the pool gauges refresh inside the scrape handler: a watch row read as a count cannot drift the way an increment/decrement tally would, and a quiet deployment pays nothing between scrapes. A failed refresh is logged and skipped rather than failing the whole render.

The series: `oxsum_http_requests_total` and `oxsum_http_request_duration_seconds` (method, route pattern, status), `oxsum_gateway_holds_total` (channel, model), `oxsum_gateway_settlements_total` (kind), `oxsum_gateway_charged_minor_total`, `oxsum_gateway_tokens_total` (direction), `oxsum_gateway_turn_seconds`, `oxsum_upstream_response_seconds` (result), `oxsum_rate_limit_rejections_total` (surface), `oxsum_idempotency_claims_total` (outcome), `oxsum_holds_swept_total`, `oxsum_open_holds`, `oxsum_db_pool_connections` (state). Pinned by `crates/server/tests/metrics.rs` (token gating, request counting, gauges) and the gateway suite's settled-turn scrape.

## 2026-10-07 — The response carries the cost; streams carry it in a trailer (roadmap P4-3, issue #138)

A gateway caller should learn what a request cost and how much runway is left without a second API call. Three headers answer it: `x-oxsum-freeze-minor` (the reserved bound — the most the request can cost) and `x-oxsum-balance-minor` (the spendable balance measured right after the hold landed) on the head of every answer that took the hold, and `x-oxsum-charged-minor` once the turn settles — a header on non-streamed answers and on post-hold refusals, an HTTP trailer on streamed ones, announced by `Trailer`.

Two judgment calls, recorded:

- **A trailer, not an SSE frame, for the streamed charge.** Injecting a `data:` frame would pollute upstream's own wire format and trip strict OpenAI clients; an HTTP trailer is transport metadata that a client which does not read it simply ignores — it still has the request id, the settled receipt on an idempotent replay, and `/ws/billing`. The relay yields `http_body::Frame`s over `StreamBody` so the settlement can append the trailer after the terminator.
- **Refusals after the hold answer through `Ok`, not `Err`.** A post-hold upstream failure needs the head stamped with the settled charge (`0`), but `GatewayError::into_response` is called downstream of where the number exists — so those arms build and stamp the response themselves. Nothing is lost: the idempotency claim stores a finished non-streamed answer whether it arrives as `Ok` or as `Err(Upstream)`, so replay semantics are unchanged.

The balance is read once at hold time — not twice, not on a timer — so a failed read omits the header rather than reporting a made-up number. Pinned by `crates/server/tests/gateway.rs` (headers on a non-streamed turn, the trailer on a streamed one, `charged: 0` on a post-hold refusal).

## 2026-10-07 — A hold that cannot settle dead-letters instead of retrying forever (roadmap P4-4, issue #140)

A watched hold whose settlement keeps failing used to retry every sweep pass indefinitely: the error went to the log, the row stayed, and the next minute tried again — no terminal state, no recorded failure mode, nothing on the operator's dashboard. `oxsum.open_holds` now carries the sweeper's bookkeeping (`sweep_attempts`, `last_error`, `last_attempt_at`, `dead_at`): every failed settle is counted and keeps its error, and at ten failures the row is dead-lettered.

Three judgment calls, recorded:

- **Dead-lettering slows the retry; it does not stop it.** A dead hold drops out of the minute cadence and retries once an hour (`last_attempt_at` is the gate), because most persistent failures are transient at that scale — a wedged ledger the operator fixes, a locked row that clears — and a stopped retry would strand the freeze forever once the cause is gone. `dead_at` is the first death, never refreshed: it says when the hold went bad, while `sweep_attempts` keeps counting the retries.
- **The watch row, not a jobs table, owns the state.** The P8-1 jobs layer will generalize retries; the dead state could have waited for it, but a per-row flag on the table the sweeper already scans is one migration and needs no queue. When the jobs layer lands, `dead_at` reads as a task's dead state rather than a separate mechanism.
- **The surface is the reconciliation report, not a new anomaly page.** Dead-lettered holds are drift by definition — a watched hold that refuses to settle — so they answer as the ninth class `holds_dead_lettered`, with `hold_key — last_error` as the sample detail. The in-flight admin list shows the same row with `deadAt`, `sweepAttempts` and `lastError`; the metrics scrape carries `oxsum_holds_dead_lettered_total` (transitions, counted once per hold at its tenth failure) and the `oxsum_dead_holds` gauge.

Pinned by `crates/server/tests/sweep.rs` (a persistently failing hold dead-letters, leaves the minute pass, re-enters on the hourly gate, and lands in the report), `crates/server/tests/admin.rs` (the payload fields) and `crates/server/tests/metrics.rs` (the gauge).

## 2026-10-07 — The Anthropic surface is protocol-native, not a translation (roadmap P5-1, issue #142)

`/v1/messages` could have translated between Anthropic's request shape and the OpenAI one already spoken upstream, or spoken Anthropic itself to channels that declare `protocol: "anthropic"`. The decision is the second: surfaces are protocol-native, each served only by the channels that speak it — a model an `openai` channel serves is not served on `/v1/messages`, and the reverse. Translation would have put oxsum in the business of emulating semantics it cannot price or verify; a second surface reuses the whole pipeline — freeze before upstream, settle after, same idempotency, same cost headers — and keeps the two dialects apart at the request parser, the upstream call and the error envelope.

Three judgment calls, recorded:

- **The adapter owns every protocol fact the relay needs.** `UsageAdapter` gained `answer_text` (the text a frame or a whole body carries, for the local estimate), `ends_stream` (the terminator: `[DONE]` versus `message_stop`/`error`) and `closing_frame` (the close emitted when upstream ended without its terminator). The SSE scanner in `relay.rs` is now fully protocol-agnostic — it assembles frames, the adapter interprets them — and `UsageRecord::merge_report` folds Anthropic's two-part report (`message_start` knows the input side, `message_delta` the cumulative output) into one record, per-field by maximum.
- **Anthropic's `input_tokens` excludes the cache counts, so the adapter adds them back in.** The record's invariant — every input subset sits inside `inputTokens` — is what the price book bills against, so `cache_read_input_tokens` and `cache_creation_input_tokens` fold into the normalized input. The write lump splits across the two tiers when `cache_creation` details it (`ephemeral_5m_input_tokens`/`ephemeral_1h_input_tokens`); a lump without the split lands in the 5-minute tier — the common case, and never silently free.
- **The credential spellings stay protocol-shaped on both sides.** A caller may present the organization key as `x-api-key` (Anthropic's spelling) on `/v1`, and upstream hears the channel credential as `x-api-key` too, with `anthropic-version` forwarded or defaulted to `2023-06-01`. Errors on `/v1/messages` come back in Anthropic's `{"type": "error", "error": {…}}` envelope with oxsum's code kept on `error.code` — an Anthropic SDK parses its own format, which is the whole point of the surface.

Pinned by `adapters.rs`'s unit tests (the folded input, the tier split, the two-part stream report), the request parser's tests (blocks, `system`, `metadata.user_id`, media refusal), the relay's merge/terminator tests, and `crates/server/tests/gateway.rs`'s end-to-end anthropic turns — whole body, streamed, refused media, cross-protocol refusal, upstream error pass-through.

## 2026-10-08 — Webhook events are a transaction-internal outbox, not a broadcast fan-out (roadmap P5-2, issue #144)

The in-process `BillingEvent` broadcast already tells the dashboard WebSocket what settled — the temptation is to fan webhooks off the same stream. The decision is a durable outbox instead: `record_usage` inserts one `oxsum.webhook_deliveries` row per enabled subscribed endpoint inside the same transaction as the usage row, the rollup and the idempotency completion. That placement is the whole design — the settlement and its notification commit or roll back together, the usage row's `ON CONFLICT (request_id)` early return means a replayed settlement (a retried turn, a raced sweep) enqueues nothing twice, and both producers — the gateway turn and the sweeper — get webhooks for free because both already settle through that seam. A broadcast fan-out would have been process-local, lossy on restart, and free of exactly the dedup guarantee the durable write already has.

Three judgment calls, recorded:

- **The claim is a lease, not a read.** `claim_due_deliveries` flips due rows `pending → sending` in one `UPDATE … FOR UPDATE SKIP LOCKED` and stamps `claimed_at`; `record_delivery` writes the outcome only when the row is still `sending` under that same timestamp — the fencing token — so a worker whose lease lapsed mid-flight cannot overwrite the next claimant's state, and two workers never hand a receiver the same delivery twice. A `sending` row older than the lease belonged to a dead worker and is claimed again. The alternative — holding a transaction open across the HTTP call — couples the lease to receiver latency, which is exactly what must not be trusted.
- **The secret is sealed, shown once, signed Stripe-style.** Endpoint secrets mint as `whsec-` + 48 hex, stored AES-256-GCM under `OXSUM_SECRET_KEY` beside channel credentials, answered in the clear only at creation; listings carry `secretLast4` — the same contract an API key's `last4` keeps. The signature header is `t=<unix>,v1=<hex>` over `"{t}.{body}"`, HMAC-SHA256: a receiver verifies with any Stripe-shaped library, and the timestamp bounds replay.
- **Only `request.settled` ships, and it is opt-in.** The decisions-catalog's other events (`deposit.*`, `balance.low`, `statement.*`) wait for their triggers to exist; per-request events are off unless an endpoint asks for them, because they are the noisy end of the catalog. URLs are HTTPS-only except localhost — the one place a plaintext webhook is a legitimate developer loop.

Pinned by `crates/core/tests/webhooks.rs` (endpoint lifecycle, the transactional enqueue and its replay-safety, the claim/backoff/failed bookkeeping, the payload shape) and `crates/server/tests/webhooks.rs` (the REST surface under organization auth, a real signed POST to a stub receiver, the reschedule on a refusing one).

## 2026-10-09 — The programmatic read surface reuses the page's queries, not new projections (roadmap P5-3, issue #146)

The four read endpoints a billing pipeline needs — usage rollup, settled billing records, a price estimate, and the catalog — could have been new storage paths, or the same reads the dashboard pages already make, exposed under the organization's API key. The decision is the second: `GET /api/v1/usage` reads `usage_daily`, `GET /api/v1/billing-records` pages the ledger's settlement entries the way `requests_page` does, `POST /api/v1/estimate-price` runs `Price::estimate_minor` — `freeze_minor`'s own dearest-candidate arithmetic on declared token counts instead of a text bound — and `GET /api/v1/pricing` answers the current price version per model. No parallel projection exists that could drift from the ledger, and the estimate is the freeze itself: a number that answers "the most this shape could settle for", never a guess that could underbid the real hold.

Three judgment calls, recorded:

- **Scoping is the page's rule, applied in the route.** An API key or an owner/admin session reads all of the organization's rows; a member session reads their own keys' rows plus the unattributed shared ones — the same `key_scope` the bills and usage pages enforce, resolved once per request by `list_keys` under the scope, not re-derived per row.
- **The catalog is public because it is credential-free, not because it is unauthenticated.** `GET /api/v1/pricing` sits behind organization auth like every `/api/v1` read — there is no anonymous caller of oxsum — but its query never selects `api_key_sealed`, so the row carries nothing the handler could leak. `upstream` prices stay in: they are what the margin view bills on, not a secret, and a caller re-deriving a bill needs them.
- **`estimate-price` takes a shape, not a request.** Declared `inputTokens`/`outputTokens`/`serviceTier` rather than an OpenAI-shaped body keeps the endpoint protocol-neutral — the Anthropic surface asks the same question — and `Price::bound` (the extracted freeze arithmetic) guarantees the answer and a real hold can never diverge.

Pinned by `crates/server/tests/apis.rs`: the rollup filtered to the credential's organization, the window's bounds, the paged walk and the other-organization isolation, the estimate matching the freeze arithmetic (clamp included), and the catalog carrying a credential-free payload.

## 2026-10-09 — The usage page's analytics are cuts of the rollup it already reads (roadmap P5-4, issue #148)

The usage page gained its analytics layer — a token-mix bar and a by-key breakdown — without a new table, a new endpoint, or a chart library: both views group the same scoped `usage_daily` rows the daily chart already sums. `key_id` was already on the rollup (issue #126) and on the REST row (#146); the page payload now carries it plus a `keyLabel` the server resolves from the keys list the scope check already fetches — a key's name, or its prefix when unnamed — so the by-key table reads "prod key" rather than a uuid. Shared unattributed usage is a row of its own, labelled "Shared (no key)", because it is nobody's key — folding it into a "misc" bucket would hide the exact spend a member is allowed to see but cannot attribute.

Three judgment calls, recorded:

- **The token mix is disjoint on purpose.** `cachedTokens` is a subset of `inputTokens`, so the bar shows `input − cached` beside the cached share — four segments that sum to the billed volume, matching the lines the price book bills rather than double-counting the cached part.
- **The bar is hand-drawn like the daily chart, and never color-alone.** Segments are flex-width divs over the existing palette tokens (`--primary`, `--pending`, `--success`, `--text-muted`), each carrying a visually-hidden text span and a title; the legend repeats every segment's count and percentage. No chart library — the standing decision against one (the bundle ships to the browser) holds.
- **Scope is visible in the table, not just enforced above it.** The by-key table sums under the same scope filter the rows already passed, so a member sees their own keys and the shared row — the place where "you cannot see other members' spend" reads as data rather than as an absence.

Pinned by `crates/server/tests/usage.rs`: the pinned row fields (keyed rows carry `keyId`/`keyLabel`, shared rows neither), the keyed row naming the paying key, and the member page naming only the member's key.

## 2026-10-09 — The email flows are one token table behind an optional mailer (roadmap P6-1, issue #150)

Verification and password recovery needed outbound mail, and the deployment question came first: many oxsum installs will never configure an SMTP relay, so mail is opt-in and every surface degrades deliberately rather than partially. `OXSUM_SMTP_URL`, `OXSUM_MAIL_FROM` and `OXSUM_PUBLIC_URL` configure together — a subset refuses to boot, because a half-configured mailer would only fail at send time, inside a request, where nobody is watching. The transport is lettre's async SMTP; the mails are plain text carrying a link under the public URL, never a bare token.

One table, `oxsum.email_tokens`, serves both mails. A token is `oxt-` plus 32 random bytes — recognizable in logs like `oxi-` and `oxs-` — stored only as its SHA-256, so a database dump verifies no address and resets no password. A `purpose` column gates consumption, so a verify link cannot reset a password; consumption is one `UPDATE … RETURNING` that checks live-and-unused and stamps the spend atomically, redeeming once ever against raced double clicks.

Three judgment calls, recorded:

- **A failed mail never fails its request.** Registration and invitation redemption mint and send after the account exists; a send error logs and answers `verificationSent: false` — the account stands and the resend endpoint is the retry. `forgot` answers its indistinguishable empty 200 regardless, because the response must never say whether the account exists; only `verify/request`, whose whole job is sending, reports a missing mailer as 503.
- **The resend cooldown lives in the mint, not the route.** `mint_email_token` refuses a second live token for one `(user, purpose)` inside sixty seconds — the check counts live tokens only, so a clicked link frees the resend instead of muting it. That placement makes the cooldown a property of the storage, safe however many routes mint.
- **The reset is the admin reset, self-served.** Consuming a reset token runs the same `reset_password` the operator endpoint runs: new password in, every session revoked, `sessionsRevoked` answered — the old credential dies with the password it was made under. The dashboard's reminder exists only when `MailConfigured` is provided true, so a mailer-less deployment never nags about a mail it cannot send.

Pinned by `crates/core/tests/email_tokens.rs` (consume-once, expiry, purpose isolation, the cooldown and its freeing on spend, case-insensitive address minting) and `crates/server/tests/email.rs` — end to end through a stub SMTP receiver, so the token under test is the one the mail actually carried: registration and redemption mail the link, the link verifies, resend honours the cooldown, forgot is indistinguishable, and the mailed reset revokes the sessions.

## 2026-10-10 — OAuth joins as a second credential, GitHub first (roadmap P6-2, issue #152)

Login needed a provider credential beside the password, and the same deployment question as the mailer came first: many installs will never register a GitHub OAuth app, so the pair `OXSUM_GITHUB_CLIENT_ID`/`OXSUM_GITHUB_CLIENT_SECRET` plus `OXSUM_PUBLIC_URL` is all-or-none at boot, and without it the login page draws no button and the endpoints answer 404. The provider round-trip lives in `crates/server/src/oauth.rs` over plain `reqwest` — no OAuth crate, because the flow is three requests and a library would own the parts that matter — and its two base URLs take `OXSUM_GITHUB_WEB_URL`/`OXSUM_GITHUB_API_URL` overrides so a test stands a stub where github.com would be.

Two tables in migration 0020 serve the two halves. `oxsum.oauth_states` is the CSRF state — `oxo-` plus 32 random bytes, SHA-256 at rest like every token the project mints — minted per attempt, ten-minute-lived, and consumed in the same `UPDATE` that checks it live, so a raced or replayed callback spends nothing. `oxsum.oauth_accounts` links a provider identity to a user keyed on the provider's own id — not the email, which a provider may reassign or unverify — keeping the verified email it arrived with as provenance only.

Four judgment calls, recorded:

- **The callback is a redirect, not an API.** The caller is a browser mid-flow, so every failure — the provider's own `error`, a spent state, a failed exchange, an identity with no verified email — answers `303` to `/login?error=oauth` and the detail goes to `tracing`, never to a query string the page would display. Success answers `303` to `/dashboard` with the session `Set-Cookie`, minted through the same `session_for` the password path shares — one session model, two credentials.
- **Resolution order is link, match, register.** A stored `oauth_accounts` link wins; then a verified-email match links the identity to that account for every login after; and only then a registration — which `invite`-mode deployments refuse, since the mode gates account creation, not the credential. The email the provider reports must be verified: `/user/emails` distinguishes checked addresses from typed ones, and an unverified one completes nothing rather than opening somebody's account.
- **An OAuth account has no usable password.** Registration stores argon2 over a random secret nobody holds, so `login` can never open it — the account is recovered through the mailed or admin reset, which sets a real password. The rest of the shape is the same registration's: personal organization, owner membership, first API key, all in the one transaction with the link row; the email lands `emailVerified` because the provider already did that work.
- **The login page asks rather than assumes.** `GET /api/v1/auth/methods` is the surface's one unauthenticated read — `{"oauthGithub": bool}` — so the button only exists where the flow does. A static page cannot know the deployment's config, and an absent-404 button would dangle.

Pinned by `crates/core/tests/oauth.rs` (one-spend and expiring states, provider isolation, link-then-match-then-register resolution, invite-mode refusal, and the unusable password) and `crates/server/tests/oauth.rs` — end to end through a stub provider on a loopback port, so the code under test ran the real exchange: the authorize redirect carries the minted state, the callback mints the session cookie, a spent state redeems nothing, an unverified email completes nothing, and invite mode refuses strangers while logging members in.

## 2026-10-10 — The anti-bot check is a gated verdict, not a middleware (roadmap P6-3, issue #154)

The two account-creation endpoints are the only surfaces a stranger can touch with no credential at all — `POST /api/v1/auth/register` in open mode, and `POST /api/v1/invitations/redeem` in every mode — so they are where a bot farm would farm. The check is Cloudflare Turnstile: free, self-hosting-friendly in the sense that it tracks nothing and sets no cookies, and it needs no Google dependency — matching the project's rule that external dependencies stay optional and env-gated.

Four judgment calls, recorded:

- **The check fails closed, on both legs.** A missing `turnstileToken` and a `success: false` verdict answer the same `FORBIDDEN` — a caller learns nothing about which leg it tripped — and a verifier that cannot be reached or answers unreadably answers `SERVICE_UNAVAILABLE` rather than waving the request through. An unchecked registration is not a registered account: the same rule the gateway applies to money admission applies to account admission.
- **Verification runs before the invitation is touched.** The redeem path checks the token first, so a forged bot answer never reaches the redemption table's one-spend race — the verifier absorbs brute force better than a `NOT_FOUND` oracle would.
- **The site key is published; the secret never moves.** `GET /api/v1/auth/methods` — already the auth surface's one unauthenticated read — answers `turnstileSiteKey` beside `oauthGithub`, because the page cannot render a widget for a check it cannot discover. The secret key exists only in `OXSUM_TURNSTILE_SECRET_KEY`, is redacted from `Config`'s `Debug`, and travels only inside the `siteverify` POST.
- **Unconfigured means unchecked, not half-checked.** The `OXSUM_TURNSTILE_SITE_KEY`/`OXSUM_TURNSTILE_SECRET_KEY` pair is all-or-none at boot — a subset is a startup error, because a half-configured check would only fail inside a registration. `OXSUM_TURNSTILE_VERIFY_URL` overrides the siteverify endpoint so a test stands a stub where Cloudflare would be; the widget script and page integration are hydrate-only, SSR emits the form without it.

Pinned by `crates/server/tests/antibot.rs` — end to end through a stub `siteverify` on a loopback port: `auth/methods` publishes the site key when configured and `null` when not, registration and redemption refuse a missing or forged token with `FORBIDDEN` and accept the stub's good token, and an unconfigured deployment registers unchecked.

## 2026-10-10 — The device grant mints the key at the poll, not the approval (roadmap P6-4, issue #156)

A CLI needs an API key and cannot paste one, so the grant is RFC 8628-shaped: `POST /api/v1/device/code` mints a two-code request — the tool's `oxd-` device code for polling and the `XXXX-XXXX` user code the approval page takes — and `POST /api/v1/device/token` is the poll. Both are SHA-256 at rest and lapse fifteen minutes after minting. The user leg is session-only (`GET /api/v1/device/request`, `POST /api/v1/device/authorize`), because a verdict is a person's action and an API key names no user.

Four judgment calls, recorded:

- **The secret is minted at the poll, never stored.** Approving stamps only `approved_by` and `organization_id` on the request row; the API key is created inside the first successful poll's transaction and delivered once. A code nobody polls expires holding nothing, and a `delivered` row answers `consumed` forever after — there is no plaintext waiting in a table for a grant that stalled.
- **Approval grants into the session's current organization.** The verdict records the organization the session acts in, so an approver can only ever grant a key where they act — there is no `organizationId` in the request to lie about. The minted key is ordinary in every other way: `created_by` names the approver, the keys page lists and revokes it.
- **Unknown, expired and decided are one answer.** `NOT_FOUND` covers all three on both legs — the lookup cannot be probed for which codes are live, and a raced second verdict spends nothing.
- **The interval is a throttle, not a hint.** `last_poll_at` stamps every accepted poll; one inside `interval` is 429 `SLOW_DOWN` with `Retry-After` and does not count as a visit. `OXSUM_PUBLIC_URL` feeds `verificationUri` when set, else the answer is the relative `/device` — the grant needs no new environment.

Pinned by `crates/core/tests/device.rs` (pending lookup, delivery-once, denial, expiry, second-verdict `NotFound`, interval throttling) and `crates/server/tests/device.rs` — the full loop over the real router: mint, session-gated lookup, approve, slowed-down poll, the delivered key authenticating against `/api/v1/balance`.

## 2026-10-11 — Tiers gate admission, discounts settle into the description (roadmap P7-1, issue #158)

P7-1 lands the two halves of the pricing roadmap line together: `tier_profiles` plus `organizations.tier` as the capability package, and `pricing_discounts` as the operator's percent-off grants — `GET`/`PUT`/`DELETE /api/v1/admin/tiers…`, `GET`/`POST`/`DELETE /api/v1/admin/discounts…`, and `tier` joining the `PATCH /api/v1/admin/organizations/{id}` body.

Five judgment calls, recorded:

- **A tier is enforced at admission, never priced.** The package is a model allowlist (a miss is a 403 before the wallet is asked) and one rolling-minute allowance keyed on the organization — shared by every key it owns, spent beside the key's own window. Nothing reads a tier inside a price formula: an organization without one runs exactly as every organization did before.
- **The discount resolves at the turn's start, beside the price version.** The gateway asks once — highest applicable percent for this (organization, model) inside its window — and that snapshot rides the turn to settlement, so an operator ending a row mid-stream never changes a turn already priced, and a replayed request re-resolves like the price it re-reads.
- **`discountPercent` is settlement schema v4, not a ledger column.** The percent is a billing input, so it lives in the self-contained description where `verify_charge` recomputes `ceil(numerator × (100 − percent) / 10⁸)` — one ceiling division over the summed numerator, matching the gross math's single-ceil rule and the freeze cap's `min` after it. v3 records carry no percent and still recompute; a tampered percent is a `Mismatch`.
- **A discount row is mutable state; the snapshot is the credential.** `discounts()` lists ended rows too — the percent in a settled description is the history, the table is only "what applies now". `end_discount` closes the window rather than deleting the row, and re-ending answers the same row: a retried delete is the same state.
- **Tier writes are idempotent by shape, discount writes by key.** `PUT …/tiers/{name}` replaces the whole package under the name — a replay writes the same row, so it carries no idempotency key. `POST …/discounts` does carry one: replayed with identical fields it answers the row it created (`validFrom` compared at the microsecond precision PostgreSQL keeps), under different fields it is a conflict. A tier still assigned refuses deletion — a deleted package never silently uncaps its members — so `set_organization_tier` and the `organizations_tier_fkey` keep clear-then-delete the only order.

One implementation note worth keeping: `tier` in the PATCH body is `Option<Option<String>>` with a hand-peeled deserialize — serde answers `None` for both an absent field and an explicit `null`, and the endpoint must tell "leave the assignment alone" from "clear it".

Pinned by `crates/core/tests/terms.rs` (tier validation, assignment, in-use deletion refusal, discount scoping/windows/most-favorable/non-stacking, replay and conflict), `crates/server/tests/terms.rs` (every new endpoint plus the PATCH's tier triple-state), `crates/server/tests/gateway.rs` (a discounted turn settling discounted, a post-end turn settling gross, allowlist and shared-window refusal), the verifier's v4 recompute-and-tamper tests, and the billing round-trip.

## 2026-10-12 — The admin audit log records calls, after they land (roadmap P7-2, issue #160)

P7-2 gives the operator surface a memory: an append-only `oxsum.admin_audit` table, one row per mutating `/api/v1/admin` call, and a cursor-paged `GET /api/v1/admin/audit` to read it back newest first.

Five judgment calls, recorded:

- **The row is written after the mutation commits, not inside its transaction.** The mutations live in different stores — a channel row, a tenant ledger entry, a discount row — and no transaction spans them. Writing the audit row second means a crash may omit an event but the log can never describe a change that did not happen; the writes it follows are all idempotent, so a client that retries a failed response lands the mutation and its audit row together.
- **`detail` is an allowlist, not the request body.** Each handler names the fields safe to keep — amounts, reasons, percents, windows, the tier package — and credentials never qualify: a channel's `apiKey`, a reset's `newPassword` and a minted batch's `codes` are deliberately absent. The operator token itself is never stored; `actor` is the literal `operator` today, a column the multi-identity surface inherits rather than rebuilds for.
- **A keyed write's replay is the same event.** The `(action, idempotency_key)` unique index absorbs a retried insert — `ON CONFLICT DO NOTHING` — so `discount.create`, `organization.adjust` and the statement transitions audit once per logical write, and the row joins back to the ledger entry the key names. Unkeyed calls (a tier `PUT`, a batch mint, a discount `DELETE`) are each their own event.
- **Reads are keyset pages over `(recorded_at, audit_id) DESC`, filtered by exact `action`.** The cursor is the same opaque `nanos:id` the organizations endpoint already serves — echoed, never constructed — and an unknown `action` is a 400, so a mistyped filter cannot masquerade as a quiet log.
- **The read is operator-only, like the writes it indexes.** The endpoint sits inside the admin router behind the same token middleware, and there is no web page — consistent with margin and reconciliation, which are API-only surfaces too.

Pinned by `crates/core/tests/audit.rs` (insert, newest-first order, cursor walk, keyed dedupe, unkeyed always-records) and `crates/server/tests/audit.rs` (a write leaving a row with its detail, cursor paging, malformed cursor and unknown action as 400, keyed replay auditing once, the credential-free channel and password rows, the token guard).

## 2026-10-12 — Suspension gates admission; member budgets read the ledger (roadmap P7-3, issue #162)

P7-3 adds the two operator levers that stop spend without touching the books: `organizations.suspended_at`, flippable through the same admin PATCH as the other billing terms, and `memberships.budget_limit_minor`, set on the same member PATCH a role change uses.

The judgment calls:

- **Suspension is a flag on admission, never a ledger event.** A suspended organization's new holds refuse `FORBIDDEN` — `/v1`, `/messages` and `POST /api/v1/holds` share `hold_for_key` and `hold`, so the check lives in the wallet, below the routes — while holds already open settle normally and money-in still lands, because the flag writes nothing into the ledger and every reserve/settle rule is unchanged.
- **The flag read serializes against the flip through a dedicated advisory lock, not a row lock.** `hold_for_key` holds the org-admission lock *shared* for its check window; `set_suspended` takes it *exclusive* before stamping the row. Holds of one organization still run in parallel, but a hold either observes `suspended_at` or committed before it — locking the organization row itself would have serialized all holds of an organization against each other.
- **A member budget is spend attributed to a person, not a key.** `hold_for_key` locks the minting member's row (`api_keys.created_by` → `memberships`), then sums `key_committed_in` over every key they minted — settled charges plus outstanding holds, the ledger's own figure, so the cap cannot drift from the books. Two of the member's keys cannot both fit under the cap because the membership row is locked for the whole check.
- **A removed member's keys keep their history, lose their cap.** The membership row is what the budget hangs on; deleting it unbudgets the keys it attributed — deliberate, since a cap on nobody's spend has no owner to appeal to. `created_by` null (keys minted by an API key, or a since-deleted account) is unbudgeted the same way.
- **The suspend write is its own webhook event, enqueued in the same transaction.** `org.suspended` joins `request.settled` in the outbox, with `changed` gating the enqueue — a replayed `suspended: true` lands the same state and announces nothing, so the flag, the audit row and the delivery stay one logical write. Reinstatement enqueues nothing; this build subscribes to suspensions only.

Pinned by `crates/core/tests/suspension.rs` (the gate, the settle-while-suspended hold, the replay, the webhook enqueue, the member-budget sums over two keys and over settled spend, the validation surface) and `crates/server/tests/suspension.rs` (the PATCH round-trip, the 403s by key and by session, the budget set/clear, the `org.suspended` subscription and delivery).

## 2026-10-08 — The demo seed writes the world the HTTP surface would (roadmap P7-4, issue #164)

P7-4 adds `oxsum seed`, the binary's only subcommand: it fills a deployment with a believable demo world — two users, an organization, three keys, deposits, a month of settled turns, an open hold, a discount and a finalized statement — prints the credentials, and exits.

The judgment calls:

- **Every write goes through the domain APIs the real paths use.** `register`, `add_member`, `update_member`, `create_key`, `create_discount`, `top_up` + `record_manual_deposit`, `hold_for_key` + `settle` + `record_usage`, `close_month` + `generate_statement` + `finalize_statement` — no seeded row is a shape the HTTP surface could not produce, so the seeded bill verifies, the open hold is the sweeper's to manage, and the constraint surface (member budget, key limit, model allowlist) has real spend to count.
- **The demo email's existence is the whole idempotency check.** `seed_demo` registers `demo@oxsum.local` first; a rerun — or a retry of a run that died mid-way — finds the account and reports `created: false` without writing anything. The seeded request ids (`seed-<day>-<n>`) and hold keys are deterministic, so a demo reads identically on every fresh deployment.
- **Posting dates are chosen, not implied.** `top_up`, `hold` and `settle` all take the booking date — an operator tool, not the HTTP surface, so the server-UTC-date rule is untouched — and the seed spreads thirty days of turns backwards, which is what gives the usage rollup, the requests page and a just-ended month's statement anything to show.
- **Seeding does not need `OXSUM_SECRET_KEY`.** The catalog read — channel names, models, price versions — opens no credentials, so `prepare` runs only when a secret is configured (to land the environment's bootstrap channel first). A deployment with channels but no key still seeds; a deployment with no catalog seeds the world minus the turns.
- **The report is the only place the owner key's secret exists in plaintext.** `SeedReport.api_secret` rides `register`'s one-shot secret; a rerun answers `None`, and the run prints it once, `shown once`, exactly like the registration response it comes from.

Pinned by `crates/core/tests/seed.rs` — one suite seeding the world and reading it back through the wallet, members, requests, rollup and statements surfaces, the other proving the rerun writes nothing; both teardown the world in foreign-key order so the suite is rerunnable on a shared development database.

## 2026-10-12 — Periodic work lives in `oxsum.jobs`, claimed `SKIP LOCKED` (roadmap P8-1, issue #166)

P8-1 replaces the in-process interval loops with a durable periodic-work table: one `oxsum.jobs` row per scheduled run, claimed `FOR UPDATE SKIP LOCKED` under a lease, dispatched by `kind`. The hold sweeper and the webhook delivery pass move onto it unchanged; scheduled reconciliation, monthly statement generation and the retention cleanup arrive with the layer.

The judgment calls:

- **One row per run, one outstanding row per kind.** The `(kind)` partial unique index over `pending`/`running` makes `enqueue_job` idempotent — the next occurrence cannot be scheduled twice — while `done`/`dead` rows carry history freely. The worker enqueues the next run only after the current one lands `done`, so a kind is a chain, not a set of independent timers.
- **Claiming is the whole coordination.** A 5-second poll claims due rows `SKIP LOCKED`; a 300-second `claimed_at` lease decides a worker died mid-run, and `finish_job`/`fail_job` fence on the claim stamp so a stale claimant cannot overwrite the next owner's outcome. Every handler is idempotent — the sweeper re-reads its watch rows, deliveries carry their own state, `close_month`/`generate_statement` upsert — so a wrongly-taken-over run is a duplicated pass, not a correctness hazard.
- **Failures back off; dead is a reporting state.** A failed run returns to `pending` on a 30s/2m/5m/15m/1h backoff and lands `dead` after five attempts, where the `jobs_dead` reconciliation class surfaces it to the operator — the same pattern as `holds_dead_lettered`. The kind's chain does not die with it: the worker re-enqueues the kind an hour out, so a transient fault yields an hour of quiet rather than either an instant retry loop or a permanently stopped pass.
- **Startup heals what a crash orphaned.** On boot the worker enqueues every periodic kind that has nothing outstanding — which both schedules a first pass the way the old loops ran one immediately, and repairs a chain whose last link died with the process.
- **`webhook_deliveries` stays its own queue.** It is a user-visible surface with per-endpoint state, its own backoff and its own ten-attempt budget; the jobs layer only schedules the *pass* over it every 30 seconds. Migrating the rows themselves would buy nothing and lose the per-delivery granularity.
- **The statements pass is the period's aggregation job.** Once a day it `close_month`s the month that just ended on every billable ledger — idempotent, so it doubles as the late-usage pickup — generates the draft (which stays a draft; issuing is still the operator's call), and flips finalized statements past due to `overdue`, which used to happen only lazily on read.
- **Retention is batched and windowed.** `OXSUM_RETENTION_PROVIDER_RAW_DAYS` (7) nulls `usage_details.provider_raw` — the privacy window, since the raw payload may carry prompt fragments — while the normalized columns stay forever; `OXSUM_RETENTION_DELIVERIES_DAYS` (30) drops terminal deliveries; `OXSUM_RETENTION_JOBS_DAYS` (90) drops finished job rows; expired `idempotency_records` go with their own `expires_at`. Each rule takes at most 10k rows a pass, so the daily cadence drains a backlog without one giant transaction.

Pinned by `crates/core/tests/jobs.rs` (enqueue idempotency, the lease's claim-once and reclaim-after-lapse, claim-stamp fencing, the backoff-to-dead path, `jobs_dead` drift, the retention rules) and `crates/server/tests/jobs.rs` (a stale hold swept through `run_due` with the next run chained, a kind retrying to dead and recovering on the cooldown, an unknown kind failing its run without killing the worker, a retention pass's chain).

## 2026-10-12 — One model to many channels: weights lead, failover follows (roadmap P8-2, issue #168)

P8-2 retires the "one model, one channel" rule from the channel-and-price decision. A model may now be priced on several channels, and a request that a route refuses before answering rotates to the next one under the same hold.

The judgment calls:

- **Weight versions with the price.** `channel_prices.weight` (1..=1000, default 100, migration 0026) is part of the versioned row — a weight change alone is a new price version, so the settlement's `priceVersion` pins the routing in force exactly as it pins the rates. The old write-time `CONFLICT` on a second channel pricing a served model is gone; the conflict was the ambiguity ban, and weights are the answer to the ambiguity.
- **The first attempt is a weighted pick, the rest are deterministic.** `route_order` draws the leader proportionally over the name-sorted candidates — the rollout of `weight` is a real preference, not decoration — and the followers run in descending weight, so a failed attempt falls over to the next-most-preferred route. `Db::servings` answers every route with its credential opened; only the gateway's relay sees them — `routes`/`catalog` expose the same rows without one.
- **The freeze is the whole candidate set's bound.** The hold reserves the *dearest* route's upper bound and forwards `max_tokens` clamped at the *tightest* ceiling, so whichever route ends up answering, the turn's own freeze covers it — "settle never exceeds freeze" is preserved under failover, not weakened. `estimate-price` answers the same dearest-route number, so the estimate still bounds the settle.
- **Only a failure before the first byte may rotate.** Unreachable, 429 and 5xx are retriable — none of them is an answer — and each rotates to the next route. A refusal the request caused (4xx) fails fast, since the same refusal would come back from everywhere. A failure once bytes are flowing is never retried: the client already holds a partial answer, and replaying upstream could bill the platform twice. When the *last* route's 429 names a `Retry-After` inside the bound (3s), the wait is honored once — failover first, a bounded wait last.
- **The bill names who answered.** The turn reroutes before it settles, so the settlement's `channel` and `priceVersion` are the answering route's own — and on an all-refusals turn, the last route tried. The description is v5: `upstreamAttempts` counts the upstream calls the one hold made, joined into the usage row beside it, and into the billing-record and anomaly surfaces. It is metadata, not a price input — the verifier checks it is a positive integer and recomputes v4's arithmetic unchanged; v3/v4 records parse with an implicit 1.
- **A shared model lists once.** `/v1/models` answers one entry per model — `owned_by` the route with the greatest weight, the one most likely to lead — while the admin channel card and the pricing catalog show every route.

Rejected:

- **Re-picking the price at failover time**: the hold is already taken; a different route's price could exceed it. Bounding the freeze across the candidate set is what keeps the promise while letting prices differ per route.
- **Retrying mid-stream**: a partial answer plus a fresh response is a corrupted reply to the client and a potential double charge upstream. Bytes started means the turn is committed to that channel.
- **Honoring `Retry-After` without a bound**: upstream would hold the caller's request open on its own schedule; 3 seconds is long enough for a rate-limit window to free and short enough to stay a failover feature, not a queue.
- **Health tracking or a channel circuit breaker**: worth having, and deliberately not in this PR — attempts are recorded (`upstream_attempts` is the evidence such a feature would read), but routing stays stateless per request.

## 2026-10-13 — The billing `mode` picks the surface; input-only modes price one side (roadmap P8-3, issue #170)

P8-3 puts the reserved `channel_prices.mode` column to work: `embeddings` and `rerank` join `chat`, and the gateway grows the two surfaces they serve — `POST /v1/embeddings` in OpenAI's shape and `POST /v1/rerank` in the Jina/Cohere shape. Both run the shared claim → freeze → relay → settle pipeline; what differs is which routes are candidates and what the usage count means.

The judgment calls:

- **The mode is the surface's filter, not a price hint.** A route is a candidate only when the channel's protocol *and* the price's mode both match the surface — a chat-priced model is `model_not_served` on `/v1/embeddings`, and an embeddings-priced one on `/v1/chat/completions`. Rates never reinterpret across modes: `inputPricePerMillion` on an embeddings price prices embedded tokens, the same spelling on a chat price prices prompt tokens, and the only safe way to keep those apart is to refuse the pairing.
- **Absent is never free.** On the wire, `outputPricePerMillion`/`maxOutputTokens` became optional *so that* an input-only price can omit its output side — and `Price::validate` now holds both directions: an input-only set with either field nonzero is refused, and a `chat` set without a positive `maxOutputTokens` is refused. The admin surface rejects a chat body missing the fields outright, before `validate` ever sees it. Migration 0027 relaxes the column check to `>= 0`; the mode rule in core is what keeps `0` from meaning "free output."
- **The freeze has no output side on the new surfaces.** `max_tokens` is not injected into a forwarded embeddings/rerank body — there is nothing it could bound — and `stream: true` is refused rather than ignored. An embeddings `input` of token ids freezes at its exact count (the ids are already tokens); strings and rerank's `query`+`documents` texts freeze at the byte bound like chat input.
- **One protocol, three usage dialects.** `adapter_for_endpoint(protocol, path)` replaces the protocol-only lookup: OpenAI carries chat completions, embeddings and rerank, each reporting usage its own way (`prompt_tokens`, with `total_tokens` standing in; `total_tokens` alone). A missing report settles `estimated` from the request's own input bound — the adapters have no answer text to estimate an output from, because there is none.
- **Settlement arithmetic is unchanged.** The description stays at v5: an input-only turn's itemization is an `input` line (plus the flat fee when the set carries one), and `verify_charge` recomputes it with the same arithmetic — mode changes which routes may serve, never how a record proves its total.

Rejected:

- **Metering embeddings through the chat surface** (`POST /v1/chat/completions` answering embeddings shapes): the freeze would still be right, but the client contract would not be — SDKs call `/embeddings`, and a surface that translates between unrelated request shapes is exactly the never-translated rule broken quietly.
- **A separate `nonStreaming` flag or a `mode: input_only` umbrella**: the mode already carries everything the pipeline needs — which surface, which adapter, whether an output bound exists. A second axis would only restate it.
- **Reading `usage.data[i].embedding` lengths as the input count**: vector dimensions are not tokens, and upstream's own report is right there. The provider's count settles the turn; the estimate exists only for a report that never arrives.

Pinned by `crates/core` unit tests (mode validation both directions, input-only bounds and itemization, the unpriced-output flag), `crates/server/tests/gateway.rs` (both surfaces end-to-end — usage, token-array freeze, estimate fallback, mode mismatch, malformed input, failover, bounded `Retry-After`, idempotent replay), `crates/server/tests/admin.rs` (the mode'd write and its refusals) and `crates/server/tests/apis.rs` (the `mode` parameter on estimate-price).

## 2026-10-14 — Services bill through a credential, not an organization (roadmap P8-4, issue #172)

P8-4 opens the metering core the roadmap promised: a deployer-owned service reports billable events — work no upstream LLM performed — against a named organization's wallet over `POST /api/v1/metering/{holds,settle,release,settlements}`.

The judgment calls:

- **A service credential is a third credential kind, not an API key.** `oxsum.service_credentials` stores `oxs-svc-` plus 32 CSPRNG bytes as SHA-256 only, answered once at mint under the admin token. It binds to no organization — the request body names whose wallet pays — because the reporting service serves every customer; and it opens only `/api/v1/metering/*`, so the highest-power credential on the platform authenticates on exactly four routes. The mirror holds too: an organization's API key can never call metering, so a customer cannot report their own charges — self-reporting is a deployer privilege by construction, not by a flag someone might misset.
- **`billableCode` resolves into the price book, not beside it.** A metered event prices through `(channel, billableCode)` → the channel's `mode: event` price — the same append-only versioned rows the gateway reads, so a metered charge and a gateway charge are priced by the same machinery and audited by the same `priceVersion` pin. A dimension the price cannot bill is refused, not silently zeroed.
- **The wallet's own hold/settle primitives carry the money.** A metering hold is an ordinary hold whose actor is `svc-<credentialId>` instead of a key, watched by the same `open_holds` sweeper table; settle charges actual usage capped at the freeze, release settles at zero, and the one-shot `settlements` call derives its hold key (`metering:<idempotencyKey>:hold`) and still watches for the operation's span, so a crash between the two writes is swept rather than leaked.
- **Replay reads the committed outcome, not just the watch row.** A retried settle/release after the watch row cleared answers from the settlement record itself — parsed out of the ledger, which is why released records carry zero-unit priced lines: `SettlementRecord::parse` needs a record it can reconstruct. A settle on a released hold (or vice versa) is `409`, not a replay — different operation, different answer.
- **The description gets a schema (v6), not an overload.** Metered settlements name the reporting `service` and omit `upstreamAttempts` — there were none; the usage row records `0` through migration 0029's relaxed check. `verify_charge` recomputes v6 with the same arithmetic, and a newer unknown schema still degrades to inclusion-only.

Rejected:

- **An organization-scoped metering key** (`api_keys` with a `can_meter` flag): it would let a customer report their own usage — the exact failure the surface exists to prevent — and a flag on the principal everyone already holds is one admin misclick from a trust breach.
- **A separate `metering_holds`/`metering_settlements` table pair**: the ledger's hold/settle pair already carries everything — idempotency, the freeze cap, the sweep — and a parallel table would need its own sweeper, its own proofs and its own drift class for no new semantics.
- **Reusing the v5 description with `service` wedged into `metadata`**: the reporter is first-class audit data — who billed this is the question a disputed metered bill answers — and description fields are cheap; an optional off to the side is how fields get lost.

Pinned by `crates/core/tests/metering.rs` (hold/settle/release/one-shot, replay after watch-row cleanup, pinned versions, suspension, insolvency, unpriced dimensions, the sweep), `crates/server/tests/metering.rs` (all four endpoints, the credential lifecycle, wrong-surface refusals, 402/409 paths, the billing-record read-back) and `crates/verify`'s v6 cases.

## 2026-10-15 — Dev builds shrink debug info: the rlib must stay under 4 GiB (issue #176)

A plain `cargo build -p oxsum-server` from the repository root on Windows failed deterministically: `link.exe` exit 1120 with ~98 unresolved externals, every one of them an `oxsum_server` monomorphization. The cause is size, not staleness — a full `cargo clean` and rebuild reproduced it byte for byte. With the default `debug = 2` the `liboxsum_server` rlib reaches about 4.6 GB, and the COFF archive's member index stores four-byte offsets: object members past the 4 GiB boundary exist on disk but are unreachable to the linker, producing a fixed set of missing symbols.

The tracked `.cargo/config.toml` now sets `[profile.dev] debug = 1` — line tables instead of full debuginfo — mirroring what `.worktrees/.cargo/config.toml` has carried locally since the worktree flow began. The alternative cures were rejected:

- **Package-scoped `debug`** (`profile.dev.package.oxsum-server`) narrows the blast radius but leaves the failure one dependency bump away: at `debug = 2` several rlibs grow multi-GB and any crate near the boundary breaks the same way. The uniform setting is also what the worktrees already proved — every test binary links `liboxsum_server` at `debug = 1` without issue.
- **`strip = "debuginfo"`** applies to final artifacts, not to the objects packed inside an rlib, so it does not shrink the archive that overflows.
- **Splitting `oxsum-server`** is the honest long-term headroom — the crate carries the whole HTTP surface plus the Leptos shell — but it is a restructure, not a fix for a config gap.

Anyone needing full debuginfo per build can still override with `CARGO_PROFILE_DEV_DEBUG=2`, accepting that the link may then fail on MSVC.

## 2026-10-10 — Shutdown drains on three signals, bounded by a deadline (issue #179)

A manual deployment run found the process surviving CTRL+C: the console event reached the
handler — CTRL+BREAK's default kill proved delivery — yet the process stayed up for minutes
with nothing in flight. Whether the wedged piece was the signal future or hyper's drain
waiting on a connection its client never lets go of, one guarantee settles the whole class:
the drain has a deadline.

- **Three signals, one select.** `shutdown_signal` resolves on `CTRL_C`, `SIGTERM` (`docker
  stop`, systemd), and `CTRL_BREAK` on Windows — the console event a deployment can always
  raise, even onto a process group created with CTRL+C disabled. A listener that fails to
  install pends rather than firing: a broken arm is not a shutdown request.
- **The drain is bounded.** Nine seconds after the signal the process exits with open
  connections still open — comfortably under `docker stop`'s ten-second SIGTERM grace, so a
  supervisor's escalated kill and the in-built deadline answer the same way. A stream killed
  mid-flight is not lost money: the sweeper settles the hold at zero, as it does for any
  crash.
- **Rejected: waiting forever.** The previous behavior — drain with no deadline — is exactly
  the reported bug, and it silently turned `docker stop` into a 10-second SIGKILL delay on
  Linux too. A lingering connection has no business holding a deployment hostage.

## 2026-10-15 — The pool's WARN thresholds are raised from sqlx's defaults (issue #182)

A dev deployment against local Docker PostgreSQL logged multi-second WARN entries for statements and pool acquisitions that were not actually problems — startup migrations, cold-query plans, the jobs claim's `FOR UPDATE SKIP LOCKED` scan on a near-empty table. sqlx's defaults (`slow_statements_duration` 1s, `acquire_slow_threshold` 2s, both `Warn`) are tuned for a production SLA, not for a laptop's Docker volume or a cold start; at that level every boot prints warnings that mean nothing and an operator learns to ignore the signal entirely.

The fix keeps the signal and drops the noise: both thresholds are raised to 10 seconds, the point at which a statement or a pool wait is genuinely worth looking at — a 10s query on this schema is either a missing index or a stalled lock, and a 10s pool wait means the pool is saturated or a backend is hung. `test_before_acquire` is turned on so a backend killed while idle (a restarted Docker container, a network blip) is detected on checkout rather than mid-query. The tier-profile query (`tier_of`) and the jobs claim are not individually slow — their WARN entries were threshold noise; the schema's indexes already cover the scans.

## 2026-10-15 — The real-provider check is opt-in, never in the gate (issue #177)

Every gateway suite drives a scripted in-process upstream, which is the right default — deterministic, offline, and fast — but it leaves the protocol edge only a real provider exercises untested: a `usage` object shaped slightly differently, a stream that closes without ceremony. `crates/server/tests/real_upstream.rs` is the standing check for that edge: register, manual-rail top-up, catalog, one non-streaming and one streaming completion against the configured provider, then the read-back and the verification chain — proof bundle, charge recomputation, signed tree head — through the same `oxsum_verify` calls the browser runs.

The judgment calls:

- **Opt-in through the environment, not a flag or a CI job.** The test is `#[ignore]`d and additionally returns early unless `OXSUM_E2E_UPSTREAM_BASE_URL`, `_API_KEY` and `OXSUM_E2E_MODEL` are all set — `-- --ignored` alone still cannot reach the network. A CI job would need a funded provider account as a secret, would bill real money on every run, and would fail red on someone else's outage: three ways a gate that must stay reliable becomes unreliable. The suite is a pre-release and after-the-fact check a developer runs deliberately.
- **The provider key lives in the shell only.** Nothing reads it from a tracked file, and the run seals it into `oxsum.channels` exactly like a deployment — which is also why the documented precondition is a reset development database: the startup check opens every stored credential, and one sealed under a forgotten key is a deployment failure by design, in tests as in production.
- **Prices are the test's own.** The channel it writes carries fixed minor-unit prices, because what oxsum bills is what it was configured to bill — the check compares the settlement against the freeze and the reported usage, not against the provider's own pricing sheet.

The first live run it codifies also produced its value already: streaming turns that complete normally settle `client_cancelled` when the client hangs up at `[DONE]` — invisible to the scripted suite, tracked as issue #178.

## 2026-10-11 — The webhook catalog completes as transaction-internal outbox events, with one documented exception (issue #200)

Issue #200 finishes the event catalog the outbox decision deferred: beside `request.settled` and `org.suspended`, endpoints may now subscribe to `deposit.credited`, `deposit.failed`, `deposit.reversed`, `balance.low`, `statement.finalized`, `statement.payment_overdue`, `statement.payment_status_updated`, `org.credit_limit_exceeded` and `anomaly.detected`. Every event that has a transaction goes inside it — the deposit's `credited` flip, `finalize_statement`'s write, the per-statement reconcile tx, `record_usage`'s tx for `balance.low` and `anomaly.detected`, the dead-letter transition in the sweeper. A change and its notification still commit or roll back together, and a replayed write still enqueues nothing.

Three judgment calls, recorded:

- **`balance.low` is a per-endpoint threshold with a latch, not a global alert.** `webhook_endpoints` gains `balance_low_minor` (armed per endpoint on create, `NULL` is off) and `balance_low_sent` (the latch, migration 0030). `enqueue_balance_low` runs inside `record_usage`'s transaction against the post-settle balance, selects armed endpoints under `FOR UPDATE` — serializing the latch against a concurrent settle — enqueues one delivery each and sets the latch in the same commit. A deposit's credit flip calls `clear_balance_low`, which unlatches only endpoints whose threshold the balance is back over: the next drop below the line fires again. There is no hysteresis band — the line is the line, and re-crossing after refilling is exactly the event.
- **`anomaly.detected` fires on the settlement kinds the anomalies page reviews.** `unpriced`, `capped`, `estimated`, `client_cancelled`, `swept`, `released` — plus a hold dead-lettering after its sweep attempts run out, which announces through the sweeper's `notify_event` because the dead-letter write has no settle transaction to ride. Clean `usage` and the zero-charge refusal kinds enqueue nothing: the event means "a human should look at this", not "a turn ended".
- **`org.credit_limit_exceeded` is the catalog's one at-least-once event.** A hold refusal surfaces from the engine's append — there is no state change of ours to enqueue inside, no transaction to ride. `notify_credit_limit_exceeded` runs at the dispatch layer after `hold_for_key` answers `InsufficientFunds`, gated on `wallet.credit_limit() > 0` so a self-funded wallet's ordinary 402 announces nothing. A crash between refusal and enqueue loses the event; a client retrying the refused request re-raises it. That asymmetry is acceptable because the event is advisory — the books are already right — while a falsely-silent `deposit.credited` would lose money's trail. `deposit.failed`/`deposit.reversed` are in the catalog with no producers yet: they wire up when a rail that reports failures (Stripe, #201) lands.

Pinned by `crates/core/tests/webhooks.rs` (the catalog's validation, the threshold and once-per-crossing latch, `deposit.credited` and `anomaly.detected` enqueues), `crates/core/tests/statements.rs` (the three statement events across finalize/suspend/overdue), and `crates/server/tests/webhooks.rs` (the `balanceLowMinor` field round-trip, `org.credit_limit_exceeded` on a refused hold against a configured line).

## 2026-10-11 — The Stripe rail keeps the caller's key as the deposit's anchor (issue #201)

D2's seam lands its second live rail: `stripe` deposits ride the same `oxsum.deposits` table as `manual` and `redemption`. `POST /api/v1/topups/stripe` opens a `pending` row, creates a Stripe Checkout Session carrying the deposit id as `client_reference_id`, and answers the hosted URL; the signed webhook at `POST /api/v1/webhooks/stripe` is the only path that moves money — `checkout.session.completed` confirms then credits, `checkout.session.expired` marks `expired`. The rail is env-gated like the mailer and OAuth before it: `OXSUM_STRIPE_SECRET_KEY` + `OXSUM_STRIPE_WEBHOOK_SECRET` + `OXSUM_PUBLIC_URL` all-or-none at boot; unset, the intent 503s and the webhook 404s.

Three judgment calls, recorded:

- **`payment_ref` is the caller's `idempotencyKey`, not Stripe's session id.** The UNIQUE `(rail, organization, payment_ref)` anchor then means exactly what it means on the manual rail: a replayed intent is the same row, answered from its stored `meta.stripeCheckoutUrl` without a second call to Stripe. The session id rides `meta.stripeSessionId` instead — the webhook looks the deposit up by it (latest first, so a recycled test id resolves to the newest row). The alternative — swapping `payment_ref` to the session id once Stripe answered — would leave the window between intent and session uncovered and make the unique key change meaning mid-lifecycle.
- **The credit's ledger key is `stripe:<session_id>`, fixed.** A webhook Stripe retries, delivers twice, or races against itself replays one credit — the confirm-then-credit transition runs `FOR UPDATE` on the deposit row and only proceeds from `confirmed`, so the second delivery of a completed session finds the row already `credited` and is a 200 no-op. `received_minor` records Stripe's reported `amount_total`; a mismatch with `expected_minor` is left for the reconciler, never auto-patched — D2's rule, now with a rail that can actually produce the drift.
- **Verified events always answer 200; the signature is the only credential.** `t=<unix>,v1=<hex>` over `"{t}.{body}"`, HMAC-SHA256 in constant time — the same shape oxsum's outbound webhooks sign with, so the verifier mirrors `signature()` byte for byte. A bad or missing header is 401 *before* the body is parsed; a verified event for an unknown session, an already-walked deposit or an event type outside the two acted on is still a 200, because a non-2xx is Stripe's retry signal and a retry of work already done is by construction safe.

The client itself is deliberately thin — `crates/server/src/stripe.rs`: a form-encoded `POST /v1/checkout/sessions` (`mode=payment`, `price_data` inline, success/cancel redirects built on `OXSUM_PUBLIC_URL`) and the signature check — with `OXSUM_STRIPE_API_URL` overridable so the suite stands a stub where `api.stripe.com` would be, the same pattern the OAuth legs use for github.com.

Pinned by `crates/server/tests/stripe.rs`: the intent opens the pending row and the stub sees the deposit id as `client_reference_id`; a replayed intent answers the stored URL without a second session; the full flow lands `expected_minor` in the purchased pool; three raced webhook deliveries credit once; a wrong-secret and a missing signature both 401; unconfigured is 503/404; `checkout.session.expired` moves the row without touching the wallet.

## 2026-10-11 — Deletion is pseudonymization: the identity goes, the books keep the UUID (issue #202)

The 2026-10-05 productization decision settled what "delete the account" means; this is the code. `DELETE /api/v1/users/me` — session-only, password-confirmed — runs one transaction that tombstones the user row (`email`/`email_normalized` → `deleted-<id>@deleted.invalid`, `password_hash` → a marker no argon2 verify accepts, `email_verified_at` cleared), revokes every session and every key the user minted, removes the memberships, nulls the `end_user`/`tags` attribution on `usage_records` and `open_holds` rows their keys paid, and drops the `oauth_accounts`/`email_tokens` rows. The ledger is untouched by construction — settlement descriptions carry no identity — so a deleted user's bills still verify.

Judgment calls, recorded:

- **Tombstone, not DELETE.** `invitations.invited_by`, `sessions.user_id` and `memberships.user_id` all carry `ON DELETE RESTRICT` — a user row that ever did anything cannot be removed without first erasing history that legitimately belongs to other people (an invitation is the receiving organization's fact). The tombstone keeps the UUID the books and the foreign keys point at while erasing everything that identifies the person — which is the decision's "the books keep the UUID, not the identity" made literal.
- **The last-owner wall only guards organizations that outlive the account.** A naive "sole owner of anything → 409" makes self-delete impossible for everyone — every registered user is the sole owner of their personal organization. The wall applies where other members remain: those organizations continue without the account. An organization whose only member was the deleter is tombstoned in the same transaction (renamed `deleted-<id>`, suspended) — the ledger under its tenant id cannot be orphaned, and a memberless organization acts for nobody.
- **Password confirmation reuses login's verifier.** `verify_password` against the stored hash, before the transaction opens so the ~100ms argon2 holds no connection; a wrong one is `InvalidCredentials`, identical to a bad login. An OAuth-registered account stores a hash over a random secret nobody knows — it can never confirm this call until the reset flow mints a real password, which the contract states rather than papering over with a weaker confirmation.
- **"The user's keys" is `created_by`, literally.** A key minted through a session records its creator; a key minted by an API key records none — it belongs to the organization, not to a person, and survives the account exactly as it would survive any member's departure. `revoked_at` rather than DELETE: `usage_records.key_id` keeps resolving to a real row.
- **No webhook event.** The catalog's `deposit.*`, `org.*`, `statement.*` events are organization-scoped; an account leaving its memberships is not money moving, and a broadcast about a person's exit has no honest recipient — the answer's own counts are the record.

Pinned by `crates/server/tests/account_deletion.rs`: the key-only 403 and the bad-password 401; the last-owner refusal lifted by an ownership transfer; the full walk (tombstone, dead session, dead keys, dead login, suspended+renamed personal org, zero memberships); the team-member delete leaving the organization untouched beside its own tombstoned personal org; and the pseudonymization of settled usage attribution with the bill still verifying through `verify_bundle`.

## 2026-10-12 — TPM is a durable window: settled usage, not spent allowance (issue #203)

The 2026-10-05 protection decision drew the line between the three per-key limits: "RPM and concurrency consume their allowance at hold creation; TPM windows count actual settled usage, with admission checking window-actual plus this estimate. Holds never occupy TPM allowance." `tokensPerMinute` implements it as the third key constraint, beside `requestsPerMinute` and `maxConcurrentHolds`.

Judgment calls, recorded:

- **The window is Postgres, not the `RateLimiter` trait.** RPM is protection — a restart forgiving the in-process sliding window is the semantics' own rule, and a refused request records nothing. TPM counts *settled* usage: billing-adjacent, permanent, and never advanced by a hold. So `oxsum.tpm_windows (key_id, window_start, tokens)` is a table, written by `record_usage_balanced` in the same transaction the usage row lands in — a replayed write (`ON CONFLICT DO NOTHING`) rolls nothing, a rollback keeps nothing, and no settle can double-count. A shared `SlidingWindow` fed at settle time would drift from `usage_records` on restart and across instances; durability is why the read is a `Db::tpm_window` Postgres sum, not a trait call.
- **One-second buckets make the minute actually slide.** `window_start = date_trunc('second', now())` — `date_trunc('minute')` would quantize to wall-clock minutes, so a settle at :59 sits in a 1-second window and a :01 burst ages out 59s early. The trailing-60s read (`WHERE window_start > now() - interval '60 seconds'`) over ≤60 rows per key is the honest sliding count the decision names. Rows prune on write inside the settle transaction, so the table stays bounded by what admission reads.
- **"tokens" is `input_tokens + output_tokens`, the normalized totals.** TPM bounds consumption, not price: the subsets (cached, reasoning) already ride inside the two totals, and the record the adapter normalized is what upstream reported. Metering rows (`key_id: None`) and zero-token settles skip the window by construction.
- **The admission estimate is the freeze's own figures.** On the gateway the check is `window-actual + estimate` where the estimate is the input bound (`counted_input` for token-array requests, `input_upper_bound` otherwise) plus `output_bound` — exactly what `freeze_minor` prices — computed after the bounds exist and before any hold is taken, so an admitted turn can never settle more than the window granted it.
- **`/api/v1/holds` checks `window_actual` only.** An amount-only hold spends no tokens — holds never occupy the allowance — so the endpoint refuses 429 `TOKENS_PER_MINUTE` only once the window is already full, rather than let a settled-out key freeze more credit it could never burn. It does not reserve tokens and cannot.
- **`TOKENS_PER_MINUTE` is its own refusal code, not `insufficient_quota`.** `insufficient_quota` is oxsum's money-quota type (`INSUFFICIENT_FUNDS`, `KEY_LIMIT_EXCEEDED`); TPM is a rate limit. `WalletError::TokensPerMinute` maps to 429 + `Retry-After` + `X-RateLimit-*` under `rate_limit_exceeded`/`rate_limit_error` — same shape as `RATE_LIMITED`/`TOO_MANY_HOLDS`, distinguished by the code.
- **Tag-scoped buckets shipped separately (issue #209).** The decision's Each/Concrete/Total tag scopes landed in the 2026-10-12 follow-up below — the per-key window landed here first, and the scopes ride a `scope` column on the same table.

Pinned by `crates/core/tests/tpm_windows.rs` (accrual, the normalized-total rule, replay-no-double-count, the unkeyed/zero skips, the trailing-minute read, prune-on-write) and the server suites: `api.rs` (create+PATCH round-trip, `TOKENS_PER_MINUTE` on a full-window hold, aged rows freeing the key) and `gateway.rs` (accrual across settles, refusal at estimate-over-cap, window expiry admitting again, RPM+TPM composing without a refused turn spending tokens).

## 2026-10-12 — Price authenticity is a signed checkpoint per priced row (issue #204)

The 2026-10-05 roadmap footnote named the last trust gap: a bill proof attests "charged correctly *given the stated price*", but the price's authenticity rested only on the append-only `channel_prices` table plus the recorded `priceVersion` — a forged current price table would still let every old bill verify. The operator's existing Ed25519 head key (`OXSUM_HEAD_SIGNING_KEY`) now signs a checkpoint per `(channel, model, priceVersion)`; a bundle embeds it as `price`, and `GET /api/v1/log/price-head` serves any version's checkpoint on demand.

Judgment calls, recorded:

- **A priced row *is* a checkpoint — `SignedTreeHead` is reused, not generalized.** The `tlog-checkpoint` body already carries exactly three facts (origin, size, root), and a versioned price row maps onto all three: `origin = oxsum/price-catalog/<channel>` (one catalog per channel — the unit an operator names and a witness keys on), `size = priceVersion` (canonical decimal, monotone, append-only — semantically right, not just a free slot), `root` = a 32-byte row hash. `sign_head` renders the note verbatim; no `SignedNote` abstraction was built for one more call site. Model stays out of the origin — `model_name` allows spaces that `Origin` forbids — and rides inside the hashed row instead.
- **Per-(channel,model,version) is the only granularity that survives the issue's own test.** "v1 stays verifiable after v2" demands a checkpoint that names the stored row, not the catalog: a per-channel-covering-all checkpoint minted today cannot speak for a v1 bill. The issue's `?channel=` shorthand resolved to `channel + model + optional version` (absent → latest, mirroring `event_price`'s `pinned`) — a channel alone cannot select among its models.
- **The hashed bytes are the bundle's bytes.** `PriceRowV1 {v, channel, model, version, weight, price}` serializes to deterministic serde JSON (declaration-order keys); the checkpoint's `root` is `Hash::digest("oxsum/price-catalog/v1", bytes)` — the tagged-BLAKE3 convention `oxsum/idempotency/v1` set. The bundle carries that exact string as `price.row`, and `verify_price_head` hashes it verbatim — the same pinned-wire rule `verify_charge` applies to settlement descriptions — so the verifier never re-serializes and the server never gets a second spelling. `weight` is inside the hash (versioned with the price, #168); `created_at`/`channel_id` are out — the row's identity is its content, and a timestamp would force time serde into the canonical form.
- **Signing stays stateless: no snapshot table.** Deterministic Ed25519 over (key, origin, version, row) means re-signing a stored v1 row later renders the byte-identical note — append-only `channel_prices` is already the store, and a snapshot table would only drift. `receipt_proof` signs on demand through `self.store.pool()` (the shared `oxsum` schema rides the tenant ledger's pool), so no `&Db` threads through its callers.
- **The bundle's `price` is self-contained and optional.** It carries `channel`/`model`/`version`/`row`/`note` plus the key (`keyName`, `publicKey`, `keyHash` — hex inside the bundle, what `PublishedKey` reads; the HTTP endpoint keeps base64 beside `SignedTreeHeadRes`). Old verifiers parse new bundles fine — serde ignores the field — and absent means "not bound", never failed: a top-up names no price, an older bundle carries none, a key-less deployment signs nothing. `verify_price_head` reports the binding by name (`Absent`, `WrongOrigin`, `BindingMismatch`, `HashMismatch`, `Unsigned`, `BadSignature`), so `/verify` renders a third sentence on a passed check rather than folding price into the pass/fail.
- **`/log/price-head` sits behind `require_principal`, like `/log/head` and `/pricing`.** Only `/log/key` is public — a checkpoint answers "what did the catalog hold", which is caller's business, while the key that makes it meaningful is everyone's.

Pinned by `crates/core/src/heads.rs` unit tests (canonical row byte-stable, sign/verify round trip, re-sign determinism), `crates/core/tests/price_heads.rs` (pinned-vs-latest read, the bundle's link fields against the entry's record, v1-after-v2 identical note, absent on top-up/no-key/ghost price), `crates/verify`'s `price_heads` module (round trip, hash mismatch, wrong origin, wrong key, absent), and `crates/server/tests/heads.rs` (the endpoint binds the named version, latest-vs-pinned selection, 404/400/401/503 refusals).

## 2026-10-12 — The statement PDF renders in-process, finalized-only (issue #205)

The credentials-and-credit decision deferred PDF invoicing — "Lago-lite statements" shipped as data, the rendered export left for later. `GET /api/v1/statements/{statementId}/export.pdf` (and the admin twin) now answer it, rendered inside the same binary.

Judgment calls, recorded:

- **`printpdf` over embedded `typst`.** The issue offered both; the deciding constraint was the single binary plus dependency weight. `typst` embeds a whole typesetting compiler — the wrong class for rendering one fixed layout a statement needs. `printpdf` with `default-features = false` is a focused PDF writer (~140 self-contained font/shaping deps, MIT), and the standard-14 `BuiltinFont::Helvetica*` mean no font bytes ship — the viewer supplies them. The QR to `/verify` is drawn as vector rectangles (`qrcode` computes the modules; `DrawPolygon` fills them), so no image codec or `images` feature is needed.
- **Finalized-only is the document's own rule, not a route preference.** A draft is the platform's working copy — the API never shows it to the organization — so the export answers a draft with the same 404 the detail endpoint gives (the org route also scopes to the caller's own org). The renderer itself re-checks `status == Finalized`, so the bytes can never describe a working copy even if a caller forgot the guard. Rendering reads the stored `statement_lines`, so the PDF cannot disagree with the detail endpoint.
- **The body is binary, not the JSON envelope.** `export.pdf` answers `application/pdf` with `Content-Disposition: attachment` — a download. It is the one response under `/api/v1` that does not wear the `{"data":…}` envelope; that is the shape a PDF must have, and the route name says so on the wire.
- **The QR and footer carry the proof window.** `logFromIndex`/`logToIndex` print beside a QR that opens `/verify`, so a paper statement points back at the checkable record — the verifiable-billing signature, not decoration. The URL is `OXSUM_PUBLIC_URL` + `/verify` when the deployment names one, else the relative `/verify` path the QR still encodes.
- **The renderer lives in `crates/core`, not the server.** `render_statement_pdf` is bytes-in/bytes-out over the stored types — domain logic, not HTTP — so it stays unit-testable (parse-back, totals carried, draft refuses) and a future rendering change is a core change, not a route one.

## 2026-10-12 — sqlx features: one source of truth at the workspace root

`crates/doubleentry`'s optional `postgres` backend binds `uuid::Uuid` and `time::Date` through sqlx (`storage/postgres.rs`), so it needs sqlx's `uuid` and `time` features — but the workspace-root sqlx entry only carried `runtime-tokio-rustls` + `postgres`. The two declaration sites disagreed, and every drift like that is a standing invitation for cargo to build sqlx (and `sqlx-postgres`) twice.

The fix puts the union — `runtime-tokio-rustls`, `postgres`, `time`, `uuid` — on the workspace entry and makes doubleentry inherit it (`sqlx = { workspace = true, optional = true }`, marked as an oxsum change since doubleentry is vendored). Deliberately still no `macros`: every query is built at runtime with `sqlx::query`, and the `query!` family would pull `sqlx-macros-core` → every driver → MySQL's `rsa` with its unfixed timing-sidechannel advisory (RUSTSEC-2023-0071).

Same change also moves lettre from `tokio1-native-tls` to `tokio1-rustls-tls` (with default features off — lettre's default set includes `native-tls`, which trips lettre's own tokio1/native-tls feature guard). openssl-sys had exactly one reverse dependency in the graph — lettre — so it drops out entirely, and the workspace is now on a single TLS stack (rustls, ring provider).

## 2026-10-12 — A tag scope is a column on the TPM window, not a sibling table (issue #209)

The protection decision's tag-scoped TPM buckets (Each / Concrete / Total, the 2026-10-05 "Attribution" line) land on top of #203's per-key window. The question was where a scope lives: a second table of scoped rows, or a slice name on `oxsum.tpm_windows` itself.

- **A scope is a column, not a table.** A scoped bucket is the same thing the plain window already is — a trailing-minute sum of settled tokens for a key — narrowed by which slice of the key's turns the rows count. `oxsum.tpm_windows` gains `scope text NOT NULL DEFAULT ''` and the primary key becomes `(key_id, scope, window_start)`; the plain window keeps `''`. Scoped rows ride the same `record_usage` upsert, the same trailing-minute read, and the same prune-on-write — a sibling table would have re-implemented all three.
- **Scope names are `tag:<name>=<value>` for Each/Concrete, `tag:<name>` for Total.** A scoped name always carries a tag, so it can never collide with the empty scope the plain window holds. A turn accrues into a bucket only when its `metadata` selects the scope — Every value under `each`, the one pinned value under `concrete`, any value under `total` — and an untagged or non-matching turn writes the plain row alone.
- **Accrual fans out inside the settle's transaction.** `record_usage_balanced` reads the key's `tokens_per_minute_scopes` off `api_keys` and upserts each bucket the turn's tags select, so a scoped cap reads the same settle that fills it and a replayed usage row still rolls nothing. The scopes live on the key row — a re-scoped `PATCH` shapes the next settle, not the one already in flight.
- **Admission checks every bucket the turn hits.** The gateway reads `tpm_window_scoped` per applicable bucket beside the plain `tpm_window`, refusing `TOKENS_PER_MINUTE` on `window-actual + estimate ≤ cap`. A full `each` bucket refuses only that value's turns — the other values' buckets stay open — and `POST /api/v1/holds`, carrying no tags, can only be refused by a `total` bucket already full.
- **Cardinality is bounded by the window, not by a cap on the map.** Each `each` row is still a trailing-minute bucket, so a high-cardinality tag value set expires with the 60 seconds it landed in — the bound the decision asked for falls out of the storage shape, not out of a counter.
- **The wire is one object naming its form.** `tokensPerMinuteScopes` maps a tag key to `{"each": N}` / `{"concrete": V, "tokensPerMinute": N}` / `{"total": N}`; serde derives no flat shape matching all three, so `TpmScope`'s `Serialize`/`Deserialize` are written by hand. `crates/server/openapi.yaml` → 0.53.0.

Pinned by `crates/core/tests/tpm_windows.rs` (a tagged settle fans into each applicable bucket, a non-matching turn writes only the plain window, scoped reads sum one bucket) and `crates/server/tests/gateway.rs` (a full `each` bucket refuses only the capped value, `concrete` counts only its pinned value, `total` aggregates across values, an untagged turn ignores the scopes) and `api.rs` (malformed scopes are 400).
## 2026-10-13 — `oxsum-integrations`: the third-party clients leave `oxsum-server`

`oxsum-server` had grown past 10k lines. A dependency analysis (`use crate::` per module) showed the handler layer — gateway, admin, routes, metering, bills, ws — is coupled through the single `AppState` every handler takes: `state.db`/`state.tenants` appear in all of them, so extracting the gateway (the "obvious" candidate at ~2.7k lines) would have meant either moving `AppState` (inverting the dependency) or a parallel state struct with every handler signature rewritten. Neither is a minimal, behavior-preserving split, so the handler layer stays put.

The clean seam the analysis did find is the four external service clients — `oauth` (GitHub), `stripe`, `antibot` (Turnstile), `mail` (SMTP) — each with zero `crate::` dependencies, already `pub`, and already imported by the integration tests as public API. They move to a new `oxsum-integrations` crate (~630 lines); `oxsum-server` re-exports the modules (`pub use oxsum_integrations::{antibot, mail, oauth, stripe}`), so every existing path — `crate::oauth::PROVIDER` in `routes.rs`, `oxsum_server::stripe::Stripe` in the tests — keeps resolving and the move is a pure relocation plus two Cargo.toml edits.

## 2026-10-13 — The next phase deepens verifiability before widening the surface

With the productization roadmap closed (P1-1 through P8-4) and the server split landed, this entry settles what the next batch builds toward. The survey against LiteLLM, Portkey, Helicone, Lago, OpenMeter, Kong AI Gateway, Bifrost, TensorZero and new-api produced twelve candidates; what survives below is filtered through oxsum's own shape — a verifiable credit wallet whose differentiator is that a user can check their bill without trusting the operator — and through what already ships (budgets, RPM/TPM/tag scopes, a versioned itemized PriceBook, deposits and a Stripe rail, statements plus PDF, the full twelve-event webhook catalog, the metering API, the jobs layer, reconciliation, signed tree and price heads, and in-browser `verify_bundle`).

The filter drops anything that re-implements a settled non-goal or is already covered. Notification UX and rate-limit response polish come off because the webhook catalog already carries `balance.low`/`anomaly.detected`/`statement.payment_overdue` and the refusal path already emits `Retry-After` plus `X-RateLimit-*`. Signed spend mandates, x402-style payment rails and multi-currency display stay out: they belong to a payment/agent-runtime surface the product boundary already declined, and `deposits.rail` keeps the seam if that ever changes. End-user bill-back reports and anomaly scoring beyond settlement kinds are real but narrower than the four below; they wait in the backlog. The chosen four, each its own issue and PR:

- **Q1 — Anchor statements and usage rollups to the transparency log.** The tree head and `sign_price_head` already sign ledger heads and priced rows, but a statement's *inputs* — the `usage_daily` rollup rows and the settled entries a statement aggregates — are not yet themselves in a log. A verified bill today proves the arithmetic over the rows the server chose to show, not that those are all the rows. Anchoring the statement's covered window (`log_from_index`/`log_to_index` already stored) and the rollup it summarizes into the append-only log closes the "prove nothing was left out" half of verifiability — the part a competitor's signed bill cannot offer.
- **Q2 — Upstream price-catalog sync with drift alerts.** Pricing is fail-closed: a dimension the book cannot bill settles `unpriced`. That makes a *stale* upstream catalog worse than an absent one — it silently under- or over-charges until someone notices the margin view drift. A job on the existing jobs layer diffs each channel's `upstream_prices` against the upstream's live catalog on a schedule, appends the drift as a reconciliation entry, and raises `anomaly.detected` over webhooks. LiteLLM's static cost map is the cautionary precedent; oxsum's versioned PriceBook already knows how to record the correction.
- **Q3 — Forecasted budget exhaustion.** `usage_daily` gives a per-key daily spend series and `budget_duration`/`spend_limit_minor` give the ceiling; a linear projection of the current window against the limit yields an exhaustion date, surfaced as a new webhook event and a dashboard line. This is the cheapest of the four — it reads two existing tables — and the most directly useful to a user deciding when to top up.
- **Q4 — OpenTelemetry trace export with GenAI semantic conventions.** `/metrics` is the only telemetry today: Prometheus counters and gauges, no per-request trace. OTLP export emitting `gen_ai.*` spans (system, model, token counts, the hold/settle timing) lets an operator drop oxsum into an existing observability stack without building a custom scraper — the standard gateway telemetry LiteLLM and Portkey already ship. Optional and env-gated, like SMTP.

Order: Q3 first (smallest, unblocks nothing else), then Q1 (the differentiator), then Q2, then Q4. Q2 and Q4 are independent of Q1 and each other and can interleave by reviewer bandwidth. Each lands contract-first: `openapi.yaml` before implementation, the behavioral docs in the same PR, per the hard rules.

## 2026-10-13 — Forecasted budget exhaustion (issue #220)

Q3 closes the "the refused hold is the first sign of trouble" gap: a periodically-limited key now warns before it exhausts.

- **The forecast rides the settle transaction, not a scheduled job.** `record_usage_balanced` already owns the transaction that attributes spend to a key, so `forecast_key_budget` runs there: it reads the key's constraint, sums the window's committed spend off the ledger postings — settled charges in the window plus every pending-layer posting whatever its booking date, because an outstanding hold still binds money now — projects the crossing, and when it lands inside the window enqueues `budget.exhaustion_forecast` beside the usage row. A job scanning `usage_daily` on a timer would lag by up to an interval and could miss a same-day burst entirely; the settle is the moment the projection changes, so the settle is where it is computed. It also means the event needs no new rollup read — the committed-spend predicate is the same one `hold_for_key` enforces, so the forecast and the refusal can never disagree about what counts.
- **The latch is a date on the key row, not a new table.** `api_keys.budget_forecast_sent_for` (migration 0033) records which window start the event was last sent for; `enqueue_budget_forecast` writes it inside the same transaction that enqueues the delivery, under the `FOR UPDATE` the settle already takes on the key row — two concurrent settles cannot each think they are first. `update_key_constraints` clears it (any `PATCH` re-arms the warning, since a changed limit is exactly when the user wants to hear the new projection), and a new window's different `window_start` makes the old latch inert. One event per key per window is the right cadence: it means "check your budget", not "a turn ended".
- **The projection is integer minor-days over the window's real length.** `forecast_exhaustion` projects `committed * window_days / elapsed_days` and solves `ceil(limit * elapsed / committed) - 1` for the crossing day in i128 — money never floats. `elapsed_days` counts `window_start..=today` inclusive (a partial first day counts as one — the conservative floor that warns earliest), `monthly` uses the calendar month's real length (a 31-day October burns slower per day than a 30-day November), `committed <= 0` projects nothing, and `committed >= limit` projects to today with `daysRemaining: 0` — "the window's own close exhausts it". A projection landing past `window_end` returns nothing: a slower burn never exhausts this period, so there is nothing to warn about. The same function feeds the keys dashboard's per-key `budget` line, so the page and the webhook can never disagree.
- **Only attributed, periodically-limited settles compute anything.** An unattributed settle (`provenance_actor` null) or a key without `spendLimitMinor` + `budgetDuration` skips the whole branch — a cumulative-only limit has no window to project inside.

Pinned by `crates/core/tests/webhooks.rs` (the projection math across daily/weekly/monthly including the real month lengths, and the once-per-window latch with re-arm on `PATCH`) beside the existing catalog tests. Contract `openapi.yaml` 0.54.0.

## 2026-10-13 — Statement anchors commit the window fold and the rollup, not a hash of the page (issue #221)

Q1 closes the "prove nothing was left out" gap: a verified bill today proved the arithmetic over the rows the server chose to show, not that those are all the rows. The anchor binds two digests, each chosen for what it commits.

- **The anchor is a ledger marker, not a table column alone.** Finalizing a statement appends one entry to the organization's own ledger — a posting that moves no money (`debit Equity:Adjustments` / `credit Equity:Adjustments` at 1 minor, balanced against itself — a posting against `Assets:Cash` or `Income:Usage` would be a ledger lie) — whose document hash is the *window* digest and whose description carries the *rollup* digest. Because it is an entry, it is inside the Merkle log: the append-only property is inherited, not re-implemented, and `anchor_entry_id`/`anchor_log_index` on the statement row are just coordinates pointing at it.
- **The window fold is over `contentHash` bytes, not ids.** `windowHash` digests `oxsum/statement-window/v1` over the concatenated 32-byte content hashes of every entry the statement covers, in `log_index` order — the same select `draft_of` ran to aggregate the lines. A dropped, inserted or reordered entry changes the fold; a client that recomputes the covered set from `GET /api/v1/entries/{id}/proof` catches all three without trusting the statement's own span fields. The span (`logFromIndex`/`logToIndex`) rides the rollup as a hint, not the check itself.
- **The rollup re-serializes the served answer, pinned to canonical bytes.** `rollupHash` digests `oxsum/statement-rollup/v1` over `serde_json::to_string(&StatementRollupV1)` — declaration-ordered camelCase fields, `Option`s spelled `null`, `v: 1` so a `V2` would hash under a new domain. `oxsum-verify` re-declares the struct wire-only rather than importing `oxsum_core` (the verifier must stay wasm-clean and the crate boundary is the point), and the byte-stability test in `heads.rs` pins the exact string — a field renamed or reordered would fail every anchor already issued. The verifier's `json!`-literal fixture sorted keys alphabetically and digested different bytes; building the fixture through the same struct removed that class of divergence by construction.
- **The signed half attests the head *after* the marker.** On a deployment with `OXSUM_HEAD_SIGNING_KEY` the anchor's `note` is the operator's signature over the tree head at exactly `logIndex + 1` — the head that already contains the marker — so the marker's inclusion is proven, not merely claimed. Unsigned deployments serve the four key fields `null` and verification still binds both digests: the signature layer is additive, and `verify_statement_anchor` returns before consulting `expected_head` when `note` is absent.
- **`keyHash` is a selector, not a second check.** The four-byte hex locates which signature line inside the note to verify; `publicKey` is what actually verifies it. Publishing both means the caller needs no out-of-band key map — the anchor is self-contained.

The detail contract grows `anchor` on both statement-detail endpoints (org and admin serve the same document); `openapi.yaml` 0.55.0. Pinned by `crates/verify`'s `statement_anchor` module (genuine signed and unsigned, truncated and reordered windows, altered lines, wrong origin, later head, foreign key, tampered note) and `crates/server/tests/statements.rs`'s two end-to-end runs against the real API answer.

## 2026-10-13 — Live upstream catalogs are probed, drift is open state, and the alert broadcasts to every subscriber (issue #222)

Q2 closes the "a stale upstream price silently mischarges the margin" gap: pricing is fail-closed, so a stored `upstream` rate that no longer matches the provider costs the deployment on every turn until someone notices the margin view drift.

- **The live catalog is only what the channel's base URL already serves.** Real upstreams (OpenAI, Anthropic) expose no machine-readable price list, so "probe the live catalog" can only honestly mean the oxsum-compatible `GET {baseUrl}/api/v1/pricing` the channel fronts — an oxsum-fronting-oxsum deployment, or any compatible proxy. The job sends `Authorization: Bearer <api key>` so a gated catalog answers, parses `{data:{models:[{model, upstream}]}}`, and diffs each `upstream` block against the stored `upstream_prices`. A channel that cannot be probed — unreachable, or an upstream serving no such catalog — records a `upstream_price_probes` row for that run (`unreachable`, `invalid`, `auth`) rather than failing the pass: drift is *detected*, never inferred from silence, so an unprobeable channel neither produces drift rows nor fails the job.
- **Drift is open state keyed by `(channel, model, field)`, not an event log.** Each divergent field lands one `oxsum.upstream_price_drift` row — `stored_minor`, `live_minor`, `detected_at`, `last_seen_at`, `resolved_at NULL` — upserted on every detection so a still-divergent field bumps `last_seen_at` instead of double-counting, and resolved the moment stored and live agree again. Reconciliation's new `upstream_price_drift` class therefore reports a stable count of *outstanding* drift (platform scope, `"platform"` samples), and a rate that re-converges clears itself with no operator touching a queue.
- **The alert broadcasts to every enabled `anomaly.detected` subscriber.** `webhook_deliveries.organization_id` is NOT NULL, so platform drift cannot use the org-scoped `enqueue_event`; a new broadcast selects `(endpoint_id, organization_id)` for every enabled `anomaly.detected` endpoint and stamps each delivery with that endpoint's own org. That is safe because `upstream` rates are already public on `/api/v1/pricing` — broadcasting leaks nothing the catalog withholds — and it means a self-hosted operator and the platform see the same alert.
- **The job rides the existing jobs layer and is env-gated like webhooks.** `SYNC_UPSTREAM_PRICES` is a daily periodic kind pushed only when `ctx.webhook` is present — its `(reqwest::Client, SecretKey)` supplies both the fetcher and the `api_key_sealed` unsealer — *and* `OXSUM_UPSTREAM_PRICE_SYNC` is set. A deployment without the flag keeps its channels but never probes them, matching the SMTP/OTLP opt-in pattern.

Contract `openapi.yaml` 0.56.0; migration 0035.

## 2026-10-13 — Turn traces export as OTLP `gen_ai.*` spans, batched on the app's own runtime (issue #224)

Q4 closes the phase: beside the `/metrics` scrape's aggregates, a deployment that wants per-turn traces gets them — optional, env-gated on `OXSUM_OTLP_ENDPOINT`, unset costing nothing.

- **The span is a handle, never entered.** The turn span is created in `run()` once `GatewayRequest::parse` knows the model, held in the `Plan`, and dropped only when the spawned settlement task ends — so its lifetime *is* the turn, including settle. Holding the entered guard would leak the span into whatever task next borrows the thread; `Span::record` fills `gen_ai.usage.*` on the non-current handle once usage is known. Nothing else changes about how the relay works.
- **Attributes carry the bounded vocabulary, same rule as `/metrics` labels.** `gen_ai.operation.name` is the surface's wire dialect (`chat`/`embeddings`/`rerank`), `gen_ai.system` the channel protocol (`openai`/`anthropic`), `gen_ai.request.model` the caller-named model, `gen_ai.usage.input_tokens`/`output_tokens` recorded at settlement. Never a request id, organization, or key — a span per turn already is the trace; high-cardinality joins belong to the collector's store, not to span attributes.
- **Hold and settle are span events, not child spans.** `hold taken` (`oxsum.hold.freeze_minor`) logs when the ledger hold lands, `settle written` (`oxsum.charged_minor`, `settlement.kind`) when the ledger write returns — both via `in_scope`, which emits the event *on* the span without making it current. Reading them against the span's own start/end yields the three timings the issue asks for: parse→hold is admission+freeze, hold→settle is the relay, settle→end is the write. Child spans would need `FollowsFrom` links and a second exporter hop for no extra answer — events carry the same timestamp on the same span.
- **The exporter batches on tokio, not on the SDK's own thread.** The default `with_batch_exporter` drives exports from a dedicated thread where no reactor runs, which the async `reqwest-client` cannot work under (it panics `there is no reactor running` on the first flush). `experimental_trace_batch_span_processor_with_async_runtime` supplies the `runtime::Tokio`-aware `BatchSpanProcessor`, so scheduled and shutdown flushes spawn onto the application's own multi-thread runtime. The alternative — `reqwest-blocking-client` — would have kept the sync processor but added a second reqwest client flavor to the tree; the async processor keeps one.
- **One subscriber, one filter.** `telemetry::init` composes `Registry` + `EnvFilter` + fmt + the OTLP layer in a single `.init()`, installed before `Config::from_env` so a bad environment still logs. `RUST_LOG` quiets traces the way it quiets logs; an unset endpoint installs exactly the fmt pipeline the binary had before. Shutdown flushes through `OtelGuard::shutdown` on the drain path — a failed flush warns and exits, it does not crash a deployment that already served.
- **HTTP, not gRPC.** `http-proto` + `reqwest-client` + webpki-roots/ring keeps the exporter on the workspace's one TLS stack and off tonic/prost codegen.

No contract change — the surface is a collector's, not oxsum's; `openapi.yaml` stays at 0.56.0.

## 2026-10-15 — Billing records name their settlement entry (issue #236)

A billing record named only its `requestId`, so reaching the charge's proof meant deriving the ledger key (`req-<requestId>:settle` → `settlement_key_for` → `entry_id_for`) — the request-ID→proof path existed in principle and in no usable form, since the derivation is oxsum's own convention and not one a caller should have to know.

- **`RequestEntry` and `SettledTurn` carry `entry_id` straight off the stored entry** — the id the ledger already assigned, read where `settlements_page` walks the log, so it can never disagree with the entry the proof endpoint would serve.
- **The path is `billing-records` → `entryId` → `GET /api/v1/entries/{entryId}/proof`.** No new endpoint: the proof surface already exists and the record now names exactly what it takes. `requestId` stays — it is still the id the caller chose and the `x-oxsum-request-id` header stamped.

Contract `openapi.yaml` 0.58.0 (`BillingRecord.entryId`, required).

## 2026-10-15 — Pool saturation answers backpressure, not an outage (issue #235)

A 10-way real-upstream burst returned six HTTP 500s — `pool timed out while waiting for an open connection` — because a `sqlx::Error::PoolTimedOut` arrived as `WalletError::Storage` and was rendered `INTERNAL_ERROR`. For a billing gateway that is the worst failure shape: it looks like an outage, is billed to nobody, and invites the retry storm that deepens the saturation.

- **Audit result: no connection is held across the upstream call.** The issue hypothesized one; the code holds none — every gateway step checks a connection out per operation (`hold_for_key` takes one for its transaction, `Plan` carries pool handles only, the relay phase is connection-free). The exhaustion is burst pressure: admission does `join!`-parallel reads plus `note_open_hold` plus the hold transaction, settle does the ledger write plus `record_usage_balanced`'s transaction — each request peaks at two-to-three simultaneous checkouts, and per-key serialization (`pg_advisory_xact_lock`, `FOR UPDATE` on `api_keys`) holds those checkouts while same-key work queues.
- **`WalletError::PoolBusy` is its own variant**, produced by `From<sqlx::Error>` on `PoolTimedOut`/`PoolClosed` and by `From<PostgresError>` when the engine wraps the same inside `PostgresError::Database` — the ledger paths reach the pool through `PostgresStore`, so both doors into `WalletError` have to know it.
- **Both surfaces answer 429 `POOL_BUSY` + `Retry-After: 1`.** `/api/v1` emits the envelope code; the gateway emits `rate_limit_exceeded`/`overloaded_error` so SDKs retry. Deliberately *no* `X-RateLimit-*` headers: those describe a limit the caller owns, and a shared pool is not one. `Retry-After` alone is the honest signal — a refusal without it invites the storm.
- **The default pool rises 10 → 20.** Each in-flight turn holds no connection during the upstream call, so concurrency is bounded by burst checkouts, not by turns; 20 covers the join-peaks of a small deployment while staying far under PostgreSQL's default `max_connections = 100`, and the runbook documents the sizing rule (twice the expected concurrent gateway requests, floor 10).
- **The load check is `#[ignore]`d, run on demand** (`crates/server/tests/gateway.rs::a_saturated_pool_refuses_backpressure_never_a_500`): fifty concurrent turns on a three-connection pool against a 250 ms upstream; zero 500s, every refusal carries `Retry-After`. It stays out of the gate for the same reason `real_upstream` does — CI validates it the day the pool code moves, not on every PR.

Contract `openapi.yaml` 0.57.0 (`POOL_BUSY` in both error-code enums).

## 2026-10-15 — Refused requests are billing records, marked not hidden (issue #233)

A 400 or 429 used to leave only a server log line: no hold, no settlement, nothing a caller could quote. Now admission refusals write a **marker entry** — a self-balancing 1-minor round-trip on the adjustments account (the statement anchor's shape), whose description is a settlement record of kind `refused` — beside a `usage_records` row carrying `status`/`failure_reason` (migration 0037).

- **In the ledger, not a side table.** The requests page, billing-records and the proofs all read the log already; a refused turn in the same log shows up on all three with no second read path. The marker moves no money — debit and credit of the same unit on adjustments — because a refusal is a billing *fact* to prove, not a charge.
- **`reason` rides the description.** The settlement description gains an optional `reason` — the caller-facing words the answer already carried — under the hash, so a bill proves the refusal it was given rather than one written afterwards. Errored turns (`upstream_error`, `upstream_unreachable`) carry the upstream refusal's message the same way; `swept` records its own.
- **`status` is derived, not stored twice.** `settled` / `errored` / `refused` map off `kind` — no second taxonomy to drift from the kinds the record already writes.
- **`request.settled` fires for refusals.** The event names `kind: refused` with `chargedMinor: 0`; subscribers that filter on kind keep behaving, and the refusal is finally visible to a webhook-only integration. It is deliberately *not* `anomaly.detected` — a rate limit doing its job is not an anomaly.
- **Only admission refusals mark.** `GatewayError::refusal_reason` admits exactly the refusal variants — a 5xx/storage fault has no caller-facing cause to record, and an upstream error is a settled turn on the usual path, never a marker (a post-hold `Err` would double-record the request).

## 2026-10-15 — Missing usage settles degraded: input-only, marked, reconciled (issue #230)

A turn upstream answered but never counted used to bill a local estimate of both sides — a charge for tokens nobody counted. The degraded policy narrows it: a turn that *ends* (completed or truncated) without a usage report bills only the locally-estimated input, recorded `estimated` with the anomaly already enqueued.

- **Only the countable side bills.** The request's own input is the one figure the caller cannot dispute — it is their text. Output was forwarded but no adapter read a count for it, and an undercount is the platform's cost where an overcount is the user's (the `ESTIMATE_WINDOW` doctrine already says so).
- **The columns keep provenance apart.** `usage_records` gains `provider_*` and `local_*` count pairs (migration 0040): a reported turn writes `provider_*`, a derived one `local_*`, each NULL when that side never happened — "not counted" is a different statement from a counted zero, which is what the degraded settle's NULL `local_output_tokens` records.
- **`usage_details.estimation` is the paper trail.** Method (`tiktoken-o200k` or a token-array's exact count), scope (`input-only` for the degraded case, `forwarded-prefix` for a client cancel), and the reason ride the row, so reconciliation sees how a figure was produced without re-deriving it from `kind`.
- **Client-cancelled is the neighbouring doctrine, not this one.** A cancellation pays what was delivered — the forwarded prefix is countable text, so its local estimate still stands on both sides. The kind stays `client_cancelled`, distinct from `estimated`.
- **Never a silent zero.** A degraded row that billed nothing would look like a refusal that served an answer. `kind: estimated` + the `anomaly.detected` delivery + the `local_*` columns together make the under-bill reviewable rather than invisible; a `released` metering row or a `swept` turn is the only zero-count shape left, and both carry `CountsProvenance::None` explicitly.

The tests name the contract: `a_missing_usage_report_settles_under_the_degraded_policy` (whole-answer case, provenance columns, the anomaly delivery) and `a_truncated_stream_settles_under_the_degraded_policy` (the SSE the issue cites).

## 2026-10-15 — Budget visibility: limit/consumed/percent per scope, thresholds as events (issue #232)

Usage stats answered token counts and charges, never "how much of the budget is burned." `GET /api/v1/budget` now returns the snapshot — `organization` (credit line beside the balances), `member` (the acting seat's `budgetLimitMinor`), `keys` (each live key in scope with `spendLimitMinor`, `budgetDuration`, `windowStart`) — and `budget.threshold_crossed` turns armed percentages into webhook events.

- **`committed` is the enforcement figure, not a second opinion.** The snapshot reads settled spend plus outstanding holds off the ledger postings — the same sums `key_committed_in` and the forecast compute — so the percent a caller sees is the percent the hold check enforces. A derived figure (usage rows, `usage_daily`) would drift from the books.
- **Thresholds are configured per endpoint, like `balanceLowMinor`.** `budgetThresholdsPercent` arms which consumption lines an endpoint hears; the eval runs inside `record_usage`'s transaction across the three scopes that carry a numeric limit — the settle's key, the minting member, the organization's credit line. No in-dashboard-only alerts: the event is the alert, and `GET /api/v1/budget` serves the dashboard's percent without a second accounting.
- **The latch is a row, not a flag.** `budget_threshold_fires` keyed by `(endpoint, scope, scope id, window, threshold)` makes "once per crossing" a uniqueness constraint written inside the settle transaction — the concurrent-burn guarantee the acceptance criteria asks for, and a natural audit trail of which lines fired when. `window_start` latches cumulative scopes on the epoch; a constraint PATCH deletes the scope's rows, re-arming the way `budget_forecast_sent_for` resets.
- **Percentages floor, and 1–100 bound the config.** `consumedPercent` is `floor(committed * 100 / limit)` — a whole number matching the threshold dimension exactly, so the line the endpoint armed is the line `consumedPercent` displays.

## 2026-10-15 — doubleentry's Postgres tests take a `DATABASE_URL` over Docker (issue #241)

The vendored ledger's `tests/postgres.rs` always started a throwaway container, so a developer without Docker could not run the conformance suite at all. `DOUBLEENTRY_DATABASE_URL` (falling back to `DATABASE_URL`) now points the suite at any reachable cluster: the harness creates each test's `de_<n>` scratch database through the maintenance connection and `DROP DATABASE … WITH (FORCE)` cleans it up when the harness drops — same per-test isolation, no container.

- **The container stays the default.** With neither variable set the behaviour is byte-for-byte what CI runs today; the external path only exists because a developer named it.
- **The URL's own database is never touched.** It supplies the server and credentials; the suite works in `de_<n>` scratch databases beside it, so pointing at the dev `oxsum` database is safe.
- **`start_isolated` prefers a dedicated container even under `DATABASE_URL`, falling back to the shared cluster only without Docker.** Its dedicated instance exists because deferred sequencing advances on a cluster-wide watermark — and CI proved the caveat is not just a slowdown: a neighbour's held-open transaction on the shared cluster stalls the watermark and the conformance suite reads unsequenced entries ("the partly-settled invoice is not open"). A Docker-less environment gets a scratch database instead and accepts that residual; CI keeps the real isolation.
- **Cleanup runs in `Drop` under `Handle::try_current`, best-effort.** A normal test exit drops the database inside the still-live runtime; a hard abort leaves one named `de_<n>` database, identifiable and safe to drop by hand — strictly better than the orphaned containers the issue cites.

All doubleentry tests must pass either way.

## 2026-10-15 — External witness anchoring: published notes over Rekor submission (issue #231)

The operator-signed heads attested *a* history the operator claims. Anchoring makes it *the* history witnesses saw: the `witness-checkpoints` periodic job (hourly, like the daily kinds registered in `oxsum.jobs`) publishes each organization's signed head to every endpoint in `OXSUM_WITNESS_URLS` as a C2SP `tlog-witness` `/add-checkpoint` body — signed note plus consistency proof from the previously anchored head — and stores the cosigned notes they return.

- **Witness set: operator-configured URLs, not embedded identities.** Any C2SP `tlog-witness` works — an internal sidecar, a hosted witness, or several for quorum. The crate's `Witness` state machine (trust, consistency verification, fork refusal) deliberately runs on their side; oxsum only publishes. We did not build an in-process witness because the operator witnessing its own log proves nothing.
- **Anchoring target: the signed notes themselves, published at `GET /api/v1/log/checkpoint`.** The issue offered Rekor or a well-known URL; the endpoint is the URL. Rekor submission would add a Fulcio identity dance (Rekor anchors signatures made under Sigstore-issued certs, not bare Ed25519 keys) for marginal gain over notes any verifier can fetch and check against keys both parties publish. If a witness with Rekor submission appears later, it plugs in as another URL.
- **State is two tables, not derivation.** `witness_checkpoints` (one row per org) marks pass progress and serves the endpoint; `witness_cosignatures` is append-only per (org, witness, head size) so the trail survives checkpoint advances — ON CONFLICT DO NOTHING keeps re-delivery idempotent.
- **Partial anchoring counts.** A witness that times out or refuses is logged and skipped; the pass still records the cosignatures that answered and retries the rest on the next run. The inverse — a head contradicting a stored checkpoint or a shrunk log — fails the job into `dead`, surfacing through the `jobs_dead` drift class.
- **Dashboard surface on `/dashboard/log`**, not the public `/verify` page: the checkpoint is per organization and `/verify` is anonymous.

## 2026-10-15 — React dashboard scaffold: Vite + embedded dist, not Next.js (issue #234)

The dashboard migrates to a standalone React frontend (`web/`) served by the same binary.

- **Vite, not Next.js.** Next's value is SSR/ISR — a server we deliberately do not want: the deployment promise is one binary, and a Node runtime (or a second process) breaks it. Vite emits plain static `web/dist`; the New API reference builds the same way (React + static assets into the Go binary). `base: "/app/"` so the SPA mounts beside the Topcoat pages without touching their routes.
- **Embedding via `rust-embed`, not a static dir.** `#[folder = "../../web/dist"]` puts the bundle inside the oxsum binary — the Dockerfile's new `frontend` stage runs `npm ci`, `openapi-typescript` codegen against `crates/server/openapi.yaml` (the typed client can never drift the contract), and `npm run build`, then the Rust builder embeds the result. `debug-embed` reads dist from disk in debug builds so a frontend rebuild needs no cargo step; a committed `dist/index.html` placeholder keeps the folder present on fresh checkouts. The runtime image is unchanged — still one binary.
- **Serving at `/app` inside the API router**, not the fallback: the Topcoat catch-all must not swallow SPA routes. Missing asset paths answer a real 404; extension-less routes fall back to `index.html` for the client router.
- **Components: Tailwind v4 + DESIGN.md tokens, Cult UI deferred (superseded 2026-10-15, see below).** The issue names Cult UI as a candidate — it is a copy-in component collection that presumes Tailwind + Radix, both of which are now in place (Tailwind via `@tailwindcss/vite`, tokens mapped from `crates/web/style/main.css` onto `@theme` colors). The scaffold's primitives are hand-built to DESIGN.md; Cult UI components get vendored per-component during parity work where an animated primitive earns its weight — no blanket dependency.
- **TanStack Query + react-router** — the de-facto stack for this shape (also what LiteLLM/New API use); no Redux.
- **Topcoat stays until parity.** `web/README.md` carries the parity checklist; `/dashboard` keeps serving the authoritative pages.

## 2026-10-15 — Dashboard reads the API, not core; `/verify` stays server-rendered (issue #234)

The React dashboard reached parity, which surfaced the four reads the Topcoat pages drew straight from core and the API never served: the members list, the in-flight holds, the entries walk and the transactions walk, plus the overview's frozen and month-spend figures.

- **Four new GETs and two new balance fields, no writes.** `GET /api/v1/org/members`, `/api/v1/holds`, `/api/v1/entries` and `/api/v1/bills` answer what `db.members`, `open_holds_for_tenant`, `entries_page` and `transactions_page` already compute; `balance` gains `reservedMinor` and `spentThisMonthMinor`. Scope rules are the same ones the Topcoat pages applied — a member reads their own keys' rows plus the organization's unattributed ones, via `usage::scoped_key_ids`/`in_scope` made `pub(super)` rather than duplicated.
- **The requests page needs no new endpoint.** `GET /api/v1/billing-records` already returns `requests_page` — key/model filtering rides the URL client-side exactly as the Topcoat page's query string did.
- **`/verify` stays server-rendered, deliberately.** Verifying a bill must work without trusting the server's code — but the SPA's bundle is server-served, so an in-browser verifier would certify itself. `/verify` keeps the server-side `verify_bundle`; the React pages link out to it rather than re-implementing it. A wasm build of `oxsum-verify` would restore browser-side verification later — the crate is already wasm32-clean — but that is a verification-surface decision, not parity work.
- **The head archive rides `localStorage`, the verdict rides the API.** The browser keeps the archived `{size, root}` and asks `GET /api/v1/log/consistency` for the proof; the Topcoat page ran the same check server-side, so nothing about the trust model regresses.
- **Admin is token-gated in the SPA like `admin.js`.** The operator token lives in the browser's `localStorage` and rides `Authorization: Bearer` — never the session cookie, so the console cannot act on a stolen session. Same model as the Topcoat admin pages.
- **Chat mints a per-browser `chat` key.** `POST /api/v1/org/keys` replaces `POST /dashboard/chat/key`; the secret stays in the browser and streams `/v1/chat/completions` with it — every turn is a billed request, same as before.

## 2026-10-15 — Cult UI adopted as the SPA component layer; every API endpoint has a surface

The earlier "vendored per-component" stance is superseded: Cult UI is now the dashboard's component layer, vendored under `web/src/components/cult/` (its registry is copy-in source, not an npm dependency).

- **The `halo-*` family is the design system's backbone.** One coherent set — `halo-card`, `halo-button`, `halo-input`, `halo-progress`, `halo-switch`, `halo-tabs`, `halo-select` (on `base-select`), `halo-badge` — rather than a pick of unrelated showpieces. On top of it, `rolling-number` animates the overview KPIs, `copy-button` guards the one-time secrets, `code-block` renders receipt and proof JSON, `typewriter` carries the chat's idle state, `bg-animated-fractal-dot-grid` backs the auth pages, and `dynamic-island` reports the live billing socket in the shell's top bar — swells to show the last settlement, falls quiet when the socket drops.
- **shadcn token aliases, not a second palette.** The vendored components speak `bg-background`/`text-muted-foreground`/`border-input`/`ring` — `src/index.css` maps those onto the DESIGN.md colors inside `@theme`, so the new components wear the same palette as the hand-built primitives instead of dragging in Cult's own zinc/blue look.
- **Dependencies per component, compiled not shipped.** `@base-ui/react`, `motion`, `lucide-react`, `clsx`, `tailwind-merge`, `class-variance-authority`, `radix-ui`, `react-use-measure`, `@hugeicons/*` serve the vendored set; the heavy demo-only components (three.js globes, shader blobs) were never vendored — `@paper-design/shaders-react` dropped out with them.
- **Two small fixes carried in the vendor.** `expandable-card`'s `animateIn` prop was dead upstream — wired into `getAnimationProps` (since removed when the component was dropped); `verbatimModuleSyntax` forced type-only imports across a few files.
- **Every endpoint now has a surface.** Three pages joined the shell — `/topup` (manual rail, Stripe checkout redirect, redemption code), `/pricing` (the public catalog, the `estimate-price` calculator, the signed price-head viewer with the verifying key), `/settings` (account details, tenant id, resend-verification, `/healthz`, delete-account). Admin gained the ops that were read-only before: statement finalize / record-payment / suspend per row, organization suspend toggle in the PATCH form, and the user password-reset tool. Bills rows open a proof-bundle drawer reading `GET /entries/{entryId}/proof` directly — complementing, not replacing, the `/verify` link.
- **Bundle split by vendor.** `manualChunks` separates react/router/query, motion, and the UI vendor set so the app code and the slow-moving libraries cache independently.

## 2026-10-16 — Signed spend mandates delegate a narrower slice of a key (issue #262)

A key holder can now hand a bearer a strictly narrower authorization than the key itself: `POST /api/v1/org/keys/{keyId}/mandates` mints a signed mandate, and the delegate presents it in `X-Oxsum-Mandate` on `/v1/*` and `POST /api/v1/holds`.

- **Ed25519 over a pinned canonical body.** The signed body is compact JSON with sorted keys — the same pinned-wire rule `verify_charge` applies to settlement descriptions — so the bytes the holder signed are the bytes the verifier rebuilds. Asymmetric on purpose: a mandate must verify against a *public* key a third party can fetch (`GET` the key row's verifying half via `mandate_verifying_key`), not a server-only secret. The per-key Ed25519 seed is minted lazily on first delegation, sealed under `OXSUM_SECRET_KEY` like channel credentials, and never leaves the database.
- **`oxsum.mandates` is metadata, not the credential.** The row keeps what was authorized (cap, models, window, nonce); the signed blob is not stored — the delegate already carries it, and a stored blob would be a second credential to lose. The mandate's committed spend is summed from the ledger itself: each delegated hold/settle entry writes the mandate id as its `provenance_correlation`, and `mandate_committed_in` reads the same postings figure `key_committed_in` derives a key's spend from — so the cap counts settled charges and in-flight holds identically and can never drift from the books. `mandate_id` on `usage_records`/`open_holds` is attribution for the bills page, not the cap's footing.
- **Narrow-only at mint; enforced at admission.** A cap above `spendLimitMinor` or a model outside `modelAllowlist` is a 400 at `POST .../mandates`, so a delegate can never hold bounds the key itself does not. At request time the signature verifies first (`invalid_mandate` on any failure), then the window (`mandate_not_yet_valid`/`mandate_expired`) and the committed cap (`mandate_exceeded`) run inside the per-key lock, alongside the key's own limits — the mandate narrows, never replaces, them.
- **No revocation, by design.** Mandates are `notAfter`-bounded and short-lived; a key that must stop delegating re-times its constraints or is revoked. A revocation table would turn a stateless credential into a polling one.
- **The settled bill names the mandate.** Description schema bumps to v7: `mandate: {id, hash}` — the mandate's id and sha256 of the presented blob — lands inside the hashed description, so `verify_charge` and `/verify` prove which delegation authorized a charge.

## 2026-10-16 — Dunning suspension rides the statements job; the marker names the suspender (issue #258)

An organization the admin opted into dunning (`dunningGraceDays` on `PATCH /api/v1/admin/organizations/{id}`) is suspended by the daily `statements` pass once a finalized statement stands unpaid past `due_date + dunningGraceDays`, and reinstated once nothing unpaid stands past grace — or when the grace is switched off.

- **The sweep lives inside `statements`, not a new kind.** Overdue flips already happen there, and cutting the line is the same pass's natural next step — a new `oxsum.jobs` kind would add a second schedule for work that must run after the flip anyway.
- **`dunning_suspended_at` marks who suspended.** The automation both suspends and reinstates, so it must never lift a suspension the operator set by hand — the marker column names the sweep as the suspender, and only a marked suspension is the sweep's to undo. The operator's own `suspended` PATCH clears the marker in passing: whoever manages the flag by hand takes the sweep's marker off, so a later pass cannot "reinstate" a suspension the operator owns now.
- **Grace `NULL` means managed-by-hand.** Switching `dunningGraceDays` off does not strand the marker: the reinstatement half lifts a sweep-set suspension when the grace is NULL too, on the reading that a switched-off automation has handed the organization back to the operator.
- **The document and the organization move in one pass.** The lapsed statement's own `payment_status` flips to `suspended` beside the organization's flag — the standing a suspended bill already names — and the `org.suspended` webhook enqueues inside the same transaction the flag writes, carrying `reason: "dunning"` so a subscriber can tell the sweep's act from the operator's.
- **The same org-admission lock, taken per row.** Each suspension writes under `pg_advisory_xact_lock(ORG_ADMISSION_LOCK, org)` — the lock `set_suspended` takes exclusive and `hold_for_key` takes shared — so a dunning flip serializes against hold admission exactly like the operator's does.

## 2026-10-16 — Anomaly scoring is a pure read-time function, never a stored column (issue #263)

The anomalies page needed a severity signal beyond the flat settlement-kind classification, and the scoring has to be trustworthy as a triage signal — which means it must be a fact of the same ledger data the bills prove, computed the same way every read, with nothing to drift.

- **A pure function, not a persisted score.** `oxsum_core::anomaly::anomaly_score` maps `AnomalyFacts` → `(u8, Severity)` with no I/O: the score is recomputed at request time from the settlement record and the `usage_records` row it settled under, so a scoring change is a code change, not a migration, and a stored score can never disagree with the facts it summarizes. The anomalies endpoint batches the `usage_records` read once per organization (`Db::anomaly_inputs`) rather than per row.
- **The kind sets the base; stored facts weight it.** `capped`/`unpriced` start at 40 (money actively at risk), `estimated` 30, `swept` 20, `client_cancelled` 10; the `capped` charge ratio, the upstream-cost shortfall, failover attempts and the repeat-offender count add capped terms on top, clamped to 100. Every input — `charged`/`freeze`, `upstream_cost_minor`, `upstream_attempts`, the request's own anomalous-row count — is a column or ledger field that already existed; scoring reads, it never writes.
- **Bands, not a bare number, drive triage.** `low`/`medium`/`high` cut at 40/70 so the endpoint filters (`?severity=`) and the page groups on a stable vocabulary; `?sort=score` orders by the number. A filter naming no band is `400`, not an empty list — a typo must not read as "all clear".

## 2026-10-17 — Display currency is decimal text over integer credits, never a float (issue #257)

Organizations wanted amounts they can read against real money without the ledger leaving credits. The answer is a display layer only: `PATCH /api/v1/org` stores `{code, ratePerCredit, decimals}` on the organization, and every amount the dashboard, exports or statement PDF renders carries the converted figure beside the credits — `4.000000 cr ≈ 0.01 USD`.

- **The rate is a string, and the math is i128.** `"0.0014"` parses to mantissa `14`, scale `4`; the render computes `minor × mantissa × 10^decimals / (10^6 × 10^scale)` in `i128` with half-up rounding — so no float ever touches money, the wire carries the exact text the org wrote, and the same amount renders byte-identically in the PDF, the CSV and the React table. A NUMERIC column or a bigdecimal dependency would say the same thing heavier; two `text` columns with CHECKs (`^[A-Z]{3}$`, decimal shape, both-or-neither) keep the pair honest without a new crate.
- **The org declares it, the operator can set it too.** The write is `PATCH /api/v1/org` behind a session — owner or admin only, a member or an API key is refused — because "an organization declares" is the issue's own words. The admin `PATCH /api/v1/admin/organizations/{id}` takes the same field since billing terms already live there; both write the same pair of columns.
- **Stored beats default beats nothing.** `Config::display_for` resolves the org's pair first, then the `OXSUM_DISPLAY_CURRENCY`/`OXSUM_DISPLAY_RATE_PER_CREDIT`/`OXSUM_DISPLAY_DECIMALS` deployment default, then renders credits alone — so a multi-tenant deployment answers each org in its own unit, and a deployment with no opinion stays silent.
- **The conversion never feeds a charge.** It is applied at render — the React `displayAmount`, the PDF's money cell, the exports' two extra columns — and the exports keep `amountMinor` as the ledger's own signed integer beside it. A bill's proof still covers the credits; the fiat figure is a view, and the verify page is untouched.
- **`decimals` is a stored render choice, not derived.** Kept on the row (0–6, default 2) rather than implied by the code — JPY at 0 and a stablecoin at 6 are both honest renderings of the same rate rule.

## 2026-10-17 — The witness pinset is the deployment's claim, served for the reader to check (issue #259)

An anchored checkpoint's worth is the cosignatures on it — but a reader told "cosigned by 2 witnesses" and handed the notes still has to take the witnesses' keys on faith. The deployment now publishes the pinset it verifies against — `OXSUM_WITNESS_KEYS`, served on `GET /api/v1/log/checkpoint` as `witnessKeys` — so a reader can run the `cosignature/v1` check itself instead of trusting a count.

- **A pin is a URL, a key name, and the cosignature selector.** `OXSUM_WITNESS_KEYS` is a JSON array of `{"url","keyName","publicKey","keyHash"}` — one entry per `OXSUM_WITNESS_URLS` endpoint, where `keyHash` is `SHA-256(keyName ‖ '\n' ‖ 0x04 ‖ publicKey)`'s first four bytes. A signature line `— <keyName> <blob>` under that URL verifies when its selector matches `keyHash` and `publicKey` verifies the blob — the C2SP `tlog-cosignature` `0x04` construction `oxsum_verify::verify_witness` checks.
- **Validation is at boot, not per request.** A pin for a URL `OXSUM_WITNESS_URLS` does not publish to, an invalid key name, a non-Ed25519 `publicKey`, or a `keyHash` that does not recompute from the pair refuses to start — the misconfiguration fails loudly rather than serving a pinset that can only ever answer `BadSignature`.
- **The pins are the deployment's claim, not proof.** Serving them on the same endpoint as the cosigned notes does not make them trustworthy — a hostile server could pin keys it controls. The trust model is the same as `GET /api/v1/log/key`: the endpoint is convenience; a verifier that wants the real guarantee pins the keys through a channel the operator does not control. What the pins buy is verifiability-in-place: `oxsum_verify::verify_witnesses` runs the check against whatever pinset it is handed, so the same code serves the dashboard's "anchored by N witnesses" line and a third party's own pinset.
- **The verdict names the failure, never just the count.** `verify_witness` distinguishes `Verified{at}` (carrying the cosigned timestamp), `Malformed`, `WrongOrigin`, `HeadMismatch`, `UnknownKey` and `BadSignature` — a forged line, a stale row and a missing pin read differently on the page, so the dashboard reports "2 of 3 verify — witness-b: BadSignature" rather than a bare pass/fail. Freshness is deliberately the caller's policy: the verdict returns the signed `at` and the page decides whether an old anchor still counts.
- **The React log page shows the pinset; it does not re-verify.** Rendering the pinned key under each witness URL tells the reader what the deployment claims; running the Ed25519 check in the browser would need a JS port of `oxsum_verify` (the crate is wasm32-clean, so a future wasm bundle could do it honestly) — out of scope here, same stance as `/verify` staying server-rendered (issue #234).

## 2026-10-17 — The deployment serves its own tlog-witness; trust roots are pinned out of band (issue #260)

A deployment that sets `OXSUM_WITNESS_SIGNING_KEY` + `OXSUM_WITNESS_TRUST` runs a C2SP `tlog-witness` beside the API: `POST /add-checkpoint` (root-level, `text/plain` both ways) cosigns foreign checkpoints that provably extend the head it last vouched for, and refuses every other.

- **Explicit trust roots, never TOFU-by-default.** A witness that accepts whatever head arrives first can already be signing a fork — the pinset *is* the trust act, matching `Witness::trust`'s "get it from a channel the log operator does not control". `OXSUM_WITNESS_TRUST` pins `origin=size:root` per tracked log; an unpinned origin is a 404. The deliberate exception is `size=0`, which onboards a brand-new log: `Witness::offer` takes the first non-empty head on trust because every log extends the empty tree and nothing earlier is provable — TOFU by explicit operator choice, not by accident.
- **Two tables, not one.** `witness_served(origin, head_size, root_b64)` is the mutable `WitnessStore` state — one row per tracked origin, advanced on every accept; `witness_vouched(origin, head_size, note)` is the append-only record of every cosigned note the witness returned, the issue's "the witness's own log is append-only and signed". The served head answers "what do I hold"; the voucher table answers "what did I say" — a witness that cannot show what it signed is a witness that can be talked into contradicting itself quietly.
- **Durable before it is signed.** `record_witness_served` + `record_witness_vouched` run in the same transaction the response commits from — the state change and the voucher land or vanish together, so a crash cannot leave a `cosignature/v1` line in the world that the witness has no memory of.
- **A per-origin advisory lock serializes submissions.** `pg_advisory_xact_lock(hashtext(origin))` inside the transaction makes the "what head do I already hold" read honest — two racing submissions for one log serialize, and the second reads the head the first accepted rather than both deciding against the same stale state.
- **Re-seeding refuses a contradictory root.** `seed_witness_trust` inserts when the row is absent, no-ops when the stored row holds the same root or a head past the pinned size, and refuses to boot (`Misconfigured`) when the stored head at the pinned size contradicts it — a restart re-pinning an older root cannot silently roll the witness back onto a fork it already moved off.
- **Key rotation is re-seeding.** The cosigning key derives from `OXSUM_WITNESS_SIGNING_KEY` the same way the head-signing key does: rotating is a restart with a new seed, and the verifier's key hash changes with it — nothing is stored under the key that has to migrate.
- **`/add-checkpoint` is not `/api/v1`.** The C2SP wire is `text/plain` end to end — the submitter files the answer verbatim as its cosigned note — so the route mounts at the root beside `/healthz` and skips the JSON envelope. The status codes are the witness's own vocabulary (404 untracked, 409 fork/shrunk, 400 malformed/bad proof, 401 unsigned) because the submitting `tlog-witness` client reads the status to decide whether to retry or to alarm.

## 2026-10-17 — End-user usage reports commit a once-only subset marker into the org ledger (issue #261)

An organization's downstream customer — its `endUser` — holds nothing: not the org's ledger, not a session, not a credential. The report has to be a bundle the org can hand over that the customer verifies alone, and the hard part is that a Merkle inclusion proof proves *membership*, never *completeness* — a malicious org could drop rows from the shown subset and every remaining row would still verify.

- **The subset is committed into the ledger, once.** `POST /api/v1/org/usage-reports` appends a usage-report marker entry — a self-balancing 1-minor round-trip on the adjustments account, the same shape `anchor_statement` uses — whose `DocumentRef` binds the subset digest (`oxsum/usage-report/v1` folded over the shown settlement entries' content hashes in log order) and whose description commits `{endUser, from, to, entryCount, chargedTotalMinor, subsetHash}`. The marker is a ledger entry, so *it* is provably in the history the signed head attests.
- **One marker per (organization, endUser, period), ever.** The marker's idempotency key is `usage-report/<endUser>/<from>/<to>` — a second request for the same span replays the standing marker rather than appending a second. This is what makes the completeness claim honest rather than a shrug: the org cannot commit one subset to the ledger and show the customer another, because the first commitment stands in the append-only history and a re-issue cannot supersede it.
- **The honest shape is (b) a commitment the customer can spot-check.** The marker's description binds `entryCount` and `chargedTotalMinor` — a customer who kept their own request log can count their calls and check the attested figures, so a dropped row surfaces as a count mismatch the signature attests. What the marker does *not* prove (recorded, not hidden): the subset is the org's claim of "all usage for X in [from, to]", not a Merkle fact — an org could under-report and the customer only detects it by cross-checking their own records or the count. Per-end-user running sub-ledgers (option a) would make completeness provable and were rejected: a second append-only structure per customer is a large machinery for a claim the count already lets the customer falsify.
- **The bundle is self-contained.** It carries the marker's proof bundle, every shown entry's proof bundle, the signed head, and the descriptor. `verify_usage_report` in `oxsum_verify` (wasm32-clean, `/verify` can run it) checks: the marker's inclusion and that its document hash equals the subset digest; every shown entry's inclusion; that the shown entries' content hashes fold to the committed digest; and the head's signature. No shown row can be swapped, reordered, or dropped without the fold breaking — and no *other* rows can be claimed as this report, because the marker binds the digest.
- **No new trust assumption.** Everything the bundle proves reduces to the same two facts every bill already proves — this entry is in this ledger (inclusion), and this head attests this ledger (the operator's signature). The marker adds nothing the org could not already claim; it makes the claim *checkable*, binding one attested subset to one period so a re-issue cannot quietly move the boundary.

## 2026-10-17 — The x402 rail is a verifier seam over the existing deposit walk; the crypto is a stand-in (issue #264)

x402-style payment rails — a payer signs an authorization, the resource server settles it on a crypto rail — are the payment/agent surface the roadmap deferred. A real chain integration is out of scope for a portfolio piece, but the interface is built honestly: `POST /api/v1/topups/x402` accepts a signed payment authorization and the deposit books through the same `pending → confirmed → credited` walk every rail shares, so the day a real verifier arrives the bookkeeping already stands.

- **The rail is a verifier, not a chain client.** `RailVerifier` in `crates/core/src/x402.rs` is the seam: `verify(payload) -> Option<VerifiedPayment>` answers the `{amount, payment_ref, payer}` the deposit credits, or `None` for anything it does not honor. A real EIP-712/x402 verifier — typed-data hashing, an on-chain payer lookup, settlement-finality checking — implements the same three-field answer and nothing downstream changes: the route, the deposit row, the ledger credit and the `deposit.credited` event are already written.
- **Today's verifier is a documented stand-in, named so it cannot masquerade.** `Ed25519RailVerifier` checks the canonical-body Ed25519 `verify_strict` signature — the same pinned-wire construction `mandates` uses — against `OXSUM_X402_VERIFYING_KEY`, the payer key the deployment pins. The wire keeps the x402 field names (`scheme`, `network`, `asset`, `payTo`, `nonce`, `signature`) so a real payload maps field-for-field, but `network: "oxsum-ed25519"` and `asset: "OXSUM-CREDIT"` are checked before the signature: a correctly-signed authorization for a chain, scheme or asset the stand-in does not serve is refused, so a real `eip155:*` payload can never be mis-read as a valid test payment. **Not built, on the record:** no chain RPC, no settlement finality, no wallet signatures — the deployment trusts whoever holds the pinned seed, which is a test fixture, not a payment network.
- **The signed `nonce` is the idempotency anchor.** `payment_ref` carries the nonce: the `(rail, organization, payment_ref)` UNIQUE means one authorization opens one deposit ever, and the ledger credit replays under the fixed key `x402:<nonce>` — `top_up` answers the standing receipt (`isNew: false`) rather than double-crediting. The `confirmed → credited` flip looks the row up by the nonce anchor, not the call's fresh deposit id, so a crashed or raced call still fires `deposit.credited` exactly once. This is deliberately not a two-phase Stripe-shaped flow: the authorization's signature *is* the confirmation — there is no webhook to wait for, so the row lands `confirmed` and credits in the same request.
- **`403`, not `400`, for an unverified authorization — and indistinguishably so.** Bad base64, bad JSON, a foreign `scheme`/`network`/`asset`, and a signature that does not verify all return `None` from the verifier and `FORBIDDEN` from the route: which check failed is the verifier's business, the same refusal-shape `redeem_code` and `parse_presented` keep. An unconfigured deployment answers 503, matching the Stripe rail's posture — the rail is configured or it is not.
- **The env is one variable, not a provider bundle.** `OXSUM_X402_VERIFYING_KEY` alone configures the rail because the stand-in has no counterpart to `OXSUM_PUBLIC_URL` or a webhook secret — there is no redirect to build and no callback to verify. When a real rail lands it can keep the pin-and-verify shape (its "verifying key" is a chain-id plus contract address, or a verification service) — the config still resolves to `Option<&dyn RailVerifier>` either way.

## 2026-10-17 — Testcontainers residue is swept externally, and the suite reads `.env` (issue #277)

The doubleentry Postgres suite's default is a throwaway `postgres:17-alpine` container per binary, and it orphaned eight of them on the local daemon in a week. Two causes, both structural: the suite read `DATABASE_URL` from the process environment only — never `.env` — so any run in a worktree or a shell without exported variables started a container even when the developer's `.env` pointed at the local cluster; and cleanup depends on testcontainers' Ryuk sidecar, which a SIGKILLed test binary or an interrupted CI step never waits for. Same shape as issue #271's database residue: create is guaranteed, delete-at-exit is not.

- **`.env` is loaded in the suite itself, matching oxsum-testkit.** `dotenvy::dotenv()` runs before `shared()` reads `DATABASE_URL`/`DOUBLEENTRY_DATABASE_URL`, so a developer whose `.env` names the local cluster never starts a container in the first place — the cheapest residue is the one that never exists. The container path stays the default when neither variable is set anywhere: it is the vendor's technique, and `start_isolated`'s deferred-sequencing tests need a dedicated cluster regardless.
- **Residue is swept by a script, not by the test process.** `scripts/clean-testcontainers.sh` removes `org.testcontainers.managed-by` containers older than `MIN_AGE_MINUTES` (default 60) and prunes their anonymous volumes. The label filter is the testcontainers contract; the age gate is what makes a sweep safe to run while another suite is live — a container younger than the cutoff might belong to a running binary, and deleting it would race the run rather than clean up after it. CI runs `--drop` beside the database sweep; locally the same command with no flag is a dry-run list.
- **No in-binary sweep for containers.** The #271 lesson applies twice over here: a per-process `docker ps` on every binary start costs the daemon a round trip × N processes, and a blocking remove inside `Drop` serializes teardown the way the blocking database drop did. The sweep lives where the residue is visible — the daemon — not inside the processes that leak into it.

## 2026-10-17 — test-integration sharded behind a single nextest archive (issue #276)

The whole-database job was the merge-time bottleneck: ~13 minutes serialized into one runner — disk-space prep (~2 min), the workspace suite under nextest (~7.6 min), the process-shared suites under cargo test (~2.8 min), and the ledger-gated doubleentry pass (~25 s). Everything except fmt/clippy/test waited on that chain.

- **Build once, run everywhere.** `cargo nextest archive --workspace` produces one `.tar.zst` holding every test binary; shard jobs download it instead of compiling, so each runner's wall clock is the suite divided by the matrix, not the suite plus the compile. The pattern is what nextest itself documents for CI partitioning — the archive keeps the build options, the run phase just supplies `--partition count:i/3`.
- **Two archives, not one.** doubleentry's postgres suite is `#![cfg(feature = "postgres")]` — a plain `--workspace` archive would carry an empty binary. A second `-p doubleentry --features postgres` archive rides the same artifact upload, and the conformance suite extracts it independently of the workspace one.
- **The process-shared suites run as raw binaries.** `generative`, `seed`, `jobs` coordinate through in-process mutexes and OnceLock shared schemas — under nextest's process-per-test model that is a race, which is why they stayed on `cargo test`. The archive hands over the same compiled binaries; running them straight from `target/debug/deps` reproduces cargo test's one-binary-per-process shape without recompiling anything, so the coordination semantics are identical.
- **Doctests stay in `test`.** They already run there; pulling them into test-extras would pay for a fresh rustc invocation for a handful of targets the fast job already covers.
- **Every shard sweeps its own service.** Each shard brings up its own postgres container and runs the scratch-database sweep before the tests (issue #271), so residue from an interrupted sibling never reaches this run. The container sweep (issue #277) lives in test-extras, the only job that can still leave a container behind.

## 2026-10-17 — The Topcoat dashboard retires to `/app`; `crates/web` becomes the `/verify` shell (issue #256)

The React dashboard reached parity (the `web/README.md` checklist), so keeping two page surfaces meant every feature change landed twice. The Topcoat pages retired to the SPA: `crates/web` is no longer "the dashboard", it is the one page that was never going to live in the bundle.

- **`crates/web` shrinks to `/verify`, not to zero.** The crate keeps the server-rendered `/verify` page (GET+POST), its `/style/main.css`, and the `BillView`/`csv`/`json` row types the server's bills-export routes still build on — everything else (`pages_*`, `session`, `heads`, `requests`, `usage`, the dashboard chrome) is deleted. Deleting the whole crate was rejected: the server needs the export types, and `/verify` needs an SSR home — removing the crate only relocates both, it does not remove them.
- **`/verify` stays server-rendered, the same call as before.** The 2026-10-15 decision already named the reason: a server-served verifier bundle certifies itself. Nothing about that changed with the move; the page keeps running `oxsum_verify` on the server and the React app keeps linking out to it.
- **Every legacy page path `301`s to its `/app` twin.** `/`, the auth pages, `/device`, `/dashboard` and each of its leaves, and `/admin` map path-for-path onto the React routes, and the query string rides along so a filtered list keeps its filter. The redirects are axum routes in `crates/server/src/web.rs`, mounted before the Topcoat fallback so they win; `301 Moved Permanently` (not axum's 308 `Redirect::permanent`) is what the issue specifies — the page is gone for good and a browser may cache the new location. The POST forms the pages served are gone with them: a stale form's POST to a page path is not a bookmark, so only GETs redirect.
- **Links minted after the move point at `/app` directly, not at the redirect.** The mailed `/verify-email` and `/reset-password` links, the OAuth callback's success and failure `Location`s, the device grant's `verification_uri` and the Stripe checkout success/cancel URLs all emit the `/app/*` path — a fresh link riding a 301 is a needless hop, and the mail/stripe URLs are produced now, not remembered from before. `/verify` links (PDF footers, `verify_url`) stay `/verify` — it is still served, not redirected.
- **The bills export stays at `/dashboard/bills/export.*`.** Those two URLs are downloads the React page links, not pages — a file the browser saves, on the session surface it always was. Redirecting them would move the bookmark for no page to land on; they sit in the server's real route table ahead of the `/dashboard/{*rest}` redirect arm.

## 2026-10-18 — Job leader election rides a Postgres advisory lock, not a new component (issue #281)

The jobs queue is multi-instance *safe* — `FOR UPDATE SKIP LOCKED` plus the claim lease means one row runs on one worker — but not multi-instance *single*: a pass that outlives its `CLAIM_LEASE_SECS` (a `statements` aggregation over many organizations, a `witness-checkpoints` round-trip) is reclaimed mid-flight and runs twice. `oxsum.jobs.requires_leader` marks the kinds that must execute under an advisory lock: a worker that claims a leader-required row but cannot take the kind's `pg_try_advisory_lock` hands the row back (`unclaim_job`) instead of duplicating the pass.

- **The lock is session-scoped on a dedicated connection, deliberately not `xact`.** An advisory transaction lock releases at commit — useless when the pass spans minutes and dozens of transactions. The worker checks out one `PoolConnection`, `pg_try_advisory_lock(hashtext('oxsum.job-leader:' || kind))` on it, and holds it for the whole `run_one`; the lock survives every transaction the pass opens because it belongs to the session, not to any one of them. The `oxsum.job-leader:` namespace keeps the keys disjoint from every other advisory lock in the codebase (per-key hold locks, the credit-limit lock, the witness serve lock) — a hash collision there would deadlock a periodic chain against a request path.
- **The yield is a yield, not a failure.** `unclaim_job` returns the row to `pending` with `claimed_at = NULL`, `attempts` unspent, `run_at` pushed 60 s out — the row is not hot-looped while the leader works, and it is reclaimable the moment the lock frees. This is the normal multi-instance handoff, so it counts `job_run(kind, "yielded")` rather than the failure path's warn.
- **A crashed leader hands off, it does not wedge.** The lock releases with the connection — a dead holder frees the kind for the next claim outright. The old behaviour this fixes: a slow pass and a reclaiming worker ran the same row concurrently, doubling outbound calls (witness offers, price probes) and webhook deliveries.
- **Postgres, not Redis.** The deployment's single point of truth already coordinates every other cross-instance decision in the system (per-key hold locks, suspension gates, the witness serve lock). A Redis `SET NX` would add a component, a second failure mode and a new env block to answer a question Postgres already answers. The `RateLimiter` trait stays the seam for a future cross-instance RPM limiter — that is a real shared-state problem, unlike leadership, which is a yes/no the database adjudicates natively.
- **Leader-required kinds:** `sweep-holds`, `witness-checkpoints`, `statements`, `retention`, `sync-upstream-prices`, `upstream-bill-sync`, `usage-anchors`, `reconcile`. `deliver-webhooks` stays unleadered on purpose: its rows are claimed individually `SKIP LOCKED`, so a second runner is throughput, not duplication.

## 2026-10-18 — The x402 rail delegates to a facilitator sidecar; the stand-in stays for tests (issue #282)

The stand-in the seam was cut for (issue #264) is now swappable for the real thing: `X402FacilitatorVerifier` in `crates/integrations` delegates `verify`/`settle` to an [x402-rs](https://github.com/x402-rs/x402-rs) facilitator over its V1 HTTP API — `POST /verify` proves the EIP-3009 authorization's signature, amount, nonce and expiry, `POST /settle` lands the `transferWithAuthorization` on-chain and answers the transaction hash, which records into `deposits.facilitator_tx_hash` (migration 0048).

- **The facilitator is a sidecar, not an in-process crate.** Embedding `x402_facilitator` would pull chain RPC connections, signer keys and finality waits into the single-binary-plus-Postgres deployment — a second failure domain inside the billing process for a dependency that already ships as a service (`ghcr.io/x402-rs/x402-facilitator`, or hosted `facilitator.x402.rs`). The same argument kept SMTP out of core and pushed the OAuth legs into `crates/integrations`: an external service is a narrow client here, never a runtime.
- **The requirements are the server's claim, not the caller's.** `paymentRequirements` — scheme `exact`, the configured `network`, `payTo`, `asset`, `maxAmountRequired` — is constructed from `OXSUM_X402_*` configuration, never from the request body beyond `amountMinor`: the deployment states what it accepts, the facilitator proves the payment meets it. A caller naming their own `payTo` would be paying themselves; the shape check (`x402Version` 1, scheme `exact`, the configured network) runs before the facilitator's round trip so a foreign wire refuses locally.
- **The caller's `payment` is the V1 wire verbatim.** Base64url of the `{x402Version, scheme, network, payload}` `PaymentPayload` — the same blob the protocol's `X-PAYMENT` header carries — passed through to the facilitator untouched (the pinned-wire rule again: the verifier settles exactly the bytes it proved, never a re-serialization). The decoded pair rides `VerifiedPayment.context` into `settle` so `/settle` re-sends what `/verify` saw.
- **One nonce settles once — the row lock is the guard, not just the credit.** `payment_ref` still anchors on the signed nonce, but an on-chain transfer is not idempotent the way a ledger append is: two requests racing the same authorization would both pass `ON CONFLICT DO NOTHING` and both settle. The deposit row is now claimed `FOR UPDATE` across the settle call — the loser waits for the winner's commit, sees `facilitator_tx_hash` already recorded, and skips straight to the ledger credit. A settle that fails leaves the row `confirmed` — a retry under the same nonce can land it, and the chain's own nonce guard (EIP-3009 rejects a reused authorization nonce) is the backstop behind the lock.
- **1:1 credit mapping, honestly stated.** USDC's 6 decimals and the ledger's 1e6-per-credit scale coincide, so `value` = `amountMinor` with no exchange-rate fiction. A non-USDC-pegged asset would need a real conversion — recorded as a limit of this rail, not finessed.
- **The stand-in stays, demoted honestly.** `Ed25519RailVerifier` remains for tests (no chain, no sidecar) and for deployments exercising the bookkeeping; `OXSUM_X402_VERIFYING_KEY` is read only when the facilitator set is entirely absent — a complete facilitator set always wins, a partial one is a startup error rather than a silent fallback. The `OXSUM_X402_VERIFYING_KEY`-only deployment's behavior is unchanged.
- **The trait went async to keep `&dyn` object-safety.** `verify`/`settle` are `Pin<Box<dyn Future + Send>>` methods — `Config` hands every request a `&dyn RailVerifier`, so `async fn` in the trait (which returns `impl Future` and is not object-safe) was out; hand-rolled boxed futures over `async_trait`, matching the codebase's no-extra-dependency posture.

## 2026-10-19 — Provider-bill reconciliation rides a provider seam and a channel-day grain (issue #283)

`usage_records.upstream_cost_minor` has always been oxsum's own `Price::upstream_cost` recomputation — an estimate, because no upstream rail exposed an invoice to check it against. Both major providers now ship admin-level cost APIs, so the estimate can be reconciled against the billed fact: `oxsum.upstream_bills` records one `(channel, bill_date)` row per day, provider total beside the day's summed estimate.

- **The grain is `(channel, day)`, because that is what the APIs expose.** OpenAI's Costs and Anthropic's Usage & Cost Admin both answer day-grained billed totals — never per-request — so the reconciliation unit is the smallest the providers will tell the truth at. A per-request bill comparison was considered and abandoned: the data does not exist to compare it against. `our_cost_minor` is the day's Σ `upstream_cost_minor` frozen at write time, so a re-run never silently recomputes a different estimate.
- **The billing credential is a second sealed column, deliberately separate from the metering key.** `channels.bill_key_sealed`/`bill_key_last4` sit beside `api_key_sealed`/`api_key_last4`, unsealed under the same `OXSUM_SECRET_KEY` but attached separately (`billKey` on the admin channel write, triple-state). A metering key serves the relay; a billing key reads the org's bill. Conflating them — reusing `api_key_sealed` for the cost call — would hand every relay credential the power to read the org's invoice, and would confuse a provider whose metering and admin keys differ by design. The conservative pick the issue offered (a dedicated column) is the one taken.
- **The provider is a trait, the OpenAI impl first.** `UpstreamBillProvider` is object-safe boxed-future (`Pin<Box<dyn Future>>`), the same shape `RailVerifier` took — `Config` holds an `Arc<dyn …>`, the test suite a scripted stub, and Anthropic's impl drops into the same seam in the follow-up PR without the job changing. Only `protocol == "openai"` resolves to a bill; any other protocol answers `None`, which is a coverage gap, not a zero.
- **A provider that answers nothing records a gap, not a zero.** `provider_billed_minor` is NULL on a day the cost API had nothing to say — unreachable, a channel with no billing credential, a day not yet closed. Writing zero there would tell the margin view "the provider billed nothing" when the truth is "the provider answered nothing": the row records the coverage gap honestly, and `drift_*` stays NULL with it rather than drifting against silence.
- **Drift is the signal, the threshold is the alert.** `drift_minor = provider − ours`, `drift_pct` over the estimate; past `DRIFT_ALERT_PCT` (5%) and `DRIFT_ALERT_FLOOR_MINOR` (1 credit) the row flags `anomaly.detected` once, latched by `reconciliation_entry_id` so a re-run cannot double-fire. The floor keeps a near-zero day's provider rounding from paging an operator — a 1% drift on a day with pennies of usage is noise, not a misbilling.
- **The margin view surfaces it channel-scoped.** `providerBilledMinor` joins the per-`(channel, model)` margin as the channel's cumulative billed total — the provider API does not split by model, so the figure repeats on each of that channel's rows and the field is documented as such. Putting a per-model figure there would fabricate a breakdown the provider does not give.

## 2026-10-19 — `/verify` runs the real verifier in the browser again, by shipping the wasm the same crate builds (issue #284)

The 2026-10-15 decision kept `/verify` server-rendered because a server-served verifier would "certify itself" — the note always carried the escape hatch: the crate is already wasm32-clean, so a wasm build restores browser-side verification. This issue takes the hatch, on the model the decision actually named: the page is a plain server-rendered form **plus** the wasm module the verifier compiles to — the "don't trust the server" property returns, and the server-rendered skeleton stays as the no-JavaScript floor.

- **The same crate, exported — not a JS port.** `oxsum_verify` gains a `wasm` feature gating `#[wasm_bindgen]` bindings in `src/wasm.rs` and a `cdylib` crate-type; `scripts/build-verify-wasm.sh` compiles it for `wasm32-unknown-unknown` and runs wasm-bindgen `--target web`. The exports (`verifyBundle`, `verifySignedHead`, `verifyWitnesses`, `verifyStatementAnchor`, `verifyUsageReport`, `verifyPriceHead`) are thin wrappers over the functions the server already calls — the proof math runs identical code on whichever side of the trust boundary executes it. The rejected alternative was the `@noble` JS port the issue also floated: two implementations would drift, and the wasm artifact *is* the Rust verifier, so nothing can.
- **JSON strings across the boundary, not tsify.** The exports take and return `&str → String` of the same wire types the Rust callers pass — the page `JSON.parse`s once and renders `{ok, detail}` — so no `tsify`/`serde-wasm-bindgen` dependency and no generated TypeScript enters the chain. `JsError` carries the same `Display` text the server-rendered verdict prints, so the failure reads identically whichever side produced it.
- **The skeleton is server-rendered *and* stays the fallback.** Topcoat serves the form as before; `verify.js` (checked into `crates/web`, served at `/verify.js`) loads the wasm module only to intercept submit — every failure mode (no `WebAssembly`, the placeholder bundle, a fetch hiccup) leaves the native POST alone, and a usage report's `subsetHash` payload keeps the server path because the page holds no published key. The verifier-serves-itself concern the #234 decision raised is answered by the shape, not ignored: the wasm *module* is data the server cannot influence at run time — it is the same artifact `cargo build` produces, served as bytes.
- **Build artifacts ride the `web/dist` pattern.** wasm-bindgen's output (`verify_wasm.js` + `verify_wasm_bg.wasm`) lands in `crates/web/verify-wasm/` over committed placeholders — a module stub that throws and the 8-byte empty `.wasm` — so `cargo build` works without the toolchain and the placeholder's failed import is what hands the form back to the server. The `wasm` Dockerfile stage downloads the pinned `wasm-bindgen` release binary and runs the script; the builder stage copies the artifacts over the placeholders, keeping the single-binary deployment — the wasm travels *inside* the binary like the SPA's dist, not as a side file.
- **The witness-cosignature check finally runs client-side, where it always belonged.** `verify_witnesses` exports the real Ed25519 cosignature verification the dashboard only ever *displayed* pins for (the 2026-10-18 witness entry's "a future wasm bundle could do it honestly" — this is that bundle). It stays an export, not a page feature: the verify form has no witness input and `/api/v1/log/checkpoint` is per-organization, so the page exercises `verifyBundle`/`verifyPriceHead`/`verifyUsageReport` and the rest are for the standalone-verifier path the bundle enables — a user can download the wasm and run the full check set offline, which is the closest "verify without trusting us" gets.

## 2026-10-19 — A second fiat rail proves the deposit seam: Airwallex, structured like Stripe (issue #285)

The deferred-scope line ("further payment rails (Airwallex — `deposits.rail` is the seam)") is exercised: a second production fiat rail joins manual/redemption/Stripe/x402 on the same `deposits` table, the same `pending → confirmed → credited` walk and the same `(rail, organization, payment_ref)` idempotency anchor — widened to admit `airwallex` by migration 0050. The point of the rail is not APAC coverage (a real deployment reason); it is that the seam the 2026-10-05 decision cut was meant to take a second fiat provider without architectural work, and this issue proves it does.

- **No SDK, a narrow client — the same posture every integration takes.** Airwallex has no mature Rust crate, so `crates/integrations/airwallex.rs` hand-writes the two legs the rail needs: `POST /api/v1/authentication/login` (the `x-client-id`/`x-api-key` pair exchanged for a ~30-minute bearer token, cached under a mutex so a burst of intents costs one login) and `POST /api/v1/pa/payment_intents/create`. That is the whole client — no refunds, no payouts, no method management — mirroring `stripe.rs`'s "the narrow client the endpoint needs, nothing else".
- **The deposit id does double duty, like Stripe's.** `merchant_order_id` *and* `request_id` both carry the deposit's uuid: the first is the field the webhook echoes back (the `client_reference_id` role), the second is the provider-side idempotency key, so a retried create never mints two intents for one deposit. The intent id lands in `meta.airwallexIntentId`, the URL and `client_secret` in `meta.airwallexCheckoutUrl`/`meta.airwallexClientSecret` — a replayed `idempotencyKey` answers the stored pair, never a second PaymentIntent.
- **A different signature shape, honestly handled.** Airwallex signs `x-timestamp + raw_body` — no `t,v1` envelope, no dot separator — into `x-signature` (hex HMAC-SHA256 under `OXSUM_AIRWALLEX_WEBHOOK_SECRET`). `verify_signature` reconstructs exactly that rather than forcing the Stripe shape on it; constant-time compare, every miss is the same 401. The webhook reads `name` (Airwallex's event-type field) rather than Stripe's `type`: `payment_intent.succeeded` confirms + credits under `airwallex:<intent_id>`, `payment_intent.cancelled`/`.expired` walk to `expired`, everything else is a 200 ack — the provider's retry contract is the same either way.
- **Money crosses the wire in the provider's units and converts honestly once.** The intent body takes `amount` in decimal major units (the rail prices USD, like Stripe's); the webhook's `data.object.amount` converts back to cents for `received_minor`, with the same rule D2 set for Stripe — a mismatch with `amount_minor` is recorded for a human, never auto-patched. The f64 parse touches only the *reported* figure; the credited amount is always the deposit's own `amount_minor`, an integer end to end.
- **The answer carries the SDK secret, deliberately documented.** Stripe's hosted page needed only a URL; Airwallex's `redirectToCheckout` needs `intent_id` + `client_secret`. The topup answer therefore adds `clientSecret` (absent on a replayed intent, which re-reads the row rather than re-creating) and the dashboard's Airwallex tab is a straight `window.location` redirect to `checkoutUrl` — the secret rides for the SDK-leg integrations a deployment may build, at the cost of a documented caveat: `client_secret` is a confirm-session credential, not a password — it may reach the payer's browser, which is exactly where the answer sends it.
- **The stand-in question the x402 rail raised does not apply here.** A fiat rail cannot be "stood in" the way a signature scheme can — money really moves or it does not — so the test floor is a wire stub (`crates/server/tests/airwallex.rs` serves login + intent endpoints and signs webhooks under the test secret), the same posture the Stripe suite proved. Sandbox access against a real Airwallex account is the PR-honest gap: the wire is documented, the stubbed suite proves the bookkeeping, and the first real deployment's sandbox run is the end-to-end confirmation the suite cannot give.

## 2026-10-19 — Model aliases are renames on one channel, recorded as v8's `upstreamModel` (issue #286)

The deferred-scope list promoted model aliases when the next-phase plan made "one public name, several upstream models" an execution item. The shape is deliberately the smallest reading of that phrase: an alias is a *rename*, not a routing layer, so a name resolves to the one channel that claims it and never fails over onto another channel's model.

- **One name, one route set — enforced at write time, not resolve time.** `oxsum.model_aliases` keys on `alias` alone (a `channel_id` FK, no per-channel namespacing), so a name can only ever be claimed once. `create_alias` refuses two collisions in the same transaction: an alias already held by any channel, and a name another channel serves *directly* through `channel_prices`. The asymmetry is the contract — a channel may claim an alias for a name it also prices, but no two channels may split one public name, which is what would turn an alias into a second routing layer. Weighted alias→several-models stays deferred; the failover machinery already does per-model multi-channel routing for direct names.
- **The alias's price wins; the upstream model's is the fallback.** `servings_named` looks the alias's own price row up on the claiming channel first and drops to the `upstream_model`'s row when the book has none — the operator may price the public name differently from the raw model, and an unpriced alias is still servable. A direct name resolves to `servings`'s every-channel answer, unchanged.
- **`model` stays the asked-for name end to end; the upstream name travels beside it.** The hold, usage row and settlement all keep `model = <alias>` — it is what the caller asked for and what its bill should say. The resolved `upstream_model` is written into the forwarded body (`forwarded` rewrites `body.model` only when the names differ) and recorded as v8's optional `upstreamModel`. Sweeps and refusals write `None`: a hold refused at admission never resolved a route, and reconstructing one after the fact would be a claim the record cannot make.
- **`upstreamModel` is metadata, not a price input — v8's rule mirrors `upstreamAttempts`'s.** `verify_charge` recomputes a v8 record exactly as v7 and only binds the field's *shape*: present means a non-empty string, anything else (empty, non-string, `null`) is a mismatch. The name enters the content hash so a bill proves which model answered, but the arithmetic never reads it — an alias changes routing and audit, never pricing semantics.
- **Aliases are configuration, hard-deleted; bills keep the name.** `DELETE …/aliases/{alias}` removes the row outright — the alias lives in `oxsum`, not in any ledger, so there is nothing to retain versioned. A settled bill that names the alias keeps it in its description, which is the record that matters.

## 2026-10-21 — A bill link is a capability token, not an end-user account (issue #300)

The downstream customer a usage report is about holds nothing: no session, no key, no account. Three ways to give them the bill were considered — a real end-user account (rejected: a third authentication system, with its own password reset, abuse surface and support burden, for a page that shows one bill), a URL carrying the bundle inline (rejected: bills get large, and a URL that carries the data leaks it into logs, referrers and mail archives), and a random capability token (chosen).

- **The database stores only the hash.** `oxsum.report_links` keeps the token's SHA-256; the plaintext is answered once at creation. A dump of the table opens nothing, the argument `api_keys` and `sessions` already make.
- **Two scopes, because the two questions differ.** `report` pins one minted report — one immutable commitment, one bill for one span; `end_user` names the customer and lists every report issued to them. The report scope is the default shape; the customer scope is the one a portal link needs, and it is deliberately a separate capability because it reveals more.
- **Expiry is bounded and revocation is a column.** 30 days by default, 90 at most: a bill link is not a standing grant, and the organization can always mint another. Revoking writes `revoked_at` and is idempotent; the row stays because it is the record of what was shared, which is why there is no delete.
- **Expired, revoked, unknown and other-organization links are one 404.** Whether a link exists elsewhere is not this organization's business, the same refusal-shape the API keeps for other tenants' ids.
- **`last_used_at` is touched on resolution, and is not a delivery receipt.** It tells the operator the customer opened the bill; a mail client prefetching the link would touch it too, so it is evidence of access, not of reading.
- **The link resolves without a session; the page that consumes it is separate.** This issue ships the capability only — the `/bill/{token}` page is the next piece, and keeping the two apart means the security model (token TTL, revocation, 404 shape) is settled before any customer-facing HTML exists.

## 2026-10-22 — The bill page is server-rendered, scriptless and one 404 (issue #302)

The customer a share link belongs to holds no account, so `/bill/{token}` is the page the capability opens. It is deliberately the same shape as `/verify`, not a React route: the numbers come from the ledger, the operator key and head are printed beside them, and a customer can read the bill with JavaScript disabled.

- **Server-rendered, not the SPA.** The dashboard is the React bundle served at `/app`; the bill page is the one page whose whole point is to be read by someone who does not trust the served app, so it stays in `crates/web` beside `/verify`. Its rendering is derived from the frozen report (`Db::usage_report` + `Wallet::usage_report_bundle`), never from a live usage query — a bill cannot grow after issue.
- **The token is the whole credential.** No session, no cookie, no account. The page resolves the token through `Db::resolve_report_link`; expired, revoked, unknown and other-organization tokens are the same 404.
- **Headers are part of the design.** `Referrer-Policy: no-referrer` keeps the token out of any outbound link; `X-Robots-Tag: noindex` keeps a bill out of search engines; `Cache-Control: private, no-store` keeps a shared cache from handing one customer's bill to the next request. A middleware on the page fallback sets them, so a future page inherits the rule instead of remembering it.
- **No third-party script, and no script at all in the floor.** The page runs nothing; the browser verifier is the next step and will be the only script, served from the binary like `/verify.js`.
- **Two scopes, one renderer.** A report link renders its one frozen report; a customer link lists every report issued to them, one section each. The per-report token is still the narrowest capability, so the customer link is the organization's deliberate choice.

## 2026-10-22 — The bill verifier runs in the browser over the bundle the page embeds (issue #305)

The bill page rendered the frozen report but could not prove it: the customer still had to paste the bundle into `/verify`. This issue closes the loop without adding an API.

- **The bundle rides in the page, not in a second request.** Each report's `UsageReport` JSON is serialized server-side and embedded as a `data-bundle` attribute on its verify button. The script checks exactly the bytes the page rendered — there is no window in which a different bundle could be fetched, and the page stays a single response. The trade-off is size: a very large monthly bill makes a large attribute, accepted for now; an endpoint is the escape hatch if it ever bites.
- **The verifier is fetched and hashed, not trusted.** `bill.js` fetches `/verify-wasm/verify_wasm_bg.wasm`, instantiates it, and shows the SHA-256 of the bytes it ran (where SubtleCrypto exists). The customer can compare the hash with a known artifact; the page never claims a verdict from a module it cannot name.
- **The bundle's own key is the trust anchor.** The verifier is handed `{keyName, publicKey, keyHash}` from the bundle, so the head's signature is checked against what the report names, and the page prints that public key. An unsigned deployment carries no key, verifies membership to the committed digests alone, and says so.
- **No JavaScript is still a bill.** The page renders the figures and the key without the script; the button simply does nothing. The check is progressive enhancement, not the page.
- **What it proves is stated, and what it does not.** The verdict names membership, the subset fold, the marker binding and the head signature, and says completeness remains the organization's claim.

## 2026-10-22 — The bill link is mailed at creation, because that is when the token exists (issue #315)

The database keeps only the token's hash, so a link can be mailed exactly once: in the response to the mint. Waiting — a later "send" button, a job, an email log — would need the plaintext at rest, which is the one thing the design refuses.

- **Mail at creation, not later.** Both mints accept an optional `email`; the mail is sent inside the request and `delivery` reports `sent` or `failed`. No token is ever queued, logged or stored.
- **No mailer is a refusal, not a silent skip.** A deployment without `OXSUM_SMTP_URL`, `OXSUM_MAIL_FROM` and `OXSUM_PUBLIC_URL` answers 400 when an address is asked for: minting a link the operator believes was mailed is worse than refusing.
- **A send failure keeps the link.** The token is in the response either way, so the operator can pass it on by hand; turning the mint into an error would invite a retry and a second link.
- **The mail is as sensitive as the bill.** It goes to the address the operator named, carries the `/bill/{token}` capability and the completeness caveat, and is not a generic notification.

## 2026-10-22 — The payment layer is the full Hyperswitch switch, not prism (issue #317)

oxsum's deposit rail has to end in one normalized, signed event that credits the wallet. Hyperswitch comes in two forms: the switch (a self-hosted service with connectors, routing, refunds, outgoing webhooks and the Control Center) and [`hyperswitch-prism`](https://github.com/juspay/hyperswitch-prism) (a stateless connector library).

**Decision: build on the full switch, developed against the hosted sandbox first ([app.hyperswitch.io](https://app.hyperswitch.io)) and self-hosted with Docker Compose for deployment.**

- **One inbound path.** The switch receives each processor's callback, verifies it, and sends one outgoing webhook per event — `payment_succeeded`, `refund_succeeded`, `dispute_opened`, and 30 others — signed with HMAC-SHA512 in `X-Webhook-Signature-512` from the business profile's `payment_response_hash_key`, carrying an `event_id` for idempotency, retried 16 times over about 24 hours, and delivered out of order (compare `content.object.updated`). Our server parses one schema. With prism, each processor's webhooks, signatures and event vocabulary would land in oxsum — the per-processor surface this layer exists to delete.
- **prism has no Windows SDK.** Its native libraries are published for macOS arm64 and Linux x86_64 only; this project is developed on Windows. The switch runs under Docker Desktop. The gRPC server image would run, but then the deployment has gained a service and kept every per-processor concern.
- **The switch brings the operator surface.** The Control Center configures connectors and shows payments — the operator story the product is built around. prism deliberately has no vault, no routing and no back office.

Costs accepted:

- Production runs the switch's app server beside its PostgreSQL and Redis, next to oxsum's own database. The deployment guide must say so rather than pretend the stack stayed two containers.
- Checkout is the Unified Checkout SDK (HyperLoader) on the top-up page, not a hosted redirect like the direct Stripe rail; the bill page stays script-free.
- The direct Stripe rail stays as a row of the rail honesty matrix; the switch is the primary rail, and the existing Stripe webhook code is the reference for the idempotency the new rail must match.

Consequences:

- W2-2: `POST /api/v1/topups/hyperswitch` creates a payment on the switch (Stripe connector in test mode, automatic capture, metadata carrying the organization and the top-up), the dashboard mounts HyperLoader, and `/api/v1/webhooks/hyperswitch` verifies the HMAC and credits the wallet idempotently by `event_id`.
- W2-3: an `event_id` table and the duplicate/replay rules, plus the out-of-order rule for `payment_*`.
- W2-5: refunds ride `POST /refunds` and `refund_succeeded` on the same webhook.

Sources: [Hyperswitch webhooks](https://docs.hyperswitch.io/integration-guide/webhooks), [prism product overview](https://docs.hyperswitch.io/integrations/prism/prism), [prism README](https://github.com/juspay/hyperswitch-prism), [local Docker setup](https://docs.hyperswitch.io/self-hosting/want-to-explore/readme-1/unified-local-setup-using-docker).

## 2026-10-22 — The bill page checks the witnesses itself, from the bundle (issue #328)

The customer's page already proves inclusion and the operator's signature. A witness is the third party that makes a fork detectable, and until now only the dashboard's log page checked it — the party the witnesses exist for could not see them.

- **The bundle carries the evidence, not a URL.** The bill page is public and sessionless, while `/api/v1/log/checkpoint` is organization-scoped. So the report bundle embeds the deployment's pinned keys (public by design), the log origin, and the cosigned notes stored for exactly the head the bundle carries. A reader needs no second call and no credential.
- **The head size is the selector; honesty is the fallback.** `witness_cosignatures(organization, head_size)` returns only that head's notes. A report whose head the pass has not anchored yet carries none, and the page says "witnesses are configured, but no cosignature covers this head yet" — it does not borrow a later anchor's cosignature, because that would attest a different head.
- **Three states read differently.** No pins → "no witness keys are pinned"; pins but no notes → the sentence above; notes → the wasm `verifyWitnesses` verdict per witness, naming the failing URL. A failed cosignature is shown while the report's own verdict stays what it is; the two claims are separate.
- **Still no third-party script.** The check runs in the same wasm module the page already loads, and `bill.js` remains the page's only script.

Pinned by `crates/server/tests/usage_reports.rs` (the report names the pinset and the origin, carries the stored note, and the bill page embeds all three) plus the verify crate's `verify_witnesses` tests for the cryptography. Contract `openapi.yaml` 0.75.0.

## 2026-10-22 — The seed creates its own demo channel when the catalog is empty (issue #330)

The demo world is the product's first impression — `docker compose up`, `oxsum seed`, a login — and on a database with no catalog it was writing top-ups and no history: the seeded turns need a price book to bill under, so an empty catalog meant zero turns and no statement.

- **The seed owns the demo catalog.** With `OXSUM_SECRET_KEY` set, an empty catalog gets one channel first: `demo`, two priced models (`demo-chat`, `demo-reasoner`), and an upstream URL that is deliberately unreachable (`http://127.0.0.1:9/v1`). The demo serves seeded history, not live traffic; a reachable URL would invite a request the demo cannot answer.
- **The key is the precondition, not an afterthought.** A channel row stores a sealed credential, and `prepare` refuses to start when a stored credential cannot be opened — so a channel can only be created with the deployment's key. Without it the seed keeps the old behavior: whatever catalog exists, or a world with no history. The docs say which case you are in.
- **The channel is ordinary state.** It is written through `Db::seed_channel`, appears in the admin channels page and `GET /api/v1/pricing`, and a deployment that later configures real channels can leave it or delete it. The seed does not special-case it afterwards.
- **The rerun stays a no-op.** The demo email is still the idempotency anchor; the second run reports "already seeded" and writes nothing, even though the channel now exists.

Pinned by `crates/core/tests/seed.rs` (a database with no catalog, seeded with the key, has turns, a statement and both demo models; the rerun writes nothing).

## 2026-10-22 — Completeness gets its own commitment: a per-customer day log anchored in the ledger (issue #336)

A report proves the shown rows are in the ledger and fold to the committed subset; it cannot prove that no other settlement for the customer exists in the span. Completeness is the organization's signature today, and it is the one verifiability gap the plan leaves open. The commitment that closes it must bind *exactly* "every settlement for this `endUser` in `[from, to]`" and must exist *before* the report is minted, or the organization chooses which rows to commit.

**Decision: a per-(organization, endUser) append-only day log, anchored in the organization's ledger at day close; the report bundle carries the day leaves, the rows and the anchor proofs.**

- **The leaf.** `H("oxsum/usage-day/v1", day, count, rows_root)`, where `rows_root` is the Merkle accumulator root over that day's settlement content hashes in log order. A day with usage has exactly one leaf; the log is strictly increasing in day, so a day can never appear twice.
- **The anchor.** At each day close a job appends one ordinary ledger entry to the organization whose description commits `{v, day, map_root, enduser_count}`: `map_root` is the Merkle root over the sorted `(endUser, log_head)` pairs of every customer with a day log at that point — a sorted-map tree, not a set — and `enduser_count` is its size. Because the anchor is a ledger entry, the organization's signed head covers it, witnesses attest it, and the report can prove it.
- **The report.** A v2 bundle over `[from, to]` (whose `to` must be an anchored day) carries, for every day in the span: the day leaf and its index, that day's rows with per-row proofs into `rows_root`, the leaf's inclusion proof in the endUser log's head at the anchor, the endUser's head inclusion proof in the anchor's map, and the two bounding leaves immediately before `from` and after `to`.
- **The checks.** Leaf indices strictly increasing and contiguous between the bounds (a dropped day breaks contiguity; a day the report does not mention cannot hide between two leaves the verifier accepted); every row's proof and the count against the leaf; each day's head against the anchor's map; the anchor entry against the ledger head the marker commits; the marker against the anchored heads.
- **What it proves.** No settlement for this customer inside the span is missing: a dropped row changes `rows_root` and the anchored head no longer matches, a dropped day breaks index continuity, an extra row fails the row root, an altered day leaf fails the anchor map. **What it still does not prove.** That the organization counted the tokens right (write-time honesty), nor anything about days sealed before the mechanism existed.
- **Late settlements.** A closed day's leaf must not change after its anchor, so the settle path refuses a booking date into an anchored day with a distinct error; the report never re-anchors a day to absorb a late row. This is the same sealing discipline a closed month already has, at day granularity.
- **Versioning.** `completeness` is optional. A bundle without it (an unanchored span, or one minted earlier) verifies exactly as today, and its verdict says completeness is the organization's claim; a bundle carrying it is checked strictly and the verdict says "complete within the anchored span". The marker's `v` moves to 2 only when the bundle can carry the proof.

**Stages**, one issue each: (1) migration and the day-leaf append inside the settle transaction, with unit tests; (2) the day-close anchor job and its ledger entry; (3) the v2 bundle and the verifier's completeness check; (4) the sealing rule, the bill page's line, and the backfill policy for unanchored history (it stays claim-only).

**Rejected.** *Per-month sums from the statement rollup*: a swapped row with the same charge passes, and the rollup is per channel/model, not per customer. *A single global MMR over all settlements*: inclusion of the shown rows is already proven; the gap is "all rows for this customer", which a global log cannot distinguish from someone else's rows. *Trusting the organization's count*: that is the gap itself. *Committing at report mint*: the organization would choose the subset after seeing what the customer might check.

**Shipped note (stage 3, issue #339).** The bundle carries the customer's **whole** day log up to the anchor day, not a span range plus bounding leaves: the head is the fold of every leaf, so showing them all is the simplest honest proof that none is missing, and the verifier rebuilds each day's leaf from the rows the proof assigns to that day (a row moved to another day changes both leaves). A range proof would shrink the bundle and is a later optimization, not a correctness change; unanchored history stays claim-only, and the report says which it is.

**Shipped note (stage 4, issue #340).** A day the anchor committed is sealed: Wallet::settle refuses a booking date that falls in an anchored day with 409 DAY_SEALED, so the leaf the anchor fixed cannot be rewritten after the fact. The reconciliation gains nchored_day_altered — the check that the seal held (a bypassed write or a hand edit shows there). History anchored before the rule keeps its claim; there is no backfill that re-anchors a mutable source, and the seal is a write rule, not a repair.

## 2026-10-22 — Credit notes ship first; refunds and chargebacks stay PSP-dependent (issue #352)

D12 splits money-back into three concepts, and only the first can ship honestly without a payment provider: a **credit note** is a reasoned ledger credit that moves no money. It posts one entry — Revenue debited, Wallet credited, the reason as the description — and an `oxsum.credit_notes` row names it, so the charge it answers stays in history beside the credit. The operator issues it from the admin surface with an idempotency key; a retried key answers the standing note, and zero or negative amounts are refused.

A refund needs the PSP to return money and the deposit to move to `reversed`, and a chargeback arrives as a dispute webhook — both depend on the Hyperswitch sandbox and are separate issues. Calling the credit note a refund would be the one thing this table exists to prevent: the page and the docs say plainly that no money moved.

## 2026-10-24 — A refund's ledger half: the deposit reversal, one entry under the deposit's own key (issue #356)

D12's refund has two halves — the ledger reversal (debit the wallet, credit cash, mark the deposit `reversed`) and the PSP call that returns the money — and only the first can be built and tested without a payment provider. The credit-note issue (#352) shipped the no-money half of D12; this one ships the books half of the refund, leaving the Hyperswitch refund call and its webhook to a follow-up issue.

**Decision: `POST /api/v1/admin/deposits/{depositId}/reverse` reverses a `credited` deposit, and nothing else.**

- **The entry is the mirror of a top-up.** Wallet debited, Cash credited, the operator's reason as the description — so the deposit's own credit stays in history beside the reversal and nothing is rewritten. The draw is on the purchased pool alone: granted credit never stood behind a rail payment, so the bonus and credit-line pools are never touched.
- **The ledger refuses what the wallet cannot cover.** The pool's `FundedReservations` limit turns a reversal the customer already spent into `402 INSUFFICIENT_FUNDS` inside the append, under concurrency — never a silent negative balance.
- **One reversal per deposit ever.** The ledger key is derived from the deposit id (`deposit-reversal/<id>`): a retry, a double-click or a second in-flight call replays the standing entry rather than posting twice, and a crash between the append and the row flip leaves a `credited` deposit the retry resumes. The row flips `credited → reversed` in the same transaction that enqueues `deposit.reversed` — the event's first producer — carrying `{paymentRef, amountMinor, reversalEntryId}`.
- **`409` for a deposit that never credited** (`pending`, `confirmed`, `expired`): nothing moved the wallet, so there is nothing to take back. `404` for one that does not exist.
- **The PSP half stays out.** No payment provider is called on this path: the books are right before the money moves, and the docs say so plainly — the same honesty rule the credit note set. The audit row lands under `deposit.reverse` with the deposit-derived key absorbing a replayed call, the way every keyed admin write does.

**Rejected.** *Reversing through a generic signed adjustment*: an adjustment draws the bonus pool first, which is exactly backwards for a refund — the customer's purchased credit is what the rail must return. *Reversing `confirmed` deposits*: the wallet never moved for them, so a "reversal" would debit money the platform never credited. *Bundling the PSP call*: it needs the Hyperswitch sandbox to test honestly, which is what made the credit note ship first too.

## 2026-10-24 — A refund rides the rail; a dispute lands as `disputedAt` + an event (issue #360)

The reversal issue (#356) shipped the books half of D12's refund; this closes the payment-provider half for the one rail that offers it today, and records the dispute a chargeback opens. Two rails of money-back now move — the wallet's reversal and the switch's refund — and the dispute is the third D12 concept, arriving by webhook rather than by operator call.

**Decision: reversing a credited Hyperswitch deposit also issues `POST /refunds` against the payment, and the `refund_*`/`dispute_*` webhooks walk the deposit's `meta`.**

- **The refund goes out after the reversal commits.** The ledger reversal — Wallet debited, Cash credited, the row `reversed` — already stands when the switch is called, so the books are right regardless of what the rail answers. A `hyperswitch` deposit's `meta.hyperswitchPaymentId` names the payment to refund; the refund's `metadata.deposit_id` points back so the `refund_*` webhooks find the row without trusting the payment id alone. `received_minor` is the refund amount when the rail reported one, `amount_minor` otherwise.
- **`refundStatus` lives on `meta`, surfaced as `AdminDeposit.refundStatus`.** `pending` while the switch works it, `succeeded`/`failed` as the `refund_succeeded`/`refund_failed` webhooks settle it. A switch-side refusal at issue time records `failed` — never silent — and a replayed reversal on a `failed` standing re-issues the call, so an operator's retry reaches the rail. A rail with no refund path answers `refundStatus: null`; the reversal path itself is unchanged for them.
- **The `meta` is the record, not a column.** Refund id and standing ride `meta.hyperswitchRefundId`/`meta.refundStatus` beside `hyperswitchPaymentId`, exactly as `stripeSessionId`/`airwallexIntentId` already do — the rail's handles are metadata, the ledger's truth is the reversal entry. `disputedAt` lands the same way.
- **`dispute_opened` records and announces, never reverses on its own.** A dispute names only the payment, so the lookup falls back to `meta.hyperswitchPaymentId`. The handler stamps `meta.disputedAt` and enqueues `deposit.disputed` — the catalog's new event — in the same transaction. Accepting the dispute is the ordinary operator reversal path: a contested charge still moves money only when a person decides it.

**Rejected.** *A `refund_status`/`disputed_at` column*: these are rail metadata, and every other rail handle already lives in `meta` — a column would bless one rail's vocabulary onto every row. *Auto-reversing on `dispute_opened`*: a dispute is a claim, not a settled clawback — the reversal that answers it is the operator's call, so the event announces and the existing path acts. *Refunding inside the reversal transaction*: the switch call is network I/O that cannot join the ledger append — it runs after commit, and a refusal is recorded on the row rather than rolled back into a half-refund.

## 2026-10-25 — The second bill provider lands behind a composite (issue #367)

The `UpstreamBillProvider` seam was cut so a second impl "drops in without touching the job" — this issue proves it does. Anthropic's Usage & Cost Admin API joins OpenAI's Costs behind a `CompositeBillProvider` that fans a channel out to whichever impl its `protocol` names.

- **The wire is genuinely different, the shape is the same.** Anthropic authenticates with `x-api-key` plus a pinned `anthropic-version` header (not `Authorization: Bearer`), and the response wraps its day buckets in `data` rather than `buckets` — but `results[].amount` is a decimal string in `currency` exactly as OpenAI's is, so the minor-unit parser is shared (`pub(crate)`) rather than duplicated. The day-grain `bucket_width=1d` and `[starting_at, ending_at)` window are the same too.
- **The composite is a dispatcher, not a priority list.** A channel names one protocol, so at most one impl can answer it — the loop "first `Some` wins" is honest because only one `Some` is possible. A channel whose protocol no impl serves returns `None`, the same coverage-gap answer a refused call gives.
- **A 200 with an error body is a malformed answer, not a zero.** `CostReportPage.data` is required, not `#[serde(default)]` — Anthropic's auth-failure body (`{"error": …}`) must fail to parse and answer `None`, not silently count as a day with nothing billed. The OpenAI impl already reads `buckets` as required; the same discipline applies here, and the test suite pins it.
- **The day-close gap is the provider's own, not ours.** Like the OpenAI impl, `daily_cost` returns `None` on an unreachable or refused call — the `upstream-bill-sync` job's "record a coverage gap" rule already handles it, so no new error channel is cut.

**Rejected.** *A per-impl `bill_provider` slot on the job context*: the job holds one `Arc<dyn UpstreamBillProvider>` — adding a second slot would teach the job to route, which is the composite's whole point. *Deriving the endpoint from `base_url` minus `/v1`*: the admin API lives under the same `base_url` the relay uses (`https://api.anthropic.com/v1`), so no string surgery is needed — a deployment pointing the channel at a proxy gets the bill through the same proxy. *Sub-day or per-request grain*: Anthropic's cost report is `bucket_width=1d` only, matching the `(channel, day)` grain the issue-#283 decision already fixed.

## 2026-10-25 — The local pre-push gate defers the full suite to CI (issue #370)

AGENTS.md's "run the workspace suite once before pushing" rule predates the current CI shape — it was written when a green targeted suite let two regressions reach main (a hardcoded table list, a wasm-only compile error). Since then CI grew the full gate end to end: `test` covers lib and doctests fast (#237), `build-tests` + three `test-shard` jobs run every nextest binary off one archive (#276), and `test-extras` runs the process-shared suites plus the ledger-gated doubleentry Postgres conformance run (#277) — all on the required-check chain for a merge.

**Decision: the local gate is `fmt --check` + `clippy --all-targets` + `nextest run -p` on the touched crates; the workspace suite runs in CI, not on the developer's machine.**

- **The required checks are what protects main, not the local run.** A regression cannot merge through a red `test-shard`, so the local full run buys only a few minutes of earlier feedback — worth paying on a cross-crate change (a trait, shared config, a workspace dependency), not on every push.
- **`--admin` merges are the exception that proves the rule.** They bypass every check, so the convention stands: never `--admin` a PR whose CI is not fully green. Docs-only PRs skip CI via `paths-ignore`, which is why they are the only ones that need it.
- **CI keeps the whole gate.** Nothing leaves the workflow — this moves where the suite runs, not whether it runs.

**Rejected.** *Keeping the local full run mandatory*: it duplicated CI coverage to guard against a failure mode branch protection now blocks. *Trimming CI to match the thinner local gate*: the shards and archive jobs exist precisely so the full suite answers in minutes on shared runners — weakening CI would re-open the hole this rule once guarded.

## 2026-10-25 — The x402 rail's first live run, and the two wire details it taught us (issue #372)

The x402 rail's `X402FacilitatorVerifier` was already shipped and tested against stubs (#282). What had not happened was a real on-chain settlement — the hosted facilitator's `verify` was answering our shape, but the question "does an actual EIP-3009 signature pass" was still open. Issue #372 ran one end-to-end against `facilitator.x402.rs` on Base Sepolia and the rail is now live-verified: transaction `0xe25f9c399dbb2f620d236246ac382b8d7cf9760bc6ec4d184442a7bff0cc4038` landed the USDC transfer on-chain, the facilitator's relayer paid the gas, and `oxsum.deposits.facilitator_tx_hash` carries the proof.

**Decision: keep the throwaway Python signer as `scripts/x402_sign.py`, and record the two wire details that bit during bring-up so a reader reproducing the exercise does not have to rediscover them.**

- **The contract's EIP-712 domain `name` is `"USDC"`, not `"USD Coin"`.** Circle renamed the token when moving from FiatToken v1 to v2, and the domain separator commits to whichever string the contract actually signed under. Probing `DOMAIN_SEPARATOR()` on the live contract returns `0x71f17a3b…` — matching `"USDC"` — where `"USD Coin"` would produce `0x2f5ab5ee…`. A signer with the wrong name produces a valid-looking signature the contract rejects with `FiatTokenV2: invalid signature`, one level above the more common "wrong v" failure.
- **`v` on the wire is `{27, 28}`, not the recovery id `{0, 1}`.** `coincurve` and most modern secp256k1 libraries return the raw recovery bit; Ethereum's `ecrecover` and the x402 wire convention both expect it shifted by 27. The script does the shift; a signature with `v` unshifted surfaces from the facilitator as `ECRecover: invalid signature 'v' value`.
- **The facilitator fronts the gas.** Under `exact` on EVM, the facilitator's relayer submits the `transferWithAuthorization` transaction itself — the test wallet only needs the asset being transferred (faucet USDC), never the chain's native token. This is what makes the rail cheap to exercise on a testnet.

**Rejected.** *A Rust signer instead of Python*: pulling `alloy` or `ethers` into the workspace to sign one authorization would drag a large dependency tree in for a once-per-deployment exercise. `coincurve` + `pycryptodome` are pip-installable and the script is short enough to audit by reading. *Hardcoding the token name*: it bit once already — the env var makes the next chain's bring-up one probe away instead of a debug session.

## 2026-10-25 — The Airwallex rail's HTTP client pins TLS 1.2 for this one endpoint (issue #374)

`api-demo.airwallex.com` produces TLS 1.3 records rustls cannot decrypt — the handshake completes (`TLS13_AES_256_GCM_SHA384`, ALPN `http/1.1`, the three-certificate chain parses), then the first post-handshake record fails with `DecryptError`. Curl over schannel answers the same endpoint cleanly, so the credentials and IP allowlist were never the problem. The failure reproduces on direct connections, DNS-pinned IPs and explicit `CONNECT` proxies, which rules out DNS hijacking and the local TUN path — the bytes themselves are the issue, not the path. Forcing `max_tls_version(TLS_1_2)` on the client makes the same login succeed.

**Decision: `Airwallex` carries its own `reqwest::Client`, built by `airwallex_of()` with `max_tls_version(TLS_1_2)`, instead of borrowing `state.http` like the other rails.**

- **Rail-scoped, not workspace-scoped.** The rest of the workspace keeps TLS 1.3 — the gateway's upstream calls, the facilitator's outbound calls, the webhook deliveries all keep the modern floor. Only this one endpoint's record format is the problem.
- **The borrowed `http: &reqwest::Client` parameter on `create_payment_intent` was the wrong seam anyway.** A rail that has a transport-specific requirement owns its own transport — the test stub's plain HTTP is unaffected, and a deployment whose Airwallex region's TLS 1.3 path is already known good could pass a different `reqwest::Client` through `Airwallex::new` without code changes.
- **Stays inside the rustls-only deployment rule.** No `native-tls`, no OpenSSL in the image — the runtime's "no OpenSSL needed" property is preserved; TLS 1.2 over rustls is still rustls.

**Rejected.** *Switching the workspace to `native-tls`*: that would put OpenSSL back in the deploy image, reversing the decision that landed the workspace on a single TLS stack. *Pinning only when `OXSUM_AIRWALLEX_API_URL` contains `demo`*: that ties a TLS workaround to a URL substring and gives prod a different code path — the demo-specific shape belongs in configuration, not branching. *Reporting the rail as broken*: the TLS layer answers over 1.2, so the rail is workable; calling it broken would undersell the integration coverage that already exists.

## 2026-10-25 — The Airwallex rail reached sandbox-verified; the hosted-page front-end stays unmounted (issue #377)

The Airwallex rail is now sandbox-verified end-to-end at the API and webhook layers: a real `x-client-id`/`x-api-key` login mints a bearer token, `POST /api/v1/topups/airwallex` returns a real `intentId`/`clientSecret`/`checkoutUrl` from `api-demo.airwallex.com`, and a webhook signed under `OXSUM_AIRWALLEX_WEBHOOK_SECRET` flips the deposit `pending → credited`. Issues #375 and #376 were what it took to get there — `api-demo.airwallex.com` produces TLS 1.3 records rustls cannot decrypt, and the login endpoint expects `Content-Length` even on a bodyless POST.

**Decision: the rail's row in `docs/rails.md` is `sandbox-verified`, and the hosted-page JavaScript SDK stays deliberately unmounted.**

- **`checkout-demo.airwallex.com` is an SDK bootstrap, not a payment page.** Unlike Stripe Checkout — which is a complete self-contained HTML form — the Airwallex hosted URL is a shell that loads `@airwallex/components-sdk` and expects the integrator's front-end to mount `AirwallexPaymentElement` with the `clientSecret` the intent returned. Building that front-end is a product commitment oxsum does not need to make: the recommended card rail is Hyperswitch, which exposes Airwallex among ~60 connectors behind one SDK surface the dashboard already mounts (issue #320).
- **The synthetic webhook is honest, not a shortcut.** Airwallex's webhook contract — `x-timestamp` + `x-signature` over `timestamp + body` under `whsec_…` — is exercised the way the deployment would exercise it: real signature, real HMAC check, real deposit walk. What is not exercised is the *delivery* from Airwallex's side, because that would need the hosted page to be mounted first. `scripts/airwallex_webhook.py` carries the recipe.
- **`sandbox-verified` is the honest level.** The rail can receive money; it cannot present the form that lets a payer give it. That is the same position Stripe's rail is in and the same level its row reports — only Hyperswitch has ever been verified end-to-end on a real checkout surface, because that rail exists to be the card rail.

**Rejected.** *Building a minimal Airwallex-SDK demo page in `web/`*: the page would prove a fact already proven — the SDK works — at the cost of a front-end surface the product does not want to ship. The recommended rail mounts the SDK once through Hyperswitch's Unified Checkout. *Reporting `live-verified`*: the deposit's `received_minor` was credited by a signed webhook whose body we generated locally — no real card touched Airwallex, so the level one row higher stays out of reach until a deployment runs a real intent through the hosted page.

## 2026-10-25 Unpin the Rust toolchain (issue #383)

- Status: Adopted
- Decision: `rust-toolchain.toml` is deleted; the repo no longer pins a Rust version. `rustup` resolves `stable` in the working tree, and `.github/workflows/ci.yml`'s `dtolnay/rust-toolchain@stable` steps drop their `toolchain:` input so CI uses the runner's stable too. The `Dockerfile` build stages move from `rust:1.98-bookworm` to `rust:bookworm`. The workspace MSRV is unchanged — `rust-version = "1.98"` in the root manifest (1.94 for vendored doubleentry) remains the floor cargo enforces.
- Why: a solo-maintained project pays the pin's cost without its benefit. The pin's real job was keeping local fmt/clippy in lockstep with CI; on one machine that lockstep is trivial — a new lint firing on a stable bump is a `cargo clippy --fix` away. What the pin guaranteed was six-weekly toil: every release needed the toml, the workflow and the Dockerfile bumped together or the three drifted apart.
- What the pin was not doing: protecting old-compiler builds. That is `rust-version`'s job and it stays. It was also not making the release binary reproducible — `Cargo.lock` pins the dependency graph; the compiler version never entered the shipped artifact's contract.
- Rollout note: the wasm stage previously copied `rust-toolchain.toml` so `rustup target add` would land on the pinned toolchain; with no pin the target simply lands on the image's stable — the ordering hazard the comment described is gone, so the comment is gone with it. `rust:bookworm` is a moving tag; a stable bump that breaks the image surfaces in the `wasm`/`frontend`/`build-tests` PR gates, not at release time, because those jobs build the stages on every push that touches their inputs.

## 2026-10-30 — Route ordering is cost-aware inside an equal weight tier (issue #438)

- Status: Adopted
- Background: `route_order` (issue #168) answered "which upstream can serve this" with a weighted pick followed by descending weight. Two routes of equal `weight` were distinguished only by channel name — an arbitrary tiebreak — so a deployment running the same model on a cheap upstream and a dear one at the same preference sent half its traffic to the dear one for no reason.
- Decision: the weighted pick now selects a *weight tier*, and the tier is represented by its cheapest `inputPricePerMillion` route. The rest of the attempt order is `weight` descending, then price ascending, then channel name — so among equally-preferred routes the cheapest leads and the dearer ones sit in failover position, and a cheaper route never outranks a heavier weight. Nothing new is stored: the ordering falls out of data `channel_prices` already carries.
- Why the tier framing rather than sorting the whole list by price: `weight` is the operator's routing intent — a 10-weight route is a deliberate fallback, and letting a cheap price flip that ranking would make "cheap but far away" silently beat "preferred". Price breaks ties, never intent. The settlement description needs no schema bump: it names `(channel, version)`, which already re-derives both the weight and the price the ordering read.
- Rejected: *a separate `costPreference`/`routePolicy` field per channel* — a second knob that says what the price book already says, and a field the settlement would then have to record for the order to stay re-derivable. *Sorting by `upstreamCostMinor` (margin)*: the platform's cost is the `upstream` price block, which is optional and often absent; the customer-facing `inputPricePerMillion` is always there and is the number the request's bill is computed from.

## 2026-10-30 — A channel carries a pool of upstream credentials, not one key (issue #444)

- Status: Adopted
- Background: `channels.api_key_sealed` held exactly one upstream key, but a provider's rate limits and quota are per-account, not per-endpoint. One account's 429 could only fail over onto a *different* channel — another provider, another price — when the next account behind the same provider would have served the turn at the same price. magpie solves this with an account pool per provider group (`internal/provider/account*.go`, `group_serve.go`).
- Decision: the channel's key material moves to a `channel_credentials` sub-table — one channel, many sealed keys, each with `position`, `status` (`active`/`disabled`), `cooldown_until`, `last_error` and `last_used_at`. `crates/core/src/pools.rs` owns the pool: `healthy_credentials` is the health filter (active, not cooling), `order` is the pick inside one channel — the conversation's sticky credential first, then least-recently-used, then the operator's `position`. The gateway now routes in two levels: `route_order` picks the *channel* order over one `Serving` per channel (the pool never inflates a channel's weight), and inside a serving's attempts the credential order walks the pool. A credential-produced refusal — a 429 or 5xx before answer bytes — cools that credential for a bounded minute and the turn rotates onto the pool's next key under the same hold; an unreachable upstream cools nothing, because it blames the channel, not one key. The pool is managed as pool members (`POST`/`PATCH`/`DELETE /api/v1/admin/channels/{name}/credentials[/{id}]`), and posting `apiKeys` on the channel re-credentials it wholesale — "set" still means "replace".
- Why credential choice stays out of settlement: the description names `(channel, version)` and recomputes from the price book alone — which account answered is an operational detail, not a billing one. Every credential of one channel serves the same model at the same price, so the bill is identical whichever key answered; recording it would buy auditability the `last_used_at`/`last_error` health columns already give the operator, at the cost of a schema bump and a second identity in every description.
- Why stickiness is process-local (`Affinity` on `Db`): the hint exists to keep the upstream's prompt cache warm for one conversation, and a provider cache lives minutes at most — an in-memory map with a 10-minute TTL is exactly the right lifetime. A restart or a second process simply picks fresh, and the worst cost of a missed stick is a cold upstream cache — never a wrong answer. Storing it would turn a cache-warmer hint into shared state that needs invalidating.
- Rejected: *channels per account* (one channel per upstream key, weighted normally) — it fakes the pool with routing config, splits the price book into per-account rows, and leaks account identity into the settlement description. *Round-robin or random pick* — ignores both the operator's order and actual usage; LRU spreads keys evenly and sticky-first keeps caches warm, both for free from data the health columns already carry. *Cooling the channel* on a credential refusal — the old single-key behavior; the pool exists precisely so one account's wall does not forfeit the route. *Persisting affinity* — shared state with nothing to share: the value a stick protects (a warm upstream cache) expires on the provider's clock, not ours.

## 2026-10-31 — The `ui/` layer moves to Radix primitives and vendored shadcn sources (issues #448–#460)

- Status: Adopted
- Background: the component layer's behavior came from `@base-ui/react` while its look came from DESIGN.md tokens. The audit against the production design systems (Stripe, Mercury, Linear, Supabase — local-notes/2026-10-31) showed the gaps were spacing/elevation grammar, not palette: no shadow ladder, a coarse type scale, no z-order names, no motion or icon rules. shadcn/ui fixes the grammar problem differently from a library — it is a *registry*: `npx shadcn add` copies component source into the repo, where it is rewritten onto the product tokens and owned like any other file.
- Decision: the behavior layer moved from `@base-ui/react` to Radix (`@radix-ui/react-*`, one package per component — never the `radix-ui` aggregate, so tree-shaking stays exact). The overlay, form and navigation pieces are vendored shadcn/ui sources (`components.json` records the registry config: new-york style, `@/components` aliases). Custom components kept their names but adopted the shadcn conventions — `forwardRef` + `displayName`, `data-slot` on the root node, `cva` for variant axes, one component per file. Three deliberate exceptions where an existing API fit the pages better than the registry's: `Card` keeps its `title`/`actions`/`flush` props over the CardHeader/CardTitle triplet, `TextInput` keeps `trailingSlot`, and `Button` keeps `tone`/`isLoading` over `variant`/`asChild`.
- Why vendoring beats the library here: the vendored file is rewritten onto `bg-surface-raised`/`z-modal`/`shadow-2` at copy time, so the palette lives in the source, not under an override layer fighting upstream defaults. Upstream drift is a non-cost — the file stops tracking upstream the day it lands, and a stale pattern is a style nit, not a CVE.
- What Radix bought over `@base-ui/react`: the shadcn sources assume Radix's prop surface, so keeping base-ui would have meant porting every vendored overlay. Radix also ships the pieces base-ui lacked for the migration — `AlertDialog`'s focus-managed confirm, `Tooltip`'s provider-shared delays — and its `data-[state=open]` attribute model is what the shadcn animation classes key on.
- Rejected: *a blanket `npx shadcn add --all`* — the registry's card/input/button collide with components whose APIs already carry product meaning (`Card`'s `actions` slot, `Button`'s loading state); vendoring them would fork the vocabulary for no gain. *Keeping `@base-ui/react` alongside Radix* — two primitive layers with different portal/focus models behind one component layer. *Publishing the restyled set back as a registry* — the value is that the files are ours, not that they are shareable.
- Pages adopted the pieces where the migration changed behavior, not everywhere it could: destructive confirms moved from inline state to `AlertDialog` (Settings delete-account, Webhooks delete-endpoint), `IconButton`'s native `title` became a Radix `Tooltip` under a root `TooltipProvider`, and loading states that showed fake `0` amounts became `Skeleton` shapes — a loading balance that prints zero is a lie the ledger would never print.

## 2026-10-31 — The transparency log's wire protocol is written down as a spec (issue #472)

- Status: Adopted
- Background: the proof surface grew object by object — `ProofBundle`, the signed-note head, `PriceLink`, consistency and witness proofs, the statement anchor, the usage-report bundle — and the only place their wire shapes lived was `crates/verify/src/lib.rs`'s serde structs. A third party wanting to verify a bill without linking the crate had to reverse-engineer the format from the implementation; `docs/proofs.md` says what each check establishes but not how it is computed.
- Decision: `docs/transparency-log-v1.md` writes the protocol down — terminology, every wire shape field-by-field, the hash domains, and each verification algorithm as implementor-independent pseudocode, plus the trust model and a versioning rule. `oxsum-verify` is now a reference implementation of a named spec rather than the spec itself, and the crate README, the `verify-oxsum-bill` package README and `docs/proofs.md` all point at the document.
- Why a spec before the component: the next artifact is a `<verify-bill>` web component on top of the npm SDK, and a component built against an unwritten protocol hard-codes whatever the implementation happened to do that day. Naming the wire `v1` gives both the SDK and any third-party verifier a fixed target — and gives a future incompatible change a version to bump against.
- Rejected: *leaving the serde structs as the spec* — an implementation is not a contract; there is no way to tell which field orders and defaults are load-bearing. *Documenting inside openapi.yaml* — the proof objects are not request/response API; they are portable artifacts a verifier carries away, and the spec's audience is the verifier's author, not the API caller.

## 2026-11-01 — The Gemini surface lands on the adapter seam; one credential spelling for AI Studio and Vertex (issue #494)

- Status: Adopted
- Background: Gemini's generate API differs from the two shipped surfaces in four ways that each looked like it wanted new plumbing: the model name lives in the URL path (`/v1beta/models/{model}:generateContent`), streaming is an action (`:streamGenerateContent?alt=sse`) rather than a `stream` field, the credential header is `x-goog-api-key`, and usage reports arrive as `usageMetadata` with its own counting rules — `promptTokenCount` already includes the cached share, `candidatesTokenCount` excludes the thinking share, and `toolUsePromptTokenCount` sits outside the prompt count.
- Decision: the whole protocol absorbs into the existing seams. `Surface::Gemini { streamed }` is one enum variant; `GatewayRequest::parse_for_model` binds the model from the path instead of the body (a `model` field may only repeat it), and `upstream_target` rewrites the path per action rather than per endpoint name. The `GeminiGenerate` `UsageAdapter` folds the counting rules back into the `UsageRecord` invariants (`input_tokens` includes cached, `output_tokens` includes reasoning) so the settle pipeline and every billing credential stay unchanged — `wallet.rs` was not touched. On the channel side, `x-goog-api-key` is the only credential spelling the gateway sends: AI Studio accepts it natively, Vertex accepts it for generateContent, so one header covers both deployments and the channel row never asks which Google flavor it points at.
- Why the wildcard route: axum allows one path parameter per segment, so `{model}:{action}` cannot be a route — `/v1beta/models/{*target}` takes both in one segment and the handler splits on the first `:`. OpenAPI cannot express the colon segment either, so the contract declares the two concrete paths the surface actually serves.
- Rejected: *a translate-to-OpenAI gateway* (run Gemini requests through an internal OpenAI shape) — that is a translation layer with all the fidelity losses this codebase refuses on principle; the surfaces stay protocol-native, never translated. *Sending Google's full OAuth2 service-account flow for Vertex* — a whole credential machinery for the one case where `x-goog-api-key` is not enough (Vertex endpoints that require OAuth are outside generateContent's API-key path); a channel pointing at those can carry a manually-minted token as its apiKey until a real deployment needs more. *A `[DONE]` sentinel appended to Gemini streams* — Gemini ends on the candidate's `finishReason`; forwarding upstream's frames untouched is the pipeline's contract, so `closing_frame` is empty.

## 2026-11-01 — `/v1/responses` rides the `openai` channel pool; response state stays upstream's (issue #495)

- Status: Adopted
- Background: OpenAI's Responses API is the successor wire the newer SDKs default to — `input` items rather than `messages`, `max_output_tokens` rather than `max_tokens`, `store`/`previous_response_id` for server-side conversation state, and a terminal `response.completed` event rather than a `[DONE]` sentinel. new-api and litellm both answer it, either natively or by translating to a chat-completions upstream.
- Decision: a third OpenAI surface, not a translation. `Surface::Responses` keeps `protocol() == "openai"` — the channel pool, the Bearer credential and the failover machinery are unchanged — while `upstream_path()` names `responses`, so `adapter_for_endpoint` resolves the new `OpenAiResponses` adapter whose `usage()` reads `input_tokens`/`output_tokens` with the `*_tokens_details` subsets. The relay's frame walker changed in one small way for it: a `data:` payload is parsed for usage *before* `ends_stream` is asked, because the terminal `response.completed` event is exactly where the final usage rides — on every other wire the terminator carries nothing.
- Why no translation layer: converting a Responses call into a chat-completions call (new-api's reverse path, `responses_handler.go`) would re-derive `input` items into `messages` and the `output[]` list back into `choices` — fidelity losses on reasoning items, tool calls and refusal records that the price bound would then have to guess at. Serving the wire natively keeps "a client that works against the provider works here" true, and the channel's own protocol stays the only dialect the surface filters on.
- Why `store`/`previous_response_id` are pass-through: Responses' stateful flow is upstream's feature, not the gateway's — a deployment that wanted response continuity would have to store output items, replay them into later turns and reconcile what that does to input bounds the caller never sent. Recording `previous_response_id` as an attribution field for the customer's own log is a follow-up if anyone asks for it; the first cut bills the turn, not the chain.
- Rejected: *translating to `chat/completions` upstreams* — the fidelity losses above, plus an adapter that then meters a different wire than the client speaks. *A `responses` protocol* — a second protocol name for the same provider spelling would split the channel pool: a model on an `openai` channel is already priceable for this surface, and a `responses`-protocol channel would force operators to register the same upstream twice. *Persisting response state in oxsum* — a whole new state machine (retention, replay, billing a chain's cumulative input) the issue explicitly scopes out.
## 2026-11-01 — Subscription quota is an entitlement counter, not ledger money (issue #496)

- Status: Adopted
- Background: the prepaid wallet is the only thing a turn could draw on, but a SaaS subscription ("Pro $20/mo includes N credits") is a different object — a fixed-period quota that refreshes on a cadence, outside the double-entry ledger. Issue #496 ports new-api's plan/subscription pair into oxsum's shape; the reference (`references/new-api/model/subscription.go`) was read for semantics and not copied.
- Decision: quota lives on `oxsum.org_subscriptions.consumed_minor` — a counter bounded by the plan's `quota_minor` snapshot — never as a ledger posting. Admission order is subscription first: `admit_subscription_draw` debits the period counter up to the freeze under a per-organization `pg_advisory_xact_lock` (`hashtextextended('oxsum.subscription' || organization_id, 0)`) — the same lock bind, cancel, draw-settle and both rollover paths serialize on, so no two transitions interleave a half-moved subscription — then `hold_for_key` reserves only `freeze − drawn_minor`. A fully subscription-covered turn still takes a 1-minor wallet hold, because the ledger refuses a zero reservation and the settlement must land on a hold; the settle writes `charged − subscription_charge` against it while the description records the full charge. The draw's idempotency anchor is the hold's own `req-<id>:hold` key on `oxsum.subscription_draws`, matching the ledger hold's derived-key rule — a replayed hold or settlement answers its first outcome rather than debiting twice. A swept hold settles its draw at 0 the same way it releases the freeze, so a dead turn cannot leak quota.
- Audit of a subscription's state transitions is the `oxsum.subscription_periods` table, not the `admin_audit` log: every binding, rollover, cancel, expiry and plan change opens or closes one row carrying the window's bounds, the quota snapshot it ran under and what it consumed — `open` while it meters, then `boundary`/`cancelled`/`expired`/`upgrade`/`downgrade`. The admin_audit row records *who called*; the period row is the *proof of what the subscription did*, so the org-facing history endpoint reads periods, not audit. Which entitlement paid a turn is bound into the bill itself: the settlement description's v9 schema carries `{subscriptionId, periodId, drawnMinor, chargedMinor}` under the content hash, and `verify_charge` recomputes the charge's arithmetic unchanged while binding the drawn/charged pair's shape — a bill proves it ran inside the Pro quota the same way it proves the figure.
- Rejected: *quota as a ledger pool* — a subscription is not a balance the org owns; posting it to the ledger would make quota transferable, provable-as-money and subject to the no-overdraft rules in the wrong places (a rollover would have to move entries). *Admin audit as the transition record* — the audit row names the actor and the action but cannot answer "what did this period consume against what quota"; the period table can. *Skipping the wallet hold entirely on a fully-covered turn* — a settlement is keyed on the hold it settles; a hold-less turn would need a second settlement path and a watch-row story of its own, for one minor unit of ledger noise. *One rollover instead of chained* — a subscription quiet past several boundaries would have its fresh window invented from `now`, silently erasing the audit for the skipped periods; rolling boundary-by-boundary closes each window the gap actually spanned.
- `never` keeps one running counter for the subscription's life: `current_period_end` is NULL so no due scan ever sees it — nothing rolls, nothing expires on time, and only a cancel or the rail's `customer.subscription.deleted` ends it. The alternative (`end = start`, expiring on first due check) makes a `never` plan die on its first use — a quota that was sold as lifetime.

## 2026-11-01 — The task relay is a turn on a minutes timescale: hold at submit, settle on the poll pass's terminal read (issue #503)

- Status: Adopted
- Background: new-api's `plugins/tasks/` relays asynchronous upstreams — image, video, batch jobs that run for minutes — where holding a socket open is wrong on both sides. Every existing surface is one request/response: a task needs a submit that returns immediately, a poll that answers coarse state, and a settle that lands whenever upstream finishes, none of which fit the stream-until-done relay.
- Decision: a task is a turn whose relay is the `poll-tasks` periodic pass rather than an SSE stream. `POST /v1/tasks` freezes the declared bound (`maxCostMinor`, or the price's `costPerRequest` when the caller names none — a task's bound is a fixed figure, not a token estimate, because the usage units are not yet known at submit), relays `{baseUrl}/tasks` under the pool's lead credential, records `external_task_id`, and answers `{taskId, submitted}`. The `poll-tasks` pass — unleadered, every 30s — asks `{baseUrl}/tasks/{externalTaskId}` for each open row and calls `settle_task` on the terminal answer: `succeeded` itemizes the result's `usage` through the `tasks` adapter (`input_tokens`/`output_tokens` when upstream counts tokens, the whole `usage` object kept as `provider_raw` for task-shaped units), `failed`/`cancelled` release at zero as `released`. The webhook (`task.succeeded`/`task.failed`) writes inside the settle's own transaction, so a landed settlement and its notification never diverge. The ledger needs nothing new: the hold is `req-<task id>:hold` and the sweeper's 30-minute timeout is the backstop for a task upstream never finishes.
- Why a periodic pass and not client-side polling hitting upstream: the caller's `GET` is a row read by design — a hundred clients polling one task costs one upstream question per tick, not a hundred. The pass being unleadered is deliberate: two workers racing one task's settle collide on the settlement's derived idempotency key, which only one can win — a loser is a wasted upstream question, never a double charge.
- Why `released` for cancel/fail rather than `swept`: a cancel is a deliberate close — the caller said the work is no longer wanted — distinct from a hold nobody settled. The kind keeps the anomalies page honest: `swept` means the backstop fired, `released` means a decision did.
- Rejected: *a `tasks` protocol adapter that streams* — there is no stream to relay; the trait's stream-side methods exist only because `UsageAdapter` is the registry's shape, and `TaskUsage` satisfies them trivially. *Holding the submit open until upstream answers with progress* — defeats the point of an async job and re-introduces the socket-lifetime problem. *A `BillingMode::Task` variant* — `costPerRequest` inside the existing `event` mode already prices a task's flat bound; a new mode would duplicate the price row's semantics for one field.

## 2026-11-01 — The entitlement read is a snapshot, not a reservation (issue #501)

- Status: Adopted
- Background: the key constraints gate requests at admission, but nothing answered "may this key call now" without spending the request — a client sizing concurrency, or a dashboard naming why a delegate fails, had to try and read the error. Issue #501 adds `GET /api/v1/org/keys/{keyId}/entitlement`; openmeter's entitlement API and bifrost's virtual keys are the shape's precedents (`references/openmeter`), read for semantics and not copied.
- Decision: the endpoint composes the same checks admission runs, but read-only — key liveness, org suspension, `modelAllowlist`, the RPM window's occupancy, the TPM window's settled count, the spend caps' committed headroom, the open-holds count, and a presented mandate's window/models/cap. `Wallet::entitlement_read` is a new, purely additive method (the hold/settle/release paths and their private helpers untouched): it reads the whole ledger-side state on one unlocked connection and folds the member and team caps into one `budget_ok`/`remaining_budget_minor` — the tightest cap in force is the answer's figure, mirroring `hold_for_key`'s layered refusal. Route-side, the request window goes through a new `RateLimiter::peek` — `admit`'s twin minus the record, so asking spends nothing — and the token window through `Db::tpm_window`/`tpm_window_scoped`. `Entitlement::compose` then names the first refusing check in the documented order (key state → capability → rate windows → money → concurrency → delegation) and carries every constraint's verdict in `checks`, because a debugging surface wants the whole picture, not just the first refusal.
- Why a snapshot and not a reservation: the answer *cannot* be atomic with the request it predicts — between the answer and the call a racing request may spend the headroom reported. Pretending otherwise would need the entitlement read to hold a freeze of its own, which is exactly the spending the endpoint exists to avoid. The contract says so explicitly: `allowed` is "a request shaped like this would be admitted now", and the atomic admission checks remain the only authoritative gate.
- Why `mandate_invalid` is an answer, not an error: half the endpoint's purpose is diagnosing a refused delegate — "why does this blob not verify" *is* the question. A blob that cannot decode, parse or verify against the key's public half returns 200 with `mandate_invalid` (the HTTP 403s on `/v1` and `/api/v1/holds` stay as they are — those paths admit or refuse, this one explains). The same reasoning puts a `mandate_cap` refusal's `remainingBudgetMinor` at the delegation's headroom, not the key's — the figure the caller needs is the cap that refused.
- Rejected: *admission-order refusal semantics* — the documented order is the composition's, not the internals of `hold_for_key`, whose per-key lock and layered checks are built for atomicity rather than explanation; a snapshot needs the deterministic, legible order. *Checking the TPM window against the estimate* — the estimate is minor units (money), the window counts tokens; the verdict is `settled < limit` ("room at all"), `remainingTpm` the gap. *A separate `HEAD` endpoint* — the `checks` breakdown and the headroom figures are the point; a bare status answers less than the debugging half of the feature needs. *A reservation token returned for the real call* — a second, hold-like artifact whose whole job is racing other callers is a new concurrency surface for a convenience no client asked for.
- The keys page's Live column polls the endpoint per visible key (30 s cadence, skipped on revoked rows — their answer cannot change). It is deliberately the thinnest use: the endpoint's real consumers are SDKs and the mandate-debugging surfaces it was designed for.

## 2026-11-01 — The semantic cache is an exact-digest store billed at the cache-read rate, not a similarity search (issue #497)

- Status: Adopted
- Background: the issue asks for a semantic response cache — a near-duplicate request re-served from a stored answer — pointing at bifrost's `semanticcache` plugin, which embeds the prompt and hits on cosine similarity above a threshold. The bill-honesty question decides the shape: a charge for "the answer to a similar question" has to defend *which* question it answered, and a 0.8-cosine hit is a judgement call the ledger cannot prove.
- Decision: v1 is an exact-digest store, not a vector search. `oxsum.cached_responses` keys `(organization_id, channel_id, model, prompt_digest)` where the digest is BLAKE3 `oxsum/prompt/v1` over the surface's role-tagged prompt, lowercased and whitespace-folded — bifrost's `normalizeText`, the two variations a paraphrase never hides behind, and the most a digest can tolerate before "same question" stops meaning anything a bill can defend. The role rides into the digest with the text, so a `user` turn and an `assistant` turn carrying the same words never collide; `excludeSystem` drops the `system`/`instructions` share for a channel that deliberately ignores it. pgvector was evaluated and rejected: dev and CI run `postgres:17-alpine` without the extension, and swapping images is infra scope the issue does not buy — the digest is the honest v1, and a similarity-search version would write its real score into the same `similarity` field rather than borrow a threshold.
- Why the hit is billed, not free: a hit is a legitimate charge — the customer asked a question and got an answer — priced at the cache-read rate rather than a fresh turn's. `Charge::Cached` folds the stored usage's `input_tokens` into `cached_tokens`, so `itemize` prices the whole input at `cacheReadPricePerMillion` (or the input rate when the book names none — the honest "no cache discount" the same rule gives a provider-side hit) and the output at its own rate: the answer was produced once, its tokens are real, and re-serving them is the cheap case, not the free one. No new `SettlementKind` — `kind: usage` stays honest because the usage *is* upstream's own counts, replayed.
- Why the hit still takes a hold: a settlement is keyed on the hold it settles, so the replay runs the same `note_open_hold → admit_subscription_draw → hold_for_key → settle` pipeline a fresh turn does — with the freeze sized at the stored usage priced under the live book, not the fresh bound. That keeps the sweeper, the subscription draw and the wallet's no-overdraft rule all un-reimplemented, and the v10 settlement's `cache` block (`{sourceEntryId, similarity: 1}`, `upstreamAttempts: 0`) names the entry the answer was produced under — `/verify` proves the reuse it charges for.
- Why the lookup sits after the route pick and before the freeze: the cache is per-channel — a row is meaningless outside the `(org, channel, model, digest)` key — so the primary route's `settings.semanticCache` is the policy that gates it, and `Serving` carries the `settings` blob so the check needs no second read. A hit skips only the reservation sized for a fresh answer; every admission check (rate windows, mandate, tier) still ran upstream of it.
- Why org-scoped-and-shared-inside: the org boundary is the privacy rule the feature stands on — one tenant's prompt and answer never serve another's. Inside one org the keys share the rows — bifrost's `disable_identity_scoping` default, flipped: the organization's own turns paid for the answer, so its other keys may reuse it.
- What is never cached: a streamed request is never looked up and never stored — the cache holds whole bodies, and an SSE client cannot be handed one; re-chunking a finished answer is a feature, not this one's. An empty or media-only prompt digests to `None` — hashing an empty shape would collide unrelated asks into one row. A failover's answer is not stored under the primary's key — it was produced under another channel's upstream, and filing it under this one's key would serve it next time as this channel's own. An estimated settle is not stored either: a hit must bill provider-counted usage, and an estimate would bill a guess at a real price.
- Rejected: *pgvector + cosine threshold* — the extension is absent from the `postgres:17-alpine` dev/CI image, and a 0.8-similarity hit answers "a similar question," which is not the question the bill names. *Free hits* — a zero charge for a served answer is the dishonest figure either way; the cache-read rate is the same doctrine a provider-side hit already prices. *Per-key scoping* — an organization's own turns paid for the answer; splitting the store by key would re-charge a customer for what it already owns. *Caching streamed answers by reassembly* — a whole-body store behind an SSE relay needs every protocol's frames folded back into one document; possible later, and the wire contract a hit serves is already fixed (whole body, `x-oxsum-cache: hit`), so v1 does not gamble on it.

## 2026-11-01 — Enterprise login is a provider registry beside GitHub; the second factor is hand-rolled TOTP over a two-phase login (issue #504)

- Status: Adopted
- Background: enterprise deployments run Okta/Keycloak/Authentik, not GitHub — and accounts holding spend need a second factor. The issue asks for OIDC federation plus TOTP; the existing shape it builds on is one hardcoded provider (`crate::oauth::PROVIDER = "github"`) and a one-leg `login` that always minted.
- Decision: the OAuth routes generalize to `/api/v1/auth/oauth/{provider}` — `provider` is `github` or `oidc:{name}` per `OXSUM_OIDC_PROVIDERS` (a JSON array of `{name, issuer, clientId, clientSecret}`, each needing `OXSUM_PUBLIC_URL` for the callback it registers). `OidcProvider` lives in `crates/integrations/src/oidc.rs` as the GitHub client's sibling: the same three legs, with discovery fetched once off `.well-known/openid-configuration` into a `tokio::sync::OnceCell` (a down IdP fails that login, never boot) and the identity read from the `id_token` — an RS256 JWS verified through `jsonwebtoken` against the provider's JWKS, `iss`/`aud`/`exp`/`nonce` validated, `email_verified` required exactly as GitHub's is. Resolution and session-minting run the unchanged `oauth_login`/`session_for` pair; `oauth_accounts.provider = oidc:{name}` keeps each issuer's `sub` namespace distinct, because a `sub` is only unique per issuer.
- Why PKCE on the OIDC leg: the `code_verifier`/`nonce` ride inside `oauth_states` (migration 0066 adds the two NULL columns — GitHub's rows simply keep them NULL), so the server-held verifier never crosses the browser's redirect while the authorize URL carries only its S256 hash. `consume_oauth_state_full` returns them with the spend, in the same single statement that already made a replay impossible — the verifier exists exactly as long as the attempt does.
- Why hand-rolled TOTP: the algorithm is forty lines — RFC 6238 is HMAC-SHA1 over a 30-second counter with dynamic truncation — and a TOTP crate would own the two decisions that matter (the ±1 step tolerance, the constant-time compare). `sha1` joins the workspace for the same reason `hmac` already sat there; `subtle::ConstantTimeEq` is the comparison, the same crate the admin token's check uses. The shared secret is sealed under `OXSUM_SECRET_KEY` before it is stored — `channel_credentials.api_key_sealed`'s exact rule — so a dump of `oxsum.user_totp` mints no code; `confirmed_at` is what makes the gate real, so a half-done enrollment can never lock its own user out.
- Why a two-phase login and a challenge, not "session + verify-on-use": the password leg's success proves the first factor, and that proof has to live somewhere the second leg can redeem — `oxsum.totp_challenges` is the artifact: `oxtotp-` + 32 bytes, SHA-256 at rest like every minted token, single-use (deleted inside the session-mint), ten-minute-lived, five-attempt-capped. A spent, exhausted or expired challenge answers `NOT_FOUND`, so a stolen password lets a probe brute neither the session nor six digits against an unbounded oracle. Backup codes — ten `xxxx-xxxx`, SHA-256-hashed and consumed on use — are the same leg's alternate credential, not a separate recovery flow: a lost authenticator is a consumed code, not a support ticket.
- What v1 deliberately does not do: `sessions.totp_verified_at` is stamped on the session the second leg mints, but no route re-checks its freshness — "require a recent factor for this action" (disable the factor itself already requires a live code, by the verify-first rule) is the follow-on, and the column exists now so the gate needs no migration later. The `LogoutRes`-shaped disable answer stays a `POST` (`/auth/totp/disable`), not a `DELETE /auth/totp` — the bodied POST is the shape every other verify-an-action endpoint already uses.
- Rejected: *`openidconnect`/`oauth2` crates* — the flow is discovery plus two POSTs and a JWK verify; `jsonwebtoken` owns the one part worth a library (RS256 verify), and a flow crate would own the discovery cache and provider registry the config already describes. *Enrolling the factor required, not optional* — a deployment that never asked for 2FA should not force it on every signup; the gate is the user's row, not the deployment's flag. *Storing the code verifier on the cookie* — the browser is exactly what PKCE distrusts; the row is the place a single-use, server-held secret can live. *A `totp` crate* — the forty lines above.
- Pinned by `crates/server/tests/oidc.rs` (a stub IdP serving real discovery/token/JWKS legs, the `id_token` a genuine RS256 JWS over a checked-in throwaway key; the full redirect chain ends in the session cookie and an `oidc:test`-namespaced link, a replayed state redeems nothing) and `crates/server/tests/totp.rs` (enroll→confirm→two-phase login, spent and replayed challenges, a backup code once-ever, disable on a live code, an unconfirmed secret never gating, a key refused at every person-action).

## 2026-11-01 — The console speaks the browser's language through i18next; the verify page stays single-locale (issue #507)

- Status: Adopted
- Background: the dashboard's entire copy was inline English — a Chinese-speaking team reading its own bills translated in their head, and a second locale meant forking every page. The issue asks for real internationalization; the question was which library and where the boundary sits.
- Decision: `i18next` + `react-i18next` + `i18next-browser-languagedetector`, with the two locale files (`src/locales/en.json`, `zh-CN.json`) compiled into the bundle rather than fetched — a language switch is a resource-swap, not a network wait, and the deployed binary still serves one immutable `dist/`. Detection runs `localStorage["i18nextLng"]` → `navigator.language` with `en` the fallback; the header's EN/中文 switch calls `i18n.changeLanguage`, which the detector caches back to `localStorage` — so a picked language survives reloads while an unpicked browser still gets its own. Keys are flat `namespace.key` pairs grouped per page (`overview.*`, `auth.*`, `common.*` for shared verbs); interpolation covers counts and names, `<Trans>` covers strings carrying inline markup (`<code>`, `<strong>`), and `settlementOutcome`/`entryKind` take `t` as an argument so the kind labels the public bill page and the dashboard agree on stay translated from one table.
- Correction (2026-11-02, issue #538): `nonExplicitSupportedLngs: true` strips `zh-CN` to `zh` before checking support, but the supported resource is named `zh-CN`. That configuration rejected Chinese and silently rendered English even after choosing 中文. Keep full supported codes (`en`, `zh-CN`) and use i18next's existing best-match lookup, which resolves `zh`, `zh-Hans`, and regional Chinese browser tags to the shipped `zh-CN` resource. Unsupported languages continue to fall back to English.
- Why i18next over the lighter candidates: `react-i18next` is the incumbent for a reason the alternatives do not match — plural rules for Chinese's zero-plural grammar, the detector's localStorage→navigator order for free, and `<Trans>` for strings that carry markup (a `formatjs`/`lingui` build would buy compile-time extraction the codebase does not need — the copy is already written, extraction is a script's job, and the runtime cost is one map lookup per string). The locale files import statically, so a missing key fails `tsc` nowhere but reads as the key itself in dev — `returnEmptyString: false` keeps an empty translation honest by falling back to English rather than rendering a blank button.
- Why `/verify` and the admin-token gate stay out of scope: `/verify` is the server-rendered check whose entire point is answering without trusting the server's bundle — a translated verdict is still the same verdict, and the page's audience is the customer's verifier, not the dashboard user; its strings stay English beside `crates/web`'s one stylesheet. The settlement-kind *labels* translate (they are the dashboard's reading of a fact), but the kind *values* (`client_cancelled`, `upstream_error`) and every API field stay English — the contract does not localize.
- Rejected: *per-namespace lazy loading* — two locales at ~450 lines each compress to nothing beside the chart code; a fetch-per-page would trade a cold string for a spinner. *A `langs` table column or per-user preference* — the browser already knows; a server preference would answer a question nobody asked before the first paint. *Translating `openapi.yaml` descriptions or error messages* — API errors are English by contract; a localized error envelope is a different feature, not this one's.
- Pinned by `npm run build` (tsc + vite) staying clean and the two locale files carrying identical key sets — a missing zh-CN key renders English, which is the failure mode the fallback is for.
## 2026-11-01 — The request log is a post-settlement, non-blocking write; a separate table, not a usage_records column (issue #505)

- Status: Adopted
- Background: a billing dispute or an SRE question reads "every request this key made to this model in this window, with token counts and latency" — an operational query, not an accounting one. `usage_records` is the settlement's accounting fact (what the entry's content hash covers); it answers "what did this turn cost" but not "which requests were turned away" — a refused admission never settles, so it has no usage row — and it carries nothing about latency, since a settlement has no clock.
- Decision: `oxsum.request_log` is one row per gateway request — settled *or* refused — written **after** the turn settles, deliberately *not* inside `record_usage_in`'s transaction. The write rides beside the settle's success but is free to fail: `Db::record_request_log` is invoked from the spawned `Plan::write` `Ok(_)` arm and from `record_refusal`, each with its error traced to `tracing::warn!` and dropped, so a logging failure can never turn a settled request into a failed one. `request_id` keys the row to the settlement entry it joins (`entry_id` carried for the `/verify` link); `tenant_id` rides beside `organization_id` so the member's org-scoped read and the operator's all-orgs read stay index-only on `(tenant_id, …)` indexes.
- Why a separate table and not columns on `usage_records`: a refused turn has no usage row to hang the columns on — `record_refusal` writes a marker entry and a `refused` usage row, but the refusal's *operational* facts (the HTTP status the gateway answered, the refusal reason as `error_kind`, the absence of any latency) belong to the request, not to an accounting record that exists to price. Keeping the operational record its own table also keeps `usage_records` honest — it stays the settlement's fact, never a log.
- Why post-settlement and non-blocking: the row's whole value is that it exists *because* the settle landed — `charge_minor`, the token counts and the served model are only true once the settle commits. Writing it inside the settle's transaction would couple a logging failure to the charge's success: a `request_log` insert that fails would roll back a settled bill, which is precisely backwards. The `ON CONFLICT DO NOTHING` keeps a replay (the sweeper's settle, a retried write) from standing a second row — the same idempotency rule `usage_records` keeps.
- Why `ttft_ms` is nullable: time-to-first-byte is only defined when upstream streamed a first byte — a non-streamed whole-body read, and a refusal that never reached upstream, have no honest figure; `NULL` records "no first byte" rather than a guessed 0. `total_ms` is the turn's whole wall clock admission→settle.
- Rejected: *writing inside `record_usage_in`'s transaction* — a logging failure would then roll back a settled charge. *Columns on `usage_records`* — a refused turn has no row to hang them on, and latency is not an accounting fact. *Recording only settled turns* — "which requests were turned away" is half the surface's purpose; refusing to log refusals would leave the read unable to answer it. *A `request_log` schema per tenant* — the table is oxsum's own operational store, not a ledger projection; it lives in the shared `oxsum` schema beside `usage_records`, scoped reads enforced by the `organization_id`/`tenant_id` pair, not by schema.

## 2026-11-04 — The thrift advisory leaves through parquet's patch release, not a dependency override (issue #395)
- Status: Adopted
- Background: the audit workflow (issue #389) went red on RUSTSEC-2026-0435 — thrift's binary-protocol `read_bytes` allocates a length the peer names, so a malicious payload can force an unbounded allocation. The crate reached the lockfile through iceberg 0.10 → parquet 58.4.0 → thrift 0.17.0, the same optional cold-tier chain whose zstd and paste advisories `.cargo/audit.toml` already ignores — but unlike those, a patched thrift exists.
- Decision: the fix is a one-package lockfile bump, `cargo update -p parquet@58.4.0 --precise 58.4.1`, which carries arrow 58.4.0 → 58.4.1 (six crates), ordered-float 2.10.1 → 3.9.2, and swaps thrift 0.17.0 for 0.23.0. parquet 58.4.1's own manifest requires `thrift = "0.23"`; 58.4.0 required `"0.17"`. The advisory leaves the graph because the dependency's patch release moved off the vulnerable line, so no `oxsum change (not upstream)` marker and no manifest change is involved.
- Why not an explicit `thrift = "0.23"` dependency in doubleentry: version unification only happens inside one semver-compatible range. parquet 58.4.0 asks for `^0.17`; a direct `^0.23` declaration resolves *beside* it, leaving two thrift copies and the vulnerable one in the lock. An earlier draft of this change did exactly that and only stopped being wrong once a full `cargo update` incidentally lifted parquet to 58.4.1 — at which point the explicit dependency was doing no work at all. `[patch.crates-io]` was rejected for the same reason it always is here: it needs a second source for a crate whose upstream already shipped the fix.
- Why not a full lockfile refresh: a bare `cargo update` also moved wasm-bindgen 0.2.129 → 0.2.130 and sha2 0.11.0 → 0.11.1. The Dockerfile pins the wasm-bindgen CLI to the crate version (the generated module embeds a schema version that must match exactly), so 0.2.130 fails the wasm job; sha2 0.11.1 deprecates `Array::from_slice`, which `oxsum-core` uses, and CI runs clippy with `-D warnings`. The targeted `--precise` bump keeps both at the versions main pins, and a re-resolution artifact that lowered winapi-util's windows-sys edge 0.61.2 → 0.48.0 (an open `>=0.48.0, <=0.61.*` range) was reverted by hand.
- Pinned by `cargo audit` staying green and by the lockfile diff against main naming only the ten packages above.
## 2026-11-04 — jsonwebtoken stays on 9 until 11 can be built without a cost (issue #548)
- Status: Adopted
- Background: dependabot bumped jsonwebtoken 9.3.1 → 11.1.0 (`4e076d7`). Version 11 removed the crate's bundled cryptography: it ships no signer or verifier unless exactly one of `rust_crypto` or `aws_lc_rs` is enabled, and with neither every encode and decode panics at first use — "Could not automatically determine the process-level CryptoProvider from jsonwebtoken crate features". The OIDC login test caught it on the first Rust-path CI run after the bump. 10.x carries the same gate, so stepping back one major does not avoid the choice.
- Decision: pin `jsonwebtoken = "9.3.1"` in the workspace manifest, with the reason recorded beside it. Version 9 bundles ring — already in the tree through rustls — and needs no feature selection.
- Why not `rust_crypto`: it pulls rsa and crypto-bigint 0.5, which makes RUSTSEC-2023-0071 and RUSTSEC-2026-0387 reachable. Both are timing advisories whose exposure here would be limited to public-key verification, and each could be justified — but two new ignore entries to defend is a worse trade than staying on the previous major of a crate whose surface here is one `decode`.
- Why not `aws_lc_rs`: it compiles AWS-LC through cmake. The release image's `rust:bookworm` builder would gain a cmake package, and every contributor machine would need cmake installed before `cargo build` works — stock macOS does not ship it.
- What would change this: a jsonwebtoken release that vendored a default backend again, or a workspace that already needs one of the two backends (if the TLS stack moved off ring, for instance), at which point the choice is free.
- Pinned by `crates/server/tests/oidc.rs` — discovery, JWKS, RS256 verify of a real signed `id_token`, and the full redirect chain — and by the audit workflow staying green with no new entries.

## 2026-11-04 — The CI gate is tuned for wall-clock: lint beside the build, strip the archive, share the image caches (issue #550)

- Status: Adopted
- Background: a PR's required checks took ~12.3 minutes end to end, while the tests themselves took 3.5. Measured on `main` at `caaae43`: `filter` 5s → `clippy` 131s → `build-tests` 281s → `test-shard` 322s. Inside those jobs, `Free disk space` cost 86s in `build-tests`, the 1242 MB nextest archive cost 37s to upload and 113s to download on every shard, and the ~500s of "running tests" was the only part doing what the gate exists for.
- Decision: three changes, each measured rather than guessed. `build-tests` and `test` drop `clippy` from their `needs` — the three jobs share a checkout and nothing else, so a lint failure still blocks the merge through its own required check while the build proceeds in parallel. Test binaries are compiled with `CARGO_PROFILE_TEST_STRIP=symbols`, halving the archive every shard downloads; the cost is a symbol-less backtrace on a CI-only failure, and the answer is to rerun that suite locally. The disk-space step deletes its four trees concurrently, and `release-image.yml` imports the `wasm` and `frontend` BuildKit cache scopes `ci.yml` writes instead of rebuilding both stages (721s post-merge).
- Why not drop the PR gate and run CI only after merge: for a billing path the pre-merge gate is the product. "Merge, then verify" is a model for repositories whose failures are cosmetic or instantly revertible; here a bad merge is a wrong bill. The savings would be real and the risk is the one thing this project cannot buy.
- Why not a single fat test job: sharding is what keeps the suite's wall-clock at a third of one runner's, and the archive is what keeps each shard from paying the compile. Splitting `test` (lib/doc, no database) from the shards is the fast-signal half of the same trade.
- What is left for the merge queue: GitHub's merge queue runs these same checks against the merge result before landing, which is where the "post-merge CI" instinct actually belongs — and it closes issue #549, where bursts of merges cancelled each other's runs. Enabling it is a repository setting plus a `merge_group` trigger, so it ships separately; once on, the `push: main` CI run can go away and leave Audit and the Release image as the post-merge work.
- Pinned by the Actions API timings in issue #550 and by the next run of the PR that carries this change: artifact size, the shard's download step and the critical path all move or the change is reverted.

## 2026-11-04 — A pull request runs the fast tier; the heavy suites run after the merge (issue #552)

- Status: Adopted
- Background: #550 brought the gate to ~9 minutes on a warm cache, and what was left was structural, not tunable. `build-tests` (compile plus a ~3-minute archive step) and the four jobs that run integration binaries accounted for roughly eight of those nine minutes. The checks that catch a broken change — `fmt`, `clippy --all-targets`, the lib/doc tests, and the two stage builds — answer in under four. Every pull request paid the integration tier regardless of whether it could plausibly affect it, in a repository that is still in a development phase with a single maintainer.
- Decision: a pull request runs the fast tier only — `filter`, `fmt`, `clippy --all-targets`, `test` (lib + doc), `wasm`, `frontend`, `publishable`, `audit`. `build-tests`, the three `test-shard` jobs and `test-extras` gain `if: github.event_name == 'push'` and run on the push to `main` instead. Branch protection is untouched: the five required checks (`fmt`, `clippy`, `test`, `wasm`, `frontend`) are all in the fast tier, and none of the heavy jobs was ever required.
- Why this does not reopen #550's "do not run CI only after merge": the fast tier keeps the pre-merge compile gate. `clippy --workspace --all-targets` compiles every test target, so a type, borrow or API error — the class of breakage that actually blocked this repository in the last month (the `Array::from_slice` removal, the jsonwebtoken provider panic) — still fails before the merge button exists. What moves past the merge is *runtime* verification: database integration, the process-shared suites, the doubleentry conformance run.
- Why the trade is acceptable now and not later: this is a development-phase repository with one maintainer, no release train and no external contributors; a red `main` costs a fix-forward commit rather than a broken artifact. The rule is explicit — a red `main` is fixed forward or reverted immediately, never left standing. When the project enters a release phase or gains contributors, the `if:` lines come off and the full gate returns to pull requests; that reversal is part of this decision, not an afterthought.
- Why not a merge queue instead: the queue verifies the merge result before landing, which is the stronger model, but it needs a repository setting this change cannot make and it does not shorten the wait — a queue run costs the same full suite. It stays the follow-up for the cancelled-run hole in #549.
- Pinned by this PR's own run, which must show no `build-tests`/`test-shard`/`test-extras` checks and a fast tier under five minutes, and by the `main` push that follows the merge, which must show all of them running and passing.

## 2026-11-04 — A main push owns its CI run, and a nightly full run covers what a burst displaces (issue #549)

- Status: Adopted
- Background: 2026-11-02's dependency burst merged `aes-gcm` 0.11 and `jsonwebtoken` 11 into `main` with no Rust gate running. The workflow used one concurrency group per ref with `cancel-in-progress: false`; that protects a run that is already executing, but not one still queued — GitHub cancels the pending run when a newer run joins the same group. The survivor was a web-only commit whose Rust jobs short-circuited on `filter` and passed vacuously, so `main` stayed red until the next Rust pull request.
- Decision: a push to `main` joins `CI-push-<sha>` — one group per commit, so pushes never displace each other — while a pull request keeps `CI-pull_request-<ref>` with `cancel-in-progress: true` so stale PR runs still cancel. A nightly `schedule` at 03:17 UTC and a manual `workflow_dispatch` force the `filter` job's `rust` output to true and skip the paths-filter action itself (a default-branch schedule has no base to diff against), so the full tier — `build-tests`, the three shards, `test-extras` — runs whatever the last push touched. The `wasm` and `frontend` path-filtered steps are skipped on those events; both stages are required checks on every pull request that can affect them.
- Why the nightly checks the head instead of replaying the cancelled commit: the scheduled run checks out the default branch's head, which contains every commit a displaced push left behind, and the heavy tier's failures are not hidden by a later head. Replaying each ungated SHA would need a durable list of them and a per-SHA checkout; the head run is the within-a-day guarantee the issue asks for.
- Why not a merge queue here: the queue is the stronger model and remains the follow-up #550 and #552 recorded — it verifies the merge result, not the branch head. It needs a repository setting and changes how merges are performed, so it belongs to the release-phase change; the per-commit group plus the nightly closes the hole with workflow code alone.
- Pinned by the first push after this change: its run's concurrency group is `CI-push-<sha>`, and a second push queued immediately behind it does not cancel the first. A manual `workflow_dispatch` run then shows `build-tests`, `test-shard` and `test-extras` executing with no Rust-path change.

## 2026-11-04 — The verifier SDK loads the wrappers, not the instance exports; Node and the clean consumer pin it (issue #561)

- Status: Adopted
- Background: R4's clean consumer installed the packed `verify-oxsum-bill` tarball and immediately reproduced issue #555 in the SDK: `load()` called the wasm-bindgen default initializer and used its return value — the raw `WebAssembly.Instance.exports`, whose functions take generated pointers — as the callable API, so every call answered `"undefined" is not valid JSON`. The same read also showed the documented signature checks were not usable: `verifySignedHead` and `verifyWitnesses` were called with fewer arguments than the wasm exports accept, and `verifyConsistency` had no wasm export at all although the README listed it. Browser use of the package had never exercised these paths; the served pages call the wasm module directly.
- Decision: after `init`, the SDK takes the callable wrappers from the imported module namespace (the #555 fix, now applied to the package too). A `verifyConsistency` wasm export was added, mapping the API's camelCase `{oldSize, newSize, path}` onto the ledger engine's proof type; the JS wrappers now match the wasm arity and return shapes, and the public key is normalised from the base64 `/log/*` spelling or the hex bundle spelling to the hex the Rust verifier reads. Under Node 20.16+ the loader passes the packaged `.wasm` bytes to `init` through `process.getBuiltinModule("node:fs/promises")`, so the same package verifies in a CLI with no server and no bundler-visible Node import.
- Why not keep the example browser-only: the browser path already had acceptance, and a browser-only example would not have caught a package whose advertised API cannot be called. The clean Node consumer is the cheaper check and exercises the tarball exactly as a downstream project installs it. The browser loader path is unchanged apart from taking the wrappers from the namespace.
- Why not trust the new wrapper API without fixtures: the fixture is a genuine settlement captured from an isolated in-process app with a signed head and a signed price link; the example asserts the positive verdicts and four rejections (tampered content, wrong hash, wrong operator key, wrong archived head), and the check fails if any of them stops behaving. The capture test itself was temporary and is not committed.
- Pinned by `scripts/check-verify-consumer.sh`: it builds the wasm, packs the tarball, asserts `dist/`, the wasm module and both licenses are present, installs the tarball into a temporary project outside the repository, and runs the example offline. The package remains unpublished; publication is a separate maintainer step.

## 2026-11-05 — Adopt the self-hosted operating release contract (issue #591)

- Status: Adopted specification; implementation and validation remain tracked separately in `docs/roadmap/self-hosted-v1/STATUS.md`.
- Evidence: baseline `02d25373021a920b1df5c7f5bf4eea1549a081ec`; organization UUIDs determine tenant schemas, key/session resolution and proof origins. `BASELINE.md` records route authority, subscription/payment and CI source findings.
- Decision: one operator-owned business per deployment, many customer accounts, no oxsum-project fee or wholesale wallet. Existing organizations remain customer billing accounts. Add Business/profile/staff authority; never automatically promote an existing customer owner. This supersedes the portfolio-only positioning and broad key authority as product targets without deleting their historical rationale.
- Adopt D01–D10: immutable sale terms separate from costs; explicit capabilities and contexts; single-customer durable operations; transaction-aware ledger effects subject to T11 prototype/rollback tests; purpose-specific payment fulfillment; paid-period authorization; original-source corrections; separate entitlement, collections, debt and revenue; measured migration/restore/candidate acceptance.
- Compatibility: keep IDs, balances, postings, tenant origins, old proof formats and legacy period policies. Unsafe customer self-credit/raw settlement is deliberately removed, documented as a breaking authority correction. Customer registration never creates staff authority. Cache remains customer scoped.
- Rejected: seller organizations replacing customer identities, a duplicate customer wallet layer, hosted seller marketplace, oxsum-project payment collection, and interpreting existing quota rollover or deposit reversal as proof of a complete commercial lifecycle.
- Validation: A01–A31 in the tracked contract. Unit green and adopted specification do not establish completion. Baseline PR CI runs only the fast tier; perform relevant isolated runtime checks before risky merges, then T29 supplies candidate gate alignment. Publication, live deployment/data migration, paid calls and independent-pilot contact require separate authorization; never run Deploy as implementation validation.

## 2026-11-05 — Retire customer self-credit; authorize manual receipts before money (#624)

- Status: Adopted, T01.a under #592.
- Customer ownership is not payment evidence. Retire POST /topups with authenticated 403, and remove its form. Keep legitimate checkout/webhook/redemption paths.
- Deployment authority explicitly records out-of-band receipts with required reason/reference. Persist immutable intent and audit before crediting, use receipt UUID for ledger idempotency, and reject changed terms or deployment-wide duplicate payment references. Retry the identical request to recover interrupted fulfillment. Staff authority and payment-order currency conversion remain dependent tasks.
- Historical manual deposits/ledger entries are untouched. Do not mutate legacy IDs or add an unsafe compatibility switch. The new receipt table uses an oxsum migration; no engine change.
- Tests: production-router role/key denial with unchanged ledger, concurrent receipt replay, altered terms, cross-customer reference collision, missing evidence and interruption recovery. Existing funding fixtures are explicitly test-only and no longer exercise the retired endpoint as a successful customer action.

## 2026-11-05 — Raw reservation and settlement are billing authority (#625)

- Customer `POST /holds` and `/settlements` are denied before body parsing. A customer who knows a gateway hold key must not choose its charge or release it. Reading holds and trusted gateway/recovery/service-metering behavior continue unchanged.
- Removed the manual customer hold bench and mutation hooks. No deployment-token compatibility bypass is added to these customer routes.
- Tests that exercised obsolete customer mutation permission now drive the production gateway for mandates, member budgets, suspension and metrics. Existing gateway tests cover key spend/RPM/TPM/concurrent caps; historical ledger/read tests seed explicit test-only hold/settlement fixtures. Dedicated production-router denial tests assert unchanged money, ledger head and watch rows across customer roles and keys.
- This is a breaking authority correction under T01/A01/A02. Staff capability expansion remains T03, credential scope tightening T05, and operation recovery improvements T10–T13.

## 2026-11-05 — Preserve customer ledgers while adding singleton seller ownership (#593)

- Status: Adopted, T02 implementation of D01/D08.
- Existing organizations already carry customer identity, members, keys, wallet schemas and proof origins. Add a distinct singleton business and customer_profiles mapping; never move ledger data or promote an existing customer owner.
- A schema trigger maps every newly created organization in the same transaction. The fixed default_business_id function also assigns legacy/global channels and subscription plans, whose historical prices/periods stay unchanged. No second business or business-wide response cache is introduced.
- Immutable migration receipt captures pre-mapping counts. A read-only ownership-preview command runs before migrations/prepare, so operators can inspect upgrades without schema or provider side effects. Explicit owner setup is T03, not an incidental signup or migration effect.
- Tests reconstruct the real pre-ownership schema, create customer funds/settlement, genuine finalized statement and signed proof, then compare IDs, history, balance and signed evidence after rerun/concurrent migration. A 1,000-customer smoke verifies mapping and lazy schema count; it is not a throughput certification.

## 2026-11-05 — Initial business ownership needs deployment authorization and owner consent (#629)

- T03.a binds an expiring random setup token to the intended owner's email. Only deployment authority mints it; an existing user accepts using their own live personal session, or a new user registers with the invitation and their own password. No customer role implies staff authority.
- The singleton business row serializes mint/accept. Owner registration, staff membership, initialized profile, consumed token and redacted audit commit together. Token plaintext is returned only once and never stored; replay of a mint key returns metadata with null token, changed owner conflicts. Lost mint responses require a new idempotency key; initialized status prevents a second owner claim.
- Fresh owner registration reuses the ordinary registration transaction helper, preserving customer/key identity and lazy wallet creation. It makes no signup bonus or provider/payment call. Setup stores invite-only registration mode. T03.b separately enforces business policy across password/OAuth signup, selected staff/customer contexts and last-owner lifecycle protection; the T03 parent remains incomplete until those integrate.
- Setup audit entries are protected against update/delete by a database trigger. Existing non-setup audit semantics are unchanged. No setup token or password is included in audit details. No new environment secret or live migration is required for local validation.

## 2026-11-05 — Business capabilities require selected live personal authority (#630)

- Separate customer and business session contexts on the existing session credential. A role cached in a browser or a customer owner membership is never business authority. Recheck session/expiry/revocation/current staff on each operation; transaction guards serialize authorization with mutations under the singleton business lock.
- Add immutable, actor-scoped mutation receipts for exact idempotent replay and atomic security audit. Replay never reapplies historical roles/policies; current permission is mandatory. Transfer retry by a demoted former owner is refused rather than granting a special stale-authority exception.
- Owner-controlled enrollment names existing registered users, with owner assignment restricted to atomic transfer. Account deletion shares the business lock and refuses an owner until transfer. Removing customer membership does not remove staff access: personal sessions may have no selected customer and still explicitly choose a current business role. No fake customer organization or additional wallet is created to preserve login.
- Saved owner registration policy replaces environment signup authority. Public password/OAuth account creation holds the policy lock in the transaction; trusted fixture/provisioning registration remains distinct. Scratch-only fixtures set explicit policy for unrelated legacy HTTP tests.
- Existing TOTP is reused. Business access requires the session to have satisfied a confirmed factor; OAuth-only/older sessions cannot bypass it. A confirmed factor cannot be silently replaced by re-enrollment. Login rechecks the verified hash under the business lock, and TOTP consumption/session creation is atomic with respect to reset. Pending challenges die on password reset. Personal security settings remain accessible without customer membership.
- The capability matrix is deny-by-default; future customer/provider/merchant services must apply these core transaction guards. No wholesale admin-router delegation or fake operator audit identity is added. Business UI remains T24; release status remains incomplete until all tasks and external gates finish.

## 2026-11-05 — Customer identity is immutable; admission reasons are independent (#633)

- Preserve customer external ID, organization ID, business mapping, keys and ledger identity. Profile name/contact are editable metadata; archive/restore never deletes history or moves in-flight operations. The external reference is assigned at creation/migration, not renamed after use.
- Reuse the customer admission lock for archive and manual suspension, with staff authority checked in the same transaction. Independent manual, dunning and archive markers all deny new admission; settlement/payment/refund recovery remains available.
- The legacy dunning implementation wrote both a combined suspension column and its ownership marker, and admin manual operations erased that marker. Replace that coupling: migrate dunning-tagged combined timestamps out of the manual column, keep dunning's own timestamp, and remove manual clearing of dunning. Dunning rechecks overdue eligibility after the admission lock. Compatibility responses still report effective suspension.
- Customer profile writes use the existing immutable mutation/audit receipt mechanism. Read capability excludes viewer-level redacted aggregates. Invitation creation and existing-team consent are a separate T04.b PR (#634), required to close parent #595.

## 2026-11-05 — Customer and team enrollment require explicit consent (#634)

- Retire direct existing-user addition. A public core add-member method is removed; trusted demo provisioning stays crate-private. Team owners/admins mint links and the recipient registers or authenticates and accepts. Live role/session state, including confirmed second factor, is re-read inside the mutation. Member role/removal writes re-read current authority under the organization lock as well.
- Extend existing invitations instead of introducing another membership system. New customer invitations reserve a UUID, name, email and external ID; only acceptance creates the customer and initial owner. The organization insert trigger selects the creation-time external ID from this reservation. The immutable profile guard remains intact; no post-creation identity override or ledger reassignment is allowed.
- A business lock serializes external ID reservations and invitation transitions; organization locks coordinate team enrollment/removal. Existing-user acceptance is token-idempotent and returns the historical receipt without replaying membership after removal. New-user redemption retains its one-time first key response and refuses a spent token.
- Creation uses scoped idempotency receipts. Plaintext tokens are returned once and stored only as hashes; retries return the original ID and null token, never resend email. Revoke and accept race to one terminal result. Expiry/revocation releases only pending external-ID reservations. Audit failure rolls back every local effect.
- Compatibility: OpenAPI 0.104.0 requires `idempotencyKey` for team invitation creation; existing unspent links remain redeemable. Legacy direct-add clients must adopt invitations. No user receives staff, funds, or a new selected context through acceptance. Browser QA remains pending the existing Ego TaskSpace recovery gate; configured anti-bot widget completion remains T23's portal work.

## 2026-11-05 — Consumption keys have spending and own-evidence authority (#637)

- Evidence: API keys previously resolved to organization-wide key/evidence authority, raw entry/proof/request-log reads omitted member filters, sockets retained an initial organization grant, and replay/cache could disclose responses across sibling keys. These conflict with T05/A01/A06.
- Decision: explicit method/route capabilities fail closed. Personal sessions manage credentials/checkout; owner/admin sessions manage account-wide financial/report/integration resources. Shared record scope filters immutable provenance before pagination and denies account-wide document markers to restricted readers. Key mutation transactions recheck current identity, TOTP, context and membership under business/session/organization locking.
- Task poll/cancel is exact-key scoped. Replay binds credential, protocol/path and body; cached responses additionally bind the consuming key. Replays with legacy fingerprints conflict for the existing 24-hour lifetime; old cache digests miss. No migration rewrites historical records or hashes.
- Compatibility: remove `x-oxsum-balance-minor` and account balances from gateway funds errors. Own entitlement preview retains effective key/member/team constraint headroom, useful for admission sizing, without exposing wallet balance. Key IDs/secrets, ledger identities and old proof formats remain unchanged. This supersedes the earlier organization-wide key/session authority and shared-across-keys cache defaults.
- Validation: production-router denials preserve money/log/watch/audit state; same-customer key/member proof/export/socket/task/cache/replay cases, revoked-key history and current-role/session negative tests. Existing functional fixtures explicitly log in for management and keep consumption on keys. Full validation results are recorded separately in T05.a evidence.

- Auxiliary evidence review found that `/verify?sample` selected the first customer named `Acme Demo`, so an ordinary same-named account could publish its latest settled bill. The example now embeds the existing public offline-consumer fixture and explicitly labels its origin. It performs no database lookup. Tests cover offline verification and non-disclosure of a same-named customer’s real settlement.

## 2026-11-05 — Explicit business integration grants (T05.b, #638)

- Replace legacy global service issuance with owner-only business-context issuance, explicit customersRead/meteringWrite scopes, selected customer IDs and mandatory bounded expiry. Reuse the existing service actor provenance without making integrations consumption keys or personal identities. Authenticated actor fields are private and every core operation rechecks current grants.
- Store hashed oxs-int- secrets in a separate table. Creation/revocation, current session/TOTP/owner checks, immutable scoped receipts and audit commit together. Exact creation retries return null secret; changed terms conflict. Composite foreign keys bind grants to one business and its customer.
- Disable all old global service credentials, preserving their rows and historical ledger attribution. Retire deployment-token minting with 403. A blanket default scope would preserve the unsafe authority; no compatibility bypass is added. Existing legacy holds remain trusted recovery work, not reassigned to a new credential.
- The existing global open_holds and usage_records keys made caller-selected metering keys/event labels collide between customers. Derive opaque hold identity from credential/customer/idempotency, keep eventId only as a settlement label, and use scoped operation identity for projection and sweeper rows.
- Persist immutable authorization terms before money effects, including original operation/body, owned event price, discount and freeze. Bind the finalization body once. This closes sibling credential/customer and changed-body replay leaks while preserving historical records. A committed authorization may finish after revocation; the next request cannot. T10–T13 still own coordinated billing operation recovery and zero-charge admission; this authorization record is not a claim those tasks are done.
- Validation covers owner/role/context, expiry/revoke, one-time secret races, audit failure rollback, pre-0077 upgrade, selected customer pagination, sibling holds, changed replay, event label collisions and production HTTP credentials. No ledger engine change.

## 2026-11-05 — Business provider configuration uses atomic authority and receipts (T06.a, #641)

- Channels already own immutable price history, aliases and encrypted credential pools. Reuse those identities and price append/read routines. Migration 0078 pins channel business identity, maps legacy fallback chains to the default business, and adds immutable mutation results. No ledger engine or historical price rewrite.
- A business provider mutation obtains the configuration advisory lock, then business/current session/capability and channel locks. Deployment recovery mutations share the configuration lock without depending on the Business schema; this preserves old upgrade fixtures. No network or external provider call holds those transactions. Pool removal and replacement cannot race between the two authorities.
- Owner-only connection/protocol/provider/billing credential changes are separate from owner/admin catalog, alias, cache and routing operations. Provider metadata is currently owner/admin only. Finance/support/viewer retain their existing permitted customer/report views, not provider configuration access. A changed endpoint/protocol requires explicitly resupplied keys and clears its billing key rather than silently forwarding it.
- Configuration, immutable result, current actor and audit commit together. A domain-separated HMAC under the deployment sealing key binds full request terms without storing secret plaintext or an offline-guessable secret digest. Exact retries return original resource/version receipts even after subsequent edits; current authority is still required. Sealing-key rotation changes replay fingerprints, so an old request conflicts instead of repeating an effect; operators retain old keys for restore and use a new action key for an intentional post-rotation change.
- Reads filter ownership before pagination and child lookup; secrets are encrypted and only tails are displayed. Stored provider error text and unknown settings fields are excluded from business responses. Legacy endpoint userinfo/query/fragment are redacted. New request types do not implement Debug; provider runtime credential/billing/probe/Serving Debug output redacts key material and endpoint details. Strict cache fields and explicit price inputs avoid hidden config or absent-rate assumptions.
- Source review found that route resolution fetched connection and credentials through separate pool reads, permitting mixed versions during rotation. Direct and alias resolution now use one short read-only repeatable-read snapshot for aliases, prices, endpoint and pool. No network call occurs inside it. A concurrent rotation/read test checks matching endpoint and key generations; legacy in-flight snapshots remain unchanged.
- T06.b #642 is required for destination/DNS/redirect enforcement and explicit connection tests across provider callers; this authority child alone does not satisfy A07 or close T06. T07 supplies serving-context/catalog restrictions, T08 the sale-book separation, and T24 the console. No capability, certification or release readiness is inferred from this intermediate API.

## 2026-11-06 — Provider calls pin validated destinations (T06.b, #642)

- Reuse reqwest, URL parsing and existing channel settings; add a shared core destination policy and an integrations transport rather than a proxy service. Public means HTTPS plus public unicast addresses. Internal is an owner-approved exact IP list for one configured connection; private/loopback/ULA are allowed only explicitly. Link-local/metadata, unspecified, multicast, mapped/transition and reserved ranges are excluded. Full DNS answers must conform, including mixed answers.
- Resolve once per request, then configure reqwest with those exact addresses and a default resolver which refuses. Disable proxies, redirects and Referer; retain hostname/TLS verification. Bounded client reuse keys by host/address set, after each request's DNS check. Encode model/task identifiers as path data and add fixed adapter query parameters separately.
- Business connection changes are owner-only; omission retains policy only at the same endpoint/protocol. A changed connection needs fresh credentials and new internal approval; legacy connections default to public. Deployment recovery shares the same explicit policy/reset rule, and bootstrap accepts OXSUM_UPSTREAM_DESTINATION_POLICY. Migration does not guess authorization from historical URLs. This deliberately requires internal/HTTP installations to approve their service after upgrading.
- Explicit owner HEAD checks commit intent and audit before a single network call. Replay never sends again, including an interrupted pending check. Terminal status/code contains no provider content. A reachable HEAD is connectivity evidence, not model or provider certification. Save performs no probe. Existing scheduled catalog/bill sync remains separately enabled by deployment configuration and also obeys destination policy.
- Source inspection found async poll/cancel re-read current channel coordinates and could send an old job ID to a new provider. New tasks persist original endpoint/policy/sealed credential; 0079 captures only upgrade-time coordinates for older tasks, whose actual submission history was never stored. Drain old jobs before upgrade and preserve the sealing key. Later channel changes do not revoke already admitted task snapshots; revoke upstream/drain explicitly when needed. This does not replace durable billing/recovery work in T10–T13.
- A07 tests use deterministic DNS and loopback stubs for blocked ranges, mixed answers/rebinding, credential redirect refusal, owner/context/replay, task route preservation and upgrade. No real provider call or deployment is implied by local tests.

## 2026-11-06 — Customer catalogs and serving share permission snapshots (T07.a, #644)

- Source evidence: `/v1/models`, pricing and estimates read the global directory; primary/fallback checks were separately implemented; experiments overwrote the original model before authority and evidence; generic tasks omitted customer tier constraints. The existing key/group/tier and business ownership are sufficient; no second access-control table is needed.
- Add `ServingAccess`, read under repeatable-read with exact customer/key ownership, expiry/revocation, model intersections and business/group channel IDs. Missing credentials fail closed. Catalogs never select provider keys. Serving resolution filters excluded channels before opening credentials, so an unrelated channel's sealed key cannot break another customer's request.
- A public alias and its resolved target must both satisfy key/tier/mandate model grants. Channel grants still apply. Experiments validate the original requested model and channel before selecting a separately authorized variant; fallback aliases receive the same checks. Existing v13 requested/served fields retain the original ask across experiments without changing old proof formats.
- Direct pricing/estimate/checkpoint reads use the same access. Historical exported proofs remain verifiable after later policy changes. Authority snapshots govern newly admitted work; task status/cancel remain bound to the original customer/key and preserve safe completion after model/group changes.
- Current generic tasks cannot reliably settle an alias whose price came from its target: the preexisting task record stores only the public name, while settlement reads its price directly. It also has no signed-mandate finalization binding. Refuse these unsupported inputs before send rather than accepting a task that cannot be settled correctly. T13 remains responsible for complete task operation/price/evidence binding; this is an explicit intermediate support limit, not completed full-release task acceptance. Direct generic tasks apply customer tier/key/group and request-rate restrictions.
- T07.b #645 must replace normalized text cache digests with complete versioned request/permission/route context. This change alone does not close A08 or the release.

## 2026-11-06 — Response-cache keys bind the complete admitted context (T07.b, #645)

- Source review found the cache digest covered only normalized prompt text behind a credential wrapper. System instructions, tools, media, generation parameters, protocol headers, permission changes and route configuration did not participate; `excludeSystem` could fold two different questions onto one row.
- Replace it with `ResponseCacheContext`: the exact caller and forwarded bodies, the concrete upstream endpoint and protocol-owned semantic headers (`anthropic-version`, `anthropic-beta`), the `ServingAccess` permission snapshot, key admission constraints, the experiment binding, every admitted route's price version/destination/settings/credential configuration/weight/price, and fallback aliases and targets, and the discount. Canonicalization sorts object keys only; every array element and string byte is preserved.
- The stored digest is `v2:` plus HMAC-SHA256 under the deployment sealing key (`oxsum/response-cache/v2`). Prompts are never stored; a deployment without a sealing key cannot cache. A sealing-key rotation intentionally invalidates old rows.
- Migration 0080 deletes pre-v2 rows rather than migrating private answers across an incomplete context: cache rows are rebuildable projections and never authority. Lookup remains organization-scoped and now binds the exact consumption key through the access snapshot, so sibling keys still cannot share an answer.
- `excludeSystem` is accepted and ignored. It was an unsafe widening: the preamble is part of the question. The OpenAPI contract moves to 0.110.0; settlement evidence, ledger identities and historical proofs are unchanged.
- The store remains limited to a whole, non-streamed answer served by the primary route with provider-reported usage. A hit still settles at the cache-read rate and records no new inference cost.

- Review corrections: bind `chat/completions` versus `responses` explicitly even when both accept identical JSON; bind all fallback targets/configurations, not just their public names. Read a keyed credential-configuration fingerprint alongside each route (all IDs, sealed credentials, positions and statuses), excluding LRU/cooldown timestamps. This catches credential add/remove/disable without relying on channel timestamps. With no available primary credential, skip the response cache. Register 0080 in the explicit migration list and verify actual upgrade execution.

## 2026-11-06 — Customer sale books preserve revisions and effective history (T08, #599)

- The accepted commercial order is customer override → assigned book → default book. A reusable sale book is the price group; existing request groups remain channel authority. Model absence falls through; explicit zero terms are selected. Each customer has one active assignment, and its selected term supplies exactly one nonstacking discount.
- Reuse `Price`'s validated conditional rules and itemized integer arithmetic behind `SalePrice`, which refuses supplier costs including rule-level costs. Published versions reference exact immutable draft revisions; `expectedRevision` prevents lost draft edits. Provider route selection and acquisition cost cannot silently become customer sale authority. An explicit legacy adapter retains channel/model/version and removes supplier cost fields without rewriting the original row.
- Immutable schedules use UTC microsecond half-open windows. New starts cannot be backdated. An append-only end event shortens a schedule or cancels a future schedule at its start; atomic replacement writes that end and the new schedule together. End events cannot be rewritten or extend the original window. The business lock serializes overlap checks, capability changes, audit and replay receipts. No generic rules engine or second ledger is introduced.
- Staff preview and internal resolution produce a self-contained customer/business/model/source/version/term snapshot. Strict normalized usage rejects new or unsupported dimensions. Exactly one discount scales the complete numerator before one ceiling. A09 inspection found unchecked cache-subset addition and bill accumulator/discount multiplication in the reused rater; checked arithmetic now refuses overflow instead of panicking or wrapping.
- The API is an intermediate T08 foundation, not a gateway billing switch. T09 binds signed sale evidence; T10–T13 bind admission, operation and settlement. Legacy proof bytes, channel versions and current serving behavior remain intact. T24 adds console flows; no UI completion or release readiness is claimed here.

## 2026-11-06 — Immutable business merchant versions (#648, T14.a)

Merchant ownership and provider account identity live separately from immutable credential versions. Identity is `(business, rail, test/live mode, account ID)`; names are business-unique and immutable. A deferred composite foreign key requires the selected version to belong to its merchant. Full replacement rotations preserve old sealed credentials for in-flight callbacks and reconciliation; disablement affects future checkout selection rather than deleting verification context.

Use the existing AES-GCM `SecretKey` for the complete credential envelope, with a keyed mutation fingerprint for exact retry comparison. Owner authority, immutable mutation result and redacted audit share one transaction under the business lock. Redacted read capability is `ReportsRead`; `MerchantSecretsWrite` remains owner-only. A configuration save validates rail fields, known mode prefixes and the shared destination-policy syntax but makes no provider request; `locallyValidated` must not be represented as external certification. Hyperswitch with Stripe remains the primary contract; direct Stripe/Airwallex remain compatibility rails. Scoped callback intake/bootstrap is a required separate child (#649); purpose-specific payment fulfillment remains T15.

## 2026-11-06 — Customer sale terms sign as their own evidence (T09, #600)

- A sale-priced bill binds *customer* terms, never the wholesale channel catalog: the supplier-side `price` checkpoint attests rates the customer never agreed to and must never see. The resolved `SalePriceSnapshot` hashes into a canonical `SaleSnapshotV1` row under the `oxsum/sale-catalog/v1` domain — `{v, businessId, organizationId, model, source, scheduleId, bookId, version, draftRevision, legacyChannel, effectiveFrom, effectiveUntil, terms}` — binding the customer, the book/draft/legacy provenance, the effective window, and the `SalePrice` (which cannot carry `upstream` costs) plus `discountPercent`. The business keys the origin `oxsum/sale-catalog/<businessId>`; the resolution instant `at` stays out of the hash as display context, the same split the snapshot's own fields make between proof-sensitive identity and read-time detail.
- The digest is content-addressed, so `oxsum.sale_snapshot_digests` (migration 0082) stores the canonical row once, keyed by it, idempotent on re-resolution — there is no snapshot table to drift and no second rendering a verifier could disagree with. `receipt_proof` reads the `sale.digest` the v14 settlement commits, serves the stored row verbatim, and signs it as a checkpoint under the same `OXSUM_HEAD_SIGNING_KEY`, `size` = the book version, `root` = the digest. `GET /api/v1/log/sale-head` re-signs the same note on demand by `digest`, or re-resolves the live terms by `(organizationId, model, at)`; a customer reads only its own terms.
- Settlement description schema v14 adds the optional `sale` block `{digest}`; the gateway writes it only once coordinated settlement resolves sale terms (T12), so today's wholesale-priced turns omit it and keep the `price` binding. `oxsum_verify::verify_sale_head` reports `NotSalePriced` for a record committing no digest — a wholesale checkpoint must never be read as proving customer sale terms — and `verify_bill` adds a `sale` sentence only then. Older bundles and versions verify unchanged; the verify crate stays wasm32-clean and the SDK gains `verifySaleHead` beside `verifyPriceHead`.
- A10 coverage: old and new positive fixtures plus altered price, discount, rate, customer, digest and signer failures in `oxsum_verify`'s `sale_heads` module, an end-to-end digest→record→sign→bundle→verify run in `crates/core/tests/sale_heads.rs`, and the independent pure-verifier build (wasm32 target) confirmed. T10–T13 still owe the gateway wiring that writes `sale` into live settlements; this PR ships the evidence machinery and the contract, not the serving switch.

## 2026-11-06 — The operation is the durable parent of every billable turn (T10, #601)

- A second table beside `oxsum.open_holds` because the two answer different questions: the watch row is the sweeper's working set — deleted the moment the hold resolves — while `oxsum.request_operations` is the turn's identity and recovery record, kept after settlement so a member (or the operator) can ask *what happened to this turn* without joining the ledger. Keeping the watch row's lifetime would lose the record; keeping the record's lifetime on the watch row would grow the sweeper's scan with every settled turn.
- Lease plus CAS rather than a lock column, because a crashed worker cannot release a lock it held: `leased_until` expires on its own, `claim_operation`'s compare-and-set on the lapsed lease lets exactly one claimant (a retrying worker or the sweeper) win, and the heartbeat's owner-matched update means a dead worker's lease stops extending instead of fencing out recovery forever. Fencing is the deadline arithmetic, not an `IS NULL` check — `settle_operation` refuses to move a terminal row, so a late settle racing a sweep lands `AlreadySettled`, never a second charge.
- `request_attempts` is separate because a turn can try several upstreams and only one settles: one row per try keeps the failover audit (which channel was asked, how it refused) beside the winning `answered` row that carries the billed `usage_json`, where a single mutable "attempts" blob on the parent would rewrite itself on every retry and lose the history the row exists to keep.
- The operation binds the hold it spends under (`hold_key`, `req-<id>:hold`), so the sweeper's second pass — `operations_past_lease` → `claim_operation` → the same `sweep_one` → `timed_out` at 0 — recovers a stranded turn through the identical ledger path the `open_holds` pass uses; rows written before the table existed keep sweeping on the old `opened_at` criterion, which is the backward-compat half.
- Replay dedup rides the unique `(organization_id, fingerprint)`: `insert_operation` is `ON CONFLICT DO NOTHING` plus a re-read, so a retried request inside its idempotency window re-reads the same operation rather than minting a second. Every operation write is best-effort — a lost row traces a warning and never fails the request, the same doctrine `request_log` already keeps.

## 2026-11-06 — A borrowed-transaction append for the Postgres backend (T11, #602)

- The settlement's durability hole T10 left visible: `wallet.settle` committed the ledger entry in the engine's own transaction, then `settle_operation`, `settle_subscription_draw`, `record_usage` and `clear_open_hold` each committed on their own — a crash between any two left money moved without the operation that explains it, or an operation closed over a charge the ledger never took. D05 asks for one customer's ledger plus the oxsum operation/entitlement/outbox rows to commit as a unit.
- The engine change is deliberately narrow: `PostgresStore::append_in_tx(&self, tx, batch)` runs the identical append — per-ledger advisory lock, dense inline index, idempotency gate, limits, MMR node writes — inside a transaction the caller opened and owns. `LedgerStore::append` is now a thin wrapper (`begin` → shared `append_batch_in` → `commit`), so every existing caller, its concurrency tests and both conformance suites are untouched. `sqlite`/`memory` get no borrowed-tx API: there is no `Transaction` type to hand them, and the seam is Postgres-specific by design.
- The one non-obvious constraint the pin discovered: the vendored schema's deferred `postings_balance_per_currency_and_layer` trigger resolves `postings` unqualified *at COMMIT*, so `SET LOCAL search_path` inside `append_in_tx` must stay set for the rest of the caller's transaction — restoring it after the append would leave the trigger reading the wrong schema. `SET LOCAL` never escapes the transaction, so the pooled connection carries nothing into the next borrower; callers' own writes are schema-qualified (`oxsum.*`) and unaffected.
- Lock order stays caller-owned (D05): the append takes the per-ledger lock inside the caller's transaction after whatever admission/key/subscription locks the caller already holds — `pg_advisory_xact_lock` is re-entrant per session, so a shared lock owner never self-deadlocks. A fault anywhere in the transaction rolls back entry, nodes, draw consumption, usage row and operation close as one — the A12 test forces exactly that and asserts nothing survives, including that the rolled-back idempotency claim frees the key for an honest retry.
- The alternative rejected: a durable saga (settle ledger first, close operation in a second write, reconcile drift by inspection) is what T10 already had — it is the failure window this change exists to close, not a fallback. The engine could host the borrowed transaction safely, so no saga was needed.

## 2026-11-06 — The zero-cost usage receipt replaces the fabricated 1-minor hold (T11, #602)

- The subscription-first admission order (issue #496) left a dishonest edge: a turn the entitlement covered whole still took a 1-minor wallet hold, because a settlement had to land on *some* hold and the ledger refuses zero-postings. That hold demanded purchased balance the request never owed — a zero-balance organization on a full plan was refused a call its entitlement should have served (A13).
- Admission now takes no wallet hold at all when `freeze − drawn == 0`: the durable `request_operations` row plus the `subscription_draws` row are the reservation's authority, and `Plan::write` settles through `usage_receipt_in` instead of `settle_in`. The receipt is a typed marker — a 1-minor balanced `Equity:Adjustments` debit/credit in the *settled* layer carrying the ordinary `Settlement` description (`charged: 0`, the `subscription` block naming what paid) — the same net-zero idiom the statement and usage-day anchors use, keyed `settlement_key_for(hold_key)` so a replayed receipt and a raced settle dedup on the same gate a wallet settle does.
- No relax of the engine's zero-posting validation: the marker carries a real (balanced) amount, exactly like the existing anchors. Balances, pending reservations and revenue are provably untouched — `settled == reserved == available == 0` after a receipt on an empty wallet — while the turn stays recorded, provable through `settled_record` and bill-verifiable like any charged one.
- The sweeper's recovery of a never-held turn needed no new path: `sweep_one`'s `HoldNotFound` arm already settles the draw at 0 and clears the watch row, and the operation's `timed_out` CAS runs as before. Settlement classification is unchanged — the receipt's description is a normal `Settlement` record, so `recent_requests`, the bills page and `verify_charge` read it as the `usage`-kind turn it was; the only difference is the postings behind it move nothing.
- What T12 inherits: `settle_in`, `usage_receipt_in`, `settle_operation_in`, `settle_subscription_draw_in`, `record_usage_in`, `clear_open_hold_in`, `complete_attempt_in`, `available_in` and the engine's `append_in_tx` — the coordinated-admission seam where customer/key/member/team constraints join the same transaction per the D05 lock order.

## 2026-11-06 — Scoped merchant event intake pins expected payments before provider calls (#649, T14.b)

T14.a established immutable merchant identity and signing versions; T14.b supplies the durable verified intake that consumes them. The binding is committed *before* any external request: `merchant_payment_bindings` pins local payment UUID, business, customer, merchant/version, exact amount/currency and an `order`/`legacyDeposit` purpose. An early signed callback and the later synchronous checkout response therefore resolve the same expectation, and `merchant_payment_refs` (`UNIQUE (merchant_id, provider_payment_id)`) guarantees one provider payment funds at most one local payment.

Signature verification always precedes normalization, and the trusted path coordinates select the merchant and signing version before the body is read — a payload can never choose its own secret. The versioned endpoint `/api/v1/webhooks/merchants/{merchantId}/versions/{versionId}` is the primary contract; the legacy `/api/v1/webhooks/{rail}` route resolves its one environment-bootstrapped merchant and may verify retained versions only inside that trusted namespace. `merchant_event_inbox` dedupes on `(merchant_id, event_id)` across every version under an advisory lock: a replay returns the stored receipt, and a reused id with different signed bytes is a `409`, never a silent overwrite. The row stores the SHA-256 payload digest and normalized coordinates — never the raw payload or a credential — and a trigger forbids identity rewrites and terminal-state regressions.

Receipt is deliberately not fulfillment. A recognized event must match the pinned binding on merchant, version, customer, provider account, mode, amount and currency before `accepted`; a mismatch `rejects`, an unknown local payment stays `pending` for recovery, and refund/dispute kinds are authenticated and retained without establishing a reference or crediting — their expected terms are T17's. A reported amount that disagrees with the pinned expectation is rejected before it can reach the ledger (narrowing D2's "record the drift" rule to refuse it outright at intake). An `accepted` `legacyDeposit` success is the only event that credits, and only through the idempotent legacy adapter under a merchant-scoped ledger key (`<rail>:merchant:<merchant_id>:payment:<provider_id>`), so a verified event lands once even across replays; an authoritative success may also revive an earlier `expired` deposit, since paid evidence outranks a premature expiration notice. An environment rail without an explicit `OXSUM_<RAIL>_MERCHANT_*` bootstrap keeps its unscoped legacy behavior but cannot open merchant-bound payments. T15 owns orders, paid/fulfilled/outbox and purpose-specific fulfillment.

## 2026-11-06 — Coordinated admission, settlement and day-close (T12, #603)

- Admission now resolves customer sale terms before the freeze: `Db::resolve_sale_price` runs at the admission instant for every gateway turn — `NotFound` is the wholesale path, never an error — and a resolved `SalePriceSnapshot` *replaces* the wholesale catalog rather than layering onto it. The snapshot's own `SalePrice` sizes the freeze, itemizes the charge and supplies its own `discountPercent` (the org-level `pricing_discounts` figure does not stack — the snapshot's terms are the customer's whole deal), and its content-derived digest is what the v14 settlement's `sale` block commits. The wholesale `plan.price` still rides the turn for `upstream_cost_minor`: a sale never changes what the provider charges *us*.
- Under pinned sale terms a turn may not fall back across models — the snapshot binds the served name's rates, and another model's price is a different deal (D04). Admission narrows the model-route chain to the requested model; same-model channel failover still works and the reroute still swaps the wholesale side so platform cost tracks the channel that answered.
- A fully entitlement-funded turn takes no wallet hold (D05): when `freeze − drawnMinor` is zero the subscription draw and the durable operation row are the whole reservation, and the settle writes `Wallet::usage_receipt` — a typed zero-cost receipt, a balanced 1-minor `Equity:Adjustments` debit/credit idempotency-keyed by `settlement_key_for(hold_key)`, the same figure a wallet settle derives. The fabricated 1-minor hold the old path took could refuse admission on an empty wallet even though the plan already paid — A13's case — and a receipt under the shared derived key means a replay and a raced settle still conflict on one entry. `DaySealed` applies to receipts too.
- The day-close seals a map only once the operation watermark allows it: `anchor_usage_days` defers an organization that still owns open `request_operations` created at or before the day's end — an operation can still settle (or be swept) into that day's leaves, so committing the anchor early would seal a map the settle path can still contradict. Deferral is a count, not a lock: the next pass re-reads, and a lapsed lease the sweeper reclaimed stops blocking on its own.
- The response-cache context gains `sale_digest`: a stored answer recomputes under the deal it was priced at, so two customers' terms never share a cached body. The cache-hit path prices its reservation under the resolved sale terms the same way the fresh-turn path does.
- A11–A14 coverage: racing member keys under a member budget assert `member_committed` never crosses the integer cap; a mid-stream cancellation settles `client_cancelled` and releases the reservation with the operation `cancelled`; an over-bound report caps at the freeze; a lapsed operation sweeps `timed_out` at zero without retransmitting; an open operation defers the day anchor and the terminal write unblocks it; a fully-covered turn on an empty wallet settles the receipt (`charged: 0` wallet-side, `subscription.chargedMinor` naming what paid); a sale-priced turn commits the resolved snapshot's digest.
- T11 (#602) is in flight but unmerged: `usage_receipt` is the single-append variant, so this PR stands alone. When the transaction-aware ledger lands, the rebase folds the draw settle, the receipt and the operation's terminal CAS into `settle_operation_in`/`append_in_tx`'s one transaction — the seam is already `Plan::write`'s three sequential writes.

## 2026-11-06 — Cost basis on usage rows, metering operations and the `cache_hit` outcome (T13, #604)

- `usage_records` gains `cost_basis` (migration 0089): `upstream_cost_minor` already separates "untracked" (NULL) from "priced at nothing" (0) but cannot say *why* — so a cost-allocation read had to re-derive provenance from `kind` and the NULL, and a swept turn was indistinguishable from an unpriced one. The column names it: `real_attempt` (a reported-usage attempt's `upstream` block priced it — including a failed-but-billed upstream charge, never fabricated to zero), `cache_reuse` (a semantic-cache hit made no upstream call), `estimate` (a local estimate priced the degraded or cancelled turn), `missing` (NULL cost — no `upstream` block, a sweep with no price, or a pre-column row), and `provider_reconciled` (reserved spelling for a later provider-bill backfill). NULL stays on pre-column rows — one figure for history, not two columns disagreeing.
- Every serving surface's `Plan::write` computes the basis at settle: `Charge::Cached` → `cache_reuse`, `Charge::Estimated` → `estimate`, a present `upstream_cost_minor` → `real_attempt`, absent → `missing`. Refusals and sweeps record `missing`. The cache hit's operation closes under `outcome_kind = 'cache_hit'` (vs `served`) so a reuse is never confused with a fresh attempt — an identical replay shares the first operation's fingerprint, so the distinction lands on the hit's own operation only when the request is byte-different (the cache digest reads the *parsed* body, the fingerprint the *raw* bytes; the A08 test respells the same ask to see both).
- Privileged event metering now produces a `request_operations` row like every billable turn — `surface = 'metering'`, `key_id` NULL, fingerprinted by the metering hold key — closed `metered` on settle so a staff-triggered internal event is never confused with customer-billable usage. `operations_past_lease` excludes `surface = 'metering'`: a metering operation has no heartbeating worker (the 60 s lease would reclaim a legitimately open hold mid-flight), so the `open_holds` sweep's 30-minute horizon owns its lifecycle instead — `sweep_metered_operation` settles it `timed_out` inside the same `sweep_one` clear arms.
- A metering `release` stopped fabricating `upstream_cost_minor = Some(0)`: it now writes `None` + `missing`, because "the platform paid nothing" and "the platform's cost is untracked" are different facts — the event simply never ran.
- A08 coverage (`crates/server/tests/serving.rs`): every surface (chat, responses, messages, gemini, embeddings, rerank) produces an operation that settles `succeeded` with one `answered` attempt carrying verbatim usage; a cache hit records `cache_reuse` with `upstream_attempts = 0`; a pre-byte refusal retries under one operation (a `refused` then `answered` attempt, one settle); a post-byte disconnect goes `cancelled` yet still prices the consumed estimate; a usage report upstream never sent settles `estimated` with `estimate` basis; a metered event lands `metering`/`metered`/`succeeded` with `real_attempt` basis. T22 (reconciliation) reads `cost_basis` to separate allocated cost from estimates; T25 inherits the metering-surface marking for staff-vs-customer cost attribution.

## 2026-11-06 — Payment orders separate paid from fulfilled, scoped by purpose (#606, T15)

- `payment_orders` pins immutable terms before any provider call — the ledger `amount_minor`/`currency`, the display amount the customer was quoted, the server-rated `rate_mantissa`/`rate_scale` conversion, the one `purpose` arm and its `purpose_ref` — and reuses T14.b's `merchant_payment_bindings` (purpose `order`) under the order's own UUID as the expected payment plus `merchant_payment_refs` as the provider-idempotency dedup, so one provider payment can never fund two orders and a retried create never issues a second charge. `payment_fulfillments` records exactly one grant per order beside its `payment_outbox` dispatch row.
- The conversion is server-rated only: the route resolves the organization's effective `Display` rate and derives `amount_minor` from `display_amount_minor` by integer inverse — `amountMinor` and `rate` are deliberately absent from the create request, so a client-supplied figure is a schema rejection, never a negotiated term. `Display::to_credit_minor` is the inverse of `render`, integer throughout, and `display.rs` keeps the doc note that its rounding never feeds a charge.
- Paid is established only by the signed merchant callback through `merchant_event_inbox`; the return page and every read surface can never mark an order paid. `apply_order_event` marks `paid` (or terminal `failed`/`expired`) and parks an amount/currency mismatch — re-checked against the order's own columns, not the binding — in `needs_attention`, an explicit discrepancy state a human resolves, never an auto-credit. T14.b's `classify` still rejects the mismatch at intake; the order re-check is the suspenders.
- Fulfillment is exhaustive on purpose and one-sided by construction: `wallet_topup` books wallet cash (`Wallet::top_up`), `bundle` books purchased credit plus a separate promo lot into the bonus pool under one fulfillment, `plan_period` activates quota through `bind_subscription` and never touches the wallet, and `invoice_repayment` repays a drawn receivable through a new `Wallet::repay_credit` arm — `debit Cash / credit CreditLine` — so no purpose may both grant quota and wallet cash, and a repayment beyond the outstanding draw is refused rather than booking excess into a pool the purpose forbids.
- The grant is idempotent under the order-derived ledger key `payment-order:{order_id}` (uuid v5 like every ledger idempotency key): a crash after the grant commits but before the fulfillment row lands leaves a `paid` order the recovery sweep's `unfulfilled_payment_orders` scan re-runs, and the retry replays the same grant rather than granting twice. `fulfill_payment_order` serializes on an order-keyed advisory lock, so concurrent duplicate webhooks and raced sweeps credit once. `kinds::RECONCILE`'s `merchant_payments::recover` gained `apply_order_event` per retained event plus the `recover_orders` pass — the event and the fulfillment retry independently.
- The checkout handle persists on the order (`checkout` jsonb, deliberately outside the immutable-terms trigger): a replayed create returns the standing order plus the exact `clientSecret`/`checkoutUrl` the first attempt minted — the provider is never asked twice for the same idempotency key.
- Customer endpoints (`POST/GET /api/v1/orders`, `GET /api/v1/orders/{id}`, `GET …/fulfillment`) sit behind `require_principal` at `Personal` capability — a session, like every checkout path, never a consumption key. `GET /api/v1/business/payment-reconciliation` reads the paid/fulfilled/needs_attention counts and the paid-unfulfilled backlog under `personal_token` + `ReportsRead`. A16/A17 coverage: HTTP tests drive create→checkout→signed-webhook→sweep end-to-end, replayed create/checkout, wrong-amount rejection, provider-id reuse refusal, and the report; the core suite covers server-rated conversion, per-purpose grants, mismatch parking, recovery single-credit and raced webhooks.

## 2026-11-06 — Promotional lots: a fourth pool that can expire (T18, #609)

- Promo is a ledger pool, not a table-side discount: the money must freeze inside the same `FundedReservations` hold as every other balance, so `Equity:Promo` joins the pools and draws **first** at admission — before `Equity:Bonus`, the wallet and the credit line — because it is the balance that can be lost to expiry. Most-restricted money first is the whole point of the pool ordering; promotional credit is the most restricted kind there is.
- `oxsum.promo_lots` is the projection that says which grant still funds how much of the pool — `source` (`admin`/`grant`/`bundle_promo`/`legacy_migration`) and `source_ref` as the idempotency anchor, `remaining_minor` the unreserved fundable part, `expires_at` NULL for never. A hold's promo share pins its lots at admission in `oxsum.promo_draws`, soonest-expiring first: the settlement debits the rows admission named rather than re-scanning, so the charge a lot took is a fact of the draw row, and a release returns to exactly the lot it was pinned on.
- The pin admits an open lot even past its `expires_at`: the ledger pool counts its remainder until the expiry pass burns it, so pin and pool availability must agree — and a due lot is the most restricted money of all. `FOR UPDATE` serializes the pin against the pass's close; a settlement releasing into a closed lot burns the released share `debit Promo / credit PromoExpiry` in the same transaction, so an expiring mid-stream lot never double-charges a turn and never refunds to cash (A20).
- Expired promo burns rather than returns: the `expire-lots` job (leader-elected, minute cadence) closes due lots and posts `debit Promo / credit PromoExpiry` for the unreserved remainder under a key derived from the lot id, so a replayed pass posts once. A cancellation — a refunded bundle's promo leg — burns the unspent remainder the same way. Promo is a grant, not a liability: no path converts it back to cash, which is why it must not live in the refundable `Equity:Bonus` pool.
- The aggregate bonus balance migrated once, lazily at `Wallet::open`, into a never-expiring `legacy_migration` lot — `debit Bonus / credit Promo` under the fixed key `legacy-promo-migration`, moving `pool_available_in(bonus)` so net balances and reserved holds cannot shift. Both migration markers (`reclassify-grants`, `legacy-promo-migration`) are excluded from spend reads as rebalancings, keeping the totals' reconciliation exact.
- Bundle fulfillment is two legs under one fulfillment row: purchased credit through `Wallet::top_up` plus the promo lot as `source='bundle_promo'` anchored on the fulfillment id — an order's `promoMinor` is the T15 arm this completes, and a refund cancels the lot while the purchased side unwinds through the deposit reversal.
- The alternative rejected: lots as *metadata over the bonus pool* (expire the lots' share at settle time). That leaves the pool unable to say how much of it is perishable — a hold would reserve bonus it could still lose, and expiry would need a second debit the `FundedReservations` check has no authorization to post. Promo's own pool makes perishability a first-class ledger fact.

## 2026-11-06 — Sold plans version the catalogue; periods run only on authorization (T19, #610)

- Three tables because the sold layer and the quota engine answer different questions: `oxsum.sold_plans` is the versioned catalogue — one immutable row per `(plan_id, version)` of price, period quota, boundary policy and overage opt-in, so a customer binding references the exact terms it was sold and a catalogue edit mints the next version rather than rewriting a sold form (published rows are trigger-immutable). `oxsum.customer_plans` is the D06 machine — `pending_payment → active → renewal_pending/past_due → active|expired`, `cancelled` the exit, `cancel_at_period_end` and `scheduled_change` explicit columns — one live row per organization under a partial unique index. `oxsum.plan_periods` is the authorization ledger: a period exists only because its authorization does (`payment_order` + order id, or `complimentary` + grant key), unique on `(customer_plan_id, period_index)` and on `(authorization_kind, authorization_ref)`, so a replayed fulfillment can never double-issue a period.
- A timer can never issue unpaid entitlement (D06): the legacy free-rolling rollover is *bypassed* for plan-bound subscriptions — `admit_subscription_draw`'s lazy path and the `subscription-rollovers` job both delegate to `plan_rollover_in` under the org's advisory lock, which either finds the next `plan_periods` row already written by a fulfilled order (`activate_plan_period`), or slips the plan to `past_due` with the subscription held in its ended window. Past-due is honest service, not silence: the ended period's unspent remainder still draws, and `overage_enabled` decides whether the overflow falls to the wallet — never a fresh quota.
- The quota machinery is reused, not forked: `open_subscription_in` mirrors each sold version into a `subscription_plans` row (`reset_period = 'never'` so the free-rolling cadence cannot also fire) and binds `org_subscriptions` under `source = 'plan'` — `admit_subscription_draw`, `settle_subscription_draw_in`, the `subscription_periods` audit and the v9 settlement's `subscription` block all see a plan-bound entitlement through the shapes they already know. `calendar_month` maps onto the legacy boundary semantics exactly (first of next UTC month); `anniversary` is the versioned alternative that rolls on the activation date's day-of-month, clamped into months too short to hold it — Jan 31 → Feb 28 → Mar 31, the anchor stuck to the activation rather than permanently drifted by a clamp.
- A `plan_period` order's `purpose_ref` now names the `customer_plans` row it funds (was `subscription_plans.plan_id` under T15's provisional arm): the fulfillment calls `activate_plan_period`, which opens period 0 under a `pending_payment` bind or the next index on a `renewal_pending`/`past_due` one — the period's start chaining off the previous period's end, never `now`, so a late renewal keeps the gap's audit. A free plan (`price_minor = 0`) binds and activates on an explicit `complimentary` grant — a priced plan refuses a grant and a free plan refuses a payment order; nothing fabricates a paid record for included quota.
- Boundary modifiers are columns, not inferred state: `cancel_at_period_end` serves the paid period to its end then closes the plan `cancelled` (and the engine row with it) rather than renewing; `scheduled_change` swaps `plan_version_id` at the boundary and waits `renewal_pending` for the new version's own first authorization — no mid-period pro-rating anywhere.
- API: `GET /api/v1/plans{,/{id}}` reads the on-sale catalogue at `ScopedRead` capability; `GET /api/v1/org/plan` plus `POST …/cancel` and `…/change` are `Personal`; staff catalogue CRUD and customer binds sit under `personal_token` + `catalogWrite`/`customersRead`/`customersWrite`. A21 coverage: purchase/renewal-failure (timer never issues unpaid period), boundary cancel and scheduled change, racing binds, the anniversary leap-month clamp, a covered draw on a zero wallet, a stale draw settling against a plan-bound subscription, and past-due remainder draws — core and HTTP suites both.
- T18 (#609) is in flight unmerged: this branch does not stack on it — the `bundle` order arm T15 left already posts promo through `Wallet::adjust`, and T19 touches none of the promo-lot machinery; when T18 lands, `grant_order`'s `bundle` arm swaps to lots while `plan_period` is untouched.

## 2026-11-06 — The customer checkout surface: session recovery, history filters and staff manual receipts (#607, T16)

- `GET /api/v1/orders/{orderId}/checkout-session` is the recovery read a returned or refreshed browser polls: the stored `clientSecret`/`checkoutUrl` plus the pinned version's `publishableKey`/`sdkUrl` and the order's own status — `paid`/`fulfilled`/`failed`/`expired` truth comes off the order, never inferred from the return URL. A still-`created` order (a create whose provider call failed) re-issues the PSP call inside this read under the same order-UUID idempotency key, so the retry can never charge twice; `create` and the session share one `drive_checkout` helper so both answer the same stored handles.
- Order history is newest-first now: the customer list orders by `(created_at, order_id) DESC` with the page cursor resolving back through the order id, and `purpose`/`status` narrow it — a bogus status is a `400`, not an empty page that reads like "no orders". `GET /api/v1/business/orders` is the same page across customers under `personal_token` + `ReportsRead`, plus an `organizationId` filter.
- A manual receipt is a payment order, not a parallel record: `POST /api/v1/business/orders` (staff session + `FundsWrite`) writes a `source: manual` `walletTopup` order already `paid` on the receipt's own evidence, then calls the identical `fulfill_payment_order` the sweep runs — wallet cash under the same `payment-order:{order_id}` ledger key, once ever, in the customer's own order history. The fiat amount rates server-side at the customer's display rate like a provider order, so a staff-supplied credit figure never enters.
- Schema 0091 makes that possible without a fabricated merchant: `payment_orders.merchant_id`/`merchant_version_id` go nullable (the composite merchant FK is skipped when both are NULL, like every SQL FK), `source`/`evidence` join the immutable terms the protection trigger freezes, and the `order_id → merchant_payment_bindings` FK is dropped — a manual order has no expected provider payment, so nothing to bind. Provider orders still write the binding in the same transaction as the terms; the binding keeps anchoring webhook classification, and a manual order's `purpose='manual'` absence means `apply_order_event` can never reach it.
- Unique evidence is enforced, not trusted: `payment_orders_manual_reference` is a partial unique index on `(business_id, evidence->>'reference')` over manual orders — one bank reference funds exactly one manual order, a reuse is a `409` mapped at insert, and the `(organization, idempotencyKey)` replay returns the standing fulfilled order exactly like the provider create. The alternative rejected: keeping manual funding on `manual_receipts` outside `payment_orders` would have made T17's uniform refund/correction surface reach two records for one kind of money.
- The `POST /api/v1/orders` request gains optional `returnUrl`/`cancelUrl` (absolute http(s) or site-absolute path only): the rails' return destinations resolve through one `ReturnUrls` helper, defaulting to the order's own `/app/orders?order={id}` page — the same page the session recovery drives back to.
- A17/A18 coverage: the HTTP suite drives the stub checkout journey end-to-end (create → checkout-session → signed webhook → paid → fulfilled → wallet), repeated browser submission answering the standing checkout without a second provider call, a failed provider leaving `created` until the session re-drives it, a `payment_failed` event surfacing `failed` on every read surface, a duplicate signed callback replaying to the same state, the manual receipt's fulfillment/idempotency/unique-reference/customer-refusal, and the staff cross-organization view. The React `/app/orders` pages mount the SDK checkout off `publishableKey`/`sdkUrl`/`clientSecret` and poll this contract.

## 2026-11-06 — A refund is a review, reserved before the PSP call; a dispute is not a refund (#608, T17)

- **Outcome-first, never approve-first.** The wallet take-back books only on the signed `refund_succeeded` webhook — the PSP's answer is the only terminal truth — never when staff approves. Approve commits the durable `provider_pending` reservation plus an `issued` credit note in one transaction, *before* the provider call runs, so a crash between the two leaves a recoverable row rather than a refund the books forgot. Booking credit at approve-time and taking it back on failure was rejected: `FundedReservations` makes a take-back on spent credit a clean refusal, which is exactly the "failed compensation" the audit must record — the books never lie about money that already left.
- **The refundable remainder is bounded at every layer.** A request may not exceed the order's funded `amount_minor` minus what live refunds (`requested`, `provider_pending`, `succeeded`) already reserve or returned — the promo leg never counts, grant is never refundable cash. The bound re-checks inside the resolution's locked transaction, so a second request that raced past the read is caught at write time. The display refund is proportional at the pinned rate and the last part takes the display remainder — partial refunds sum to exactly the funded total, no stranded cent.
- **Reservation before the provider call; durable retry; definite-failure release.** `refund_id` doubles as the provider's idempotency key, so a re-issued call can never mint a second refund. A transport failure leaves the row `provider_pending` with a bounded backoff (`30s,2m,5m,15m,1h`, `REFUND_MAX_ATTEMPTS=5`) the `refund-retries` sweep drains — never a lost refund, never an infinite loop. Exhausting the budget or a definite refusal releases the reservation and reverses the note, freeing the bound for the customer to ask again.
- **No silent negative wallet.** A refund the customer already spent refuses the take-back at the ledger (`InsufficientFunds`): the refund still lands `succeeded` — the PSP returned the money, that is fact — but the note stays `issued` and the audit detail records the compensation the ledger owes but cannot carry. `Liabilities:Wallet` never dips below zero; the customer's purchased pool is the ceiling.
- **A dispute is deliberately not a refund.** `dispute_opened` opens a contested record — a claim the provider arbitrates — and its lifecycle ends `won`, `lost` or `closed`, never `refunded`. Only `lost` moves money, re-charging the drawn receivable through `dispute-recharge/<dispute_id>` — the same `debit Cash / credit CreditLine` shape `repay_credit` posts — so the customer-visible ledger reads a contested-charge restoration, never a granted refund. The recharge clamps to the outstanding draw (`repay_credit` refuses outright over it, so the resolve caps the contested amount at the receivable); a `lost` dispute over nothing drawn is refused, staying open for an outcome the ledger can carry.
- **`oxsum.finance_audit` is the resolution record.** Every staff decision — `refund.approve`, `refund.decline`, `refund.release`, `dispute.resolve` — writes an immutable row naming the actor, the target, the reason and the customer-visible note it produced, keyed `(business_id, action, idempotency_key)` so a replayed resolve answers the standing audit rather than opening a second. The credit note's `audit_ref` and the dispute's `audit_ref` point back, so a correction↔refund↔provider-event chain reads end to end.
- **API.** `POST /orders/{id}/refund-request` and `GET /orders/{id}/refunds` are the customer's `Personal` surface — the request is a review, never a grant. `GET /business/refunds{,/{id}}`, `POST /business/refunds/{id}/resolve`, `GET /business/credit-notes`, `GET /business/disputes{,/{id}}` and `POST /business/disputes/{id}/resolve` are the staff finance surface behind `personal_token` + `ReportsRead`/`RefundsWrite`. Coverage: bound+replay, approve→reservation→signed-settle, partial sums, `refund_failed` release, retry budget exhaustion, dispute-not-a-refund, `lost` re-charge (drawn, capped, beyond-the-draw refused), spent-credit never-negative, provider timeout→retry→settle — 8 core + 5 HTTP tests.
- **T18 (#609) seam.** `payment_orders.promo_minor` and T18's promo-lot machinery (`grant_promo_lot`, `Equity:Promo` first-pool draw, `promo-lots/{id}/cancel` burning to `Equity:PromoExpiry`) are on main; this branch does not wire lot-cancellation into a refund of a promotional order — the bound already excludes `promo_minor`, so a refund of a bundle order refunds purchased cash only and never reaches into granted promo. A refund that should cancel the lot is the seam: `request_order_refund`'s bound plus `settle_order_refund`'s take-back arm is where the lot's `cancel` joins. T20 (#611) reuses `finance_audit` for plan-fee journal resolution; T21 (#612) reuses `dispute-recharge`'s receivable-restore shape for statement-level chargebacks.

## 2026-11-06 — Plan fees bill in arrears on immutable journals (T20, #611)

- **Billing is arrears with deferred revenue, not advance invoicing.** A fulfilled `planPeriod` order prepays its period (`debit Assets:Cash / credit Liabilities:UnearnedPlanFees`) inside `activate_plan_period`'s own transaction — the money lands beside the period it authorized, never in the wallet pools. The close (boundary rollover, renewal arm, boundary cancel — every path shares the one `journal_closing_period_in` writer) recognizes the fee: `debit Assets:Receivables / credit Income:PlanFees` for charges, `debit Income:Discounts / credit Receivables` and `debit Equity:Promo / credit Receivables` for the covering credits — then reclassifies the prepay `debit UnearnedPlanFees / credit Receivables` up to the bill. Revenue, receivable and liability each keep their own account, so a journal read never blurs what was billed, owed or held.
- **`posted` is the honest unpaid state, and the same fact `past_due` carries.** What the prepay did not cover — an overage-enabled plan's settled overflow, or a fee a scheduled change raised — stays `posted` as the open receivable. A later fulfilled `planPeriod` order settles every open journal oldest-first (`debit Cash / credit Receivables`, `paid_order_id` pinned on completion) before booking its own period's prepay, so arrears always collect before new money defers — and the `invoice_journals_posted` partial index plus `GET /business/journals?state=posted` is the dunning read T21 builds on.
- **A journal is one row of terms plus an append-only line set, both trigger-enforced.** `invoice_journal_lines` refuses UPDATE/DELETE outright — corrections append (a void's reversal set, a credit note's `discount` line). `invoice_journals` forbids DELETE always, freezes identity/period/currency, lets `total_minor` move only on a live journal and only to the lines' exact sum (a `paid` journal's total may only *fall* under a refund's discount correction — the guard's one re-price), and walks `draft → posted | void`, `posted → paid | void`, `paid → void`, plus `paid → posted` only while `disputed_minor` rises — the lost dispute's receivable restore, never a silent erasure. The `(customer_plan_id, period_index)` partial unique over non-void rows makes the close idempotent by construction: a replayed rollover or a second writer finds the standing journal and stops.
- **Corrections route through the journal, never around it.** A `planPeriod` refund skips `refund_debit` entirely — plan-fee cash was never wallet money — and instead appends its credit note as a `discount` line on the journal the order paid (via `paid_order_id`) or prepaid (via the period's `authorization_ref`), booking `debit Discounts / credit Cash` on a paid journal or `credit Receivables` on a posted one. A mid-period refund lands through the close's own scan of applied notes on the authorizing order instead — the two paths are disjoint in time, so no note counts twice. A `lost` dispute on the order re-charges the journal receivable (`debit Receivables / credit Cash` under `dispute-recharge/{dispute_id}`) and walks `paid → posted` — the contested figure restored as past-due — while a non-plan order keeps T17's `CreditLine` recharge untouched. `disputed_minor` tracks open contests and is cumulative: the record-time bump plus the resolve's recharge, so a dispute whose open was never recorded still restores the receivable under its own count.
- **The staff void is a finance action, not a delete.** `POST /business/journals/{id}/void` under `FundsWrite` appends reversing lines (the bill nets to zero), posts the mirroring entry — the open receivable reversed on a `posted` journal, the settlement unwound to `Liabilities:RefundPayable` on a `paid` one (what the customer is owed back) — lands `void` with the reason, and writes a `journal.void` `finance_audit` row (the action check widened for it). A replayed key answers the standing voided journal; a void frees the `(plan, period)` slot for a reissue naming it in `replaces_journal_id`.
- **Overage is billed at par, deliberately.** `usage_overage` is the period's `SUM(usage_records.charged_minor)` for an overage-enabled metered plan — `sold_plans` carries no separate overage rate, so the settled charge is its own price. The alternative (a markup field) was rejected: the spec's fee-conservation test reads the billed figure against the settled one, and a second price would double-count what the wallet already charged inside the period.
- **A22 coverage**: the close writes the journal its lines sum to (base fee only, plus the overage-enabled arm's `usageOverage` line), a fulfilled order pays the receivable and lands `paid`, a refund appends the credit-note discount without touching wallet pools, a lost dispute restores the receivable `paid → posted`, a complimentary period is `paid` by its grant's write-off, and a posted journal refuses terms edits, deletes, line edits and backward states at the trigger — 8 core tests plus the 3-endpoint HTTP suite (customer list/detail, staff list/detail/void).
- **T21 (#612) seam.** Statements and credit-line dunning read the same receivable `Assets:Receivables` carries: the `posted` journal is the open balance, `disputed_minor` the contested share, `paid_order_id` the order a chargeback names, and `invoice_journals_posted`/`?state=posted` the work list an overdue sweep drains. `replaces_journal_id` is where a void-and-reissue statement correction links its successor.

## 2026-11-06 — Plan statements restate the journal in customer terms; the plan's suspension is a flag, not a state (T21, #612)

- **The plan statement is a separate document from the usage statement, deliberately.** `oxsum.statements` already bills monthly *metered* arrears (issues #124, #258); what T20's `invoice_journals` bills per plan *period* is a different receivable on a different cadence, and sharing the table would conflate the credit-line meter bill with the plan-fee invoice. `plan_statements`/`plan_statement_lines` restate the journal's accounting vocabulary (`base_fee`, `usage_overage`, `promo_credit`) in customer terms (`planFee`, `usage`, `discount`, `refundCredit`, `disputeChargeback`, `paymentApplied`), issued inside the same close transaction the journal posts — the bill and the customer-readable document are one commit.
- **Suspension is `dunning_suspended_at`, not a D06 state.** The plan lifecycle (`past_due`, `expired`, `cancelled`) already means something; a new `suspended` state would fork every `state IN (...)` read the machine drives (`plan_rollover_in`, `customer_plan`, the one-live-plan unique). Instead the overdue sweep stamps `customer_plans.dunning_suspended_at` — the same shape `organizations.dunning_suspended_at` (issue #258) proves — and the wallet-hold admission paths (`hold_inner`, `hold_for_key`) read it under the org-admission lock beside the org-level flag. The consequence the A23 test leans on: a suspended plan's *quota* (already paid for) still serves — only the wallet-side overage refuses — and a payment clearing the statement lifts the flag in the reconcile's own transaction.
- **The credit limit caps the overage arm, never the subscription draw.** `customer_plans.credit_limit_minor` (NULL unlimited) bounds `freeze − subscription.drawn_minor` — the wallet-side overflow an overage-enabled plan already opted into. `admit_subscription_draw` computes `outstanding = Σ(issued|past_due statements' remaining) + Σ(open_holds freeze_minor)` inside the org's subscription advisory lock and refuses when `outstanding + this_overage > credit_limit_minor`. Subscription-drawn quota never counts — it is already paid for — so a capped binding can never strand a paid period's service.
- **The dispute chargeback keeps the restored receivable visible.** T20's `recharge_journal_dispute_in` restores the journal's receivable (`paid → posted`); the statement side appends a `disputeChargeback` line onto the *next* open statement — or a fresh `open → issued` one when none stands (`customer_plan_id` nullable for a wallet-only chargeback) — so the customer always owes against a document, never a silent ledger figure.
- **A23 coverage**: the close issues the statement its lines mirror (7 tests), a payment reconciles it (full and partial), the sweep walks `issued → past_due → grace → suspend` and the hold path refuses a suspended plan's wallet draw, the release-review restores and audits, the credit limit refuses at admission, and an issued statement refuses edits/deletes/backward states at the trigger — plus the 3-endpoint HTTP suite (customer list/detail, staff list/detail/release-review).

## 2026-11-06 — The financial-facts read is one projection; repairs are audited writes, never balance edits (T22, #613)

- **One read model, two surfaces.** `Db::financial_facts` answers the eight categories — usage (Σ `usage_records.charged_minor` settled), rated (each row re-priced by `Price::itemize` at its stored `price_version`, falling back to `charged` when the price is gone or the usage fails validation), entitlement (`subscription_draws.settled_minor` + `plan_periods.consumed_minor`), funding (paid/fulfilled `payment_orders` + credited deposits), earned (`invoice_journals.total_minor` posted/paid), collected (`settled_minor`), refund (succeeded refunds + journal `discount`/`promo_credit` lines), receivable (issued/past_due `plan_statements` outstanding + posted journals with no live statement). The customer's `GET /api/v1/org/usage-report` is the same call scoped to the caller's `organization_id` — a projection, never a second total — so the customer figure and the business sum for that organization agree by construction.
- **Three clocks, not one.** `timeBasis` picks which day a turn counts under: `booking` reads `usage_records.settled_at::date`, `service` joins the settlement entry's `booking_date` in the tenant's own `ledger_<tenant>.entries` schema (one schema-qualified query per tenant — a single SQL cannot span schemas), `payment` treats the settle instant as the pay instant for metered rows while the money tables periodize on their own stamps (`paid_at`, journal dates). The money-side figures always read their own tables' dates regardless of the usage clock.
- **Coverage separates untracked from zero.** `cost_coverage` buckets settled turns by `cost_basis` (`real_attempt`, `cache_reuse`, `estimate`, `missing`, `provider_reconciled`) with NULL folding into `missing`, counts `untracked_turns` inside each bucket (`upstream_cost_minor IS NULL`), and keeps `errored`/`refused` turns as `failedAttempts` — a real attempt that billed zero is the platform's honest zero, never mistaken for a coverage gap. The `byProviderDay` grain (`settled_at::date`, channel, model) is the shape the upstream-bill diff already reads.
- **Reconciliation is memory over the same projections.** `reconcile-financial` (leader-elected, hourly) runs `run_reconciliation` for the day just closed; `reconciliation_runs` keys `(kind, period_start, period_end)` so a replay re-runs under the run's own identity (`ON CONFLICT DO UPDATE`), freezing `expected_totals`/`actual_totals` beside it. The checks are the conservation invariants per organization — `usage − entitlement ≥ 0`, `earned − collected = receivable` — plus `missing_cost` on settled turns with untracked cost (cache-reuse exempt). `reconciliation_exceptions` dedupes `(kind, source_table, source_ref)` while open, so a re-run updates the standing row rather than minting a second.
- **A repair is a financial action, not an overwrite.** `POST …/repairs` under `FundsWrite` writes a `reconciliation_repairs` row (trigger-immutable, `before`/`after` snapshots, `audit_ref` into `finance_audit`) and performs the correcting write in the same transaction: `reclassify_cost` does the real `UPDATE` of `cost_basis`/`upstream_cost_minor` under `FOR UPDATE`; `adjust_journal`/`link_payment`/`mark_duplicate` record the audited instruction — journal lines and payment bindings are immutable records, so the next run's re-check is the close, not a declaration. `resolve`/`waive` carry a mandatory `resolution`, and the `finance_audit.action` CHECK widened to `reconciliation.resolve|waive|repair` like `journal.void` before it.
- **A24 coverage**: the customer projection reconciles to the business-scoped sum, facts derive from source rows (zeros without planted money, never a snapshot), a run flags `missing_cost` and the replay updates rather than re-mints, a `reclassify_cost` repair rewrites basis and audits, coverage keeps a real zero distinct from untracked — 5 core tests; the HTTP suite covers every endpoint (8 staff + the customer projection) including the `FundsWrite` refusal of a customer session.
- **T24/T27 seam.** `GET /api/v1/business/reconciliation-exceptions?open=true` is the console's work list; `POST …/repairs` is the operational-resolution surface T24 mounts. T27's hardening pass reuses `reconciliation_runs.last_error`/`state='failed'` for job-failure telemetry — a failed run lands rows, not silence.

## 2026-11-06 — Self-hosted install: one migrate step, readiness over liveness, intent declared in env (T28, #619)

- **The compose `oxsum-migrate` service is the migration, made visible.** The serving binary has always migrated at boot under the advisory lock; self-hosting turns that into an operator-facing step — `oxsum --migrate-only` runs the same `Db::migrate` and exits, so `docker compose run --rm oxsum-migrate` is what an install walks and `service_completed_successfully` is what `oxsum` waits on. Boot-alone still migrates (single binary stays deployable); the compose step exists so an upgrade's schema move is a log line of its own, not a mystery inside a serving restart.
- **`/readyz` is a readiness verdict, not a second liveness.** `/healthz` answers "ok" whenever the process is up — deliberately cheap, deliberately ignorant. `/readyz` answers 200 only when the database answers AND `oxsum._migrations` has run to `Db::latest_schema_version()`; an unmigrated database and a stale one both answer 503 with the level named. The compose `healthcheck` polls it, so `docker compose ps` shows `(healthy)` only for a deployment that could serve — "up but unmigrated" reads as starting, which is exactly the mid-upgrade shape.
- **`GET /api/v1/version` is the deployment's name-tag: version, gitSha, schemaVersion.** Unauthenticated — a probe arrives before any account exists and the answer reveals nothing a login page would not. The gitSha is baked at build time (crates/server/build.rs): `OXSUM_GIT_SHA` passed as a Docker build arg (`.git` is dockerignored), `git rev-parse HEAD` in a real checkout, `unknown` in a tarball — a source archive still compiles.
- **Operator declarations validate, they do not configure.** `OXSUM_RAILS` names the deposit rails the operator intends; the boot refuses either half of a mismatch — a declared rail with incomplete credentials, or a configured rail no declaration names — because both are the same "deployment doing something its operator did not say" failure. `OXSUM_CUSTOMER_SELF_REGISTER=true` requires the mailer set, moving the "registration that cannot mail" failure from inside a request to the boot line. Neither writes state: the business's own registration policy and the rail credential sets remain the configuration.
- **Demo data is opt-in and fresh-database-only.** `oxsum seed` now requires `OXSUM_DEMO_SEED=true` and refuses a database that already holds organizations — the two guards together mean a production deployment can never seed by accident (the flag is unset) nor seed over live books (the org check fires first). `Db::seed_demo`'s own idempotency check is unchanged for the tests that call it directly.
- **Backup ships the database, restore refuses the live one.** `scripts/backup.sh` writes a custom-format dump plus a manifest (schema version, organization and ledger counts, SHA-256); `scripts/restore.sh` verifies the manifest, restores only into a fresh database name — never over the source or a deployed `POSTGRES_DB` — and hands `--migrate-only` the job of closing any version gap. Keys ride inside the dump (credentials are sealed rows, not files), so the restored deployment needs the same `OXSUM_SECRET_KEY`; the manifest's SHA is what makes "this is the dump I took" checkable.
- **The release publish is a workflow of its own.** `release.yml` (`workflow_dispatch` + `workflow_call`) builds and pushes `latest`, `sha-<sha>` and an optional version tag — the deploy side effects (SSH, restart) are gone from it, so a candidate can be published without ever touching a server. `deploy.yml` calls it and adds the deploy job; both stay manual-only, which is what lets T29 build a candidate gate on the publish half without risking a deploy trigger.

## 2026-11-06 — The customer portal's branding is config, not a record; a refund stays a request (T23, #614)

- **Seller branding is a pure config read, never a stored record.** `GET /api/v1/branding` serves `{name, logoUrl, accentColor}` straight out of `OXSUM_BUSINESS_NAME`/`OXSUM_BRAND_LOGO_URL`/`OXSUM_BRAND_ACCENT_COLOR` — public (the login and register pages brand before a session exists), no database, no per-org state. The alternative — a `businesses.branding` column staff edits — was rejected for this surface: branding is the deployment operator's identity, not a customer-scoped fact, and env-var configuration keeps a self-hosted seller's rebrand a restart-and-serve, never a migration. The accent validates `#rrggbb` at startup so a typo refuses the boot rather than breaking a CSS custom property on every page.
- **The accent maps onto the `primary` token family, not a parallel palette.** `applyBranding` writes `--color-primary` plus its derived `--color-primary-hover` (a fixed 12% shade) and the `--color-primary-soft`/`--color-accent` tints (88% toward white) onto `:root` — every `bg-primary`/`text-primary` consumer follows without a prop thread or a second theme. A `null` accent removes the override, so a redeploy back to stock reverts cleanly. This keeps DESIGN.md's single-palette rule: branding re-hues the existing tokens, it never introduces a second component vocabulary.
- **The portal reuses the T16 checkout rather than growing a second payment flow.** The billing page's "pay now" creates a `invoiceRepayment` order whose `displayAmountMinor`/`displayCurrency` is the credit→fiat conversion done in integer decimal-string math (BigInt, half-up — never a float, the same formula `displayAmount` renders) and routes the browser into `/app/orders/{id}` — the page that already mounts the rail's checkout and polls the session. A repayment's fiat figure derives from the statement's ledger-minor receivable at the org's own display rate; the server rates it again server-side, so the client-supplied figure is a quote, never a negotiated term.
- **The refund CTA is a review ask, never a grant — and it shows the bound.** The order detail's refund card reads `GET /orders/{id}/refunds` for the refundable remainder plus the standing requests, and `POST /orders/{id}/refund-request` records a `requested` review under an idempotency key. A customer never sees an approve control — staff alone resolves — so the UI physically cannot promise money the backend would refuse.
- **The onboarding is a map, not a wizard.** `GettingStarted` (issue #559) already walks create-key → top-up → first-request → bill with live server truth per step; T23 keeps it — a checklist that reads real state beats a second setup flow that could drift from it.

## 2026-11-06 — The business console gates on capabilities, audits every write, and `finance_audit` finally gets its read (T24, #615)

- **The console is capability-driven, not role-driven.** `/app/business` reads `BusinessPrincipal.capabilities` — the same list `require_capability` enforces — and mounts each section behind the capability it consumes, so a role change is a server fact the nav follows rather than a second matrix the frontend mirrors. Support sees customers and the overview; finance sees the money surfaces; the backend's `Forbidden` remains the guard, a hidden nav item only saves a refused click.
- **The console switches context explicitly.** A session's context stays `customer` until the console's own mount calls `POST /api/v1/session/context` — staff can hold both a customer seat and a staff role, and the explicit switch is the documented rule (#630), so the console honors it in-page instead of asking the user to leave and come back. The chrome names the staff role beside the business so the acting authority is never ambiguous.
- **`GET /api/v1/business/audit` is the resolution ledger's read.** `finance_audit` was write-only — corrections were provable to the database, never to a page. The console's audit section needed the read, so it landed contract-first: `ReportsRead`, `(created_at, audit_id)` keyset on `after`, an `action` filter over the nine audited actions. It stays a read — no repair, no edit path, the rows remain immutable.
- **`session/context` reports the staff seat in either context.** `context_info` used to answer `business: null` whenever the session was in customer context, which made the console undiscoverable — the nav could never learn the membership existed until after a switch it could not offer. The read now answers the `BusinessPrincipal` a staff membership carries in both contexts while `context` keeps naming the acting authority: `customer` + `business: Some` means claimable, never selected (every `/business/*` call still demands the switch and its second factor), and `null` still means no seat. The membership read deliberately skips the second-factor gate — it grants nothing, and a staff member enabling TOTP today would otherwise lock their own console entry behind a factor the session predates.
- **A second close stays `NOT_FOUND` — idempotency lives in the audit key, not the endpoint.** The reconcile-exception close deliberately refuses a settled exception (T22's never-silent rule), so the console test asserts the audit-level guarantee instead: replaying the same close under the same `idempotencyKey` mints exactly one `finance_audit` row — the `ON CONFLICT (business_id, action, idempotency_key)` is what makes a retried decision write-once, and a cross-action replay (`waive` then `resolve` under one key) is a new decision, not a replay.
- **A fabricated journal id is `NOT_FOUND` before the capability check.** `POST /business/journals/{id}/void` resolves the journal's ledger tenant before `require_capability` runs (the wallet needs the tenant), so a denial test must void a *real* journal — the fixtures publish a plan, bind the customer and roll period 0 past its close to mint one.
## 2026-11-06 — The version-2 evidence bundle: the whole committed period, manifest-bound, verbatim on re-download (T25, #616)

- **The manifest is what the ledger commits; the artifacts are what it names.** A usage report's marker binds a subset digest over entries the customer can count; a whole-period bundle cannot fold one digest over heterogeneous rows — so the commitment is a *manifest*: a typed list of `{kind, id, digest}` rows (singular kinds — `usageRecord`, `planStatement`, `paymentOrder`, `refund`, `dispute`, `creditNote`, `daySeal`) folded under `oxsum/evidence-manifest/v1` into `manifestDigest`, and each artifact digested under `oxsum/evidence-artifact/v1/<kind>` at write time. The marker is the established self-balancing 1-minor `Equity:Adjustments` entry whose `DocumentRef` binds `oxsum/evidence-bundle/v1` to the manifest digest — so "which rows this export commits" is itself inside the Merkle log. The verifier's check chain is: every manifest row's artifact is served and re-digests to its committed figure → the served manifest re-digests to `manifestDigest` → the marker binds that digest → each committed entry adopt-verifies and includes in the head → the head's note verifies under the published key. A dropped row fails `MissingArtifact`, a changed figure `ArtifactMismatch`, a foreign bundle `Malformed` on the organization binding — each rejection names its step, never a bare false.
- **Digest canon is typed structs, never `serde_json::json!`.** `json!` objects serialize keys alphabetically (BTreeMap), but both writer and verifier canonicalize through `serde_json::to_string` on structs whose fields serialize in declaration order — `kind, id, digest` for a manifest row. The wire-format rule is now stated twice in code (writer in `oxsum_core`, verifier's `ManifestEntryWire` with a manual `Serialize` in `oxsum_verify`): a manifest hashed through a `json!` map commits a digest no verifier can reproduce — the test fixture itself was the first offender. Any new evidence artifact type must hash through a declared-struct, or its digest differs between minter and verifier.
- **Verbatim bytes on re-download, under the `data` envelope.** The mint freezes `bundle_json` once — every digest the manifest commits is over that serialization — so the download endpoint cannot re-serialize without risking a drifted byte. The owner download and the public token download splice the stored bytes inside `{"data":` … `}` byte-for-byte; the served `data` object is the same string the verifier digests. A re-issue replays the standing row through the scope's idempotency key (`evidence-bundle/<org>/<kind>/<span-or-statement>`), so the committed period can never grow a second bundle.
- **Share tokens are bearer capabilities with a spendable budget.** `oxe-<random>` answers once at mint; the database stores SHA-256 only. Resolution is one atomic UPDATE gated `revoked_at IS NULL AND expires_at > now() AND download_count < max_downloads` returning the bundle id — the download spends its budget in the same statement, so two concurrent fetches cannot overspend a `maxDownloads: 1` token. Every dead-token case (expired, revoked, exhausted, unknown) answers the same `410` — indistinguishable, so a guessed token leaks nothing — while a storage failure answers `500`: a `410` would falsely tell the holder its token died. `download_count`/`last_download_at` give the operator the "who fetched this" audit the issue asks for; `DELETE .../share` stamps `revoked_at` on every live share of the bundle and reports the count.
- **AccountManage capability, not Personal.** `customer_route_capability` classifies every new `/org/evidence-bundles*` and `/org/statements/{id}/evidence` route AccountManage — an Owner/Admin session, never Member and never a consumption key. The bundle exports the organization's *entire* billing span (every customer's usage rows, all journals, all payment orders) — the same exposure class the usage-report mint already sits at, so the contract marks each `x-customer-role-access: owner-admin` and `x-consumption-key-access: denied`.
- **Supplier cost is redacted by default, and the scope block says so.** A usage artifact's `upstreamCostMinor`/`real_attempt` is the *platform's* supplier spend — a different organization's books — so the default mint redacts it from the export while always shipping the customer's `chargedMinor` and the row's `costBasis` spelling (`real_attempt`, `cache_reuse`, `estimate`, `missing`, `provider_reconciled`). `OXSUM_EVIDENCE_UPSTREAM_COST` opts the figures back in for an internal-audit deployment; the bundle's `scope.upstreamCostIncluded` records which case the mint ran under so a verifier never has to guess why the field is absent. The `scope` block is where the honest exclusion lives — `excluded` names other organizations' rows, out-of-span history and in-flight operations — per the proofs.md discipline that a bundle proves the committed set, not the world.
- **Legacy compatibility is a `version` dispatch, not a new type.** `verify_evidence_bundle` reads `version` off the wire: `2` runs the full-period check chain; `1` (or absent — the legacy usage-report bundle shape) answers `WrongVersion` rather than misparsing, because the verifier crate's `verify_usage_report` already owns v1's semantics. The `verify-consumer` example keeps its v1 fixtures untouched and adds a *real* minted v2 bundle (captured by the env-gated `dump_v2_fixture` test, verified before it is written) — so the compatibility claim is exercised, not asserted.
- **A10/A26 coverage**: a genuine minted bundle verifies end-to-end over HTTP (9 server tests — mint, replay, list, download, statement scope, share create/list/revoke, public download, 410 on dead tokens, download-count audit); 11 verifier tests in `oxsum_verify` cover tampered artifact/manifest/marker/entry/proof/note/origin/key rejections and the unsigned-deployment path; the packed `verify-oxsum-bill` tarball verifies the genuine bundle and rejects the tampered/dropped/wrong-org mutations offline. The `OXSUM_EVIDENCE_UPSTREAM_COST` flag round-trips through config in both directions.

## 2026-11-06 — The candidate gate proves one SHA and produces evidence, never a release (T29, #620)

- **The gate takes a SHA, not a ref.** `release-candidate.yml` accepts an exact commit and every job checks it out explicitly; the `resolve` job refuses a SHA that is not an ancestor of main. A candidate is what merged, never what a branch happened to point at when the run started — the manifest records the resolved 40-char commit, so the artifact set and the verdicts can never drift apart.
- **The heavy tier is inherited, not re-run.** A promoted SHA is a main commit, and ci.yml's post-merge push run already covers the three nextest shards, the process-shared suites and the doubleentry conformance suite on that commit. The candidate workflow runs the fast tier (fmt, clippy, lib/doc tests), the wasm and frontend builds, the packed-SDK consumer check, the T26 developer-examples fresh-install gate (`scripts/verify-examples.sh` on a scratch deployment) and the image build — the things the push run does not produce — and names the post-merge run as gate 0 in the runbook. Re-running ~30 runner minutes of database suites on a dispatch would buy nothing the SHA does not already carry; a displaced push's missing gate is covered by the nightly and by re-dispatching ci.yml, not by duplicating the tier here.
- **The image is built and inspected, never pushed.** The candidate `image` job runs the full Dockerfile with `GIT_SHA=<sha>` baked in, loads it into the runner's daemon and records its identity in the manifest — there is no registry login in the workflow at all. Publishing stays `release.yml`'s manual half and deploying `deploy.yml`'s, so a gate run is provably incapable of leaking a candidate into `latest`.
- **Evidence is artifacts, not assertions.** The `candidate-bundle` upload carries the manifest (SHA, run URL, image identity, per-gate verdicts), `SHA256SUMS` over every produced artifact, the exact `verify-oxsum-bill` tarball the clean consumer installed, the real wasm bundle and `web/dist`, and a release-notes draft cut from the candidate's own `openapi.yaml` changelog paragraph — the contract's words, not a re-summary. A release manager reviews a bundle; nothing about the run requires trusting a log line.
- **The live smoke is a script, not a job.** `release-candidate-smoke.sh` needs a Docker daemon with room for a real postgres volume and runs minutes, so it lives beside the other operator scripts rather than inside the workflow: it brings the candidate image up on a throwaway compose project, walks `--check-config` → `--migrate-only` → `/readyz` → `/api/v1/version` → the T03 setup flow → a staff invitation, then runs `check-verify-consumer.sh` for the genuine-plus-tampered verifier half. The workflow covers the deterministic subset (packaging, checksums, consumer validation); the runbook documents the smoke as gate 2, operator-run, before promotion.
- **A30 mapping**: same-SHA gates (workflow, fast tier + image), startup/version/readiness plus one customer and one business surface (smoke steps 1–5), browser-verifier positive + negative and the clean consumer (workflow `verify-consumer` job + smoke step 6), checksums/provenance/notes (`candidate-bundle`), and no publication claim — nothing in the gate pushes.
## 2026-11-06 — The support matrix proves every row; examples run against a real listener (T26, #617)

- **Docs claim what a test proves.** `docs/integrations.md` is a support matrix, not marketing: every "supported" row names the endpoint and cites the suite that exercises it, and every "not supported" row names its refusal behavior — 404 for unrouted surfaces (no `/v1/images|audio|fine_tuning|batches|moderations`), `400` for a media part that has no computable input bound, schema rejection for a scope outside `customersRead`/`meteringWrite`. `docs/unsupported.md` is the refusal catalog's home — each boundary (retired `POST /api/v1/topups` always 403, client-supplied `amountMinor`/`rate` a schema rejection, a credential on a foreign customer 404, a dead evidence-share token an indistinguishable 410) names its error code so a caller tests against truth. The provider-account import OAuth flow (issue #506) is listed as deferred, never implied.
- **`listBundleManifests` is inspection, not verification.** The SDK helper is a pure function over the bundle JSON — it returns the committed `{kind, id, digest}` manifest rows verbatim and throws on a malformed manifest, matching the verifier's own refusal to guess. It deliberately does not fetch, digest or check: a developer lists what a bundle *attests* and runs `verifyEvidenceBundle` for what it *proves*. `examples/verify-consumer` prints the manifest before verifying, so the walkthrough shows the split.
- **Examples drive documented endpoints only, never fixtures.** `examples/consumption` walks invite → redeem → manual-receipt top-up → metered call → usage/bills → evidence bundle → offline verify; `examples/scoped-management` mints an `oxs-int-` credential, reads its selected customer, bills an event and is itself refused the customer's own records; `examples/webhook-tools` signs real provider bytes under each rail's documented scheme and asserts the intake's dedup/409/single-credit semantics. A consumption example cannot invent a card checkout — `POST /api/v1/orders` needs a provider callback — so it uses `POST /api/v1/admin/organizations/{id}/manual-receipts`, the deployment-authority stub, and *says* that is the stub; the refund/order rail flow is documented as needing a configured rail rather than faked.
- **`scripts/verify-examples.sh` is the fresh-install gate T29 wraps.** It builds the binary, creates and migrates a scratch database (`oxsum_examples_*`, dropped on exit like the test suites), seeds the demo world for a priced channel and owner, binds a free port, runs every example against the real listener, and exits non-zero on the first failed assertion — so a candidate gate can call one script and get the whole examples surface's verdict.
- **A27 coverage**: the new `examples_surface` suite (3 tests) proves the refusal catalog answers its named codes, a scoped credential cannot reach past its grant (foreign customer 404, scope-refused metering, no customer-records read, no self-widening mint), and a 16-way signed retry storm single-records under the versioned intake; webhook signature schemes and the pending→accepted→applied walk are covered by `merchant_events.rs`, scope/metering edges by `integration_credentials.rs`/`metering.rs`, the SDK export by `check-verify-consumer.sh` end-to-end.
- **The evidence-bundle scope key carries the organization** — a T25 bug the consumption example surfaced on a second customer: the marker's deterministic entry id doubles as the global `oxsum.evidence_bundles` primary key, and a bare `period/<from>/<to>` maps *every* organization minting the same span onto one row — the second mint's `ON CONFLICT` silently skipped and that organization's download 404'd. The scope key is now `period/<org>/<from>/<to>` / `statement/<org>/<id>` — same replay-within-a-ledger semantics, unique across the global catalog. The verifier treats `bundleId` as opaque, so no wire shape changed.

## 2026-11-06 — Acceptance records evidence, not verdicts; the gaps stay named (T30, #621)

- **The acceptance matrix cites, it does not summarize.** `ACCEPTANCE-RESULTS.md` maps every A-scenario to the suites, scripts and run outputs that prove it — a row without a citation is a gap, not a pass. Where coverage genuinely splits (A15 stub surfaces vs. live providers, A18 reservation semantics vs. provider money-out) the row records both halves and marks the external half pending — a "pass" that hid its untested half would be the one dishonesty the release cannot afford.
- **Executed evidence and inherited evidence are labeled differently.** This acceptance ran the journey suites (22 tests: console, examples, install ops, evidence bundles), `check-verify-consumer.sh`, `fmt` and script syntax — recorded with counts. The heavy tier (nextest shards, process-shared, doubleentry conformance) is *inherited*: the candidate SHA is a main commit, its post-merge CI run is the evidence — named gate 0 rather than re-run for the record.
- **The gaps are written down, not resolved.** D09's normalized 1,000-customer shape is unmeasured at target scale, `soak.sh` is unrun to completion, browser journeys are contract-level not pixel-level, and no rail has moved real money — each is a stated release limitation in `RELEASE-CHECKLIST.md`, because a limitation the reader can see is safer than coverage they assume.
- **Launch materials name what they cannot prove.** `SECURITY.md` gives reports a private channel and the redaction rules; `SUPPORT.md` tells operators which diagnostics (`/api/v1/version`, migrate logs, backup manifest) travel with a report; `RELEASE-CHECKLIST.md` ends in the external-action list where every entry carries its prerequisite — merchant sandbox needs credentials, pilots need real operators, publication needs a G5 decision. A checklist that ended in "launch" without naming who authorizes it would be fabricating the gate.
- **A31 stays external-pending by construction.** The contract's own rule — never simulate outside-user evidence — means T30's job is to make the external list *executable* (exact prerequisites, expected artifacts, who performs it), not to produce the evidence itself.

## 2026-11-06 — The candidate gate installs wasm-bindgen from crates.io, not GitHub releases (T29 follow-up)

- **A pinned upstream release asset is not a durable install source.** The first `release-candidate.yml` dispatch failed on all three jobs that install `wasm-bindgen-cli`: the workflow `curl`ed `github.com/rustwasm/wasm-bindgen/releases/download/v<lockfile>/…`, and the `rustwasm` org's move to `wasm-bindgen/wasm-bindgen` left `v0.2.129` without that asset — the pin was correct, the source was not. `cargo install wasm-bindgen-cli --version <lockfile> --locked` compiles the same version from crates.io, whose published versions are never removed, so the gate no longer depends on an upstream maintainer keeping an old binary uploaded.
- **The verifier's wasm-bindgen version stays the lockfile's.** Bumping to `0.2.130` would have made the release asset exist again but would have changed the toolchain that post-processes `crates/verify` — a verifier-facing byte change smuggled inside a CI fix. Installing the pinned version keeps the gate honest: the artifact the candidate produces is built by the same wasm-bindgen the lockfile already commits to.

## 2026-11-06 — The receive/refund chain was certified against a simulated provider completion, and the boundary is written down (T31 partial)

- **Fake-IP DNS is an environment fact, not a defect to route around.** The local Clash/FlClash fake-IP layer resolves `*.airwallex.com` into RFC-2544 `198.18.0.0/15`; `ProviderNetwork::Public` correctly refuses that range, so `POST /topups/airwallex` 503s at `merchant_checkout::create`. The destination policy is T06's SSRF guard working as specified — weakening it (an `internal` policy for a benchmark reservation, or a DB edit the immutable-versions trigger refuses anyway) would be the defect.
- **The chain was still exercised end to end where the product owns the boundary.** The provider-side step ran through `curl` on the system proxy (which does route the fake-IP) to mint a real demo intent; the in-repo receive path — `webhooks/airwallex` signature verify → `confirm` → `credit` → `/admin/deposits/{id}/reverse` → ledger conservation — then ran live against the deposit that create left pending. What the run cannot cover is Airwallex's own hosted-checkout capture and a live refund call *to* the provider: both need a public-internet path the local DNS does not offer and a merchant with live-refund permission, and both stay on the external list.
- **A soak's credential must match the capability the probe reads.** `scripts/soak.sh` drove `authorization: Bearer` against `/api/v1/balance`, but a consumption key is structurally `ScopedRead`-only — `Personal` routes refuse it by design, not by a missing scope. The executed run substituted the session cookie, which serves the same drift-probe intent; a `SOAK_AUTH` flag would upstream the choice without changing the measurement.

## 2026-11-06 — The sccache wrapper is a job-level dependency, not a global given (T29 follow-up)

- **`RUSTC_WRAPPER=sccache` at workflow scope made every rustc a sccache exec — including in jobs that never installed sccache.** The crates.io wasm-bindgen install (#677) turned the download step into a compile step, and gate run 42233665702 surfaced the latent gap: `wasm` and `verify-consumer` carried `rust-cache` but no `mozilla-actions/sccache-action`, so `cargo install` died with `could not execute process 'sccache … rustc' (never executed)`. The examples job had sccache and got further — into the *next* gap, `npm pack` running before `build-verify-wasm.sh` left `web/verify-sdk/wasm/` on the committed placeholders, and the SDK's own pack-time assert refused the empty tarball. Both failures are the same shape: a step assumed a predecessor the job graph never declared.
- **The fix declares the dependency where it is used, not globally.** `sccache-action` was added in front of each `cargo install` (#679), and `build-verify-wasm.sh` now runs ahead of `npm pack` inside `verify-examples.sh` — the same ordering `check-verify-consumer.sh` already had. A workflow-level "install everything" step would have hidden the same coupling again; putting each prerequisite beside its consumer is what lets a future job copy the pattern without inheriting a missing step.

## 2026-11-06 — Real-provider certification ran on test-tier quota; Airwallex's 403 is a credential finding (T31 follow-up)

- **A certification call is sized to prove the path, not to spend.** The Deepseek run used `max_tokens=4` and a one-word prompt — `prompt 6 / completion 1` tokens — because what the evidence must show is *the gateway's own hold→settle→export chain against a live upstream*, not a usage volume. `x-oxsum-freeze-minor=1`/`charged=1` plus the offline `verify.mjs` PASS is the whole claim; a bigger call would add cost, not coverage.
- **A provider 403 after the network is proven clean is a credential verdict, not a transport bug.** `api-demo.airwallex.com` was reachable (`114.117.133.34`, real IP, proxy-bypassed, IP-pinned — all 403 the same `authentication/login`). The receive side had already been certified in-repo; the create side failing on `unauthorized` narrows the external blocker from "environment" to "a working demo credential" — a materially smaller ask, and the evidence documents it instead of retrying indefinitely.

## 2026-11-07 — Candidate acceptance requires executed evidence on the same SHA (#620 follow-up)

The green candidate run on `930794c` coexisted with a failed full CI run. Being
an ancestor of main is not proof of passing post-merge checks. The candidate
resolve job now validates the exact CI SHA, main branch, completed result, all
three shards and executed process-shared/engine-conformance steps, retaining
that evidence with the manifest. A missing, failed or skipped check refuses
the candidate; a manual full CI run supplies evidence when path filtering or
conditional conformance skipped it. No repository protection is bypassed.

The same candidate image is now booted by the workflow on an isolated Compose
stack, and its version must equal the requested SHA. A temporary project
directory keeps operator configuration untouched. The tested image is exported
for download together with its smoke log; it is never published. This
supersedes the T29 choice to leave live smoke entirely to the operator.

## 2026-11-07 — Prompt payment fulfillment retains the durable worker boundary (#692)

The browser checkout exercise showed signed success leaving an order paid
until the next daily reconcile. Add a leader-required `payment-fulfillment`
kind with a five-second cadence and the existing bounded merchant recovery
pass. Keep callbacks short and preserve the observable paid/fulfilled state
boundary instead of moving wallet writes into the provider HTTP callback.
The real scheduler regression postpones daily reconcile, then requires the
payment worker to credit a signed order within ten seconds. Existing mismatch,
duplicate, bundle and refund tests continue to cover the shared writer.

## 2026-11-07 — Rust style follows the Google guide, with one pinned formatter and named exceptions (#697)

- **The guides apply, but not the `[chrome]`/`[Fuchsia]` annotations.** The applicable parts of the [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html), the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and the [Rust Style Guide](https://doc.rust-lang.org/nightly/style-guide/) are the workspace standard. Rules annotated `[chrome]` (small crates, flat layout, no `async`, no `std::thread`) and `[Fuchsia]` (its mutex) describe Google's environment, not this deployment, and are not adopted. Application crates (`oxsum-core`, `oxsum-server`, `oxsum-integrations`, `oxsum-web`, `oxsum-testkit`) bind the style and organisation rules; API-design rules are followed rather than mechanically applied. `oxsum-verify` is `publish = true`, so the API-design rules bind there.
- **Vendored `doubleentry` keeps upstream's style.** It is not reformatted, reordered or restructured. Formatting a vendored crate makes every upstream merge a conflict and turns the copy into a fork; keeping upstream's layout is the cheaper and more honest choice. Any necessary behavioural change still carries the `oxsum change (not upstream)` comment and its own entry here.
- **One pinned nightly rustfmt, one version file.** `imports_granularity = "Module"` (one `use` per module) and `format_macro_matchers` are nightly-only, so CI and contributors need the same formatter. `rustfmt-nightly` at the repository root is the single version source; `scripts/fmt.sh`, `scripts/fmt-check.sh`, the CI `fmt` job and `.githooks/pre-commit` all read it. Formatting runs `cargo +<version> fmt -p <our crates>`, never `cargo fmt --all`, so `crates/doubleentry` is excluded by construction. A nightly bump is its own PR containing the one-line version change and the resulting formatting diff, which keeps local and CI output reproducible; "latest nightly" is rejected because two runs at different times can use different formatter versions.
- **`oxsum-verify` is hardened wholesale.** All eight public enums (`ChargeCheck`, `HeadError`, `WitnessVerdict`, `PriceError`, `SaleError`, `AnchorError`, `UsageReportError`, `EvidenceError`) and all eight public-field structs (`ProofBundle`, `PriceLink`, `SaleLink`, `BillVerdict`, `PublishedKey`, `WitnessPin`, `WitnessNote`, `StatementAnchorCheck`) are `#[non_exhaustive]`, with `new` or builder constructors. Adding a variant or field is a breaking change for a pattern-matching downstream user unless the type is already non-exhaustive, and a verifier crate's errors are exactly the kind of type that grows. The wire shape does not change: `non_exhaustive` is a compile-time attribute and serde ignores it.
- **`FromStr` only where `parse` already fails with a reason.** `FromStr::Err` is a `Result` error; a parser that models one failure dimension as `Option` would need a fabricated `Err = ()`, so its inherent `parse` stays. Parsers that already return their domain error get `impl FromStr`, with `parse` kept as a thin wrapper for discoverability.
- **A poisoned mutex panics.** The three mutex sites in our own code use `.expect("... mutex poisoned ...")` instead of reading the poisoned interior, each carrying a targeted `#[expect(clippy::expect_used)]` because the workspace lint would otherwise deny the deliberate panic. A lock only becomes poisoned after a panic left its invariant half-updated; continuing on that state is how a billing inconsistency becomes silent. The seven occurrences inside vendored `doubleentry` keep upstream's recovery behaviour and are covered by the vendored exemption.
- **Non-leaf modules only declare, re-export and glue.** `oxsum-verify`'s `lib.rs` and the gateway's `mod.rs` are split by domain into leaf modules, one domain per PR. Public paths are preserved through re-exports, so the split is invisible to callers.
- **Application APIs take `&str`/`String`, not `impl Into`.** Four public builder parameters in `oxsum-core` and `oxsum-server` named their concrete types, and `impl Into<String>` only obscures what a caller may pass; the call sites are ours. `doubleentry`'s library-style API is unchanged.
- **`missing_docs` is a workspace lint after the docs exist.** Public functions and modules are documented first, then `[workspace.lints.rust] missing_docs = "warn"` is enabled. Enabling it first would turn every phase red for work the plan already schedules.
- **Documented exceptions.** `testkit`'s `#[ctor::dtor]` runs at process exit to drop scratch databases, not before `main()`, and only in test builds. `Arc` is used for shared wallet/tenant state, `Arc<dyn Trait>` and tokio primitives; `Rc` is unused. Long SQL string literals are not wrapped by rustfmt and are not split by hand. `serde_json::Value`, `serde_json::Map`, `std::fmt::{Formatter, Result}` and `std::io::Error` stay module-qualified because they are common or ambiguous names. Short-lived private tuples such as `Option<(Client, SecretKey)>` stay tuples because they are destructured at the call site. Boolean parameters that remain are documented role switches, not `enum` candidates this refactor changes. `with_*` builder methods share their name with closure accessors in `doubleentry`; the naming rule is not enforced against vendored code. Serde `serialize_with` callbacks take `&T` by signature, so `date_opt::serialize` keeps `&Option<time::Date>`; changing it would no longer compile. The `oxsum-verify` builder setters keep `impl Into<String>`: they are the published builder API, where accepting both `&str` and `String` is the convention, while application code takes `&str`.
- **High-signal lints only.** The refactor enables `non_exhaustive` and `missing_docs`, not `clippy::wildcard_imports` or `clippy::absolute_paths`. Those would flag the documented exceptions above and force `#[allow]`s, which is a worse invariant than a review rule. Production glob imports are checked by review and by the fact that `src/` is small enough to scan.
- **The refactor changes no behaviour.** It does not touch `crates/server/openapi.yaml`, the database schema, the ledger, or any billing path, and it neither deletes tests nor marks them ignored. The one observable change is the poisoned-mutex panic above, which turns silent inconsistency into a crash.

## 2026-11-07 — Promo writers take the ledger append lock before promo row locks (#723)

- Status: Adopted. Fixes the `40P01` deadlock the post-merge run on `8fdfe92` raised in `a_racing_lot_pin_and_expiry_never_apply_both`.
- Background: a hold pins its promo lots under the ledger's append advisory lock, then appends. Expiry and cancellation locked the `promo_lots` row first and only reached the append lock through `burn_promo_in`; settlement locked `promo_draws`/`promo_lots` in `consume_promo_draws_in` first; granting inserted the lot row first. Each writer could therefore hold a promo row and wait for the append lock while a hold held the append lock and waited for that row — a genuine lock-order inversion, not a flaky test.
- Decision: every transaction that writes `oxsum.promo_lots` or `oxsum.promo_draws` acquires the ledger append advisory lock before its first promo row lock. `Wallet::lock_append_in` centralises the acquisition; `hold_for_key` calls it after its key and admission locks, and expiry, cancellation, `settle`, `settle_in`, `grant_promo_lot_in` and the legacy migration call it at the start of their promo work. The later engine append re-enters the same transaction-scoped lock.
- Why not reorder the hold instead: `hold_inner`'s pre-claim keeps the pool split, the lot pin and the append in one serialized region, and moving it after the pin would reopen the split to races. `hold_for_key` takes the lock from the pin onward. Moving the other writers forward preserves both and gives the whole family one order.
- Pinned by `a_racing_lot_pin_and_expiry_never_apply_both` (50 consecutive local runs green) and the core promo/settlement and server `faults` suites.

## 2026-11-08 — Adopt the complete operator-launch contract (#725, #726)

The user accepted P01–P11 and N00–N21 in the November 8 execution brief. [The tracked contract](roadmap/operator-launch/CONTRACT.md) preserves all R01–R14/J01–J18 requirements and the existing release gates. The target is self-hosted AI-service billing and operations for API operators and embedded AI applications; one Business owns supply, sale terms and collection for many customer ledgers. The established Rust/PostgreSQL/React/doubleentry/verifier architecture stays.

Daily plan revenue recognition is an explicitly accepted new versioned policy, with compatible period activation and immutable legacy history. N11 must prove cumulative integer rounding, final remainder and absence of double recognition. N21 must add bounded application namespaces and service-managed accounts with later verified human consent, without inventing logins or widening existing machine credentials.

Implementation, focused issues/PRs and normal protected green merges are authorized. External certification remains distinct: the DeepSeek smoke has one persistent CNY 0.10/five-attempt maximum, local payment/mail fixtures are allowed, and a fresh pinned disposable Hyperswitch stack is required. Public publication, live payments, paid cloud, external contact and existing remote upgrades are excluded from this run. Missing PSP credentials, independent pilots or qualifying capacity hardware affect only their exact acceptance rows. The execution ledger records all progress and spend across contexts; software gaps cannot be relabeled external.
## 2026-11-08 — Plan-fee revenue recognition is versioned on the plan binding, daily for v2 (N11, #737)

- Status: Adopted (policy boundary and semantics). Implementation lands with N11 once #779's journal/statement corrections settle; this entry records the contract that work must satisfy.
- Background: today every plan-period close recognizes the whole fee at once. `journal_closing_period_in` books the accrual `debit Receivables / credit Income:PlanFees` for the full `total_minor` in the close transaction, so a thirty-day plan's price becomes income at its period end. N11 changes that timing to follow completed service days; the earlier description of day-one recognition was incorrect.
- Decision: recognition policy is a versioned attribute of the plan binding, not a global switch. `policy_version` rides the customer plan/sold version — `v1` keeps the existing close-time full recognition, `v2` releases each completed UTC service-day's earned share out of `Liabilities:UnearnedPlanFees` into `Income:PlanFees`. Only newly bound `v2` versions default to daily recognition; every plan already on `v1` keeps it until a renewal rebinds a `v2` version. No committed journal, closed period or issued statement is rewritten. An explicitly selected v2 version starts its policy at paid period activation; ordinary v1 renewal stays v1.
- The v2 accrual: the authorizing payment still lands `debit Cash / credit Liabilities:UnearnedPlanFees` up front (collection is not revenue). Each completed UTC service-day's earned share posts `debit UnearnedPlanFees / credit Income:PlanFees` under a unique `(period, day)` effect — computed with checked integer arithmetic, the cumulative earned figure derived from the days elapsed, and the exact final remainder allocated at period close so the day-rows and the close can never double-count one minor. A `v2` close does not also run the `v1` full-fee accrual on the same fee.
- Explicitly out of scope: recognizing delivered service daily is not customer upgrade proration, does not alter the sold fee, and does not mean the PSP charges daily. Those stay separate commercial actions. This policy corrects an internal recognition discrepancy; it is not a claim of statutory accounting compliance.
- Why a version boundary, not a global date: a single cutover date would have the same business's customers recognizing the same period under two policies on either side of one midnight, and reconciliation would have to read both at once. Versioning on the binding keeps one plan on one policy for a whole period, keeps the legacy path intact for plans that never rebind, and makes the mixed-book window explicit and finite.
- Why not retroactive: committed journals are evidence. Restating them would fork the ledger's own verifiable history; the correction lands at the next natural boundary instead, which the sold-version machinery already sequences.
- Pinned by the N11 acceptance criteria (#737): reviewers must reproduce opening/action/closing amount tables and verify conservation without treating quota as cash or collection as earned revenue.
## 2026-11-08 — The runnable demo owns its database and verifies exported bytes (#728)

- A demo invocation creates one random database or stops. It never falls back to the provisioning database, retries startup by clearing tables, or loads a checkout's `.env`. A private temporary working directory blocks dotenv discovery; generated application keys and filtered environment prevent accidental provider, mail or merchant activation. Python psycopg supplies database creation/drop without assuming a local `psql` binary or a particular Docker container.
- Owner consent, customer invitation, manual fixture funding and explicit sale assignment share the maintained Node example's bootstrap. The Python entry point remains the official OpenAI SDK streaming example. Its funding is a synthetic receipt, never a PSP observation; the model route is constrained to the loopback stub.
- Verification happens after the server and database are gone, using the built npm tarball installed outside the workspace. The immediate settlement's retained content hash, pre-turn key/head and signed price/sale bindings must verify; altered inputs must fail. Closed-period/witness evidence stays a separate scope. The local key is retained before use but is not an independent trust source.
## 2026-11-08 — Pin and isolate the disposable payment switch (#757)

The previous local sidecar combined a moving source clone with moving images,
shared demo credentials and a PostgreSQL 17 volume mounted above its data
directory. Rebuild the explicitly disposable fixture as a random task-owned
Compose project, pin all image digests and source/migrations/config at one
verified snapshot, and checksum the architecture-specific Diesel tool. Keep
PostgreSQL at `/var/lib/postgresql/data`, Redis AOF, router files and captured
mail in named volumes. No automatic volume deletion or source reset is part
of setup. Persist private generated encryption/admin/merchant identity before
network mutations and reconcile one-time keys on retry.

Use the pinned router's supported Superposition file fallback for this local
fixture; it avoids a moving or unpersisted demo policy service. Retain the
upstream `dirty` build metadata and immutable digest honestly. Remove obsolete
copied configuration and shared-password seed scripts; render the exact
upstream files at prepare time. This is not a production Hyperswitch topology.

The dummy `paypal_test` connector exercises the actual switch's payment/refund
API but does not constitute external PSP certification. API-only smoke uses
locally signed event fixtures with explicit provenance. Public SDK acceptance
requires both configured SDK/backend HTTPS origins and actual browser traffic.
Temporary proxies allow only assets/client operations or one exact callback,
never admin routes. Preserve all TLS and public-destination rules when local
DNS or tunnels fail. Actual Stripe sandbox keys, provider-delivered callbacks
and the owner merchant selection path remain their own acceptance/software
work; the presence of a switch key cannot stand in for them.
## 2026-11-08 — Candidate evidence binds bytes and executed steps (#752)

- A successful job without its required successful steps is insufficient release evidence. Manual/nightly CI builds the WASM/frontend stages, and the candidate gate requires named fmt, Clippy, lib/doc, archive, shard, process-shared and engine execution steps. Old path-filtered runs cannot be grandfathered in.
- Version-2 candidate manifests distinguish source SHA from workflow revision, Docker configuration ID from archive checksum and registry digest, and bind platform/source labels to the actual saved image. The SDK is copied after the clean consumer verifies it, never repacked. Archive inspection does not extract Docker layers or execute package scripts.
- Offline validation establishes internal consistency only. Online validation additionally requires the successful exact workflow attempt, its available unexpired bundle, byte-equivalent manifest, and the successful referenced full-CI attempt. Public promotion/deployment is separate; creating a valid local manifest grants no authority to publish it.

## 2026-11-08 — Preserve external identity and uncertain refund bounds (#760)

Pinned Hyperswitch v1.129.0 source proves that `payment_id`/`refund_id` in the
body identify duplicate creates; a header alone is insufficient. Persist a
new-operation opt-in on expected payment bindings and refund rows. Migration
0099 leaves historical rows false and keeps the SQL default false for older
writers; new code opts in explicitly. 0098 is reserved by the independent
business-profile PR. Old immutable bindings are never rewritten to imply that
a previous call used a body ID. Known old handles can be retrieved; an unbound
old operation requires reconciliation instead of a new create.

Retrieve before create, validate exact terms and local metadata, and retrieve
again after duplicates or lost responses. Use the existing destination-policy
client, ten-second total HTTP limits and no response/secret-bearing errors.
The first recovery lease is 60 seconds, longer than the bounded read/create/read
exchange plus DNS; each of five attempts is claimed atomically before I/O. A
crash spends its claim and retains the reservation. Exhaustion clears the due
time but does not release funds or reverse an issued note. Only a proven first
pre-insert refusal or authenticated terminal evidence can release it.

The audit also found that a supplied unknown refund ID fell back to the first
pending refund, and provider refund IDs were looked up without the order. Both
are unsafe for repeated partial refunds. Scope lookup to the authenticated
order and exact amount/currency/customer; missing IDs require exactly one
unambiguous reservation, and supplied unknown IDs remain for reconciliation.

The compatibility `fail_order_refund` entry point must also retain ambiguous
reservations; an error string cannot serve as proof of non-creation. Rotate the
bounded inbox scan by the existing last-attempt timestamp so retained unresolved
events do not starve later customers. Historical refund metadata sometimes used
the refund UUID as `order_id`; accept only that exact alias on migration-fenced
rows while rejecting other supplied customer/payment/order contradictions.

## 2026-11-08 — One provisioning authority for developer examples (#769)

The example runner previously created a database through a discovered local
Docker container while composing the application's URL from `DATABASE_URL`,
and always dropped through Docker. Those could name different servers. It also
inherited deployment variables, printed temporary credentials and installed SDK
dependencies directly into source examples.

The shell entry now delegates to a Python runner using psycopg for one explicit
provisioning connection. It creates and drops only its random database through
that same connection. Runtime environments omit deployment/provider/mail/proxy
settings, an empty `.env` terminates dotenv lookup, and each run generates secrets
and a loopback port. No demo seed or fixed-password fallback is needed. Consumers
and SDK builds live in temporary copies; supplied binary and SDK bytes are frozen
and hashed before use. Errors, essential skips and cleanup failures are failures.
A mode-0700 evidence directory retains mode-0600 logs and a sanitized manifest.

The maintained shared consumption bootstrap is supplied by PR #751. Integration
must include that PR before the default clean-checkout command can pass; explicit
`--examples-root` records source hashes for testing that dependency now. The
runner's independent verifier exercises retained genuine fixtures after its
server and database stop. The absent witness is reported separately; this gate
does not claim a fresh closed-day export contains the call just made or that
local signed callbacks are real payment-provider certification.
## 2026-11-08 — Persist public business identity after explicit owner save (#756)

- Supersedes T23's environment-only identity decision above: business owners
  now manage the public identity through their own session, as accepted in N03.
- Store the profile on `oxsum.businesses` with a monotonically increasing
  revision; revision zero means unsaved defaults. This distinguishes explicit
  null clearing from environment fallback without per-field precedence flags.
- The accepted owner-setup name is authoritative for initialized businesses.
  Unsaved logo/accent retain environment defaults. An explicit full save fixes
  every value; environment changes never overwrite it. No boot-time job writes
  or races the owner, and migration preserves existing customer/ledger data.
- Reuse the business lock/current TOTP authority and immutable mutation audit.
  Add a narrowly named owner-only `profileWrite` capability. Require the expected
  revision and retain each save result to make stale edits and lost responses
  deterministic. Historical bills and money configuration remain independent.
- Public branding now requires a database read; failure returns an error rather
  than showing another deployment identity. URL/color/email validation also
  applies to environment branding at boot. No new service or framework is used.
- Currency/conversion configuration and funded-history guards remain the next
  N03 child; this profile API cannot change them.
## 2026-11-08 — Explicit sale-book publication and activation in the portal (#731)

The existing sale-book contract already carries optimistic draft revisions,
immutable publications, UTC windows, scoped replacements and customer previews.
Expose those same operations in the Business Prices section without a second
pricing engine. Separate save, publish and activate actions; select published
versions explicitly and show effective versus scheduled/ended/canceled history.
Owner/admin manage catalog terms; owner/finance manage customer assignments and
model overrides. Read authority stays separate from mutation capability.

Parse user credit decimals with BigInt before converting to bounded safe integer
minor units. Preserve every existing conditional/cache/reasoning/per-request
field when loading a draft. Keep exact API microsecond timestamps in window
comparisons; local timezone conversion and JavaScript millisecond truncation
would mislabel a boundary. Refuse stored out-of-range values instead of silently
rounding them. The preview endpoint remains the pricing authority and performs
no model request. A lost-response retry retains its original idempotency key;
a changed payload creates a new intent. Existing admitted snapshot and signed
sale-proof semantics are unchanged.
## 2026-11-08 — Derive missing refund coordinates without rewriting events (#759)

Pinned local Hyperswitch v1.129.0 returns a refund object's own metadata. The
previous client sent its refund UUID as `order_id` and omitted local payment
and customer IDs, whereas tests fabricated those missing IDs in callbacks.
The real response therefore could not settle through the versioned intake.

Send the committed reservation's distinct refund/order/customer coordinates on
new requests. For existing metadata-poor events, derive only absent coordinates
from a provider reference already bound inside the exact authenticated merchant
and version. Preserve supplied contradictions and reject malformed UUIDs. Keep
an early event pending until its reference exists and re-evaluate it in recovery.
The inbox normalization and raw digest remain immutable; the resolution is a
join over immutable bindings. Do not relax the event-history trigger or change
old stored payloads. The environment adapter uses the same domain resolution.

This closes a correlation defect, not external PSP certification. The local
switch also exposed separate provider idempotency/unknown-outcome concerns,
tracked in #760; successful one-shot refund evidence cannot close those gates.

## 2026-11-08 — Collection conversion belongs to the Business (#765)

The earlier display preference deliberately belonged to the customer and moved
no money. Later payment orders reused that value to derive purchased credits,
so a customer could change its own collection rate or submit a different
currency code. Separate collection authority without changing any historical
order, payment binding, display setting or ledger entry.

Migration 0101 adds revisioned business collection terms and immutable revision
receipts. Existing `OXSUM_DISPLAY_CURRENCY`/`OXSUM_DISPLAY_RATE_PER_CREDIT` bootstrap
this policy once; later restarts cannot replace it. The owner can save a reasoned,
idempotent expected-revision change with the explicit `collectionWrite`
capability. Other staff read current terms; customers get a safe read-only quote.
A new order resolves these terms under the same business lock as its immutable
create. The API never accepts a caller's rate. Personal display preferences
remain presentation only. Existing keys replay their original order before
current quote validation, including a changed rate after the first create.

Historical provider bindings and order currency codes constrain adoption. A
currency already committed by any payment intent cannot be relabeled. Legacy
unbound Stripe/Hyperswitch/Airwallex deposits retain their existing USD contract;
mixed historical currencies are reported and block new collection rather than
rewriting them. Legacy credit-denominated manual receipts carry no invented fiat
currency. Rate revisions apply only to new orders. The customer UI sends the
revision it displayed; a concurrent rate change refuses that new quote before
provider I/O. Generic and legacy binding insertion shares the business lock so
currency edits cannot race past the history guard.

The conversion remains checked integer arithmetic with at most eight fractional
digits and a signed 64-bit mantissa. Current payment rails still use two provider
minor digits; this change does not imply general ISO currency exponent support.
Touched payment inputs parse decimal strings through BigInt, reject excess
precision/unsafe integers and retain one idempotency key for unchanged retries.
The separate N10 commercial offer authority audit is #768.

## 2026-11-08 — Separate legacy reversal from authenticated external outcome (#763)

The old admin reversal used current deployment credentials and an idempotency
header that pinned Hyperswitch v1.129 does not use as its refund identity. It
also treated an HTTP result as terminal and retried failures without a durable
bound. Keep the already-booked local reversal, but queue a separate immutable
operation in the same transaction. Pin original payment terms and credentials,
use a body UUID only for genuinely new operations, and spend leased attempts
before bounded read/create/read I/O. Unknown is an explicit API state. Exhaustion
cannot imply either external success or definite failure.

Migration 0100 preserves historical deposit JSON and ledger references. Existing
handles can be read with their original pinned context; no old row receives new
create authority. Rows written by an old binary after migration are fenced on
replay and projected as unknown until durable evidence exists. Authenticated
callbacks match exact merchant/version/payment/customer/refund/amount/currency;
old unbound callbacks remain retained observations only. A late success may
supersede a failed event, because money actually returned cannot be erased by
event delivery order. Success never downgrades and no callback repeats the local
wallet debit. This change introduces no external reconciliation guesses or
ledger rewrites. Migration numbers 0098/0099 belong to independent task PRs.
## 2026-11-08 — Bundle offers own promotional grants (#774)

- Problem: a customer-supplied `promoMinor` previously determined the promotional
  grant. A conversion/display rate cannot authorize free promotional credit.
- Decision: publish immutable, business-owned offer versions under current
  `CommercialWrite` authority, including exact fiat/purchased/promo quantities and
  optional validity. The customer selects a version; the deprecated promo input
  is only a zero/absent or exact-match assertion. The order pins all sold terms.
- Retirement stops new orders; an admitted order and its retry retain the original
  terms. Optimistic family revisions prevent stale publications. Mutation receipts
  retain the original result, while current authorization is checked even on replay.
- Keep the existing rate columns truthful: reject ratios not exactly representable
  with eight decimal places instead of recording an approximate conversion. No
  floating-point money is introduced in core or browser inputs.
- Legacy history is unchanged. A database insert guard fences new snapshot-less
  bundle rows from old binaries; old unfulfilled bundles require operator review.
  Fulfilled old effects and their purchased refund eligibility remain intact.
- Promotion validity begins at `paidAt`. Already expired delayed grants are issued
  and burned atomically; every retry uses the same lot. Refund review waits for
  complete bundle fulfillment. A confirmed partial/full refund cancels the remaining
  promo lot before marking success, preserving hold-release and retry conservation.
- Scope: bundle authority is one child of #768. Collection authority (#765), payment
  recovery/correlation (#759/#760/#763), plan authority and daily revenue recognition
  remain separate work; all must be tested together on the final candidate.
## 2026-11-08 — Quote and fund one sold plan period (#776)

Plan activation previously accepted a missing/foreign/unpaid order UUID and
could close an active period early. A renewal also applied its fee to older
receivables before funding its own service. These are authorization and
conservation defects, not supported flexible-payment behavior.

Each new paid intent pins customer, sold version, period index/bounds, exact
fee and Business collection conversion; a required fingerprint binds the
customer's reviewed quote. Provider minor units use checked integer ceiling,
without inverse-rounding the sold fee. One live intent per plan/index prevents
parallel checkouts. Definite failed/expired intents may be replaced; unknown
outcomes retain the standing order and reservation.

Initial service starts at paid activation. Manual renewal opens at the prior
UTC boundary, preserving the existing sold calendar/anniversary policy and
late-renewal chain. No advance entitlement, automatic PSP subscription or
customer proration is introduced. A captured unresolved payment must finish
before assignment change/cancel. New periods prepay their entire own fee;
explicit statement repayment is isolated as #777 without dropping N10 debt
requirements. Legacy completed authorizations replay, and unsafe pending
legacy orders remain reconciliation work rather than receiving invented terms.

Focused tests uncovered a partial-prepay statement mismatch and a second
wallet bootstrap waiting behind its own transaction's journal writes. Close
now reconciles every prepaid amount and renewal reuses one opened wallet.
New version renewal moves its exact quota mirror and overage policy together.
N11 versioned daily recognition will retain historical policy and postings.


## 2026-11-08 — Preserve migration identities and align upgrade fixtures (#805)

The integrated migration registry had entries outside full-version order after
parallel task integration. Upgrade fixtures sorted SQL files but still used
pre-renumbering cutoffs 0100, 0102 and 0103. The refund fixture therefore called
current collection code against a schema missing collection authority, and the
plan fixture compared unrelated later columns as though they were old data.

Keep every applied full migration name and SQL byte unchanged. Sort the embedded
registry by its full version, matching the runner's max-version readiness value
and the historical fixture order; assert that every SQL file appears once with
identical contents. The two distinct 0098 names remain valid separate records.
The already-integrated names are 0107_deposit_refund_recovery,
0108_bundle_offers and 0109_plan_payment_authority. Earlier decision references
to planned numbers describe their development history, not current filenames.
Fixtures stop immediately before the migration they exercise and still compare
all old fields; new nullable authority fields are checked explicitly as null.
An applied migration is not replayed or renamed, and no history is rewritten.


## 2026-11-08 — Promote candidate image bytes through Distribution (#801)

A successful candidate archive must determine the published image. Rebuilding
its source in Release produced an independently unvalidated artifact, and
implicit `latest`/SHA tags made the accepted output unclear. Release now requires
source SHA, candidate workflow run/attempt and one explicit version, downloads
the bundle, rechecks online provenance and transfers original configuration and
layer member bytes through the registry Distribution API. Docker archives do
not carry an authoritative registry manifest: a deterministic OCI transport
manifest is created around the unchanged blobs. No layer recompression or image
build occurs. This also avoids Docker Desktop daemon loopback differences and
the containerd `.Id`/config-ID ambiguity in isolated rehearsals.

The tool freezes a private bundle snapshot, distinguishes missing manifests
from auth/network errors, rejects a version occupied by different config/layer
bytes, and downloads by immutable digest to verify raw blob hashes and rootfs
diff IDs. A separate receipt binds source, provenance, archive, config/platform
and registry digests; it never mutates the candidate. Registry uploads accept
only same-origin upload locations. The only cross-origin read is an allowlisted
HTTPS GitHub blob redirect, without an Authorization header. Production token
exchange accepts only GHCR's exact realm and the selected repository scope.

Distribution offers no atomic compare-and-swap for tags. The workflow serializes
all its jobs, and the helper checks before tagging and after download. Operators
must exclude other registry writers during promotion; no global immutability is
claimed. A partial write without a receipt remains incomplete and can be retried
with the same candidate/version. A fixture-only mode requires a numeric loopback
registry and marks receipts `local-fixture`; it cannot bypass production online
provenance. It uses a pinned ephemeral registry and never runs the application.

The legacy deploy caller receives only the Actions-read permission required by
its reusable promotion workflow. Its missing explicit selectors now fail before
publication/SSH; N13.c separately replaces combined publication/deployment with
immutable reference deployment and database-compatible rollback. SDK publication
and final candidate acceptance remain separate units.

## 2026-11-08 — Check authentication declarations before API code generation (#800)

Nineteen OpenAPI operations referenced an undefined `apiKey` scheme; seventeen
also required that scheme and a personal cookie together. The existing router
accepts machine headers as consumption keys and blocks cookie fallback, so those
declarations could not represent the actual authority. Correct the contract to
the audited `ScopedRead`, `Personal` and `AccountManage` boundaries without
changing runtime authorization.

The frontend's existing code-generation step now runs a Node contract check
before generating types. It rejects undefined schemes, cookie/machine conjunctions
and denied-key contradictions, and pins the reviewed commercial/financial route
matrix. `js-yaml` is an explicit development dependency at its already-locked
version so the check parses YAML without depending on a transitive import. The
Docker frontend stage already runs codegen, which makes this check part of the
protected frontend gate without another CI job. HTTP authority tests remain
responsible for runtime enforcement.

## 2026-11-08 — Confirmed plan refunds allocate original cash, not debt relief (#802)

- A trusted PSP refund is a full cash outflow. A partly paid invoice is not permission to credit receivables instead. V1 refunds debit the original period's unearned prepayment before close, recognized-fee contra after close, or the retained refund payable after a journal void. A cash refund reduces the original bill and its applied payment by the same amount; it does not repay unrelated debt.
- Migration 0110 records one immutable `plan_refund_effects` allocation per refund. Original order, period, journal, note, event and ledger identities remain linked. The same transaction records cash, allocation and customer documents. A late success on a terminal failed/released refund retains that terminal row and reversed note, while its immutable cash effect consumes refundable funds and prevents activation.
- A lost chargeback can exhaust the original payment before its refund succeeds. The complete observed cash outflow still posts; only the bounded remaining source adjusts the bill. Excess is an explicitly audited `unallocated_minor` debit to `Equity:Adjustments`, held as reconciliation suspense pending finance review. It is neither normal earned revenue nor invented customer receivable, and `bookedWithReview` does not claim full allocation.
- Legacy recovery defaults to a preview, requires explicit refund identity and personal `FundsWrite` authority, and never calls a provider. It reads the original entry's actual account/direction/amounts. An old receivable-credit refund is corrected by appending the missing cash and receivable legs; an already-correct cash entry is retained without a second outflow. The original note entry remains immutable. Unsupported mixed historical corrections refuse with a finance-review action instead of guessing.
- The converse ordering uses the same rule: a chargeback after a refund records the complete outflow, restores only still-collected original payment as debt, and retains excess suspense in `dispute.resolve`. Both writers serialize on the plan and replay the actual source entry regardless of journal state.
- These are correctness repairs for every V1 period. Daily revenue recognition remains separately versioned work: it can allocate `deferred_minor` and `earned_minor` from period facts without treating the existing deferred balance of another period as this refund's money.


## 2026-11-08 — Separate digest deployment from publication (#810)

Compose now requires one explicit OXSUM_IMAGE_REF for both migration and serving;
no accepted deployment defaults to latest. Local candidate smoke may select its
explicit pre-publication tag and retains the actual version endpoint response
as hashed runtimeVersion evidence in the version-2 bundle. Promotion remains
compatible with older v2 bundles; deployment refuses those lacking actual
runtime evidence rather than inventing schema/package identity from source.

Deploy is manual and read-only toward GitHub/registry. It validates candidate
provenance and the explicit published digest before authenticated SSH transfer
of a narrow plan/helper. Pinned known hosts replace trust-on-first-use scanning.
The plan is not signed and must remain in that trusted transfer boundary. The
host pulls/saves/verifies image content before downtime, snapshots the operator
environment, migrates and serves the same digest, then checks exact readiness
and source/package/schema. Private current/previous receipts and a host lock
prevent silently adopting unknown containers or concurrent managed writers.

Readiness requires exact schema equality. Additive migrations do not establish
older-image compatibility. Same-schema rollback needs retained prior success,
unchanged database/key environment and an explicit operator behavior decision;
schema-changing rollback restores to a separate database. Migration/start/probe
failure never triggers a guessed automatic rollback or a success receipt.
Legacy unmanaged containers require explicit maintenance and retained backups
before first managed adoption. Tests distinguish real loopback HTTP with mocked
Docker state from future real candidate deployment acceptance. SDK publication
remains N13.d; no public action or live host is exercised by these tests.

## 2026-11-08 — Never rebill already-settled usage at plan close (#803)

A settled overflow request already credits Income:Usage and either consumes
wallet funds or draws its approved credit line. Summing that same request's
charged amount into a plan-close usage_overage line opens a second receivable
and credits Income:PlanFees again. This is a conservation defect under every
recognition policy, including the existing period-close policy. It is not a
policy choice that can wait for daily recognition.

New closes omit this aggregation entirely, preserving the sold fee and its
source-linked prepayment. Historical usage_overage lines remain readable and
immutable. Their explicit compensation is tracked separately by #809: original
entries remain, uncollected duplicates reduce receivables and collected excess
becomes RefundPayable, never a claimed cash refund. No historic record is
silently changed by deploying the forward fix.

Tests for legacy partial documents construct the old journal/statement shape
explicitly and use doubleentry's public transaction append API for real source
postings. They no longer rely on generating a new erroneous receivable in
production to test dunning/refund/renewal behavior. Cash-funded and credit-funded
usage regressions use actual wallet holds, settlements and linked usage records,
then compare income, receivable, wallet balance and customer statements across
close and replay. Daily fee recognition and statement repayment remain N11/#777.


## 2026-11-08 — Publish the original candidate SDK through a bounded npm relay (#814)

The old source-directory npm publish recipe reran prepack and could replace the
artifact the candidate consumer verified. Publication now requires candidate
source/run/attempt and embedded version plus an explicit tag, freezes and
validates the bundle online, and gives only the original tarball to real npm.
Lifecycle hooks and npm provenance generation are disabled; package publishConfig
and ambient npmrc/proxy/Node preload settings cannot override the path.

A bounded loopback relay lets npm form a genuine publication request while keeping
the remote registry token out of its process/config. The relay accepts only one
selected package PUT, validates exact attachment bytes and SRI, then forwards
through Python's exact-origin client with redirects disabled. Only the transport
metadata's tarball URL changes; compressed package bytes do not. Production uses
registry.npmjs.org HTTPS. An explicit numeric-loopback fixture mode is separately
labeled and cannot bypass provenance for remote publication.

Existing versions require metadata integrity and downloaded original-byte equality.
Occupied tags pointing elsewhere refuse; the helper never implicitly moves latest.
An existing version with a missing tag requires explicit operator registry repair,
not another publish. Jobs serialize, and other writers need the same exclusion
because npm tags have no compare-and-swap. A partial write without a verified
receipt remains incomplete. The independent receipt binds original hashes and
provenance; local real-npm tests do not imply a public SDK release or J14 closure.


## 2026-11-08 — Verify official CI tool assets before installation (#822)

GitHub browser-release downloads for mold and sccache repeatedly returned HTTP
504 before the Rust gates ran. The official release asset API returned the
same pinned archives. One Python installer now downloads those exact asset
IDs over HTTPS with bounded curl retries, checks size and SHA-256, and stages
safe archive members before atomically installing an unprivileged tool-cache
directory. A conflicting existing cache is refused. Mold remains 2.40.0;
sccache is pinned to the 0.19.0 that the failing action had selected.

Preseed the Actions tool-cache directory and retain Mozilla's setup action:
its pinned version input finds the verified cache, while its Node action still
exports runtime cache credentials/service version and reports post-run stats.
Replacing that action with a shell PATH export would lose hidden runner
context. No compiler flag, test gate or candidate acceptance rule is relaxed.
The old mold asset has no publisher-provided metadata digest; its checked-in
pin is explicitly the digest of two matching official downloads, not a claim
of signed publisher provenance.

## 2026-11-08 — Plan statement writers acquire their owner before the document (#811)

`extendGrace` previously held its statement while updating the customer plan,
opposite the plan-first close/refund path. The overdue worker's initial
`FOR UPDATE SKIP LOCKED` ran in autocommit, so its locks disappeared before the
per-statement transaction; that transaction then acquired statement before
plan and could suspend a statement that was already paid, voided or extended.

Read immutable business-scoped statement coordinates without locking, acquire
the owning customer plan, and re-read/validate the statement under its lock.
Statement-only writers need no subscription or ledger lock afterward. Paths
that need the wider lifecycle retain subscription → plan → journal → ledger →
statement ordering; journal void takes no plan after its document locks.
Reconciliation callers must own the plan before invoking statement allocation.
No deadlock retry conceals an inverse edge.

The overdue pass reads keyset pages of possible work, then uses `SKIP LOCKED`
for plan and statement inside each short transaction. It rechecks state,
outstanding amount, due date, grace and suspension before changing either row.
Skipped or no-longer-eligible rows consume no transition budget, and a page
cursor continues beyond a locked oldest prefix. The pass limit bounds changed
statements, not scan volume; an entirely locked backlog can be read across
several pages but never waits for those row owners. Already-suspended or
inside-grace past-due rows are omitted from candidate work. Counters report
only committed first transitions.

Controlled PostgreSQL tests observe `pg_blocking_pids` and attempt `NOWAIT`
locks while operations are paused at real plan or ledger locks. They cover
staff grace/void review, a verified refund beside review, locked plan/statement
progress, pending payment/grace updates and concurrent workers. The original
code fails the lock-ownership and locked-oldest progress cases; fixed code
retains exact cash, journal and statement amounts.


## 2026-11-08 — Fixed sanitized diagnostics rather than log bundles (#819)

Operator support needs deployment identity and recovery state without transferring
customer data or credentials. A dedicated operator-token-only GET exposes fixed
boolean configuration flags and count aggregates, never arbitrary stored strings
except a validated migration name. Database counts use a consistent read-only
transaction after exact schema comparison; unavailable or incompatible storage
yields null counts. Five-second statements and a six-second endpoint deadline
bound the read, including pool acquisition. The request cannot claim jobs,
reconcile balances, migrate storage or contact external services.

The standard-library collector validates every retained field, drops error
bodies and unknown schema, disables redirects/proxies, and sends the token only
to the protected endpoint. Explicit private files supply the token and optional
environment presence; values are never evaluated or retained. Output is a new
0600 file. Public probes are separate reads and can straddle a restart. This
feature supports diagnosis, not provider/PSP certification or upgrade/restore
acceptance; N17/J16 retain those separate required rehearsals.

## 2026-11-08 — Reverify compressed CI caches before using official fallback (#828)

The verified official asset API remained intermittently unavailable after
#822, blocking required checks before compilation. Cache the small compressed
archives independently of Cargo output and verify their fixed size and SHA-256
on every use. Copy restored bytes into private staging before validation and
extraction. A local composite action restores exact keys, installs and saves
each tool before any downstream tests. It preserves Mozilla's sccache runtime
credential setup and all existing required checks.

A cold install tries the pinned asset API and, only on bounded network/HTTP
failure, the corresponding official release URL. Both sources must match the
same pins. Integrity and local installation failures are not fallback signals.
There is no third-party mirror, tool downgrade or release acceptance exception.

## 2026-11-08 — Correct proven duplicate usage without claiming cash refunds (#809)

The old plan close recognized settled usage again as PlanFees and an invoice
receivable. #803 prevents new duplicates; existing posted facts require explicit
compensation under #779. Daily recognition does not postpone that correction.

- Scope is one exact original journal with a base-fee/usage pair. Verify actual
  settled Income:Usage postings, the original accrual and source-specific payment
  entries, including old `payment-order/<order>/journal/<journal>` Cash/AR legs.
  Mutable payment counters alone cannot authorize a correction.
- Debit PlanFees by the duplicate, credit only its still-open receivable and put
  collected excess in RefundPayable. Cash and customer pools never move. Negative
  usage and payment-to-payable lines preserve all original document evidence and
  the paid/void writer guards. The report's evidence SHA-256 binds source entries,
  content hashes, period bounds and lines into the correction entry description.
- Keep migration 0111's correction immutable and unique by journal. Ledger,
  document counters, effect and audit commit together. Subscription/plan/journal
  and ledger locks precede the original statement. Replays are inert.
- Default the local CLI to read-only preview using an already initialized wallet.
  Explicit apply audits actor, reason and key, including precise refused outcomes.
  Void/refund/dispute, unsupported prior corrections or inconsistent source
  evidence require manual review rather than guessed allocation. No provider is
  called, no broad account scan mutates records and no new HTTP surface is added.
- Fence every corrected funding order before new refund/provider authorization.
  Keep later confirmed cash facts in explicit suspense with source-journal audit;
  do not silently ignore cash, borrow another period's prepayment or consume a
  correction payable twice. Existing related refund/dispute history refuses the
  correction itself, including a concurrently created request.

Validation and limits are recorded in
[the correction evidence](roadmap/operator-launch/evidence/legacy-plan-usage.md).

## 2026-11-08 — Expose complete business model and route configuration (#818)

Use the existing catalog and routing operations under business sessions for
model versions, aliases, weights, fallback order and response-cache settings.
The connection/secret editor keeps its stronger owner capability. No API field,
authority boundary, routing algorithm or price policy changes in this UI unit.

The model form edits the complete base/conditional sets and upstream cost fields.
Channel base billing rates remain legacy customer fallback; supplier costs are
separate and missing cost is not zero. Decimal text becomes checked BigInt minor
units before conversion to a safe JSON integer. Optional absent versus zero,
empty supplier objects, all rule fields and exact service-tier text survive a
weight-only edit. Unsafe stored values refuse the editor. Appending a version
uses the existing append semantics; it does not pretend to provide optimistic
revision protection that this endpoint lacks. The displayed starting version
and immutable history make that behavior explicit.

Fallback editing loads existing state before enabling submission. Cache expiry
requires a positive duration or an explicit no-expiry choice. Every mutation
retains one key for unchanged retry, creates a new key after an edit, and keeps
drafts in memory through failures. UI labels are available in both locales.
Actual browser acceptance remains distinct from source/build/input tests.


## 2026-11-08 — Bind backup evidence and restore to explicit targets (#827)

Auto-detecting a Compose database after the operator selected DATABASE_URL could
mix the dump, inventory and restore across clusters. A shared standard-library
helper now requires one explicit container/database or PostgreSQL URL, never
sources `.env` and removes ambient libpq overrides. Credentials stay in private
files/process environment, not argv or errors. Exported repeatable-read snapshot
identity binds pg_dump and logical schema/counts; physical settings remain
observations. Dump and portable version-2 manifest are private, flushed and
published without overwriting, with the manifest last as completion marker.

Restoration requires the complete manifest, freezes the adjacent private dump,
verifies its size and SHA-256, and creates only a fresh safely quoted target.
Transactional pg_restore and exact inventory/schema comparison precede an atomic
success receipt. A created target remains after failure; lost creation responses
are recorded as unknown outcomes, never cleaned up by guessing. The drill shares
this path and no longer drops a default target. Old manifests/dumps are retained
but require a separately reviewed legacy recovery procedure instead of fabricated
checksums or silently weaker validation. Migrations, traffic switches, old-proof
checks and candidate acceptance remain explicit separate operations.

## 2026-11-08 — Expose merchant settings under their existing capabilities (#831)

The Integrations navigation accepts ReportsRead so the existing redacted merchant
contract is reachable by admin, finance and viewer roles. The integration-key
subtree remains gated by IntegrationsManage; merchant mutation dialogs require
MerchantSecretsWrite and are destroyed if that capability is lost. Mutation
credentials have no persistent storage or retained query cache. Complete
rail-specific versions prevent hidden fields from a different rail entering a
request, and changing the rail clears its draft.

This UI uses the current API without adding a default-merchant selector. Current
order creation resolves the environment bootstrap record. Configuration status,
checkout selection and external PSP certification remain distinct evidence;
enabling a merchant cannot be presented as selecting or certifying that route.

## 2026-11-08 — Make statement review one financial transaction (#820)

A document-only void left the posted journal receivable outside customer credit
and admission; a grace extension cleared the entire plan's dunning marker even
when another statement remained overdue. Both violate the existing debt model.

- Share the transaction-level journal void between journal and statement review.
  Acquire business, plan, journal, ledger and statement locks in that order;
  validate original ownership/period, source-linked charges and net payments
  before effects. Cash remains unchanged; only actual collected amounts become
  refund payable. Journal, ledger, statement and audit commit together.
- Refuse mixed chargeback/reissued/inconsistent documents instead of reversing
  guessed sources. Historical document-only voids must retain exactly one
  opposite line per original; their immutable document history stays intact
  when a new explicit request reverses the matching journal.
- Use one debt projection for customer credit, overage admission and dunning
  restoration. Include posted journals without live statements, retaining their
  last document due date or posting time plus the original binding's current configured net terms. Admission evaluates
  effective past-grace debt even when an old writer cleared the stored marker.
  Grace/void/payment restore only debt suspension, after checking all debt;
  cancellation, renewal, quota, manual suspension and archive do not change.
- Exact target/action/reason/key replays are inert. Conflicting key reuse refuses;
  audit names the real reversal entry or null for a zero-money operation.
  `outstandingMinor` describes debt; overage admission additionally counts holds.
  A new explicit request for an already-financially-void source may clear an
  old writer's stale debt marker, with a new audit and no ledger entry; exact
  audited retries still return before that action.

This adds no payout, repayment or daily recognition feature. Validation and
remaining release limits are recorded in
[the review evidence](roadmap/operator-launch/evidence/statement-review-finance.md).

## 2026-11-08 — Resolve one dispute in the ledger transaction (#826)

The former three committed phases released authority and the dispute lock before
posting money, then acquired a new transaction for terminal state and audit. A
second decision or failure could therefore leave a won/open dispute with a lost
chargeback entry. Keep the business and live session locks until the complete
resolution commits. Read immutable coordinates first, then use subscription →
plan → dispute → journal → ledger → statement order; never acquire the order row
after these owners. Non-plan legacy allocation also uses `append_in`, under the
credit lock, so audit failure rolls back its entry and idempotency claim.

Retained entries must use the exact derived key/id, sequenced settled USD legs,
expected allocation shape and amount. Terminal dispute pointers and immutable
audit identity/allocation must agree. An orphaned old middle-phase effect refuses
with explicit source review; no generic retry guesses a compensation or rewrites
terminal history. Keys bind actor, target, outcome and trimmed optional reason.
An unchanged key is inert; a different terminal outcome conflicts; an unused key
requesting the standing terminal outcome returns that row without another audit.

This boundary does not repair the separate legacy non-plan incoming-cash sign,
drawn-debt clamp or partial-event amount problem. #829 owns that original-source
cash conservation and historical correction before #777 repayment launches.
Stop and drain old writers during deployment: concurrent old binaries do not
participate in the new transaction boundary. Controlled lock and audit-failure
fixtures prove the new code's boundary, not mixed-version write safety.


## 2026-11-08 — Preserve non-plan chargeback cash without inventing debt (#840)

Every newly resolved non-plan loss credits Cash for its complete authenticated
amount. A partial fiat dispute must convert exactly using the pinned order
amounts. Missing amounts, currencies, case identifiers, fractional credit-minor
results and incompatible duplicate cases are refused with durable inbox reasons.

The original same-customer confirmed order, immutable fulfillment and exact
`payment-order:<order_id>` sequenced entry must agree. Verify its content hash,
settled positive USD postings and absence of reversal. Only the original Wallet
credit can fund a take-back: subtract all subsequent settled Wallet debits and
current pending Wallet reservations. Later credits never replenish this source.
Bound it again by complete prior actual chargeback cash and the current loss.
Any refund history or unsupported previous chargeback forces the whole new loss
into `Equity:Adjustments` suspense. Its audit retains the source and review reason.
Repayment-origin and consumed funding also remain suspense: directly debiting
CreditLine would invent new-period draws and cannot reopen already-paid monthly
statements safely. No debt document, revenue or entitlement is changed.

A source advisory lock spans non-plan fulfillment grants and their final record.
Resolution takes the same lock before the dispute and ledger append locks.
New grants after a lost dispute are refused; new refund requests and approval
are fenced. Ledger append locking serializes source reads with spends and holds.
Bundle promo cancellation uses the same loss transaction; reserved releases burn
under the existing closed-lot policy. Plan ownership ordering stays unchanged.
Stop and drain old writers when deploying. This fixes forward losses only;
#841 owns non-plan refunds and #842 owns explicit legacy chargeback compensation.


The source lock is claimed with `pg_try_advisory_xact_lock`. A contested
resolution/refund/inbox attempt returns `PoolBusy` and drops its transaction; a
fulfillment retries off the pool for at most five seconds. A `Db`-shared semaphore
queues non-plan grants before they hold a pooled connection, with width
`max(1, (max_connections - 1) / 3)`. Existing independent grant paths can need the
source guard plus two connections. Pools below three refuse non-plan fulfillment
with `PoolBusy` before grant; the operator recommendation remains at least ten.
Clones share the semaphore; separately constructed `Db::from_pool` wrappers over
one pool do not, and callers must reuse/clone their handle. Independent processes
have separate pools and still coordinate through the nonblocking database lock.
Cancellation drops the permit and guard. This bounds this new source-lock flow;
it is not a general throughput or arbitrary concurrent workload guarantee.

## 2026-11-09 — V2 paid invoices and immutable daily accounting (#838)

New publications pin accounting policy 2. Existing sold versions, period authorizations and ordinary renewals retain policy 1; switching requires explicit new-version selection and payment at its supported boundary. Migration 0113 adds immutable policy, service interval, exact original fee and authorization snapshots plus append-only source-linked effects. It does not rewrite prior entries or stored exports.

V2 activation issues its paid journal and statement immediately. Its only money entry is Cash to UnearnedPlanFees. Invoice timing and earning timing are separate: each fully completed UTC service day releases cumulative `floor(net_fee * elapsed_microseconds / duration_microseconds)` minus prior net earned, with checked i128 intermediate arithmetic. Exact service endpoints handle partial days, month lengths and leap years. The final endpoint takes the exact remainder. Backlog books on the current date with a separate service date; close cannot accrue the fee again, even after void. A bounded hourly durable worker skips locked plans and advances past them.

Confirmed refunds reduce net consideration without changing service bounds, quota or renewal entitlement. After catch-up, the same cumulative rule on the reduced fee determines earned contra and the remaining original deferred reduction. Cash returns once; refunded/chargeback source limits and review suspense remain. Lost disputes leave service and revenue intact and move the original collection to receivable or consume original refund payable. Financial void reverses net earned and deferred into receivable/payable without moving Cash. All effects share plan, journal and ledger locking and commit atomically with their documents.

Reports distinguish net billed, earned, deferred movement, collected, receivable, cash refund and noncash discount. V2 filters select effect booking, service attribution or payment-cohort dates; a filtered deferred/receivable amount is a movement, not an ending balance. Recognition and cashless void retain the original payment cohort; actual refund/dispute cash is attributed to its booking date. V1 retains its historical posted-journal cohort. Confirmed refunds are counted once from source allocations/ledger cash, excluding matching journal credits. The service-clock usage ledger join now uses the actual `ledger_<tenant>` schema and tolerates customers without a created wallet.

Statement evidence commits the captured policy, original inputs and listed effects with their ledger entries; the verifier rejects missing referenced daily entries. Captured exports remain byte-stable after later days or adjustments. Actual delivery, completeness beyond the captured effects, and statutory accounting compliance remain outside the proof claim.


Independent review of #838 found that recapturing a statement at concurrent mint
or after a marker-only crash could bind a new manifest to an old deterministic
marker. The writer now commits immutable exact inputs first under the same unique scope
string; concurrent publishers reload the winner before appending. It checks
an existing marker's document digest. Failed export insertion leaves recoverable
inputs; prior unmatched orphan markers fail closed. This preserves old bundles
and avoids modifying the ledger engine while making the crash gap recoverable.

## 2026-11-09 — Retain actual non-plan refunds separately from approval (#841)

A confirmed provider return credits complete Cash once. The original paid order,
immutable fulfillment and verified sequenced funding entry determine how much
original Wallet funding is still retained; later credits cannot refill it. Held,
spent, repayment-origin or unproven source remains explicit Adjustments suspense,
without inventing receivables or reopening old CreditLine statements.

Migration 0115 retains immutable `nonplan_refund_effects`, including exact event,
source revision, cash/correction identities and hashes and original Cash booking
date. A nullable credit-note link supports an authenticated but unapproved cash
fact without fabricating approval or a human audit actor. Existing refund/note
terminal states and applied entry pointers remain immutable. Effects fence new
payout claims and unperformed grants and consume refundable bounds even when old
state says declined, failed or released. Current state cannot erase actual cash.

Old refund rows retain `cash_policy_version=0`; new atomic writers explicitly
record version 1. An old nonterminal row may hide a separately committed Cash
entry. Only a new version-1 row with no actual Cash effect can establish that
another pending partial refund has not yet consumed source. Stop and drain old
writers before deployment; mixed binaries do not share this safety boundary.

Historical recovery is an exact local owner/finance command with an explicit
trusted receipt, default preview and required source-revision pin on apply.
A retained old Cash entry is never repeated. Proven wrong-source Wallet is
restored with Adjustments/Wallet only, using the balance and pending reservations
immediately before the old return's sequence position. Unproven source cannot
justify restitution: conservative zero allocation is not proof of a wrong debit.
Preview writes nothing; apply commits compensation, effect, document projection
and audit together, retaining refused outcomes and binding exact request terms.

Verified source inspection checks the canonical bytes of every sequenced entry
before its boundary through the caller's borrowed connection. Inspection is
bounded to 20,000 entries; exceeding this bound retains fresh Cash in review
suspense and refuses historical Wallet restitution. This budget does not prove
that original funding has been spent. Effect parent rows are acquired before
source/refund/append locks so immediate foreign-key checks cannot invert a staff
business lock. This boundary includes single-connection callback and recovery
support, not a claim that every financial operation uses a single connection.

Successful non-plan fulfillment must await source-guard rollback before returning,
including a fulfilled replay. SQLx queues dropped transaction rollback, which let
an immediate refund fail with `PoolBusy` in CI 42525666821 and 42526621594. A
controlled regression pauses that cleanup and observes the PostgreSQL advisory
lock from another pool; it fails before this fix and passes with awaited release.
Worker refund claims treat source contention as no acquired lease, preserving
their bounded retry contract without consuming a provider attempt.

## 2026-11-09 — Skip current ledger DDL on independent wallet opens (#867)

The full CI run on `4fc5581a` exposed a production lock inversion: an independent
wallet open reapplied the ledger's trigger DDL while the gateway's borrowed
settlement transaction retained relation locks from its hold read. PostgreSQL
aborted the settlement's append with `40P01`. Retrying only the opener cannot
recover the losing settlement, and taking an append lock in the migrator alone
would still invert against a transaction that already read ledger relations.

The vendored PostgreSQL engine now owns a `ledger_meta.schema_revision` field.
The revision is BLAKE3 over a versioned domain, the extension DDL and the complete
canonical PostgreSQL schema bytes. Under the existing database-wide migration
lock, a matching revision checks ledger identity and returns without replacing
constraints, triggers or views. Missing, NULL or different revisions apply the
full schema. Identity validation happens before touching a foreign ledger.
Wallet account tightening and lazy data migrations retain their existing paths.

The schema SQL no longer opens or commits its own transaction. One SQLx-owned
transaction now owns the migration advisory lock, schema pin, complete DDL,
identity and revision stamp. Any failure, including the final stamp, rolls all
of them back. Cancellation releases the transaction lock through ordinary
connection cleanup. This supersedes the older session-lock/nested-BEGIN
migration mechanism recorded above; no extra pooled connection is required.

This is a record of applied schema bytes, not a certification of manual database
changes. A real revision transition requires stopping and draining every old
server and worker before starting the new version; the ordinary application
migration lock is not a live-writer fence. An old binary does not honor this
marker, and mixed-version rolling upgrade safety is not claimed. Old ledger
schemas without the field upgrade through the same atomic path.

## 2026-11-09 — Durable bounded provider acceptance guard (#869)

The launch exercise needs a run-wide external spending ceiling independent of
customer funding and gateway retries. A standalone local test guard owns the
real provider credential and reserves each fixed request in the original shared
journal before transmission. The application uses its existing explicitly
configured internal provider route and a separate synthetic token. No production
transport or public-address policy is weakened.

The guard supports only one current DeepSeek model, a fixed tiny prompt, whole/SSE
responses, explicit non-thinking mode and 32 output tokens. Peak rates and a
conservative serialized-input allowance are checked before each attempt. An
exclusive process lock plus atomic replacement/file-and-directory fsync retains
reservations across concurrency and interruption. Each scenario can attempt only
once; neither successful usage nor ambiguous failure replenishes the allowance.
The report labels this a controlled mediated path and identifies the actual
binary/SDK bytes. Existing direct-production DNS/provider matrix and final
candidate gates remain separate.


## 2026-11-09 — Bounded composed ledger evidence (#873)

A quota-funded semantic-cache hit returned 200 but failed settlement because the
v14 description's subscription and source-entry provenance exceeded the vendored
512-character limit. A standalone test on main reproduced the missing receipt;
removing provenance or shortening accepted model names would conceal the defect.

The vendored `Description` limit is now 16,384 Unicode scalar values, at most
65,536 UTF-8 bytes before JSON escaping. Construction and serde decoding share
that finite bound, so storage rehydration and native/WASM verification agree.
Control-character validation is unchanged. PostgreSQL and SQLite already use
TEXT without a length CHECK, so no schema mutation or migration is needed.
Settlement v14 fields and canonical encoding stay byte-for-byte unchanged; old
hashes and golden vectors remain valid. The larger bound accommodates known
composed metadata, including 200-byte model/alias names and delegated quota/cache
receipts; it does not promise that arbitrary policy output fits. The unbounded
policy count and returned reasons are the separate admission-budget issue #875.

Old 512-character binaries cannot decode longer newly written descriptions.
Upgrade the server and distributed/served verifier together. A schema-identical
rollback is unsafe after such entries exist unless the retained reader supports
the new bound; otherwise restore the pre-upgrade backup. No package publication
or candidate acceptance follows from this implementation.

## 2026-11-09 — Funded and free requests retain admission authority (#849)

A subscription covering the whole bound formerly skipped `hold_for_key`, so
manual/dunning/archive restrictions, current key constraints, mandate windows
and concurrency could be skipped with it. Explicit zero pricing also conflicted
with the positive-only watch schema. Preserve the money model and repair the
admission boundary:

- `Wallet::admit_gateway` commits quota, wallet share, current key/organization
  authority, watch and leased operation together. Rejected admission rolls all
  of these back. Both fresh upstream requests and cache hits use it.
- `open_holds.wallet_minor = 0` is an explicit admitted nonmonetary reservation;
  positive values name the monetary share, and `NULL` preserves legacy watches
  whose presence alone cannot certify admission. The priced bound may be zero.
  Concurrency counts ledger monetary holds plus explicit zero-wallet watches,
  including promo-funded holds once. A turn never counts in both sets.
- Money caps retain wallet, bonus and credit-line committed postings; quota is
  already paid entitlement, not a second budget charge. Compare existing spend
  plus wallet share to the current key/member/team/mandate cap. Exact cap plus
  zero is permitted; a cap already exceeded refuses. Promotional budget policy
  is unchanged. Plan-statement dunning remains overage-only; organization
  restrictions apply regardless of the funding mix.
- Atomic settlement/recovery records the existing typed balanced Adjustments
  marker for a zero-wallet turn, releases quota, writes usage, closes the
  operation and removes its watch together. It does not create wallet money or
  usage revenue. A missing legacy hold keeps its old cleanup behavior. Recovery
  retains known actor/mandate provenance but does not invent a lost mandate hash,
  provider report, cache source or sale commitment.
- Gateway operation identity includes the server request id. An independent
  identical body owns a separate watch; the HTTP `Idempotency-Key` claim resolves
  explicit replay before admission and continues to return its original answer.
- Lock key advisory, organization admission and the operation's organization FK share before subscription lifecycle,
  then ledger append before key/member/team rows. Settlement takes the same key
  advisory before subscription/append and its usage forecast. Transactional
  calendar, anchor, immutable hold and legacy closing receipt reads use the
  borrowed connection; lazy gateway rollover reuses its initialized wallet.
  This avoids holding one pool slot while waiting for another behind ledger locks.
- Route/model authority is compared again before commit. A changed permission
  snapshot refuses for retry; the admitted turn's alias, route and price remain
  pinned rather than being re-priced during execution.

Stop and drain old writers before upgrading; an old process does not participate
in the explicit zero-wallet reservation protocol. This is development correctness
work, not provider/browser/PSP or final candidate acceptance. The separate lazy
cancellation defect is tracked in #854 and must close before the customer journey.

## 2026-11-09 Independent financial source snapshots (#848)

A reconciliation cannot use its projection as both assertion and observation.
Actual plan/refund totals now come from canonical verified ledger entries in one
repeatable-read snapshot. Original raw keys, derived IDs, official sequence
positions, registered accounts and source ownership are checked separately;
posting hashes do not bind those database relationships or prove service delivery.

V1 retains a coherent original-posting journal cohort, including later net
corrections; v2 compares signed movements under explicit booking/service/payment
clocks. The dashboard's legacy statement-issued-date receivable filter remains a
different cohort. Unchecked usage/entitlement/funding/cost fields are absent from
actual totals and listed as exclusions. Payable and suspense are source evidence,
not income or receivables. Missing sources report incomplete coverage, and
independently balanced document drift still produces exact amount exceptions.

Zero-money recognition has no official sequence position. Neighboring sequenced
service days and retained correction service boundaries can establish its order;
transaction-start time, booking date and PostgreSQL tuple transaction IDs cannot
establish an exact historical commit order. An unprovable refund/void boundary
excludes that period's schedule and marks incomplete coverage. This preserves the
verified monetary comparison without claiming an independently reconstructed
allocation whose source chronology was never retained. Statement payment lines
are compared by signed amount and exact original allocation identity as well as
their aggregate, so equal totals cannot conceal a changed order or correction.

Each completed attempt is immutable. The stable run is only a latest-result
pointer; exact keyed manual retries return their original attempt. A nonblocking
transaction lock prevents competing identical captures, with current personal
capabilities checked on every request. Capture and publication use one connection;
storage/size failures roll back and preserve earlier evidence. Hard row/entry/
byte/interval bounds refuse oversized attempts rather than silently truncating.
The migration labels retained old projection-only evidence as legacy rather than
claiming it was independently verified.
