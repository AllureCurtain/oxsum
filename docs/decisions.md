# Technical decisions

New decisions go on top. Overturned decisions are never deleted; mark them "Superseded" and name the decision that replaces it.

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

- Status: Adopted. Replaces the Topcoat part of "full-stack Rust: axum + Topcoat" below; the axum, doubleentry and WASM-verification conclusions stand.
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
