# Product specification

The Business sold-plan editor preserves exact credit amounts with up to six
decimal places. Invalid precision or unsafe integer amounts cannot publish an
immutable plan version; explicit zero and blank unmetered quota remain distinct.

## Self-hosted release target

The accepted [self-hosted release contract](roadmap/self-hosted-v1/README.md) (#591) supersedes the portfolio-only product direction. Operators own providers, sale prices and merchant collection; the oxsum project takes no payment or cut. Release one supports one business with many individual/team customer accounts. Existing organizations remain customers; business staff authority is separate. The contract specifies planned behavior, and [status](roadmap/self-hosted-v1/STATUS.md) distinguishes implementation and external validation. Shipped behavior below changes only as the corresponding issues integrate.


Current direction: one self-hosted Business serves personal/team customers through
the portal and embedded AI application integrations. The November 8
[operator-launch program](https://github.com/AllureCurtain/oxsum/issues/725)
retains the full product target, including N21's embedded lifecycle and N11's
versioned daily revenue policy. These planned extensions become shipped only
when their implementation integrates. Historical decisions remain in
[decisions.md](decisions.md); current software, external certification and
release acceptance are distinct statuses.

This document records product behavior: who can do what, and how every situation is billed. Technical implementation lives in architecture.md, technology rationale in decisions.md, progress in TODO.md.

Composed settlement evidence (#873) retains subscription, cache and other
provenance together under the ledger's 16,384-Unicode-scalar description bound.
The current native and browser verifiers read that same bound; older
512-character readers require an upgrade for longer new entries. This repairs
the quota-funded cache settlement failure without dropping evidence. Arbitrary
configured guardrail output still requires the admission budget tracked by #875.

Every page and flow below carries a status: **shipped** means this repository does it today, **planned** means the text describes the vision and names the issue that will build it. A planned page or flow is not a smaller feature that happens to be missing; it is documented behavior that no code implements. The shipped dashboard is the React app under `web/` (issue #234), served at `/app`: every member page, the admin console and chat live there as `web/src/pages/*.tsx`, and the REST and gateway behavior they call is in `crates/server`. The server-rendered Topcoat pages retired to it (issue #256) — every legacy `/dashboard/*`, auth and `/admin` path answers `301` to its `/app` twin, so the page names below read `/app/<name>` even where the text still says `/dashboard/<name>` for continuity with older notes. Issue #61 is the audit that added these statuses; the resolutions are recorded in docs/decisions.md. Two public pages stay server-rendered on purpose, in `crates/web`, outside the React bundle. `/verify` is a plain form: an in-browser check can run the served verifier (the wasm build of `oxsum_verify`, issue #284), and the form's server-side POST keeps it working where that bundle is not shipped. `/bill/{token}` renders a shared usage report from the capability token alone (issue #302); with JavaScript it can run the same verifier over the bundle the page embeds (issue #305), and without JavaScript the figures and operator key still render.

## Operator launch target (accepted 2026-11-08)

The next stage is [operator launch](roadmap/operator-launch/CONTRACT.md), tracked by [#725](https://github.com/AllureCurtain/oxsum/issues/725). Both the built-in business/customer portals and scoped embedded application adoption are required. The initial journey is a single owner, invite-only customers, one model, prepaid pay-as-you-go and Hyperswitch; advertised plans, teams, postpaid, refunds, disputes and evidence remain required. New plan accounting will use a versioned daily service-period policy without changing historical postings or active legacy policy. These are implementation targets, not claims that their UI or acceptance is already complete. Exact status and external gates live in [EXECUTION.md](roadmap/operator-launch/EXECUTION.md).

## One-liner

Self-hosted AI-service billing with a Business portal, customer accounts and
scoped integration APIs. Calls reserve against authorized funds or entitlement
and settle the recorded usage under pinned terms; customers can retain and
independently verify evidence within its declared scope.

Deployment operators can read a sanitized diagnostic snapshot under the operator token (#819): build/schema identity, configuration presence and aggregate provider/merchant/recovery queues. It contains no customer identities, secret values or payloads and performs no external probes or repairs. Counts do not certify connectivity, settlement conservation or release readiness. See [diagnostics and recovery](operator-diagnostics.md).

## First operator and customer journey

1. Deploy the binary and PostgreSQL, then use the explicit setup token to
   initialize the owner. The first public registrant never becomes the owner.
2. Sign in at `/app` and choose Business context (`/app/business`). Configure a
   serving route, customer sale terms and the selected funding/admission path.
   Use the documented Business APIs where an editor is still pending; the
   deployment token remains a technical bootstrap/operations credential.
3. Invite a customer or deliberately enable open registration. The customer
   obtains a permitted key and authorized funding or entitlement, then calls
   the gateway. An entitled request can work with zero purchased wallet balance.
4. Inspect the settled request and immediately retain its proof and expected
   hash. Run the WASM verifier or packed SDK, including a tampered-copy rejection.
5. For a closed UTC period, export the appropriate report, statement or evidence
   bundle. These formats verify different claims; see [proofs.md](proofs.md).

The `/app/getting-started` checklist uses personal customer authority (#901):
live visible keys, permitted chat models, effective customer sale terms and a
sample funding snapshot. Actual first-call receipt evidence completes its final
two steps. It requires neither an operator token, positive wallet balance nor a
report link. Complete candidate journey acceptance remains a separate gate.

The business setup page now reads `/api/v1/business/readiness` under the current
personal business session (#843). Its five facts cover initialized identity,
configured chat supply with an available decryptable credential, a matching
active default sale book, explicitly selected enabled checkout with compatible
collection/runtime configuration, and a customer admission path. Extra staff,
sold plans and an existing customer are optional for this solo pay-as-you-go
setup. Restricted facts are identified and never counted as complete. These
configuration facts do not certify provider calls, payments, callbacks or mail
delivery. Customer-specific model/key/funding readiness and first bill evidence
remain the separate N09 customer unit.

## Initial owner browser setup (shipped, #748)

`/app/setup` accepts the deployment-minted email-bound token through the existing new-user registration or personal-session consent endpoint. It shows the current identity, requires explicit consent, preserves draft fields on errors, and keeps the secret out of URLs and persistent browser storage. Existing users sign in through the normal password/TOTP flow and return to setup. A signed-in staff member can recover a lost acceptance response by opening the console; no token replay can initialize an already-owned business. Registration stays invite-only. Setup does not claim that provider, price, merchant or customer configuration is already complete.

## Roles

| Role | Who | Can do | Status |
| --- | --- | --- | --- |
| Business owner/staff | Explicitly initialized owner and appointed staff | Current Business capabilities, separated from customer membership | `/app/business` with a user session and explicit Business context; editor and invitation extensions remain tracked by #725 |
| Deployment operator (legacy admin) | Whoever deploys oxsum | Manage channels and prices, top up or adjust any organization, suspend or reinstate an organization's spend, assign tier profiles and grant pricing discounts, inspect retired service credential history, view all anomalous requests, view the charged-versus-upstream margin, run monthly closings, audit every admin write | Channels and prices are shipped — the `/api/v1/admin` endpoints and the `/admin/channels` page, under the operator token; so are adjusting any organization's balance and the signup bonus, organization suspension (#162), the tier/discount endpoints (#158), the audit log (#160) and legacy service-credential metadata (#172; issuance retired by #638) |
| Organization owner | Organization creator | Everything inside the organization, including transferring ownership | Shipped: keys, balance, the ledger, and membership management with the ownership transfer |
| Organization admin | Appointed by the owner | Invite and remove members, manage all keys, view the organization's full bill history | Shipped: managing all keys, inviting by link with explicit recipient consent, removing and re-roling members (but not the owner's own membership); the bill history ships as the bills page — the organization's transactions, exportable as CSV or JSON — which owners and admins read unfiltered |
| Organization member | People an owner or admin added | Create and revoke their own keys, view organization balance and their own request records | Their own keys, the balance and their own request records are shipped (the requests page, #55, shows the turns their own keys paid for); the bills page shows a member the transactions their own keys paid plus the organization's shared history — top-ups and adjustments carry no key, so every member sees them |
| API caller | Programs holding a key | Call the gateway, spending the key's organization balance | Shipped |

The deployment token is not a customer role. Business staff use current
capability checks and their own sessions; being a customer organization owner
never confers Business authority. Legacy "platform admin" references below name
the deployment-token compatibility API, not the recommended portal identity.

Customer authority is distinct from the spending credential. Consumption keys read only their own key-attributed records and safe catalog/head metadata; personal sessions manage keys, members, checkout and account context. Member sessions see their own keys' history plus shared unattributed funding/adjustments, including through raw entries, proofs, exports and billing WebSocket. Account-wide statement/report/day-anchor documents remain owner/admin-only even through generic evidence reads. Customer statements, reports/share links, webhooks and experiments require owner/admin sessions. See [the capability inventory](customer-capabilities.md).

## Accounts and organizations

### Registration and login (shipped)

- Login is email plus password; passwords are stored with argon2. When the deployment configures a mailer (`OXSUM_SMTP_URL`, `OXSUM_MAIL_FROM` and `OXSUM_PUBLIC_URL` together — issue #150), registration and invitation redemption mail a verification link, the dashboard reminds an unverified user and offers a resend, and a forgotten password resets itself through the mailed link at `/forgot-password`. A deployment without a mailer simply sends nothing: verification reads `verificationSent: false`, the forgot answer stays indistinguishable, and the admin reset remains the recovery path — `POST /api/v1/admin/users/{userId}/password-reset` (issue #91, shipped) revokes every session the user holds and answers how many died with the old credential. The self-service reset through the mail revokes the same way.
- A login mints a session: the response names the user, the organization and the role, and a cookie (`oxsum_session`, `HttpOnly`, `SameSite=Lax`, `Path=/`, plus `Secure` when the deployment is behind TLS) carries it from then on. The session acts as the user's organization in their role, expires thirty days after login, and rechecks current membership for customer access. Business context independently rechecks staff authority. Logging out revokes it. An unknown email and a wrong password answer the same 401 with the same message, so neither reveals whether an account exists.
- Sessions and consumption keys resolve the same customer identity with different capabilities: scoped metadata/evidence reads accept either; personal management and checkout require a session, and account-wide reports/statements/webhooks require owner/admin. Explicit machine credential headers never inherit cookie authority. The gateway (`/v1` and `/v1beta`) accepts consumption keys only.
- The technical deployment operator holds `OXSUM_ADMIN_TOKEN`, which authorizes `/api/v1/admin` for compatibility operations and bootstrap. This does not replace the initialized Business owner/staff session model. The `/admin` pages open with the same token: channels and prices, organizations, in-flight requests, anomalies, reconciliation and closing are all shipped. The same token also guards `GET /metrics`, the Prometheus scrape of the process's own telemetry. Not "the first registrant becomes admin" — on a public instance, whoever registers first would own it.
- Registration mode is selected by the initialized business owner (`POST /api/v1/business/registration`); `OXSUM_SIGNUP` is parsed only for legacy configuration compatibility:
  - `invite` (default): registration only through invitation links — `POST /api/v1/auth/register` is refused, and an owner or admin's link is the way in
  - `open`: anyone can register, and invitation links work too
  - Both modes ship, and the links ship with them.
  - The two public account-creation endpoints can sit behind a Cloudflare Turnstile check (issue #154): with `OXSUM_TURNSTILE_SITE_KEY`/`OXSUM_TURNSTILE_SECRET_KEY` set, the register page draws the widget and the server verifies its answer before any account work — refused answers are `FORBIDDEN`, an unreachable verifier `SERVICE_UNAVAILABLE`. Unconfigured, both endpoints run unchecked.
- Registration takes a password of at least 12 characters. Length is the only rule: composition classes and expiry cost users more than they buy.
- Passwords are hashed before the database transaction opens, so a slow argon2 hash never holds a connection.
- GitHub OAuth login ships (issue #152) as a second credential beside the password, enabled only when the deployment configures the app pair (`OXSUM_GITHUB_CLIENT_ID`, `OXSUM_GITHUB_CLIENT_SECRET` and `OXSUM_PUBLIC_URL` together): the login page draws "Continue with GitHub", the flow mints its single-use CSRF state, and the callback resolves the provider-verified email — a linked `oauth_accounts` identity first, then an email match that links it, otherwise a registration when signup is `open` (an `invite` deployment refuses new accounts there but still logs existing ones in). The account the callback creates has no usable password — the reset flow is how one gets set — and its email lands verified because the provider already did that work. A deployment without the pair draws no button and answers the endpoints 404.
- Enterprise OIDC login ships (issue #504) as a provider registry beside GitHub: `OXSUM_OIDC_PROVIDERS` names each IdP (`{name, issuer, clientId, clientSecret}`), the login page draws a button per entry off `auth/methods`' `oidcProviders`, and `GET /api/v1/auth/oauth/oidc:{name}` runs the authorization-code + PKCE flow — discovery and JWKS off the provider's `.well-known`, the `id_token` verified RS256 with `iss`/`aud`/`exp`/`nonce` checked, the email must be verified. Resolution is the same link → verified-email match → register the GitHub leg runs, on `oauth_accounts.provider = oidc:{name}` so each issuer's `sub` namespace stays distinct.
- TOTP second factor ships (issue #504) as a per-user opt-in: the session user enrolls an authenticator secret (`POST /api/v1/auth/totp/enroll` → a base32 secret and otpauth URL, sealed at rest under `OXSUM_SECRET_KEY`), confirms it on a live code (`/confirm` mints ten single-use `xxxx-xxxx` backup codes, shown once and stored as hashes), and from then on the password login withholds the session behind a challenge — `POST /api/v1/auth/totp` trades `{challengeToken, code}` for it, the challenge single-use, ten-minute-lived and five-attempt-capped, a backup code consumed on use. `/disable` removes the factor on a live code, so disabling is itself second-factored.

### Organizations

- Shipped: signup creates the user, the personal organization, the owner membership and the first API key in one transaction, and returns that key's secret once.
- Shipped: the organization's ledger is not created at signup — it is created on first use, so an account that never spends costs nothing and a failed ledger migration cannot leave a half-registered user (docs/decisions.md).
- Shipped: the tenant id of an organization is its own UUID without dashes, so a ledger schema is `ledger_<32 hex characters>` and nothing has to be chosen, probed or made unique.
- Shipped: a second person makes the organization a team. When the first membership other than the creator's is added, the organization flips from `personal` to `team`, one way; the ledger does not migrate, because it belongs to the organization, not to the person.
- Shipped: a user belongs to several organizations. `POST /api/v1/orgs` creates a `team` organization — its own ledger, its own keys, its own balance — with the user as its owner, and `GET /api/v1/orgs` lists every organization they are a member of, oldest membership first, with the role they hold.
- Shipped: the acting organization is the session's choice, switched from the dashboard's top-right corner or `POST /api/v1/session/organization`. The choice lives on the session row, so it survives reloads and carries every request the cookie makes: the overview, keys, members, bills, requests and the transaction log all scope to it. Switching names a membership, nothing else — an organization the user is not a member of answers `NOT_FOUND`, so the endpoint does not say whether it exists. An API key cannot list, create or switch: a key belongs to exactly one organization.
- Customer-team invitations require consent (#634). The owner/admin creates a seven-day link with an idempotency key and optional intended email. The Members page binds the recipient email and optionally requests mail delivery. A new user registers through the link as a member; an existing user signs in and explicitly accepts. The link grants no staff role or money. A new user's first key is shown once; an existing user's keys and session context stay unchanged. Unknown, wrong-email, revoked and expired links are `NOT_FOUND`. Exact creation replay returns the same invitation ID with no secret or repeated mail. Acceptance replay returns the historical receipt without restoring later removed membership. Owners/admins may revoke unaccepted links; accepted membership is managed separately.
- Business staff invitation previews identify the inviting business and exact
  staff role, with the documented nil customer UUID. The browser labels staff
  consent and its result separately from customer membership. Staff-only
  registration creates no customer organization, API key or funds. A live
  preview is required before the browser enables acceptance.
- Shipped: an owner cannot leave directly; ownership must transfer first. There is no leave action at all — removing yourself is the owner removing a member, and for the organization's last owner that is refused. See "Membership management" below.
- Shipped: account deletion by pseudonymization (issue #202). `DELETE /api/v1/users/me` — session-only, password-confirmed — erases the identity while the books keep their history: the user row's email is tombstoned to `deleted-<id>@deleted.invalid`, the password hash is blanked so no login can ever succeed, every session and every key the user minted dies, and their memberships are removed. An account that is an organization's last owner *with other members still in it* is refused — transfer the seat first; a personal organization whose only member was the account is tombstoned (renamed, suspended) rather than deleted, because the ledger under its tenant id cannot be orphaned. The attribution the account's keys wrote is pseudonymized — `usage_records.end_user`/`tags` and the `open_holds` watch rows are nulled — which is exactly what the mutable store exists for: the ledger is never touched, settlement descriptions carry no identity, and a deleted user's bills verify exactly as before. An OAuth-registered account carries a password nobody knows and must set one through the reset flow before it can confirm the act. v1 does not support deleting organizations.

### Membership management (shipped)

An organization is a set of memberships, each one an account plus a role. Four actions change that set, from the members page or from `/api/v1/org/members` and `/api/v1/org/ownership`:

- **Invite a member**: issue a link and let the intended person consent, either by registration or authenticated acceptance. `POST /api/v1/org/members` is retired and returns 403. A team invitation grants only `member`; role changes follow separately.
- **Remove a member**: the membership goes, the account and its own personal organization stay. Naming someone who is not a member is a 404, so removing twice cannot look like it worked.
- **Change a role**: between `admin` and `member`. Promoting to `owner` is not a request the API can express — ownership is transferred, never assigned, so an organization always has exactly one owner.
- **Set a member budget**: the same `PATCH` that changes a role also takes `budgetLimitMinor` — a cap on what the member may commit across every API key they minted: their settled charges plus their outstanding holds, read from the ledger itself. Clearing it is an explicit `null`; a member with no cap is unlimited. Removing the member uncaps the keys they leave behind — the cap hangs on the membership row — and a key minted by a key (no person acting) carries none.
- **Group members under a team budget** (issue #440): `/api/v1/org/teams` manages named subgroups of members, each with its own `budgetLimitMinor`. Assigning a member (`PATCH /api/v1/org/members/{userId}` with `teamId`) moves their keys under the team's cap for their *next* hold — the budget hierarchy's middle layer between member and organization credit line. Committed spend follows the roster: the money a member's keys already spent moves with them, so the cap a reassignment surfaces is the team's figure for its members, not a member's own history. Dissolving a team unassigns its members — the seats keep their own budgets; the team cap simply stops applying. Deletion and reassignment serialize with admission: an earlier reservation remains valid, and a later admission reads the committed roster and its applicable cap (#859).
- **Transfer ownership**: the target becomes the owner and the acting owner becomes an admin, in one transaction. The owner seat is never empty and never doubled.

Who may do what, and the gaps that are closed on purpose:

| Actor | Add, remove, change a role | Transfer ownership |
| --- | --- | --- |
| Owner | Yes, including on other admins | Yes — to any other member; to themselves is a conflict |
| Admin | Yes, except on the owner's own membership (403) | No (403) |
| Member | No (403): a member may read the list, not change it | No (403) |
| API key | No (403), even the owner's own key: a key is not a person and names no role | No (403) |

- The last owner cannot be removed or demoted: that would leave an organization nobody owns. The answer is a 409 saying ownership has to be transferred first. In practice the only reachable case is the sole owner acting on themselves, since admins may not touch an owner at all.
- Every new endpoint checks the acting role in the server, and so does the page: no control is rendered that the server would refuse, and a refusal is shown in the server's own words rather than silently swallowed. (docs/decisions.md, "membership management is a person's action".)


## Where credit comes from

Every gateway admission, including a cache hit, quota-funded turn or explicit
zero price, enforces current organization/key/mandate authority and a durable
concurrency reservation (#849). Quota, wallet share, watch and operation commit
together; a refused request leaks no quota or money. Normal settlement and crash
recovery release the reservation atomically with its usage and evidence. A free
turn draws no paid quota and records a zero-cost receipt. Monetary caps retain
their wallet/bonus/credit-line basis: quota is not counted twice, an exact cap
admits a zero-wallet turn and a cap already exceeded refuses it. Organization
manual/dunning/archive restrictions block every funding path; plan-statement
dunning blocks only wallet overage. An admitted turn keeps its pinned sale terms.

Credit comes from the verified payment and operator-authorized sources below. Customer credentials cannot assert that money arrived.

| Source | Who | Notes |
| --- | --- | --- |
| Manual receipt | Deployment administrator or business staff | **Shipped** (#624, extended #607). `POST /api/v1/admin/organizations/{organizationId}/manual-receipts` requires a positive amount, idempotency key, unique payment reference and reason. Authorization/audit commits before crediting purchased funds; identical retries recover safely. The staff path `POST /api/v1/business/orders` (personal session + `FundsWrite`) records the receipt as a `source: manual` `walletTopup` payment order — the fiat amount rates to credits at the persisted Business collection rate, `evidence.reference` is unique per business, and the same `fulfill_payment_order` path a webhook-paid order takes credits the wallet exactly once, so the manual top-up appears in the customer's own order history and the reconciliation counts. Customer `POST /api/v1/topups` is retired and returns 403. Promotional grants remain adjustments. |
| Signup bonus | Automatic | **Shipped** (issue #60). `OXSUM_SIGNUP_BONUS_MINOR`, default 0: self-registration credits the new organization's bonus pool with the amount, booked as an adjustment carrying "signup bonus" as its reason — an invitation's redeem joins an existing organization and grants nothing. At 0, registration still writes no ledger entry. |
| Adjustment | Platform admin | **Shipped** (issue #60). `POST /api/v1/admin/organizations/{organizationId}/adjustments`, and an "Adjust" form on each row of `/admin/organizations`. Positive grants, negative deducts; the reason is required and becomes the entry's description, covered by its proof. A grant lands in the bonus pool; a deduction draws the bonus pool first and only spills into purchased credit once granted money is gone — the platform takes back what it gave before touching what the user paid for. A deduction deeper than the combined balance is the ledger's own no-overdraft refusal — `402 INSUFFICIENT_FUNDS`. |
| Redemption code | Operator mints; customer personal session redeems | **Shipped** (issue #118, decision D2). `POST /api/v1/admin/redemption-codes` mints a batch under the operator token — `oxr-` plus 32 random bytes, only the SHA-256 is stored, so the mint answer is the only place the codes are readable. The holder redeems through `POST /api/v1/redemptions` and the amount lands in the purchased pool: the code's row is claimed atomically (`FOR UPDATE`), the deposit and the ledger credit follow under the fixed idempotency key `redemption:<code_id>`, so a retried or concurrent redeem of one code credits once. Unknown, spent and expired codes are all the same `NOT_FOUND`. On-chain rails join the same `deposits` table later; a top-up is the `manual` rail of it already. |
| Stripe checkout | Customer personal session | **Shipped** (issue #201) on deployments that configure `OXSUM_STRIPE_SECRET_KEY`, `OXSUM_STRIPE_WEBHOOK_SECRET` and `OXSUM_PUBLIC_URL` together. `POST /api/v1/topups/stripe` opens a `pending` deposit keyed on the caller's `idempotencyKey`, creates a Stripe Checkout Session for the amount and answers `{checkoutUrl, depositId}` — a replayed key answers the stored URL, never a second session. The user's card payment happens on Stripe's hosted page; the signed `checkout.session.completed` webhook at `POST /api/v1/webhooks/stripe` confirms the deposit and credits the purchased pool under the fixed ledger key `stripe:<session_id>`, so a replayed or raced delivery credits once ever, and `checkout.session.expired` marks the deposit `expired`. The signature is verified before anything is read — a bad or missing `Stripe-Signature` is 401 — and a verified event is always answered 200, since a non-2xx is Stripe's retry signal. An unconfigured deployment refuses cleanly: the intent 503s, the webhook 404s. |
| Airwallex payment page | Customer personal session | **Shipped** (issue #285) on deployments that configure `OXSUM_AIRWALLEX_CLIENT_ID`, `OXSUM_AIRWALLEX_API_KEY`, `OXSUM_AIRWALLEX_WEBHOOK_SECRET` and `OXSUM_PUBLIC_URL` together. `POST /api/v1/topups/airwallex` opens a `pending` deposit keyed on the caller's `idempotencyKey`, creates a PaymentIntent (logging the credential pair in for a bearer token) and answers `{checkoutUrl, intentId, clientSecret, depositId}` — a replayed key answers the stored URL and secret, never a second intent. The user's payment happens on Airwallex's hosted page; the signed `payment_intent.succeeded` webhook at `POST /api/v1/webhooks/airwallex` confirms the deposit and credits the purchased pool under the fixed ledger key `airwallex:<intent_id>`, so a replayed or raced delivery credits once ever, and `payment_intent.cancelled`/`payment_intent.expired` mark the deposit `expired`. The signature is verified before anything is read — a bad or missing `x-timestamp`/`x-signature` pair is 401 — and a verified event is always answered 200, since a non-2xx is Airwallex's retry signal. An unconfigured deployment refuses cleanly: the intent 503s, the webhook 404s. |
| Payment order | Customer personal session | **Shipped** (issue #606) on a deployment that bootstraps an explicit merchant (`OXSUM_<RAIL>_MERCHANT_*` / `OXSUM_HYPERSWITCH_*`): `POST /api/v1/orders` pins an immutable order — `purpose` (`walletTopup`/`bundle`/`planPeriod`/`invoiceRepayment`), `purposeRef`, the quoted `displayAmountMinor`/`displayCurrency` and the `idempotencyKey` — before any provider call, takes bundle amounts from the published offer snapshot, pins the exact sold plan-period fee from the customer's quote and rounds only that fee's fiat quote upward to the smallest supported currency unit; other purposes derive the ledger `amountMinor` from the quoted fiat amount at the persisted Business collection rate (never a client-supplied rate), and answers `{order, providerPaymentId, clientSecret, checkoutUrl, publishableKey, sdkUrl}` — a checkout to complete, never a paid order. A replayed `idempotencyKey` returns the standing order and the same checkout handles, so a retry never issues a second charge; changed terms under a reused key are `409`. `paid` is established only by the signed versioned webhook through `merchant_event_inbox` — the return page and every read surface can never mark paid — and a paid-but-mismatched event parks in `needsAttention`, never auto-crediting. `GET /api/v1/orders{,/{orderId},/{orderId}/fulfillment}` reads the caller's own orders — newest first, filterable by `purpose` and `status` (issue #607) — and the honest `paid`-but-unfulfilled mid-state the recovery sweep drains; `GET /api/v1/orders/{orderId}/checkout-session` is the live checkout read a returned or refreshed browser polls (the stored handles plus the lifecycle truth, and a still-`created` order re-issues the provider call under its own UUID). `GET /api/v1/business/orders` is the staff's cross-organization read under `ReportsRead`; `GET /api/v1/business/payment-reconciliation` reads the operator's paid/fulfilled/`needsAttention` counts and the paid-unfulfilled backlog. The dashboard's `/app/orders` pages list the history, mount the provider checkout per rail and poll the fulfillment truth — never trusting the return URL. Fulfillment is exhaustive on purpose — a top-up credits wallet cash, a bundle posts purchased credit plus a separate promo lot, a plan period activates quota never wallet cash, an invoice repayment repays a drawn receivable only — and idempotent under `payment-order:{orderId}`, so a crash and a raced sweep credit once ever. |
| Hyperswitch switch | Customer personal session | **Shipped** (issues #319, #320) on deployments that configure `OXSUM_HYPERSWITCH_API_KEY`, `OXSUM_HYPERSWITCH_WEBHOOK_SECRET`, `OXSUM_HYPERSWITCH_PUBLISHABLE_KEY` and `OXSUM_PUBLIC_URL` together. `POST /api/v1/topups/hyperswitch` opens a `pending` deposit keyed on the caller's `idempotencyKey`, creates a switch payment (automatic capture; confirmation is the browser SDK's job) and answers `{paymentId, clientSecret, publishableKey, depositId, sdkUrl}` — a replayed key answers the stored handles, never a second payment. The dashboard's Hyperswitch top-up tab loads `HyperLoader.js` from `sdkUrl` (`OXSUM_HYPERSWITCH_SDK_URL`) and mounts Unified Checkout against `clientSecret`; card data never touches oxsum, and a confirm or cancel both redirect back to the page, which watches the balance until the webhook credits it — the only third-party script on the dashboard lives on that one tab. The payer's browser confirms through the switch's own SDK; the HMAC-signed `payment_succeeded` webhook at `POST /api/v1/webhooks/hyperswitch` confirms the deposit and credits the purchased pool under the fixed ledger key `hyperswitch:<payment_id>`, so a replayed or raced delivery credits once ever, and `payment_cancelled`/`payment_expired` mark it `expired`. Every verified event is recorded in `oxsum.payment_events` under its `event_id` before it acts, so a replay is acknowledged and credits nothing. The signature is verified before anything is read — a bad or missing `X-Webhook-Signature-512` is 401 — and a verified event is always answered 200, since a non-2xx is the switch's retry signal. An unconfigured deployment refuses cleanly: the intent 503s, the webhook 404s. |
| Solana top-up | Customer personal session | **Shipped** (issue #1117) on deployments that configure the `OXSUM_SOLANA_*` set together (`RPC_URL`, `PAY_TO`, `RATE_MICRO` required; `SPL_TOKEN_MINT`, `DECIMALS`, `COMMITMENT`, `INTENT_TTL_SECS`, `MIN_MINOR`, `MAX_MINOR`, `OPENS_PER_MINUTE` optional). `POST /api/v1/topups/solana` opens a `pending` deposit whose `payment_ref` is a freshly minted `reference` account key and answers `{reference, payTo, amountLamports, splToken, expiresAt}` — the terms the paying transaction must honor. The customer's wallet transfers that amount to `payTo` carrying `reference` as a read-only non-signer account (the Solana Pay recipe). `POST /api/v1/topups/solana/confirm` — or the 15-second `solana-deposits` poll — runs `getSignaturesForAddress(reference)` then `getTransaction(signature, jsonParsed)` through the rail's `ChainLookup` seam: the transaction must have succeeded, carry the marker, pay the configured recipient at least the asked lamports (or the configured SPL mint's base units; a worthless stand-in token is refused), after which the deposit confirms and credits the purchased pool under the fixed ledger key `solana:<signature>` — exactly once. **One on-chain signature credits at most one deposit ever, across all organizations** (`deposits_solana_tx_hash` is unique): a transaction carrying several markers pays once but does not spend twice. `OXSUM_SOLANA_DECIMALS` is the asset's base units per whole unit — required whenever `SPL_TOKEN_MINT` is set and verified against the mint's on-chain decimals at boot, so a 6-decimal mint can never silently price at SOL's 9. `OXSUM_SOLANA_COMMITMENT` defaults to `finalized` (mainnet-grade; set `confirmed` for a devnet loop). Expiry is judged on the paying transaction's on-chain `blockTime`, so a payment before `expiresAt` credits even when confirmed after; `MIN_MINOR`/`MAX_MINOR`/`OPENS_PER_MINUTE` bound the intent's open surface. An unconfigured deployment answers 503. |
| Published credit bundle | Current owner or finance publishes; customer selects a version | **Implemented** (#774). `/app/business#business/bundles` publishes immutable fiat price, purchased credit, promotional credit and optional validity. `/app/orders/new` selects the published version; no customer-supplied bonus can determine a grant. Retirement stops new orders while admitted orders retain their original terms. Promotion expires from the recorded paid instant, including delayed recovery; a confirmed partial or full refund cancels its remainder without cash reimbursement. Bundles must finish fulfillment before refund review. Legacy fulfilled grants and refund eligibility remain unchanged; legacy unfulfilled bundles without trusted terms require operator resolution. |
| Promo lot | Platform admin grants, bundle fulfillments post | **Shipped** (issue #609). `POST /api/v1/admin/organizations/{organizationId}/promo-lots` grants a lot — `{"amountMinor", "expiresAt"?, "reason"?, "idempotencyKey"}` — and `POST …/promo-lots/{lotId}/cancel` burns a lot's unspent remainder. A lot is granted credit with an expiry: the `oxsum.promo_lots` row and its `debit Adjustments / credit Equity:Promo` entry commit in one transaction under `(source, sourceRef)`, so a replayed grant answers the standing lot. Its money draws before every other pool — it is the balance that can expire — and a hold pins the exact lots that fund it in `oxsum.promo_draws`, soonest-expiring first. The `expire-lots` job closes due lots once a minute: the unreserved remainder burns to `Equity:PromoExpiry`, never back to refundable money — a release into a closed lot burns the same way, and a cancellation burns the unspent part rather than refunding it to cash. A `bundle` payment order's promo leg lands as a `source: "bundle_promo"` lot anchored on the fulfillment; the aggregate bonus pool of pre-promo ledgers migrated once into a never-expiring `legacy_migration` lot, net balances unchanged. The organization reads its own lots at `GET /api/v1/org/promo-lots` (`?includeClosed=` keeps burned rows). |
| Credit limit | Platform admin | **Shipped** (issue #122, decision D4). `PATCH /api/v1/admin/organizations/{organizationId}` sets the whole credit limit — a grant of spendable credit the organization may draw before paying. Once set, holds draw the line after granted and purchased credit run out, so `available` can carry the organization into debt; a top-up repays the drawn line before it adds purchased balance. Shrinking the limit below what is already drawn is refused; setting `0` once the line is repaid retires it. The grant is a ledger entry like every other money movement, so the dashboard shows the limit, the amount used and the available remainder, and the history carries it as an adjustment. |
| Subscription plan | Platform admin defines, binds; org may buy through Stripe | **Shipped** (issue #496). A plan is a fixed-period credit quota that lives outside the ledger — an entitlement, not a balance: `GET`/`PUT /api/v1/admin/subscription-plans{,/{name}}` defines it (`quotaMinor`, `resetPeriod` `daily`/`weekly`/`monthly`/`never`, `allowWalletOverflow`, `priceMinor`, `stripePriceId`, `sortOrder`), and `PUT`/`DELETE /api/v1/admin/organizations/{organizationId}/subscription` binds or cancels it. One active subscription per organization; binding a different plan closes the open period `upgrade` or `downgrade` (the plans' `sortOrder` decides which) and opens a new one. A gateway request draws the period's quota first and the wallet holds only the overflow; a plan with `allowWalletOverflow: false` refuses the request once the quota is exhausted — the same 402 an empty wallet gives. Quota resets on the plan's boundary: lazily on the next draw and eagerly on the hourly `subscription-rollovers` job; a `never` plan's counter never resets — it ends only by cancel. The org reads its binding and its period history (`GET /api/v1/org/subscription`, `GET /api/v1/org/subscription/periods`), and on a Stripe-configured deployment buys a plan through `POST /api/v1/org/subscription/checkout` — `invoice.paid` renews the period at the invoice's boundary, `customer.subscription.deleted` cancels the binding. A settled turn's v9 description names `{subscriptionId, periodId, drawnMinor, chargedMinor}`, so `/verify` proves which entitlement covered the charge the same way it proves the figure. |
| Sold plan | Business staff publishes; customer buys through a `planPeriod` order | **Shipped** (issue #610). The versioned catalogue a business sells from: `GET`/`POST /api/v1/business/plans` mints immutable versions (`priceMinor`, `quotaMinor` per period, `periodKind` `calendarMonth`/`anniversary`, opt-in `overageEnabled`, `sortOrder`), `POST /api/v1/business/plans/{id}/retire` stops new binds, and `GET /api/v1/plans{,/{id}}` is the on-sale read. `PUT /api/v1/business/customers/{id}/plan` binds a customer — `pendingPayment` for a priced version until its `planPeriod` order fulfills, active at once on an explicit `complimentaryGrant` for a free one; the customer reads its plan and authorized periods at `GET /api/v1/org/plan` and acts at `POST /api/v1/org/plan/cancel` and `…/change`. Every period runs only on authorization (`planPeriods`: the fulfilled order or the grant) — a timer never issues unpaid entitlement. `GET /api/v1/org/plan/payment-quote` quotes the caller's assigned sold version, exact fee and target period. New `planPeriod` orders require its `expectedPlanQuote` fingerprint, and activation rechecks a matching paid order. Manual renewal opens only at the current UTC boundary, then fulfillment opens the next period chained off the previous end; a payment failure at the boundary leaves the plan `pastDue`, the ended period's remainder still serving but no fresh quota. `cancelAtPeriodEnd` serves through the paid period then closes; `scheduledChange` swaps versions at the next boundary — never mid-period pro-rating. `anniversary` plans roll on the activation date's day-of-month, clamped into short months (Jan 31 → Feb 28 → Mar 31). Quota still meters through the `org_subscriptions` engine — the plan layer pins when its window may move. |
| Plan fee journal | V2 paid activation; v1 period close; customer reads its own, staff lists and voids | **Shipped** (issue #611). Each `planPeriod` bills an `invoice_journal` — `baseFee`, `discount` (applied credit notes, or a complimentary grant's write-off), `promoCredit` lines summing to `totalMinor` — walking `draft → posted → paid | void`. Wallet/credit-line settlement already records usage income and its payment or debt; a new close never adds it again. Retained historical `usageOverage` lines require the explicit correction tracked by #809. Legacy v1 bills in arrears: the `planPeriod` order prepays into `Liabilities:UnearnedPlanFees` at activation, the close recognizes `Income:PlanFees` against `Assets:Receivables` and reclassifies the prepay, and an unpaid remainder stands `posted` as debt. Renewal state and debt are separate: `pastDue` can mean a new period still lacks payment even when every older bill is paid. A fulfilled renewal order prepays its own period in full. Outstanding journals remain separate debts; explicit statement repayment is tracked by #777. The customer reads `GET /api/v1/org/plan/journals{,/{id}}`; staff lists cross-organization at `GET /api/v1/business/journals` (`state=posted` is the dunning work list) and voids under `FundsWrite` — reversing lines net the bill to zero and post the mirroring entry, history rewritten nowhere. A confirmed `planPeriod` refund records the full cash return once against its original deferred fee before close, recognized-fee contra after close, or refund payable after void. Post-close bill and applied payment decrease together, preserving other outstanding debt. Its immutable source allocation and any unallocated review suspense are retained; [legacy recovery](plan-refund-recovery.md) appends corrections. A lost dispute restores the receivable (`paid → posted`). Journal lines and posted terms are trigger-immutable — corrections always append. |
| Historical usage correction | Owner/finance local operation, exact journal only | **Implemented** (#809). Default preview verifies original accrual, period usage settlements, payment entries and statement sources. Apply appends Dr PlanFees / Cr original open Receivables and/or RefundPayable, with matching usage/payment-allocation lines and immutable audit. Cash and wallet funds never move; original entries remain. Combined void/refund/dispute or inconsistent history explicitly refuses. [Runbook](plan-usage-correction.md). |

| Plan statement | Issued beside every journal; customer reads its own, staff reviews past-due | **Shipped** (issue #612). Every journal issues a `plan_statement` in the same commit (v2 paid activation or legacy v1 close) — `planFee`/`usage`/`discount`/`refundCredit` lines summing to `totalMinor`, `dueAt` set by the plan's `netTermsDays` — walking `issued → paid | pastDue | void`. Each journal prepayment appends one order-linked `paymentApplied` line and advances `paidMinor` once in the close transaction, leaving the gross `totalMinor` unchanged. Full coverage walks the statement `paid`; partial coverage leaves the exact remainder outstanding, and a later renewal only funds its own period; a lost dispute surfaces as a `disputeChargeback` line on the next open statement or a fresh one of its own. The `statement-past-due` job flips an unpaid issued statement `pastDue` past its due date and suspends the plan's new holds once `dueAt + graceDays` passes — a debt restriction cleared in the same commit only when no other debt remains past grace. Eligibility is rechecked after locking the current plan and statement, so a concurrent completed payment, void or grace extension cannot trigger suspension from a stale candidate; busy customers do not block independent due work. New wallet admission checks effective past-grace debt immediately, including current statements, before the worker stamps a flag. A plan's `creditLimitMinor` caps the facility: an overage-enabled plan's wallet overflow refuses once `outstanding + overage` crosses it, `outstanding` being open statement debt, exact posted journals left without live statements, and current holds. The customer reads `GET /api/v1/org/statements{,/{id}}` and its plan's `credit` block (`creditLimitMinor`, `outstandingMinor`, `nextDueAt`); staff lists at `GET /api/v1/business/statements` (`state=pastDue` the dunning work list) and acts at `POST /api/v1/business/statements/{id}/release-review` under `FundsWrite` — `extendGrace` returns the selected statement to `issued` and extends its due date; other debt past grace retains suspension. `void` atomically reverses a proven one-to-one journal and statement, clears its receivable and puts collected funds in `RefundPayable` without moving Cash. Mixed chargeback, reissued or inconsistent sources require explicit financial reconciliation. Retained historical document-only voids remain debt in customer credit and admission; a new explicit review may reverse the exact backing journal. Exact request replays are inert; changed target/action/reason conflicts. Renewal, entitlement and manual restrictions remain independent. Statement lines and issued terms are trigger-immutable. |
| Plan statement | Issued beside every journal; customer reads its own, staff reviews past-due | **Shipped** (issue #612). Every journal issues a `plan_statement` in the same commit (v2 paid activation or legacy v1 close) — `planFee`/`usage`/`discount`/`refundCredit` lines summing to `totalMinor`, `dueAt` set by the plan's `netTermsDays` — walking `issued → paid | pastDue | void`. Each journal prepayment appends one order-linked `paymentApplied` line and advances `paidMinor` once in the close transaction, leaving the gross `totalMinor` unchanged. Full coverage walks the statement `paid`; partial coverage leaves the exact remainder outstanding, and a later renewal only funds its own period; a lost dispute surfaces as a `disputeChargeback` line on the next open statement or a fresh one of its own. The `statement-past-due` job flips an unpaid issued statement `pastDue` past its due date and suspends the plan's new holds once `dueAt + graceDays` passes — a debt restriction cleared in the same commit only when no other debt remains past grace. Eligibility is rechecked after locking the current plan and statement, so a concurrent completed payment, void or grace extension cannot trigger suspension from a stale candidate; busy customers do not block independent due work. New wallet admission checks effective past-grace debt immediately, including current statements, before the worker stamps a flag. A plan's `creditLimitMinor` caps the facility: an overage-enabled plan's wallet overflow refuses once `outstanding + overage` crosses it, `outstanding` being open statement debt, exact posted journals left without live statements, and current holds. The customer reads `GET /api/v1/org/statements{,/{id}}` and its plan's `credit` block (`creditLimitMinor`, `outstandingMinor`, `nextDueAt`); staff lists at `GET /api/v1/business/statements` (`state=pastDue` the dunning work list) and acts at `POST /api/v1/business/statements/{id}/release-review` under `FundsWrite` — `extendGrace` returns the selected statement to `issued` and extends its due date; other debt past grace retains suspension. `void` atomically reverses a proven one-to-one journal and statement, clears its receivable and puts collected funds in `RefundPayable` without moving Cash. Reissued or inconsistent sources require explicit financial reconciliation. Retained historical document-only voids remain debt in customer credit and admission; a new explicit review may reverse the exact backing journal. Exact request replays are inert; changed target/action/reason conflicts. Renewal, entitlement and manual restrictions remain independent. Statement lines and issued terms are trigger-immutable. |
| Financial facts and reconciliation | Staff reads cross-organization figures; the customer reads its own projection | **Shipped** (issue #613). `GET /api/v1/business/financial-facts` answers financial categories derived from source rows — `usage`/`rated`/`entitlement`/`funding`/`earned`/`collected`/`refund`/`receivable` — under `booking`, `service` or `payment` clocks with `organizationId`/`model`/`channel`/`from`/`to` filters (`ReportsRead`); `GET /api/v1/org/usage-report` is the customer's own projection of the same read, so its total reconciles to the business sum by construction. `GET /api/v1/business/cost-coverage` buckets settled turns by `cost_basis` (`missing` = untracked cost, never a silent zero; `errored`/`refused` separated as failed attempts) at summary and provider/day grain. The `reconcile-financial` leader job runs hourly over the day just closed; `reconciliation_runs`/`reconciliation_exceptions`/`reconciliation_repairs` are its memory — conservation invariants (`usage − entitlement`, `billed − collected = receivable`) plus `missingCost` checks write deduplicated exceptions a staff actor `resolve`s, `waive`s or `repair`s under `FundsWrite` + `finance_audit`, a repair being an audited correcting write (a `reclassifyCost` before/after snapshot, an adjust/link/duplicate instruction), never a balance overwrite. |

- The unit is credit, 1 credit = 1_000_000 minor. What a credit maps to in real money is the deployer's choice; oxsum does not care. (Shipped.)
- Display-side fiat conversion is shipped (issue #257): an organization may declare one display currency — an ISO code plus a decimal `ratePerCredit` string and a decimals count — through `PATCH /api/v1/org` (an owner- or admin-session write; members and API keys are refused) or the operator's admin `PATCH /api/v1/admin/organizations/{id}`. Wherever the dashboard or the statement PDF renders an amount, the converted figure rides beside the credits (`4.000000 cr ≈ 0.01 USD`), and the bills CSV/JSON exports gain `displayAmount`/`displayCurrency` columns. The conversion is pure presentation: the ledger, every proof and every content hash stay in integer minor units, the rate travels as decimal text so no float ever touches money, and the org's stored rate wins over the deployment-wide `OXSUM_DISPLAY_CURRENCY`/`OXSUM_DISPLAY_RATE_PER_CREDIT`/`OXSUM_DISPLAY_DECIMALS` default.
- The balance is one number to the organization but four pools in the ledger (issue #116, decision D1; issue #122, decision D4; issue #609): promotional credit in live lots lives in `Equity:Promo` — the balance that can expire — granted credit (the signup bonus and admin grants) in `Equity:Bonus`, purchased credit in `Liabilities:Wallet`, and an organization's undrawn credit in `Liabilities:CreditLine`. Holds reserve promo → bonus → wallet → credit — most-restricted money first — settlements charge in the same order, and every balance read sums the four; the pool split is how the ledger keeps perishable, granted, paid-for and borrowed credit apart, never something a caller manages.
- Beside the ledger's three pools sits a fourth source a turn may draw on — a subscription's quota (issue #496): a counter on `oxsum.org_subscriptions`, not ledger money, so its spend is recorded on `oxsum.subscription_draws` and its boundaries audited on `oxsum.subscription_periods` rather than posted to the ledger. Admission order is subscription first: `admit_subscription_draw` debits the period's counter up to the freeze inside the organization's advisory lock (replaying under the hold's key so a retried hold draws once), then `hold_for_key` reserves only `freeze − drawn` — and a fully-covered turn takes no wallet hold at all (issues #602, #603): the durable operation row and the draw are its whole reservation, and its settle writes a typed zero-cost receipt — a balanced `Equity:Adjustments` entry under the same derived key a wallet settle would take — so an empty wallet never refuses a turn the plan already pays for. Settle charges the draw `min(charged, drawn)` — the released share returns to the counter — and the wallet's entry the `charged − subscription` remainder; a swept hold settles its draw at 0 the same way it releases the freeze, so a dead turn cannot leak quota. Every state change is a row on `subscription_periods`: a rollover closes the window `boundary`, a rebind `upgrade`/`downgrade` on the plans' `sortOrder`, a cancel `cancelled` — the period's own quota and consumed figures are snapshots, so a month's burn is provable against what that window actually carried.
- Every funding event — a top-up, a redemption, a Stripe, Airwallex or Hyperswitch charge or a signed crypto-rail authorization — is one row on `oxsum.deposits` keyed by `(rail, organization, payment_ref)`, carrying expected versus received amount and a status walk of `pending → confirmed → credited | reversed | expired`; a `credited` row names the ledger entry that moved the money, a `reversed` one names the entry that took it back (`reversalEntryId`, `reversedAt`). That single record is what reconciliation (P4-1) reads against each rail's own reporting. (Shipped, issue #118; the stripe rail landed with issue #201, the x402-shaped rail with issue #264, the Airwallex rail with issue #285, the Hyperswitch switch with issue #319; the reversal half with issue #356, the refund through the rail and the dispute record with issue #360.) [docs/rails.md](rails.md) is the honest matrix — which rail has been exercised to which level, and what a deployment still owes a real sandbox run for.
- The x402 crypto rail (issues #264, #282): `POST /api/v1/topups/x402` accepts a signed payment authorization `{scheme, network, asset, amountMinor, payTo, nonce, payer, signature}` and credits it through the same deposit walk. The verifier is a trait (`RailVerifier`) with two legs — verify, then settle. The real rail delegates both to an x402-rs facilitator sidecar (`OXSUM_X402_FACILITATOR_URL` + `OXSUM_X402_NETWORK` + `OXSUM_X402_ASSET` + `OXSUM_X402_PAY_TO`, all four or none): the deployment states its own payment requirements, the EIP-3009 authorization `nonce` anchors `payment_ref`, and the settle is serialized behind the deposit row's `FOR UPDATE` so a nonce settles at most once — the facilitator's transaction hash is stored on `facilitator_tx_hash`. The `OXSUM_X402_VERIFYING_KEY` stand-in checks an Ed25519 test-scheme signature and settles trivially — development and the test suites run on it, no chain needed (docs/decisions.md).
- Adjustments never modify history; they book a new entry. The original entry and its proof stay valid.

Sold-plan cancellation is effective at the paid boundary even when the next
request arrives before the rollover worker (#854). Lazy admission closes the
plan and its quota-period history together, then rechecks the subscription
state. A cancelled period supplies no new quota; ordinary wallet authority and
funding decide that request. A refusal rolls back the lifecycle and reservation
together; every retry still rejects the ended quota. Earlier admitted draws remain settleable against
the retained cancelled period. This differs from an uncancelled past-due plan,
whose remaining paid quota retains the documented service policy.

## API keys (shipped)

- Keys belong to organizations and hold no money themselves. The creator is recorded for permissions and audit; a key minted through a session records who minted it, a key minted through the API records no creator, because no person is acting there. Members see and revoke only the keys they created; owners and admins see and revoke all of them.
- The plaintext shows once at creation. The database stores only the SHA-256 hash and a prefix; the dashboard uses the prefix to help users recognize keys.
- Format: `oxs-` followed by 32 random bytes, so secret-scanning tools can recognize them.
- Optional name and expiry. Revocation is permanent.
- Per-key constraints are implemented (backend): a key's `spendLimitMinor` caps its committed spend, enforced atomically with the hold append. `budgetDuration` (`daily`, `weekly`, `monthly`) makes the limit periodic over the current UTC calendar window — settled charges inside the window plus every outstanding hold — instead of cumulative. `modelAllowlist` restricts the gateway models the key may call: a request naming another model is refused before upstream sees it, while a null list allows every served model. `requestsPerMinute` caps requests inside a rolling minute — spent at admission on the gateway, refused 429 with `Retry-After` — `maxConcurrentHolds` caps the holds a key may keep open at once, counted from monetary pending entries plus explicit zero-wallet reservations, and `tokensPerMinute` caps the settled input+output tokens a key may burn inside a trailing minute (issue #203) — counted from the durable window a settlement feeds, never a hold; a gateway request over window-actual plus its own estimate is refused 429 `TOKENS_PER_MINUTE`. `tokensPerMinuteScopes` slices that same allowance by the request's `metadata` tag (issue #209): each tag names one scope — `each` fans a bucket out per value, `concrete` pins one named value, `total` aggregates the whole tag — and every bucket a turn lands in is checked `window-actual + estimate ≤ cap` alongside the plain window. A full per-value bucket refuses only the turns carrying that value; the other values' buckets stay open. Scoped rows accrue in the same settle transaction and expire with the trailing minute, so a high-cardinality tag value set cannot outgrow the window. A periodically-limited key also gets an early warning (issue #220): each settle attributed to it recomputes the window's burn rate — settled charges inside the window plus every outstanding hold — and when the projected spend crosses `spendLimitMinor` before the window ends, a `budget.exhaustion_forecast` webhook fires once per key per window (re-armed by a constraint `PATCH` or the next window) and the keys dashboard shows the same projection as a line per key. Consumption itself is answerable too (issue #232): `GET /api/v1/budget` returns every scope's `limit`/`committed`/`consumedPercent` — the organization's credit line, the acting member's `budgetLimitMinor`, and each key's `spendLimitMinor` windowed by its `budgetDuration` — the same committed figure the hold check reads, and the keys dashboard names the percent under the budget line. `budget.threshold_crossed` joins the webhook catalog: an endpoint armed with `budgetThresholdsPercent` (say `[50, 80, 95]`) hears one delivery per scope per window when consumption crosses an armed line — `data` naming `scope`, the scope's own id, `limitMinor`, `committedMinor`, `consumedPercent`, `thresholdPercent` and `windowStart` — latched inside the settle's transaction so a replay or a racing settle can never deliver the same crossing twice, and re-armed when the limit itself is re-set.
- Per-member budgets are implemented (backend, #162): `PATCH /api/v1/org/members/{userId}`'s `budgetLimitMinor` caps committed spend across every key the member minted — the sum is the ledger's own, so it counts settled charges and outstanding holds alike and cannot drift. A member's cap and their keys' own limits both apply; whichever bites first refuses the hold `KEY_LIMIT_EXCEEDED`.
- Signed spend mandates are implemented (backend, issue #262): a key holder hands a bearer who does not hold the key a strictly narrower slice of that key's authority, as an Ed25519-signed blob the delegate presents in the `X-Oxsum-Mandate` header on the gateway. `POST /api/v1/org/keys/{keyId}/mandates` mints one — `capMinor`, an optional `models` subset, an optional `requiredPolicies` subset (issue #502), and a `notBefore`/`notAfter` window — and `GET` on the same collection lists what the key has issued (members only the keys they created). A mandate may only narrow, never widen, the key's own constraints: a cap above `spendLimitMinor`, a model outside `modelAllowlist`, a policy outside `requiredPolicies`, or an inverted window is refused at mint. Presentation is verified against the key's public half — the seed it signs under is sealed in `api_keys.mandate_signing_seed` like a channel credential — and `hold_for_key` enforces the window, the model subset and the committed-under-mandate cap under the key's per-key advisory lock, the committed figure summed from the ledger itself by the mandate id each entry carries. A bad or unverifiable blob is 403 `INVALID_MANDATE`; outside the window is `MANDATE_NOT_YET_VALID`/`MANDATE_EXPIRED`; over the cap is 429 `MANDATE_EXCEEDED`. Mandates are `notAfter`-bounded and never revoked — a key that must stop delegating revokes or re-times its own constraint, and a delegate's window runs out. A settled delegated turn's v7 description names the mandate (`{id, hash}`), so `/verify` proves which delegation authorized the charge.
- The entitlement read is implemented (backend + keys page, issue #501): `GET /api/v1/org/keys/{keyId}/entitlement?model=&estimate=&tag=` answers whether a request so shaped would be admitted *now* — admission's own checks composed read-only, so the caller learns the answer without spending a request slot, a window slot or a hold, and the check itself is free and never rate-limited. `allowed` plus `reason` names the first refusing check — `key_inactive`, `org_suspended`, `model_not_allowed`, `rpm_exceeded`, `tpm_exceeded`, `budget_exhausted`, `concurrent_holds`, or a presented mandate's `mandate_invalid`/`mandate_window`/`mandate_model`/`mandate_cap` — while the headroom fields (`remainingRpm`, `remainingTpm`, `remainingBudgetMinor`, `concurrentHolds`, `retryAfterSecs`) and the per-constraint `checks` object say how every constraint stands, not just the first refusal. The money verdict folds the key's `spendLimitMinor` with the minting member's and team's caps, `remainingBudgetMinor` reporting the tightest; the token window judges `settled < cap` rather than the estimate (a money figure, not tokens); and a presented `X-Oxsum-Mandate` adds the delegation's own verdict — an unverifiable blob is the `mandate_invalid` *answer*, not an error, because diagnosing a refused delegate is half of what the endpoint is for. The scope rules are the revoke's (a member reads only keys they created, a foreign id is `NOT_FOUND`), and the read is a snapshot, not a reservation — a racing request may spend the headroom it reports, which is why the atomic admission checks stay authoritative. The keys page polls it into a Live column (`ready` / the refusing reason / `off` for a revoked key).
- Request groups are implemented (issue #508): the platform admin manages named channel subsets per organization in Admin → Channels, and an owner/admin assigns a group by name when creating or editing keys. The key list shows the group's channel names as its effective access set. The API's `GET /api/v1/org/groups` and `GET /api/v1/admin/groups` include stable `channelIds` and readable `{id, name}` channel identities. Grouped requests are filtered at admission across models, cache, retries and fallbacks. Ungrouped keys can reach every channel; an empty group denies all; deleting an assigned group returns 409. Consumption keys cannot manage keys, member sessions cannot change assignments, and in-flight requests keep their admission snapshot.
- The **guardrail pipeline** is implemented (backend + admin surface, issue #502): the platform admin writes named policies through `PUT /api/v1/admin/guardrail-policies/{name}` — `keyword` (a regex list, each pattern `flag` or `block`), `pii` (the built-in scan over the answer's text, always a flag) or `moderation` (an HTTP callout to the org's own endpoint, its bearer sealed on the row like an upstream key) — and binds them by `PUT /api/v1/admin/guardrail-bindings/{channel|key}/{target}`. A key's own `requiredPolicies` names policies its turns must run; a mandate's is a subset-checked narrowing, never an addition. Resolution is per request: the turn's set folds the channel's binding, the key's binding, the key's requirement and a presented mandate's into one deduplicated run, so a policy written or bound takes effect on the next call. `pre_call` judges the request's texts before the hold — a `block` answers 403 and costs nothing; `post_call` judges the completed answer — a `block` settles `Charge::Nothing` (the hold fully refunded) and still refuses, while a streamed turn's verdicts can only be recorded, which the v11 settlement's `guardrail` array marks `streamed: true`. A moderation endpoint that cannot answer fails *closed* before the hold and *open* after it — the upstream cost is real either way, so an outage must not eat a settled turn.
- Organization suspension is implemented (backend, #162): the platform admin's `PATCH /api/v1/admin/organizations/{id}` takes `suspended`, and a suspended organization's new holds refuse `FORBIDDEN` — every admission path, gateway and holds endpoint alike — while its in-flight holds settle and its balance still accepts top-ups and redemptions, so a suspended line can be repaid. Suspension changes admission, never the ledger: it reserves nothing, releases nothing and rewrites nothing. A suspension enqueues one `org.suspended` webhook delivery per subscribed endpoint; reinstatement announces nothing. The same PATCH's `dunningGraceDays` automates the flip (issue #258): the daily `statements` job suspends an opted-in organization once a finalized statement stands unpaid past `due_date + dunningGraceDays`, marks the suspension as its own (`dunningSuspendedAt`), and lifts a sweep-set suspension once nothing unpaid stands past grace — or when the grace is switched off with an explicit `null`. A suspension the operator set by hand is never the sweep's to undo.
- Device authorization (issue #156): a CLI or other input-constrained tool mints a request through `POST /api/v1/device/code`, prints the short `userCode` and points the user at `/device`; the signed-in user approves or denies there, and the tool's poll of `POST /api/v1/device/token` receives the minted key — the key lands in the approver's current organization, named `device <code>`, created_by the approver, and revocable like any other. The key is minted inside the successful poll, so its secret is never at rest; requests lapse fifteen minutes after minting, and unknown, expired and decided codes all answer the same `NOT_FOUND`.

## Gateway (shipped)

### Supported endpoints (shipped)

- `POST /v1/chat/completions`, streaming and non-streaming — the OpenAI surface, served by `openai` channels
- `POST /v1/responses`, streaming and non-streaming — OpenAI's successor wire (issue #495), served by the same `openai` channels, called upstream at `{baseUrl}/responses`. `input` is a string or the items list, `instructions` is the system prompt, `max_output_tokens` the only ceiling spelling, and `store`/`previous_response_id` are pass-through — the gateway keeps no response state. Usage settles against `usage.input_tokens`/`usage.output_tokens` (the reasoning subset already inside the output side), and a stream ends on `response.completed`, never `[DONE]`
- `POST /v1/messages`, streaming and non-streaming — the Anthropic Messages surface, served by `anthropic` channels, with errors in Anthropic's own envelope and `x-api-key` accepted as the credential spelling
- `POST /v1beta/models/{model}:generateContent` and `POST /v1beta/models/{model}:streamGenerateContent` — the Gemini surface (issue #494), served by `gemini` channels. The model is the path's segment (a `model` body field may only repeat it), streaming is the action rather than a `stream` field, and the credential spelling is `x-goog-api-key`. Usage settles against `usageMetadata`: `promptTokenCount` includes the cached share, `candidatesTokenCount` excludes the thinking share, and `toolUsePromptTokenCount` sits outside the prompt count
- `POST /v1/embeddings`, non-streaming — OpenAI's embeddings shape, served by `openai` channels whose price for the model is `mode: embeddings`. `input` accepts a string, an array of strings, or token arrays, which freeze at their exact count; there is no output side, so `stream` is refused and no `max_tokens` is forwarded. Usage settles against `usage.prompt_tokens`, estimated from the input bound when upstream reports none
- `POST /v1/rerank`, non-streaming — the Jina/Cohere shape (`query` plus `documents`, strings or `{"text": …}` objects), served by `openai` channels pricing the model `mode: rerank`; usage settles against `usage.total_tokens`
- `POST /v1/tasks`, `GET /v1/tasks/{taskId}`, `POST /v1/tasks/{taskId}/cancel` — the async task relay (issue #503), served by `tasks`-protocol channels. Submit freezes a declared bound (`maxCostMinor`, or the price's `costPerRequest`), relays `{baseUrl}/tasks` and answers `{taskId, submitted}`; a `poll-tasks` periodic pass asks `{baseUrl}/tasks/{id}` every 30s and settles the hold on the terminal answer — `succeeded` at the result's usage, `failed`/`cancelled` at zero — and fires `task.succeeded`/`task.failed` webhooks. The caller's `GET` is a row read, never a second upstream call; cancel asks upstream to stop best-effort and releases the hold either way
- `GET /v1/models`: lists only models a `/v1` surface can relay — a configured price, and not an `event`-mode billable code
- The **response cache** (issue #497; context completed in #645) — the settings key keeps its historical `settings.semanticCache` spelling: a channel opted in there re-serves a stored answer when a request repeats an earlier turn's complete admitted context on the same `(organization, channel, model)` key — whole non-streamed answers only, keyed by a versioned HMAC of the exact caller and forwarded bodies, protocol-owned headers, the customer/key permission snapshot, the selected experiment and mandate, the primary route's configuration and price version, and the discount. Text is exact: case, whitespace, tools and generation parameters all distinguish questions, and the legacy `excludeSystem` field is accepted but ignored. The lookup runs before any fresh-turn freeze: a hit answers the stored body with `x-oxsum-cache: hit` and settles a real charge — the original turn's usage repriced at the cache-read rate (the whole input counted as `cache_read`, folding into the input rate when the book names none) under a fresh-turn-shaped hold sized to the replay's cost, never the fresh bound. The match is exact — an equality of the keyed context digest, not an embedding or similarity search — so the settlement's `cache` block always carries `similarity: 1.0` beside the `sourceEntryId` of the ledger entry the answer was first produced under; `/verify` proves the reuse it charges for, and `upstreamAttempts` reads `0`. Org-scoping is the privacy rule: a cached answer never crosses organizations, and it never crosses two sibling keys either. `ttlSeconds` bounds a row's freshness when the policy names one. A streamed request is never looked up and never stored — the cache holds whole bodies an SSE client cannot be handed. The upgrade to this key format discards old normalized-prompt rows: a cached answer is rebuildable, and an incomplete key is not migrated.
- `Idempotency-Key` on any turn endpoint makes a client retry the same turn: the identical request replays the first response (or the settled receipt for a streamed turn), a different body under the same key is refused 422, and a retry while the first turn still runs is refused 409. A retry never produces a second hold or charge.

The surfaces are protocol-native, never translated: a model an `openai` channel serves is not served on `/v1/messages` or `/v1beta`, and so on — though `/v1/responses` and `/v1/chat/completions` share the `openai` protocol's channels, since they are two spellings of the same protocol. On Anthropic's surface, `system` accepts a string or text blocks; a `tool_use`/`tool_result`/`thinking` content block contributes its serialized JSON to the input bound, and a media block (`image`, `document`, `audio`, `video`) is refused 400 — the same rule the OpenAI surface applies to its content parts, the Responses surface applies to `input_image`/`input_file`/`input_audio` parts, and the Gemini surface applies to its `inlineData`/`fileData` parts. The normalization follows Anthropic's report: `input_tokens` excludes the cache counts, so the billed input adds `cache_read_input_tokens` and `cache_creation_input_tokens` back in, and the 5-minute/1-hour write tiers come from `cache_creation.ephemeral_*_input_tokens`. Gemini's `usageMetadata` reports like OpenAI's with its own names: `promptTokenCount` already includes `cachedContentTokenCount`, while `candidatesTokenCount` excludes `thoughtsTokenCount` — the billed output folds it back in, the billed input adds the separate `toolUsePromptTokenCount`, and a streamed turn's last event ends on a candidate `finishReason` rather than a `[DONE]` sentinel. The Responses wire renames the same shape once more: `input_tokens`/`output_tokens` with the subsets under `*_tokens_details`, terminal usage riding `response.completed`.

v1 has no images, audio or Responses API. Request content is limited to what has a computable input bound; messages containing images get a 400. This is decided: the input token upper bound must be computable for the freeze promise to hold (see "How much to freeze" below). Image support is re-evaluated post-v1.

### Channels and prices (shipped)

- Channels are configured by the platform admin: name, upstream base_url, upstream API key pool, served models, and the upstream protocol the channel speaks — `openai`, `anthropic`, `gemini` or `tasks`; the protocol selects the adapter that normalizes its usage reports and the surface its models answer on (`/v1/*` for the first two, `/v1beta/*` for `gemini`, `/v1/tasks` for `tasks`), and a name with no adapter is refused at write time.
- One model may be served by several channels. Each route carries a `weight` — the relative preference for leading a request — versioned together with the price it was written beside. The first attempt is a weighted pick, and the pick chooses a *weight tier*, not a channel: of the routes equally preferred, the cheapest by `inputPricePerMillion` leads and the rest fall back (issue #438), so two channels the price book weighs the same never let the dearer one lead. The pick also reads what upstream has done lately (issue #499): a process-local health window — an EMA of each channel's time-to-first-byte and error rate — scales a route's effective weight, so a channel that is 2× slower than the fleet's healthy baseline counts as half its weight for the pick, and one whose recent attempts error past half is dropped entirely for a 60-second cool-down after which the decayed window re-admits it. The stats are a routing hint only — a slow channel sheds leads, it is never billed differently — and a cold start is the healthy default: weight and price carry the pick until the first samples land. A refusal that arrived before any answer bytes (upstream unreachable, 429, 5xx) rotates the turn to the next route under the same hold, and a lone or last route's 429 naming a short `Retry-After` is waited out once. A failure once bytes are flowing is never retried — the client already holds a partial answer, and a replay could charge upstream twice. The freeze is sized for the dearest candidate, so a bill never exceeds what was reserved whichever route answered, and the settlement names the channel that actually served the turn plus how many upstream calls it took (`upstreamAttempts`).
- Channels are configured by the platform admin: name, upstream base_url, upstream API key pool, served models, and the upstream protocol the channel speaks — `openai`, `anthropic`, `gemini` or `tasks`; the protocol selects the adapter that normalizes its usage reports and the surface its models answer on (`/v1/*` for the first two, `/v1beta/*` for `gemini`, `/v1/tasks` for `tasks`), and a name with no adapter is refused at write time. A request group (issue #508) is the admin's other way to shape reach: a named subset of an organization's channels (`PUT`/`DELETE /api/v1/admin/organizations/{organizationId}/groups/{name}`, `GET /api/v1/admin/groups?organizationId=`), assigned to a key by `groupId`, narrows which channels that key's requests may route through — where `modelAllowlist` limits models, a group limits channels, and a key in no group reaches them all.
- One model may be served by several channels. Each route carries a `weight` — the relative preference for leading a request — versioned together with the price it was written beside. The first attempt is a weighted pick, and the pick chooses a *weight tier*, not a channel: of the routes equally preferred, the cheapest by `inputPricePerMillion` leads and the rest fall back (issue #438), so two channels the price book weighs the same never let the dearer one lead. A refusal that arrived before any answer bytes (upstream unreachable, 429, 5xx) rotates the turn to the next route under the same hold, and a lone or last route's 429 naming a short `Retry-After` is waited out once. A failure once bytes are flowing is never retried — the client already holds a partial answer, and a replay could charge upstream twice. The freeze is sized for the dearest candidate, so a bill never exceeds what was reserved whichever route answered, and the settlement names the channel that actually served the turn plus how many upstream calls it took (`upstreamAttempts`). A request group (issue #508) narrows the same candidates before the pick: a key assigned to one routes only through its group's channels — the filter applies to the requested model's routes and to every fallback's, so a fallback may not serve through a channel the group forbids.
- A request may also serve under a **different model** than it named (issue #500): the platform admin configures an ordered fallback chain per model (`oxsum.model_fallbacks`, driven through `GET`/`PUT`/`DELETE /api/v1/admin/model-fallbacks/{model}`), so when every route of the asked-for model is exhausted the turn tries the next model in the chain — each fallback resolved through its own channels and priced by its own book, under the same one hold. A fallback the caller may not reach — outside the key's `modelAllowlist`, a presented mandate's `models` or the organization's tier — is skipped rather than served: permission to reach a model is re-judged per hop, never inherited. The v12 settlement records `servedModel` — what answered and priced the turn — and `requestedModel` (the ask) when the two differ; the chain caps at 8 and de-duplicates so a misconfigured loop cannot cycle.
- An organization may split its own traffic across models as an **experiment** (issue #509): a named set of `{model, weight}` variants an owner or an admin session writes through `PUT /api/v1/org/experiments/{name}` and ends through `POST …/end`. A request carrying `X-Oxsum-Experiment: {name}` and naming an *open* experiment lets the relay pick a variant by weight and rewrite the request's `model` to it before routing — the served model still resolves, prices and proves like any request's, and the caller's own allowlist, tier or mandate still constrains the landing model. The v13 settlement's `experiment` block (`{id, variant}`) records which arm served, and `GET /api/v1/org/experiments/{id}/results` groups the experiment's recorded usage by variant — turns, token and charge sums, errored count — for the offline scorer. An unknown or ended name leaves the request's `model` untouched; scoring stays offline — oxsum reports which variant served and what it cost, it does not judge outputs or promote a winner.
- One channel carries a **pool** of upstream keys rather than one (issue #444): `channel_credentials` rows hold each sealed key beside its health state, and the rate limits and quotas a provider applies are per-account — a single account's 429 need not forfeit the route. Routing picks in two levels: `route_order` chooses the channel (weighted, cost-aware inside a tier) exactly as above, and inside the picked channel a second pick orders its healthy credentials — the conversation's sticky credential first (it keeps the upstream prompt cache warm; affinity is a process-local hint keyed on the request's `end_user`, nothing shared), then least-recently-used, then the pool's stored order. A credential the upstream refused (429 or 5xx before answer bytes) cools out of the pick for a bounded minute while the turn rotates onto the pool's next key — same model, same price — before leaving for a differently-priced route; an unreachable upstream cools nothing, since it blames the channel, not one key. Credentials are managed as pool members: `POST /api/v1/admin/channels/{channelName}/credentials` appends one, `PATCH …/credentials/{credentialId}` switches it between `active` and `disabled` (disabled is kept on record, never picked), `DELETE` removes it — except the pool's last *pickable* credential, which stays so a channel never promises routes it cannot serve — and posting `apiKeys` on the channel itself re-credentials the pool wholesale. The credential pick is never billed: the settlement names `(channel, version)` as before, so a turn settles identically whichever of the channel's keys answered.
- Upstream API keys are stored encrypted; the admin API and the channels page show only the last 4 characters of each.
- Where the channel is itself an oxsum deployment, the daily `sync-upstream-prices` job (env-gated by `OXSUM_UPSTREAM_PRICE_SYNC`, issue #222) probes its `GET {baseUrl}/api/v1/pricing` with the unsealed credential and diffs the live `upstream` rates against the stored ones. A disagreement opens a drift row that broadcasts `anomaly.detected` and lands in the reconciliation report as `upstream_price_drift`; it resolves itself the moment the two rates agree again. Every probe writes an outcome row (`ok`, `unreachable`, `invalid`, `auth`) so a channel that stops answering leaves evidence rather than silence — and a channel pointing at a non-oxsum upstream is left alone: drift is detected, never inferred.
- Each model's price is a full price set, not just two rates: the input and output prices (minor units per million tokens) and the model's max output, plus any of the priced dimensions — a cached-input rate, cache-write rates for the 5-minute and 1-hour provider tiers, a reasoning-token rate, and a flat per-request fee. A dimension with no rate folds into its side's base line: cached input bills at the input price, reasoning at the output price. The set may also carry the upstream's own prices for the same usage — the data the margin view reads; an organization's bill never shows it. Every price declares a billing `mode`: `chat` prices both token sides, while `embeddings` and `rerank` are input-only — their sets carry no `outputPricePerMillion` or `maxOutputTokens`, and the mode decides which `/v1` surface may serve the model. A mode mismatch is `model_not_served`, not a reinterpreted rate: a chat-priced model is never an embeddings route, and vice versa.
- A price can also carry conditional rules: a `match` on the request's `serviceTier` or an input-token window picks a different whole set — a match swaps the set, never a single field. When several rules could apply the most specific one wins (the count of present conditions), and two rules that could match the same request at the same specificity are refused when the price is written — an ambiguous price book never reaches a bill.
- After the price set is picked and the gross lines computed, one discount may apply: `pricing_discounts` rows grant a percent off, scoped to an organization and/or a model inside a validity window (issue #158). The single most favorable matching row applies — discounts never stack — resolved when the turn starts, and the settled description snapshots `discountPercent` so the bill still recomputes after the row has changed or ended. The freeze stays the undiscounted bound; a discount only ever lowers the charge.
- Separately from pricing, an organization may carry a **tier** — a named capability package (`tier_profiles`: a requests-per-minute window shared by all the organization's keys, and a model allowlist) the platform admin assigns through `PATCH /api/v1/admin/organizations/{id}`. The gateway enforces it at admission — a model off the list is `403`, an exhausted minute is `429` — and it is never an input to the price formula. An organization without a tier is unconstrained.
- A channel may also serve a model under an **alias** (issue #286): a public name claimed on the channel through `POST /api/v1/admin/channels/{channelName}/aliases`, renaming the upstream model the channel relays. An alias is a rename, not a routing layer — the name resolves to the claiming channel alone, so a request for it never fails over onto another channel, and the gateway forwards the upstream name. `GET /v1/models` lists aliases beside the direct names, owned by the claiming channel. One name answers one route set: an alias already claimed anywhere, or a name another channel serves directly, is a conflict. The turn is priced by the alias's own price row when the channel wrote one — the upstream model's otherwise — and a settled turn's description names the asked-for alias in `model` and the served model in `upstreamModel` (description v8). Aliases are configuration, not ledger data: a delete is hard, and bills already written keep the name.
- Changing a price never overwrites the old one; it creates a new version. The version in force when a request starts is the version used to settle it. So a price change only affects later requests; in-flight requests and old bills are untouched.

### How much to freeze (shipped)

When a request arrives, compute the most it could possibly cost, freeze that much, then contact upstream.

- Input upper bound = UTF-8 byte count of all text content in the request, plus a fixed per-message format overhead. Mainstream tokenizers are byte-level BPE, so one token spans at least one byte and the byte count never undercounts tokens. The cost is overshooting — roughly 4x for English, 2x for Chinese — and the excess is refunded in full at settlement.
- Output upper bound = the request's `max_tokens` or `max_completion_tokens`. If absent, the model's configured maximum output; if larger than that maximum, the maximum applies. The input-only surfaces have no output side — the bound there is zero and nothing is forwarded.
- Token-array `input` (embeddings) is the exception to the byte bound: the ids are already tokens, so the bound is their exact count rather than an estimate.
- Freeze = the most the turn could possibly cost under every set that could match it: input bound at the dearest applicable input rate (a cache-write tier prices above fresh input, so it prices the bound), output bound at the dearest output rate, plus the flat request fee — rounded up once. The request's own `serviceTier` narrows the lane: a rule that needs a tier the caller did not ask for cannot hit, and does not inflate the freeze.
- When relaying, the gateway always writes the output upper bound into the request. Upstream output can then never exceed the estimate the freeze was based on.
- If available balance cannot cover the freeze, return 402 without contacting upstream. The error states how much the freeze needs, the current available balance, and suggests lowering `max_tokens`.

No tokenizer-based freeze estimation: tokenizers differ per model, and an underestimate means actual spend can exceed the freeze. new-api's #7554 is the reverse failure — token estimation inflated tens of times, pre-holding 4M at once. Byte estimation overshoots, but the bound is guaranteed and the excess is always refunded.

### How settlement works (shipped)

When relaying a streaming request, the gateway always writes `stream_options.include_usage = true` so upstream returns usage in the final chunk.

| Situation | Charge | Settlement type |
| --- | --- | --- |
| Completed normally, usage received | usage × the price version from request start | `usage` |
| Upstream returned an error before emitting anything | 0, full release of the hold | `upstream_error` |
| Could not connect or timed out, no response received | 0, full release | `upstream_unreachable` |
| Answer ended (completed or truncated) with no usage report | The locally-estimated input only — output is never billed when nobody counted it | `estimated` |
| Client disconnected mid-stream | Cancel the upstream call immediately; estimate locally from what was forwarded | `client_cancelled` |
| Usage-priced cost exceeds the freeze | Charge the freeze only; record the excess as an anomaly for admin review | `capped` |
| Hold timed out with no settlement (e.g. the gateway crashed) | 0, full release, recorded as an anomaly | `swept` |
| Usage names a dimension the price book cannot bill (a tool call, a media token, a foreign event kind) | Charge the part the book covers, capped by the freeze; the unpriced rest is the platform's flagged loss — never a silent zero, never folded into a rate it does not belong to | `unpriced` |
| Admission refuses the request before any hold — a rate limit, an exhausted key budget, insufficient funds, a model not served | Nothing — no hold exists to settle; a self-balancing marker entry records the refusal and its reason | `refused` |

(The settlement kinds and the anomalies page that reviews them — `capped`, `estimated`, `client_cancelled`, `swept` and `unpriced`, read out of the ledgers — both ship.)

- Terminal status on billing records (issue #233): every record carries `status` — `settled` for every settled outcome including the anomalous kinds, `errored` for the turns that served nothing after their hold (`upstream_error`, `upstream_unreachable`, `swept`), `refused` for the requests admission turned away — plus a `failureReason` when the turn has one, in the caller-facing words the answer already gave. A refused request writes a self-balancing marker entry (debit and credit of the same minor unit on the adjustments account, like the statement anchor) whose description is a settlement record of kind `refused` — so a 400 or 429 is a row on the billing page that proves like every other record, instead of existing only in server logs.

- Local estimation: count input and forwarded output with tiktoken's `o200k_base`; the result never exceeds the freeze. The bill is explicitly marked "estimated".
- The degraded settle (issue #230): a turn upstream answered but never counted bills only the locally-estimated input side — output was forwarded but cannot be counted, and a charge for uncounted tokens is a charge for nothing. It lands as `kind: estimated` (marked, never a silent zero), the usage row records the estimate under `local_*` columns with `local_output_tokens` NULL — "not counted" rather than a fabricated zero — and `usage_details.estimation` keeps the estimate's own paper trail (method, `input-only` or `forwarded-prefix` scope, reason). A client-cancelled turn is the neighbouring doctrine, not this one: it pays what was delivered, and the forwarded prefix is countable text, so its local estimate stands on both sides. Every degraded settle enqueues `anomaly.detected` for asynchronous reconciliation.
- The user never pays more than the freeze — that is the gateway's promise. Underestimates, missing usage and upstream overcharges are the platform's cost.
- **Upstream token padding** (observed in the field, issue #1054): some providers count more `prompt_tokens` than the client sent — the relay prepends system or reasoning scaffolding before billing. The freeze is sized from the caller's own bytes, so upstream's padded count exceeds it and the settlement clamps to `capped`: the platform absorbs the difference between what the client sent and what upstream bills. `capped` is the honest signal that a channel's `inputPricePerMillion` was priced against the client's counting, not the upstream's — an operator seeing repeated `capped` settlements on one channel should raise that channel's input rate to match the padding, not treat it as a defect.
- The hold timeout defaults to 30 minutes and must exceed the longest possible single request. A background job sweeps timed-out holds. The sweeper and normal settlement share the same idempotency key, so both cannot succeed; whichever lands first wins. A hold whose sweep keeps failing is counted and, at ten failures, dead-lettered: it keeps retrying hourly, carries its last error, and shows on the in-flight page and in the reconciliation report (issue #140).
- Every response carries an `x-oxsum-request-id` header; the bills page's settlement rows lead with that same request id, so a row joins the header a client logged to the ledger entry that billed it.
- A caller learns what a request cost from the response itself: every answer that took the hold carries `x-oxsum-freeze-minor` (the most it can cost), and the settled charge arrives as `x-oxsum-charged-minor` — a header on a non-streamed answer or a post-hold refusal, an HTTP trailer on a streamed one.
- An organization can hear about each settled turn without polling: register an HTTPS endpoint (HTTP only to localhost) through `POST /api/v1/webhooks`, subscribe it to `request.settled` — and, since #162, `org.suspended`, which fires once when the platform suspends the organization's spend — and every event is POSTed there signed: `x-oxsum-signature` is `t=<unix>,v1=<hmac>` over `"{t}.{body}"` with the `whsec-` secret shown once at creation. Deliveries retry on a backoff until a ten-attempt budget runs out, and the last fifty per endpoint are listed back for inspection (issue #144). The full catalog ships since #200: `deposit.credited` (money in, with `rail`/`paymentRef`), `deposit.failed`/`deposit.reversed`/`deposit.disputed` (for the rails that report them — `deposit.disputed` fires when a rail's `dispute_opened` lands, issue #360), `balance.low` armed per endpoint by `balanceLowMinor` and firing once per crossing, `statement.finalized`/`statement.payment_overdue`/`statement.payment_status_updated` for the billing documents, `anomaly.detected` for the odd settlement kinds plus upstream price drift (issue #222 — platform scope, every subscribed endpoint, `data` naming `channel`/`model`/`field` and both rates), `budget.exhaustion_forecast` for a periodically-limited key whose projected spend crosses its cap inside the current window (issue #220), and `org.credit_limit_exceeded` when a credit-lined organization hits its line.
- Own key-attributed data is readable as JSON under a consumption key: `GET /api/v1/usage` (the daily rollup), `GET /api/v1/billing-records` (settled turns, cursor-paged), `POST /api/v1/estimate-price` (what a declared token shape would freeze at — the gateway's own arithmetic) and `GET /api/v1/pricing` (the public catalog: every model's current price version, never a channel credential). A member session sees its own keys' rows plus the shared unattributed ones, as on the pages (issue #146).

Two settled trade-offs:

- On client disconnect, cancel the upstream call immediately. Rationale: users must not pay for content they never received; letting upstream keep running means the platform pays upstream for nothing. The cost is settling on local estimation, marked on the bill.
- On mid-stream interruption with no usage, settle degraded — the locally-estimated input only — rather than copying sandbase's free-transaction approach. Free is friendliest to users but rewards deliberately cutting the connection; the platform's cost becomes an exploitable loophole. Billing the one countable side and flagging the anomaly is the honest middle ground. The never-pay-more-than-the-freeze promise still holds.

### How one request is booked (shipped)

- One request maps to two entries: the freeze and the settlement. Idempotency keys: `req-<id>:hold`, and the settlement's key derived from it (`oxsum_core::settlement_key_for`).
- The settlement entry's description carries a compact versioned JSON (`"v":3`): the request id, model, price version and settlement kind, the metered usage dimensions, the itemized lines the charge sums from — each `[item, units, pricePerMillion]` — and the rule that priced the turn, when one did. The description is hashed into the entry, so what the user verifies is not just "how much was charged" but "by how many of what, at what rates, under which rule". The description caps at 512 characters, which is why a line is a tuple, not an object — see docs/decisions.md.
- Beside every settlement that lands, the same path writes the turn's normalized usage record (`oxsum.usage_records`, issue #102): the full token dimensions — cached and reasoning tokens as subsets of the totals they belong to — the caller's `user`/`metadata`/`service_tier` attribution, the settlement kind, and `usage_details.provider_raw`, upstream's usage object verbatim for the reconciliation window. The row also records who produced the billed counts (issue #230): `provider_input_tokens`/`provider_output_tokens` carry the report upstream made, `local_input_tokens`/`local_output_tokens` the estimate oxsum counted — each NULL when that side never happened, so "not counted" is never confused with a counted zero. The row is mutable-store data by design: attribution is pseudonymized when an organization is deleted and `provider_raw` is nulled by the daily retention job once `OXSUM_RETENTION_PROVIDER_RAW_DAYS` (default 7) passes, so none of it can live inside a hashed ledger entry. The entry stays the source of truth for the charge; a swept turn leaves the same row at zero, still naming whose turn it was.
- The row also carries `upstream_cost_minor` (issue #112): what the price's `upstream` block made of the same usage — the platform's own cost for the turn, computed under the same set the customer priced with. `NULL` means untracked, not free: a price with no `upstream` block, a swept turn, or history from before the column. `GET /api/v1/admin/margin` sums charged against upstream cost per channel and model and counts the untracked rows; it is the deployer's view for spotting margin erosion and upstream misbilling, and no organization-facing endpoint ever shows it.
- On a deployment running `upstream-bill-sync` (issue #283, `OXSUM_UPSTREAM_BILL_SYNC` + `OXSUM_SECRET_KEY`), the daily pass also records what the provider *actually billed* — `oxsum.upstream_bills`, one `(channel, day)` row holding the provider's total beside the day's summed estimate, pulled through a channel's separately-sealed `billKey` admin credential (OpenAI's Costs API and Anthropic's Usage & Cost Admin API are the two impls, dispatched on the channel's `protocol`). The margin view answers it as `providerBilledMinor`, channel-scoped; drift over the alert threshold flags `anomaly.detected`. A provider that answers nothing records a coverage-gap day, not a false zero.
- Each settled turn also rolls into `oxsum.usage_daily` in the same transaction (issue #126): one row per organization, booking day, key, channel and model summing turns, the token counts and the charge. That rollup is what the usage page reads; a replayed usage write lands no row and rolls nothing, so the dashboard's sums cannot double-count a retried write.
- The full request state (in flight, settled, anomalous) was going to live in oxsum's own `requests` table, which was never built. The requests page (#55) reads what already exists instead: each settled gateway request is a settlement entry whose description carries the model, the token counts and the prices, read back as `SettlementRecord` (`crates/core/src/billing.rs`) and listed by `Wallet::recent_requests`. In-flight gateway holds are the sweeper's `oxsum.open_holds` watch table, shown live on the overview, and the ledger stays the source of truth. Only what needs proving goes into the ledger. See docs/decisions.md, "the requests page reads the ledger".

## Metering external services (shipped, issue #172)

Business-owned integrations can bill declared external events through `/api/v1/metering/*` only with an owner-issued integration credential carrying `meteringWrite` and the target customer in its explicit allowlist or stable application namespace. The separate `customersRead` scope exposes only selected customer profiles. Both require a current owner session to issue/revoke; no customer, staff admin, finance, support, viewer or deployment-token credential can mint integration authority. Secrets are shown once, stored hashed, expire within 365 days and are revocable. Repeated issuance returns metadata without the secret. Revocation does not rewrite history or prevent trusted hold recovery.

Legacy global `oxs-svc-…` keys are disabled at upgrade; their IDs/hashes/metadata remain for audit and no global compatibility switch exists. The owner issues a replacement with specific customers. Existing old holds remain for the trusted sweeper; a new credential cannot take them over. No funds-crediting or unsupported management scopes are advertised.

- Pricing comes from the existing price book: the request's `channel` plus `billableCode` resolve a `mode: event` price, whose version is pinned to the hold so a later price change cannot move an in-flight event. The declared `usage` follows the normalized usage shape and requires `eventType`; a dimension the price cannot bill is refused rather than partial-billed.
- The money path is the wallet's own: `holds` freezes the declared bound (suspension `403`, insolvency `402`, sweeper-watched like every hold), `settle` charges actual usage capped at the freeze (`usage`/`capped`), `release` lets it go at zero (`released`, a settlement kind the metering surface added), and `settlements` freezes and settles a completed event in one call. Every write binds credential, customer and body; an exact retry replays the committed answer even after the watch row is gone. Changed body/operation conflicts; sibling credentials cannot inspect, settle or release each other's holds. Event labels stay in settlement evidence, while internal usage identity is scoped to avoid cross-customer label collisions.
- A metered settlement's description is schema v6: it names the reporting `service`, carries the `eventType`, and omits `upstreamAttempts` — nothing was relayed, and the parsed record reports 0 — so `verify_charge` recomputes it like every gateway bill and the organization's bills page and browser verification cover it unchanged.

## Bills and verification

### Bills page (shipped)

`/app/bills` (`web/src/pages/Bills.tsx`) lists the organization's transactions, newest first: top-ups, adjustments — the signup bonus among them — and settled requests. Each row carries the date it was booked, its kind, the detail (a settled gateway turn shows its request id, model, token counts, freeze and settlement kind — the fields the entry's own record holds; a top-up or an adjustment shows the reason it was written), what it moved in credits signed — money in reads positive, a charge negative — and the content hash its proof verifies against. A hold bills nothing yet and is not listed; the overview's in-flight list is where it shows. (Shipped.)

- Member scope is the requests page's rule: a member reads the transactions their own keys paid plus the key-less organization history — top-ups and adjustments — while owners and admins read everything. The exports and the proof-bundle read apply the same scope. (Shipped.)
- Every row links to `/verify` with its entry named (`?entry=<id>`); the verify page fetches the proof bundle for that entry — from the reader's own organization, in the session's scope — and runs the browser-local check on load. The row's link also carries the content hash; a bare `?entry=` link does not, and the page says why the field stays empty — the hash is the half of the check that has to come from the verifier's own record, not from the server that issued the bill (issue #183). (Shipped.)
- CSV and JSON export: `GET /dashboard/bills/export.csv` and `GET /dashboard/bills/export.json` answer the page's own list as a file, with `Content-Disposition: attachment`, so a browser saves it and a command-line client fetches the same bytes with the same session cookie. Both files carry the same fields and the same rows — `bookedOn`, `entryId`, `kind`, `amountMinor`, `description`, `contentHash`, plus the settled turn's parsed `request` in the JSON — so a record can be archived and checked against the ledger later; the amount is the ledger's signed integer in minor units in both files, where the page renders the same amount in credits, and the CSV is RFC 4180. (Shipped.)
- The page lists a hundred transactions at a time: **Older** and **Newest** links page through the history, the position traveling as the URL's own `?before=` — a ledger list's cursor is the log index below which the page reads, so a page is a link and works in a browser that never hydrates. The exports are not pages: an archive means the whole history, so they walk the same read to the log's start and carry every transaction — bounded at 50 000 rows (issue #1081): an organization whose history passes the cap is refused with `413 PAYLOAD_TOO_LARGE` and told to narrow the window, rather than served a truncated file or an unbounded read. (Shipped, issue #93.)

### Usage page (shipped)

`/app/usage` (`web/src/pages/Usage.tsx`, issues #126 and #148) is the daily-rollup view, three answers over the trailing 30 UTC days: a bar chart of the charged credits per ledger booking day; a token-mix bar splitting the window's billed volume into fresh input, cached reads, output and reasoning — the segments are disjoint, since cached input is a subset of the input total; a by-key table summing each key's turns, token counts and charge — a key labels as its name or, unnamed, its prefix, and shared unattributed usage is a row of its own; and the channel-and-model table — turns, the token sums and the charge. The numbers come from `oxsum.usage_daily`, the rollup `record_usage` maintains beside every usage write, so the page reads pre-aggregated rows rather than scanning the request history, and a day nothing settled draws a zero-height bar instead of disappearing. Member scope is the bills page's rule applied at read: a member's chart and tables sum their own keys' usage plus the organization's unattributed shared rows, owners and admins see the organization whole — the by-key table is where that scope is visible, a member names only the keys their rows may name. (Shipped.)

The transaction log page is still what it was (`/app/log`, `web/src/pages/Log.tsx`): the organization's ledger entries, newest first, each with its index, id, description (for a settled request, the compact versioned JSON carrying the model, the metered usage, the priced lines and the settlement kind) and content hash. It lists twenty-five entries on the overview and pages a hundred at a time on its own page, the same `?before=` walk the bills page uses, and it is readable by every member — the raw log beside the bills page's transaction view of the same ledger.

### Browser verification

- Shipped: every entry verifies on the public `/verify` page, the one surface still server-rendered (issue #256) — its skeleton posts a proof bundle and a content hash to the server, which runs `oxsum_verify::verify_bundle` and renders the verdict. When the deployment ships the wasm bundle (issue #284), the page's `verify.js` loader runs the *same* `verify_bundle` in the browser — the verifier's bytes travel to the user rather than the user trusting a verdict the server rendered — and a browser without it keeps the server-side form. The chat page links to it with both fields prefilled after every settled turn. For a usage settlement the page goes further (#106): the description is a versioned billing credential, and `verify_charge` recomputes the recorded charge from the usage and priced lines inside the entry — a record from a schema the verifier does not know reports inclusion only, with the reason named.
- Shipped (#92): each visit to the bills page archives the organization's signed tree head in the browser's `localStorage`, keyed by the log's origin, after verifying the operator's signature on it. On a later visit, when the log has grown, the page fetches the consistency proof (`GET /api/v1/log/consistency?from=`) and verifies it plus the new head's signature in the browser — a one-line status reports the ledger append-only, a shrunk or rewritten log reports the failure in words and the archive is never overwritten on a failed check. The user does nothing. The public endpoint it mirrors is shipped (`GET /api/v1/log/consistency?from=`, B-7); the check also covers what #94 had left open: the operator's signature on the head is verified in the browser on every visit.
- Shipped (#231): heads can be anchored by parties outside this deployment. `OXSUM_WITNESS_URLS` names external C2SP `tlog-witness` endpoints; the hourly `witness-checkpoints` job signs each organization's current head and publishes it to every configured witness (`/add-checkpoint`, note plus consistency proof from the previously anchored head), and the cosigned notes they return are stored append-only. `GET /api/v1/log/checkpoint` serves the latest anchored head with every witness cosignature — so a third party can check that the history the operator signs is the history the pinned witness-key holders attested; organizational independence requires separate operational trust, and a fork attempt contradicts a witness's own record. The dashboard's transaction log page shows the anchored head and its witnesses.
- Shipped (#259): the cosignatures are checkable, not just listed. `OXSUM_WITNESS_KEYS` pins which `cosignature/v1` key each witness's signature line must verify against — a JSON map of URL to `{keyName, publicKey, keyHash}`, the same trust choice the operator's key at `/api/v1/log/key` already asks for. The checkpoint endpoint serves the pinset as `witnessKeys`, and the transaction log page runs `oxsum_verify::verify_witnesses` over the stored notes — reporting "anchored by N witnesses" only when the cosignatures actually verify under the pinned keys, and naming the witness whose line fails (a forged line, a stale row and an unpinned URL read differently). A verifier that does not trust the deployment's pinset substitutes its own. Shipped (#328): the customer's bill page runs the same check over the pinset and the notes the report bundle embeds, so the person the witnesses exist for sees "witnessed by N" — or the honest "witnesses are configured, but no cosignature covers this head" — without an account.
- Shipped (#204): the price a bill names is bound to a signed catalog snapshot. The operator signs a checkpoint per `(channel, model, priceVersion)` — the same Ed25519 key that signs tree heads, on a `tlog-checkpoint` body whose origin is `oxsum/price-catalog/<channel>`, whose `size` carries the price version and whose `root` carries a hash of the canonical row bytes — and a fresh proof bundle embeds it as `price`. `/verify` checks the binding in the browser and reports a third line: the catalog row the bill charged under is the one the operator attested, so a forged current price table cannot answer for an old bill. Signing is stateless and deterministic, so a stored version signs identically whenever asked — a v1 bill still verifies after v2 lands — and any version's checkpoint is fetchable at `GET /api/v1/log/price-head`. A bundle without `price` reports "no signed price binding", never a failure: a top-up names no price, an older bundle carries none, and a deployment without `OXSUM_HEAD_SIGNING_KEY` signs nothing.
- Shipped (#600): the *customer* sale terms a bill charged under are bound the same way, on their own checkpoint. When a turn settles under a resolved `SalePriceSnapshot` the settlement description commits `sale.digest` — the tagged-BLAKE3 hash of the snapshot's canonical row — and `receipt_proof` embeds a `sale` link: the operator-signed checkpoint of that row, origin `oxsum/sale-catalog/<business>`, `size` carrying the book version, `root` the same digest. The canonical row names only customer-visible identity — the customer, model, book/version/draft/legacy provenance, effective window, sale price and discount — and can never carry a supplier `upstream` rate, so no wholesale figure leaks into customer evidence. `resolve` records each digest → canonical-row mapping as it resolves, so a committed digest re-signs identically however the book has since moved; any stored row's checkpoint is fetchable at `GET /api/v1/log/sale-head?digest=<hex>` (or re-resolved by `organizationId`+`model`+`at`), customer-scoped like the bill itself. The settlement description schema steps to v14 for the `sale` block — older records verify unchanged — and a wholesale-priced bill commits no digest, so the verifier reports `NotSalePriced` rather than letting an old channel checkpoint answer for customer terms.
- Shipped (#260): a deployment can *be* a witness, not only use them. With `OXSUM_WITNESS_SIGNING_KEY` + `OXSUM_WITNESS_TRUST` set, `POST /add-checkpoint` answers other logs' checkpoint submissions: it cosigns each head that provably extends the one it last vouched for and refuses every other — 404 for an origin it was never told to trust (the pinset is the trust act; there is no trust-on-first-use), 409 for a fork or a shrunk history, 400 for a missing or bad proof, 401 for an unsigned note. The accepted head lives in `oxsum.witness_served`, every cosigned note it returned in `oxsum.witness_vouched` — both written in the transaction the response commits from, so a crash cannot leave a signature the witness has no memory of. Two oxsum deployments point their `OXSUM_WITNESS_URLS` at each other's `/add-checkpoint` and mutually witness with no third software.
- Third-party witness cosignatures are built (issues #231 and #259 above): witnesses beyond the operator attest the same head, and the deployment serves the pinned keyset a reader verifies them against.
- Shipped (#261): an end-user can be handed a *signed usage report* — the subset of settlement entries attributed to them over an ended span, provable against the organization's own ledger. `POST /api/v1/org/usage-reports` mints one `{endUser, from, to}`: it commits a self-balancing marker entry (`oxsum/usage-report/v1`) into the organization's ledger whose `DocumentRef` binds the subset digest and whose description commits the report's figures, once ever for that span. The answer is a self-contained bundle — the marker, every shown entry with its own inclusion proof under one head, and on a signing deployment the operator's note — which `oxsum_verify::verify_usage_report` checks offline; the `/verify` page accepts a pasted report and runs the same check. Completeness is proven when the deployment has anchored the span (issues #337-#339): a daily job commits each customer's day-log head into the organization's ledger, and the report carries the anchor, the head and every day leaf — the verifier rebuilds each leaf from the rows assigned to that day, so a dropped day or a misattributed row fails. Without an anchor — an unanchored span, or history from before the day log — the marker's "these are all the settlements" stays the org's signed claim, and a customer who kept their own request log detects a dropped row as a count or total mismatch. A day the anchor has fixed is sealed: a settlement booking into it afterwards is refused with `409 DAY_SEALED`, so the committed leaf cannot be rewritten after the fact. The subset is frozen at mint (`usage_report_rows`), so a re-fetch can never grow to absorb usage the org omitted. Shipped (#300): the report can be shared as a capability link — `POST /api/v1/org/usage-reports/{reportId}/links` answers a one-time token whose SHA-256 alone is stored, expiring 30 days out by default (90 at most) and revocable; `POST /api/v1/org/usage-reports/links` with an `endUser` gives one link that lists everything issued to that customer. The link resolves the report without a session — the public `/bill/{token}` page (issue #302) is the customer's side: it renders the frozen figures beside the operator key and the ledger head, needs no JavaScript, and carries no third-party script; pressing **Verify this bill** (issue #305) runs the same `oxsum_verify` checks in the browser over the bundle the page embeds. It is not an account. Shipped (#307): the dashboard's **Reports** page (`/app/reports`) issues a report and mints either link scope, shows the token exactly once, and lists every link with its expiry, last use and revocation. Shipped (#315): either link can be mailed to an address at creation — an optional `email` on the mint — because that is the only moment the plaintext token exists; the response's `delivery` reports `sent` or `failed`, and a deployment without a mailer refuses the address rather than minting a link nobody receives.
- Shipped (#616): the organization itself can be handed a *version-2 evidence bundle* — the complete committed record of a closed period, not one customer's subset. `POST /api/v1/org/evidence-bundles` mints `{from, to}` and `GET /api/v1/org/statements/{statementId}/evidence` mints the single-statement export: a typed manifest of every committed artifact (usage records, plan statements, invoice journals, payment orders and fulfillments, refunds, disputes, credit notes, day seals) folds under `oxsum/evidence-manifest/v1`, and a marker entry commits `oxsum/evidence-bundle/v1` → `manifestDigest` into the ledger once per scope. `oxsum_verify::verify_evidence_bundle` — the npm package's `verifyEvidenceBundle` — re-digests every served artifact, refolds the manifest, checks the marker, adopt-verifies each entry into the signed head and verifies the note, each rejection naming its step. The frozen `bundle_json` is served verbatim on every download. Share tokens (`oxe-…`, stored as SHA-256, answered once) open `GET /api/v1/evidence/{token}` session-free, carry an expiry and an optional `maxDownloads` cap spent atomically, revoke through `DELETE …/share`, and every dead-token case answers the same `410` while `downloadCount`/`lastDownloadAt` audit the live ones. The bundle's `scope` block states what it does not cover — supplier cost figures (redacted by default; `OXSUM_EVIDENCE_UPSTREAM_COST` opts them in, `costBasis` always exports), other organizations' rows, out-of-span history and in-flight operations. The v1 usage-report bundle verifies unchanged — version dispatch fails closed rather than misparsing.

Verification proves: this record was not altered after being written, history was not rewritten, for a versioned settlement the recorded charge agrees with the usage and rates inside the entry, and — when the bundle carries `price` — the price it names is the catalog row the operator attested; a usage report over an anchored span additionally proves it carries every settlement of that customer in the span, and a v2 evidence bundle proves every artifact its manifest commits is served and digests to what the ledger bound. It does not prove: upstream really returned that many tokens. The page states this plainly, without inflating it. The surface-by-surface map — including what the anchored proof covers and what stays a claim — is [proofs.md](proofs.md). (Shipped.)

### Credit notes (shipped)

A credit note (issue #352, decision D12) is the operator giving value back with a reason: `POST /api/v1/admin/organizations/{organizationId}/credit-notes` posts one ledger entry — Revenue debited, Wallet credited, the reason as its description — and records the note beside it; `GET` on the same path lists what was issued, newest first. A retried key answers the standing note, so a flaky connection cannot credit twice. It moves no money: no PSP is called, no deposit is reversed, and the admin card says "Credit notes" beside the adjustment form rather than merging the two. A refund (money back through the rail) and a chargeback (the bank clawing back) remain separate, PSP-dependent work.

### Deposit reversal — a refund through the rail (shipped)

`POST /api/v1/admin/deposits/{depositId}/reverse` reverses a `credited` deposit (issues #356, #360, decision D12): one ledger entry — Wallet debited, Cash credited, the operator's reason as its description — and the row flips to `reversed` naming `reversalEntryId` and `reversedAt`. The ledger key is derived from the deposit id, so a retry or double-click replays the standing entry rather than posting twice — one reversal per deposit ever. The wallet's own no-overdraft rule refuses a reversal the purchased pool cannot cover (`402 INSUFFICIENT_FUNDS`: the customer already spent the credit), a deposit that never credited is `409`, and an unknown one `404`; the `deposit.reversed` webhook event fires from the same transaction. The deposits view in the admin console carries the action with a required reason.

A reversed `hyperswitch`, `airwallex` or `stripe` deposit queues a separate durable external refund in the same transaction (#763, #1041, #1047). It pins the original merchant/version, provider payment, amount/currency and a body request identity. Recovery retrieves before creating and after a lost response; one leased attempt is spent before I/O, with at most five attempts. `refundStatus` is `unknown` until a matching provider handle is observed, then `pending`; only an exact authenticated refund-settled/refund-failed event establishes `succeeded`/`failed` (`refund_succeeded`/`refund_failed` on Hyperswitch, `refund.settled`/`refund.failed` on Airwallex). HTTP status, retry exhaustion and a local ledger reversal do not prove money was returned. Exhaustion stops automatic I/O while retaining the unresolved state; a late exact callback can still settle it. Historical rows preserve their original metadata and never gain a fresh create identity: known pinned handles are retrieval-only, and unbound outcomes require reconciliation. A rail with no refund path answers `refundStatus: null`. A `dispute_opened` webhook stamps `disputedAt` and fires `deposit.disputed` on Hyperswitch; accepting a dispute is a separate, deliberate reversal action.

### Order refunds, corrections and disputes (shipped)

The payment-order surface owns its own refund lifecycle (issue #608) — distinct from the legacy deposit reversal above, which stays for the deposit rails that predate orders. A refund on a `payment_order` is a *review*, never a grant the customer gives itself: `POST /api/v1/orders/{orderId}/refund-request` records `requested` with the amount asked back in credit-minor (`None` names the refundable remainder), `GET /api/v1/orders/{orderId}/refunds` lists the caller's refunds plus the `refundableMinor` it may still ask back. The request may not exceed the order's funded `amountMinor` minus what live refunds already reserve or returned — granted promo never counts — so no wallet can be refunded below zero.

Staff resolves under `RefundsWrite`: `POST /api/v1/business/refunds/{id}/resolve` approves or declines. Approve commits the durable `provider_pending` reservation plus an `issued` credit note (the order-linked correction on `oxsum.credit_notes`) *before* the PSP call — `refundId` is the durable request identity. New Hyperswitch calls put it in the required body field; other adapters use their documented idempotency mechanism within the bounded recovery window. The signed `refund_succeeded` webhook is the only terminal truth: it books the take-back (`debit Wallet / credit Cash`), applies the note with its entry, and lands `succeeded`. A `refund_failed` event releases the reservation and reverses the note. Recovery claims at most five attempts before I/O, with 1m/2m/5m/15m delays between attempts. An unknown result at exhaustion remains `provider_pending` with `nextRetryAt: null`, holding the bound and issued note for reconciliation. A first-attempt pre-creation refusal may release; a later refusal cannot disprove an earlier ambiguous result. New Hyperswitch operations pin body `payment_id`/`refund_id` and retrieve before create. Historical unbound operations require reconciliation; known historical handles are retrieval-only. A refund the customer already spent refuses the take-back at the ledger — the refund still lands `succeeded` (the PSP returned the money, that is fact) but the wallet never goes negative; the audit records the compensation owed. `GET /api/v1/business/refunds{,/{id}}` and `GET /api/v1/business/credit-notes` are the finance reads.

A signed `dispute_opened` event opens a contested record on `oxsum.disputes`, ending `won`, `lost` or `closed` at `POST /api/v1/business/disputes/{id}/resolve` (`evidenceDue` records the contesting state). Every new decision commits live staff authority, the exact customer's ledger/document effects, state and immutable audit together (#826). Unchanged retry keys are inert; changed request terms and different terminal outcomes conflict. Retained inconsistent ledger/audit history requires [source review](dispute-resolution-review.md), never implicit cash replay. A plan loss records complete observed cash against its original receivable, refund payable or explicit suspense. A new non-plan loss credits the entire authenticated Cash loss, taking back only the original Wallet credit still provably retained after all later settled debits and current reservations. Later top-ups cannot replenish that source. Consumed funds, prior credit repayment and unproven return history go to explicit review suspense; verified prior refunds consume their original source once, and a new atomic pending refund with no verified Cash entry consumes none; no old credit debt or paid document is guessed open. Exact partial cases are supported; missing/mismatched amounts/currency/case ID or fractional credit-minor conversion are retained as rejected or review-required inbox events. Bundle losses cancel unreserved source promotion atomically, and held releases burn under the closed-lot rule. A lost non-plan source cannot receive a new fulfillment grant or new refund approval. Historical wrong-direction chargebacks use the [exact local correction workflow](nonplan-chargeback-recovery.md) (#842); explicit statement repayment remains #777. Non-plan refund cash and exact historical recovery follow the source rules below (#841).

### Monthly statements (shipped)

Business staff inspect monthly wallet credit in the customer detail's **Monthly credit terms** card (#997). `reportsRead` permits inspection; `commercialWrite` (owner/finance) permits an explicit, reasoned approval of the total facility, future invoice payment terms and current optional monthly dunning grace. This facility is separate from sold-plan receivable caps. Credit, terms, immutable mutation receipt and actor audit commit together; a refused shrink below drawn/reserved credit changes none of them. Exact retries, including no-op limit changes, return their historical receipt without restoring old terms. Archived accounts reject new decisions. Terms changes do not rewrite issued documents, collect money or clear manual/archive restrictions; the existing dunning job evaluates grace separately.


Business reports readers can inspect a customer's monthly wallet-credit statements in a separately labeled customer-detail card (#1007). Each document distinguishes all usage charges from credit-funded charges, recorded repayment and remaining credit debt. Its original issue terms, dates, lines and proof-window identifiers remain visible. The page displays elapsed unpaid due dates as overdue without changing stored history. Reads use the recorded payment projection; monthly generation/finalization/repayment through Business authority remains tracked by #1006, separately from sold-plan statements.

An organization on a credit limit settles monthly (issue #124, the second half of roadmap P3-2). The platform issues one statement per organization and UTC `YYYY-MM` period — the month's settled usage itemized by channel and model, with how much of it the credit line carried — and a statement exists only for a month with usage. A daily `statements` job (issue #166) closes the month that just ended on every billable organization's ledger and generates its draft — doubling as the late-usage pickup, since both calls are idempotent — while issuing the document stays the operator's call.

- The lifecycle is `draft → finalized`: the platform generates a draft (and regenerates it as late usage lands), then issues it — finalizing locks the lines and totals, snapshots the organization's `paymentTermsDays` into the due date, and pins the ledger window (`logFromIndex`/`logToIndex`) the lines prove, so the document is anchored to the verifiable log. Drafts are the platform's working documents and never appear to the organization.
- Payment standing is `pending → paid | overdue | suspended`: a pending statement flips to `overdue` lazily when it is read past its due date, suspension is the platform's standing for a bill unpaid past grace (a suspended bill still pays), and `paid` the ledger derives, not an allocation table — every credit-line repayment, however it arrived (a top-up, a redemption, a recorded statement payment), settles the oldest outstanding draw first, so a statement's `paidMinor` is a fact of the ledger and self-heals on every read and every money-in. An organization the admin opted into dunning (`dunningGraceDays`, issue #258) has its *spend* suspended by the same daily pass when a statement stands unpaid past `due_date + dunningGraceDays`, and reinstated once nothing unpaid stands past grace — the document's own standing moves to `suspended` alongside.
- The organization reads its issued statements through `GET /api/v1/statements` and `GET /api/v1/statements/{statementId}`; the platform's half of the lifecycle lives under `/api/v1/admin/statements` (generation, detail, finalization, payments, suspension). `PATCH /api/v1/admin/organizations/{organizationId}` sets `paymentTermsDays`, snapshotted at issue. Paying is just funding the wallet — there is no organization-side "pay this statement" call, because a repayment always lands on the oldest debt first.
- A finalized statement also downloads as a rendered PDF (`GET /api/v1/statements/{statementId}/export.pdf`, admin twin `/api/v1/admin/statements/{statementId}/export.pdf`, issue #205): the header, the itemized lines, the totals, and a footer carrying the proof window beside a QR to `/verify` — the paper pointing back at the checkable record. Rendering is pure Rust inside the single binary (`printpdf`, standard-14 fonts, the QR drawn as vector rects — no headless browser, no embedded typst). Only a finalized statement exports — a draft answers 404 — and the document renders the stored lines, so the PDF can never disagree with the API's own answer.
- A finalized statement is anchored to the transparency log (issue #221): finalizing appends a marker entry to the organization's ledger whose document hash is the *window fold* over the covered entries' `contentHash` bytes in log order (`logFromIndex`–`logToIndex`), and whose description carries the *rollup* digest over the statement's own figures and lines. The detail endpoint answers the anchor (`anchorEntryId`, `anchorLogIndex`, `windowHash`, `rollupHash`, `origin`, and on a signing deployment the operator's `note` over the head right after the marker with `keyName`/`publicKey`/`keyHash`). `oxsum_verify::verify_statement_anchor` re-runs the check a bill's reader needs: the covered entries fold to `windowHash`, the statement re-serializes and digests to `rollupHash`, and the signed note attests the head at exactly `logIndex + 1` — a dropped, inserted, reordered or repainted entry fails with a named reason.

## Pages

### Organization view (logged-in users)

| Page | Content | Who sees it | Status |
| --- | --- | --- | --- |
| Overview | Available balance, the frozen total and this month's spend; in-flight requests with their frozen upper bound and live streaming progress | All members | Shipped (`web/src/pages/Overview.tsx`). The three figures read from the organization's ledger through `/api/v1/balance` (`reservedMinor`, `spentThisMonthMinor`): the frozen total is the wallet's reserved balance — the sum of the outstanding holds — and this month's spend is what settlements charged on or after the first of the current UTC month, so a top-up is not spend and an outstanding hold is not either. Progress is forwarded characters, not tokens (docs/decisions.md). |
| Chat | Top up, pick a model and chat; each turn's billing live, with a verify-this-bill link | All members | Shipped (`web/src/pages/Chat.tsx`) |
| Requests | The organization's settled gateway requests, newest first: booking date, request id, model, key, status (the settlement kind the bill records), input and output tokens, and the charge in credits. Filterable by key and by model, with the filters in the page's URL. A search card runs the request log — every request settled *or* refused — as a query over key/model/channel/time/status/charge | All members; a member sees only the turns their own keys paid for | Shipped (`web/src/pages/Requests.tsx`, `GET /api/v1/billing-records`, #55; the request-log search on `GET /api/v1/requests`, #505). Every row is read from the settlement entry the gateway wrote, so the numbers on it are the ones the entry's content hash covers; a turn still in flight has no settlement yet — the overview lists those live |
| Bills | The organization's transactions — top-ups, adjustments, settled requests — with the date booked, the signed amount in credits, the detail and the content hash, newest first; a per-row verify link; CSV and JSON export of the same list, carrying the amount as the ledger's signed integer | Every member, scoped to what their own keys paid plus the shared organization history | Shipped (`web/src/pages/Bills.tsx`, `GET /api/v1/bills` + `/entries/{id}/proof`, `/app/bills`) |
| API keys | Create, revoke | Members manage their own; admins manage all | Shipped |
| Members | Members, roles, join dates; invite, remove, change role and transfer ownership | List readable by members; management by owner/admin | Invitation creation and explicit recipient consent replace direct addition. Existing account/key/ledger identities stay unchanged; link delivery is optional |
| Transaction log | The organization's newest ledger entries with their content hashes | All members | Shipped, but not part of the original spec; the bills page is now the settled, exportable view of the same ledger |

Request-log HTTP status records the gateway response selected for a turn, separate
from its accounting outcome. Failures before a response carry the returned error
status; an SSE response already started retains 200 even if the body later fails.
Cancellation before a response status is selected leaves the field absent.
Historical operational records and immutable ledger evidence are not rewritten.

### Platform admin

The platform admin surface is the `/api/v1/admin` REST API under the operator token (`OXSUM_ADMIN_TOKEN`), and the React admin console at `/app/admin` drives it directly from the browser — the token is typed in once, kept in `localStorage` like the chat page's key and sent as `Authorization: Bearer` (docs/decisions.md). The console's sections are shipped, and so are the adjustments and signup bonus the first needs.

| Page | Content | Status |
| --- | --- | --- |
| Organizations | Every organization's balance; top up, adjust | Shipped: the list paginates a hundred at a time (`?limit=`/`?cursor=` on `GET /api/v1/admin/organizations`, a Load more on the page), plus a per-row adjust form — `POST …/{organizationId}/adjustments` for the write |
| Channels & prices | Configure upstreams, model prices and fallback chains; view price version history | Shipped: `/admin/channels` on the `/api/v1/admin` endpoints — list, create or repoint a channel, append a price version, read a channel's whole history, and edit an ordered model fallback chain |
| Guardrails | Named policies (keyword, PII, moderation) and the channel/key bindings that run them | Shipped: `/admin/guardrails` on `GET`/`PUT`/`DELETE /api/v1/admin/guardrail-policies[/{name}]` and `…/guardrail-bindings[/{scope}/{target}]` — policy CRUD for all three kinds, binding replacement in run order for channels and keys, a write-only moderation bearer, and the pre-call / post-call / streamed boundary in the page copy |
| In-flight requests | Every unsettled hold, globally | Shipped: `/admin/in-flight` on `GET /api/v1/admin/holds` — the sweeper's watch table joined to the organizations |
| Anomalies | The settled turns that did not price cleanly, summarized per channel | Shipped: `/admin/anomalies` on `GET /api/v1/admin/anomalies` — the `capped`, `estimated`, `client_cancelled`, `swept` and `unpriced` settlement records, read back from the ledgers themselves, each scored 0–100 into a `low`/`medium`/`high` severity band (`?severity=`/`?sort=score`, issue #263), with a per-channel count and charged total |
| Closing | Monthly closing; after closing, that month accepts no new entries and a closing record is produced | Shipped: `/admin/closing` on `GET`/`POST /api/v1/admin/closings` — the close runs as a claimed `close-month` job off the request path, the call answers its `jobId` and `GET /api/v1/admin/closings/jobs/{jobId}` reports the pass's per-organization progress while it walks (issue #1090); sealing commits doubleentry's period seals (the log's tree head, the closing trial balance, chained onto the seal before it), and the sealed watermark then refuses any entry dated into the month |
| Reconciliation | The drift between the ledgers and the projections that describe them | Shipped: `/admin/reconciliation` on `GET /api/v1/admin/reconciliation` — one entry per drift class (usage rows without their settlement entry, settled turns without a usage row, credited deposits without an entry, amount mismatches and stuck deposits, orphaned hold watches and unwatched holds, non-dense logs, dead job rows, and platform upstream-price drift — issue #222), each with a count and a bounded sample; read-only, it never repairs |
| Requests | Every request across every organization, settled or refused — the operator's all-orgs read of the request log | Shipped: `/admin/requests` on `GET /api/v1/admin/requests` (issue #574) — filters for organization, model, channel, status, minimum charge and a time window, cursor paging, and a proof link wherever the row names an entry; the member endpoint's row shape plus the org each billed |

Every mutating call on the admin surface is also audited (issue #160): one append-only `oxsum.admin_audit` row per write — the action, its target, the request's safe fields (never a credential) and its idempotency key — readable newest-first through `GET /api/v1/admin/audit`, cursor-paged, and listed in the console at `/admin/audit` (when, actor, action, target and detail). Like margin, the page shows the newest page and the API adds the narrowing (`?action=`) and the cursor.

### Business staff console

The staff surface is the `/api/v1/business` REST API under a personal session's business context, and the React console at `/app/business` (issue #615) drives it directly — the sidebar shows the item only once `GET /api/v1/session/context` answers a business principal, and opening the console switches the session into business context so a staff member never calls staff APIs under a customer context by accident. Sections group by job — operations (setup checklist, customers, payments, refunds, disputes), receivables (statements, aging, reports, the reconciliation-exception work list), catalog (channels, prices, plans) and access (staff, integrations, audit) — and each hides behind the `BusinessCapability` it needs; the API's `require_capability` is still the guard, the nav is the convenience. Every mutating control takes a reason and a fresh idempotency key and lands an immutable `oxsum.finance_audit` row the audit section reads back through `GET /api/v1/business/audit` (`ReportsRead`, `?action=` filter, `after`/`limit` keyset paging) — the one read the console added, since `finance_audit` was write-only before it.

## Phase C: chat page (shipped)

- The chat page lives in the dashboard (`/app/chat`), behind the login session. Checkout requires a personal session and verified payment/fulfillment; it never grants self-declared credit. The chat itself goes through the gateway, which takes an API key only (#17) — so the page mints a key named `chat` and keeps it in the browser's local storage. The user never handles it by hand, but there *is* a key on the wire: product.md's earlier "no API key needed" is revised to match.
- Conversations live only in browser-local storage; the server keeps nothing. Simple to build, and no duty to store users' conversations.
- Each AI reply shows that turn's billing underneath: the frozen upper bound and streaming progress live, then the settled charge — amount, settlement kind, token counts, price version. "Verify this bill" opens `/verify` with the bundle and the content hash prefilled.

## Historical scope note

Two short sections used to sit here ("Not in v1" and "Planned, in one place"). They described an earlier slice and are not the current boundary. Current behavior is the shipped sections of this file. Planned operator-launch work is the section near the top of this file and [the operator-launch contract](roadmap/operator-launch/CONTRACT.md). Calls the API refuses are [docs/unsupported.md](unsupported.md).

Read in that light: the gateway serves more than text chat (Anthropic Messages, Gemini, embeddings, rerank, and the task relay are specified under "Gateway"). Hyperswitch is a collection rail beside Stripe, Airwallex, and x402. Display-currency conversion is a view over integer credits; the ledger itself stays in minor units. Pagination of the hard-truncating lists shipped as #93. Later gaps are filed as issues, not collected in a single "nothing is unbuilt" sentence.

## Customer billing authority (#625)

Customers may inspect reservations but cannot create raw holds, select actual usage charges or release a gateway hold. Both raw mutation routes return 403, and the overview no longer offers the manual hold bench. Gateway and service-metering admission/settlement and historical hold recovery remain available under their existing trusted authority.

## Self-hosted customer identity foundation (#593)

Each deployment has one Business; existing organizations are its individual/team customer billing accounts. Migration preserves account IDs, wallet balances, ledger origins and signed evidence. Customer external IDs initially equal organization UUIDs. New organizations acquire business ownership in the same creation transaction; unused accounts do not create physical ledger schemas. A customer owner has no business staff authority. The business starts uninitialized with no staff; explicit authenticated setup is the dependent T03 workflow. The stored registration policy defaults to invite. Only the initialized business owner can open it; legacy `OXSUM_SIGNUP` cannot override it.

## Initial business ownership (#629)

The deployment administrator explicitly invites the initial business owner by email through a short-lived, single-use setup token. The named person accepts while logged in, or registers with the token on a new installation. Setup cannot be publicly claimed by an arbitrary customer owner and cannot run again after initialization. Setup records the business name and invite-only policy. Business staff sessions, role management and registration policy follow the explicit-context rules below.

## Business staff and explicit session context (#630)

Logins retain customer context by default. `POST /api/v1/session/context` explicitly selects `customer` or `business`; each use rechecks the live session and current membership. A business context cannot use customer ledger/team endpoints, and a customer context cannot call business operations. API keys and the deployment token cannot stand in for a staff session. Removing a customer membership never removes a staff role; a user without a customer seat can still log in and explicitly select business context. Personal verification, second-factor settings and account deletion accept a personal session in either context.

Owner appoints already registered users as admin, finance, support or viewer. Only protected ownership transfer assigns the owner role; it promotes an existing staff member and demotes the actor atomically. Owners must transfer before deleting their personal account. Staff removal immediately prevents subsequent business calls while preserving independent customer access. Password reset, logout and expiry invalidate staff access too. A confirmed second factor must have been satisfied by the session before business access; older/OAuth-only sessions need a new password-plus-TOTP login. Re-enrollment cannot replace a confirmed factor without first disabling it with a valid code.

Core uses explicit capabilities: owner holds all listed business capabilities; admin configures customers/catalog/routing and reads operational reports; finance handles commercial terms/funds/refunds and financial reports; support reads customers/requests and suspends customers; viewer reads redacted overview/reports. Only owner manages staff, ownership, registration, provider/merchant secrets and integration credentials. These are authorization primitives; resource APIs are delivered in their respective task cards, not automatically exposed through the deployment admin router.

Fresh and upgraded businesses are invite-only until an initialized owner selects `open`. Password, GitHub and OIDC account creation check saved policy inside the creation transaction. Existing users can still log in while closed. `OXSUM_SIGNUP` is retained as a parsed legacy value and cannot override the policy. Existing invitation, email verification and configured bot-check controls continue to apply. Public registration grants only customer membership, never a staff seat.

Staff, ownership, registration and context writes require idempotency keys. A repeated key with changed terms conflicts. Identical replay never reapplies an old change and still checks current authority; ownership transfer retry by the demoted former owner therefore returns forbidden. Security receipts and redacted audit rows commit with the change and cannot be edited or deleted.


## Customer profile and lifecycle (#633)

Business owner/admin can list/read customer accounts, change display name and billing contact, and archive/restore them. Support can read customers and set or lift manual suspension; finance can read but cannot change identity/profile/lifecycle, and viewer sees no customer details. Every lookup scopes the account to the selected business. Stable external IDs initially equal organization UUIDs and cannot be changed or reassigned; business customer invitations assign their reference before acceptance. Names need not be unique.

Archive is a reversible admission restriction, not deletion. Keys, memberships, tenant/ledger IDs, debt, balances, historical proofs and in-flight requests remain intact. Customer reads, settlement and incoming payments continue. New gateway/service admission checks archive as well as independent manual and dunning reasons. Restoring an archived customer cannot bypass overdue or manual suspension. Manual reinstatement cannot erase dunning, and dunning recovery cannot lift manual or archive restrictions. Each business change has a reason, idempotency receipt and immutable audit; no ledger posting is generated for profile or state changes.

Business owner/admin can invite a new customer by intended email, display name and stable external ID (#634). Creating the invitation reserves an identity but creates no ownerless account. Acceptance creates the original reserved organization/profile and initial customer owner together. Live invitations and existing customers cannot reuse the external ID; expiry/revocation frees only the pending reservation. Existing users retain their original personal account. All invitation transitions and immutable audit/receipts share a transaction. T04 requires both #633 and #634 to integrate.


### Consumption credentials (#637)

Existing consumption keys keep gateway access and their own usage/status/proofs. They cannot administer credentials or customer accounts, initiate checkout/redemption, inspect customer-wide financial history or set up outbound webhooks. Customer balance and budget reads require a personal session. Response headers and funds-refusal messages contain no customer-wide balance. An entitlement preview still reports the effective key/member/team admission headroom, never the account wallet balance.

Task polling/cancellation and cached responses belong to the exact submitting key. Reusing a gateway idempotency key with another consumption key, protocol/path model or body conflicts. Old unbound replay records conflict until expiry; old cache rows miss. No ledger history, key identity or proof format is rewritten. WebSockets recheck current session/membership before each delivery and periodically while idle; logout, membership removal or account/context switching ends old access.

The public `/verify?sample` example uses the captured public verifier fixture. It never selects a customer ledger by display name; choosing the demo name cannot publish customer bills (issue #637).


### Business provider configuration (#641, T06.a)

Business owners configure their own encrypted provider pools and billing credentials through current business sessions. Only the owner can alter the connection destination or protocol; owner/admin may manage the catalog, cache and routing configuration. These roles can read redacted provider metadata. Finance/support/viewer cannot read provider configuration; they retain separate customer/report capabilities. Every business configuration mutation records the current actor/reason with the configuration and an idempotent result in one transaction. A save never makes a provider call. Endpoint changes require explicitly supplied provider keys and clear the previous billing credential. Historical channel/price identities and in-flight snapshots stay stable. T06.b supplies the destination policy and explicit connectivity checks below. The business console now supports owner connection/credential writes (#754), including an exact internal destination grant, add-before-remove rotation, billing credential replacement/clearing and a separate authenticated HEAD check. The channel list uses the same catalog-read capability as the API; only owners see secret writes and checks. Failed unchanged submissions reuse their idempotency key while the dialog remains open; editing starts a new intent. Drafts and secrets are not persisted by the app and are cleared on success or close. The set-by-name connection operation explicitly warns about replacing an existing credential pool. N04 #818 exposes owner/admin model versions, full retained history, relative route weights, scoped aliases, ordered fallback chains and response-cache policy. Each model edit preserves optional/conditional billing rates and supplier cost as separate fields, uses exact integer conversion, and appends a complete immutable version. The existing catalog/routing capability checks remain authoritative; no UI save performs inference. Base channel billing rates can still apply when customer sale terms are absent, so they are not labeled as supplier cost.


Provider destinations default to public HTTPS; only the owner can approve exact internal IPs for a connection. DNS, redirects and credential forwarding obey the same policy across channel inference, tasks, catalog and bill sync. Explicit non-inference HEAD checks have audited intent and stable replay results. Task poll/cancel uses the originally admitted endpoint/key snapshot after later configuration changes. Historical proof/ledger identity is unchanged. Connectivity is distinct from certified provider/model support.

### Customer model authority (#644, T07.a)

Model lists and prices reflect the customer's actual tier and the consumption key's model/channel grants. Public aliases must also have permission for their resolved targets. Experiments and fallback chains can choose only authorized paths and keep the original requested name in billing evidence. Catalog reads need no provider decryption. Existing task history remains bound to its submitting customer/key after a later grant change; new tasks apply the current customer/key/group snapshot. Generic task aliases and signed mandates remain unsupported until T13 completes their price and operation binding, and are explicitly refused before send. T07.b completes cache request context.


### Customer plan standing guidance

Customer plan guidance distinguishes an ended paid period from overdue debt
using the authorized period bounds and credit standing. Plans and Billing may
show both notices: manual renewal and statement repayment remain separate.

## Customer sale terms (#599)

The Business Prices workflow uses the selected English or Simplified Chinese
console locale, including validation, schedule states and preview dimensions.
Translations change display text only; protocol values and exact amount parsing
remain identical.

Business staff can prepare reusable sale books, save immutable draft revisions, publish exact versions and schedule default or customer terms through the Business Prices editor or API. The reusable book is the commercial price group; an existing consumption request group still only restricts channels. A customer has at most one active book assignment. Each model resolves by customer override, assigned book, then default book, with missing models falling through and explicit zero prices retained. Supplier acquisition costs never appear in a sale term.

Publishing alone does not activate a version. Assignments/defaults pin published versions with nonoverlapping UTC `[from, to)` windows. A replacement ends the old schedule and starts the new one atomically; historical rows and previously resolved snapshots remain intact. One selected percentage discount applies to the full integer price numerator before a single upward rounding to minor units. Staff previews reject unknown/unpriced dimensions rather than promising a partial price. Owner/admin manage book contents/defaults; owner/finance manage individual customer terms. All mutations require a reason and an idempotency key with atomic audit.

This is the T08 configuration and preview foundation. The current gateway still settles its existing channel price snapshots; T09–T13 adopt sale snapshots for evidence and coordinated serving. The business console is T24 work. No provider-price choice or saved sale configuration alone changes the bills already being produced.

### Business merchant configuration (T14.a, #648)

A merchant belongs to the Business, with stable provider account identity, rail and test/live mode. **Direct Stripe is the active primary payment rail** — it is the only rail live-verified end-to-end on the deployment (capture→webhook→credited, plus refund recovery from #1047). Airwallex retains compatibility status. Hyperswitch is an **optional orchestration layer** deferred until multi-PSP checkout is actually needed: it is env-gated (`OXSUM_HYPERSWITCH_*` unset ⇒ the rail answers cleanly 503/404) and adds a six-service self-hosted switch, so a deployment that only takes one card processor carries no reason to run it. New merchants start disabled. Only the current business owner can create or rotate credentials and change checkout enablement. Owner/admin/finance/viewer can read redacted settings and immutable version history; customer accounts, machine credentials and support roles cannot manage this configuration.

Each rotation supplies a complete version, encrypts its API/signing credentials with `OXSUM_SECRET_KEY`, and selects it for future checkout. Prior versions remain immutable so an in-flight operation keeps its merchant/signing context. Disablement stops future selection without erasing callback history. Version status `locallyValidated` confirms only field, mode-prefix and destination-policy checks; saving performs no provider call and does not certify live or sandbox payments. No plaintext credentials or ciphertexts appear in read responses or audit.

The business Integrations page exposes owner creation, complete credential rotation and explicit enable/disable confirmation. Owner/admin/finance/viewer can browse paginated redacted settings, retained versions and events; integration-key controls remain owner-only. Secret drafts remain in transient memory and are cleared when the dialog closes or owner capability is lost. The page shows callback URLs for each retained version and distinguishes local validation from payment certification. The owner explicitly selects one enabled merchant per checkout rail or disables that rail, with a revision precondition and audit reason. An untouched rail retains its environment bootstrap for compatibility; explicit unavailable selections never fall back. Customers choose from safe available rail/name/test-live options. New orders pin the chosen merchant/version; retries retain the original SDK/signing context. Background refreshes never silently replace the selected route or an unresolved payment intent.

This configuration foundation does not redirect legacy checkout automatically. Scoped event intake and environment bootstrap are T14.b (#649); purpose-specific orders and customer checkout follow in T15/T16. Their completion is required before these settings collect customer money.

The staff business console authenticates through the context-aware session
identity. Customer balances, organization switching and billing sockets mount
only in customer context. Its **Customer portal** action explicitly returns
to an existing customer membership; a business-only identity remains a valid
staff session and is never redirected to login merely for lacking that seat.

Browser acceptance for the customer and staff surfaces is recorded in
`docs/roadmap/self-hosted-v1/evidence/closeout-browser.md`. The setup checklist
uses live, capability-filtered reads and follows its section links; denied
financial reports are hidden from support. Customer checkout polls the order
through fulfillment without remounting an unchanged payment widget. Narrow
tables preserve access to every column and action through local scrolling.

Verified payment callbacks durably mark orders paid. The leader-protected
`payment-fulfillment` job runs at startup and every five seconds (plus the
worker poll interval), recovering accepted events and draining up to 100 paid
orders through the existing idempotent fulfillment writer. Customer credit
therefore becomes available promptly without waiting for daily reconciliation.
An interrupted pass retries from durable state; the order-derived ledger key
prevents duplicate credit. Daily reconciliation remains an additional fallback.

## Persistent business identity (#756, N03.b)

The business owner edits name, public contact email, help URL, logo and accent
under **Business → Business profile**. All staff can read the effective profile;
only the explicit `profileWrite` capability can change it. The public branding
endpoint and both portal contexts resolve the same values. Contact/help are
public information, not SMTP credentials or proof of email delivery.

Before the first explicit save, an initialized business uses the name accepted
at owner setup, with environment logo/accent defaults; an uninitialized business
uses the deployment name too. A saved profile takes precedence for every field,
including nulls, so restarts or environment edits cannot silently undo a clear.
Full replacements require the expected revision, audit reason and idempotency
key. Exact replay returns the original save receipt after rechecking current
owner/session/second-factor authority. Changes do not relabel funded accounts,
change currency/conversion, or rewrite historical evidence and documents.

### Sale-book operator workflow (#731)

The Business Prices editor creates named books, saves an expected draft revision,
publishes a selected immutable revision, and separately schedules a default or
customer assignment. Finance can manage customer assignments and model overrides;
owner/admin can edit and publish catalog terms. It shows complete default history,
customer windows and effective UTC state, with confirmed end/cancel actions.
Customer preview distinguishes effective resolution from a draft comparison and
shows exact line items, selected terms and charge without provider traffic.
Unchanged retries retain an idempotency key; validation/network conflicts retain
the form. Exact decimal parsing refuses amounts the browser cannot represent as
safe integer minor units rather than rounding them.


### Collection currency and credit conversion

The owner configures collection at `/app/business#business/collection` (#765).
The persisted currency and exact amount paid for 1 credit determine new provider
orders and staff-recorded fiat receipts; customer display settings cannot alter
purchased value. Empty deployments may bootstrap once from `OXSUM_DISPLAY_*`;
explicit owner saves take precedence thereafter. A rate revision affects only
new intents. Existing orders and their proofs retain the original conversion.
Currency cannot be changed across committed payment history. Mixed historical
currencies are shown as requiring reconciliation and new collection is refused.
A configured conversion does not prove merchant readiness or PSP certification.


### Versioned daily plan accounting (#838)

New sold versions select policy 2 and show that policy on plan, period and invoice reads. Paid activation issues the paid invoice/statement and defers the original fee. Completed UTC service days release earned income; invoice issue and collection do not imply earned revenue. The original fee, version, authorization and exact service endpoints are immutable. Partial days use elapsed microseconds; the final endpoint receives any integer remainder. The hourly catch-up worker and close cannot recognize one day twice. Free complimentary v2 periods issue zero invoices and no money entries.

Ordinary v1 renewal retains period-close recognition. A new policy requires explicit version selection at the paid activation boundary. Refunds reduce the net fee and allocate the reduction between earned contra and original deferred fee using the same completed-day rule. They do not prorate entitlement. Disputes preserve service revenue and restore original collection debt; void removes earned/deferred into original debt/refund payable while leaving Cash unchanged.

Financial facts expose `billedMinor`, `earnedMinor`, `deferredMinor`, `collectedMinor`, `refundMinor`, `discountMinor` and `receivableMinor` separately. Deferred and receivable v2 figures are signed movements within a filter, or the cumulative position without a filter. Cash refunds count once; noncash promotional credits stay separate. See [API clocks](api.md#daily-plan-accounting-and-financial-clocks-838).

Application namespaces (#846) give an embedded developer a stable owner-issued
identity independent of runtime credentials. Each namespace has a bounded set of
service-managed accounts. Creating one does not create a person, a membership,
a staff role or funds. Separate applications may use the same external customer
ID without sharing keys, balances or records. Replacement scoped credentials can
recover original provisioning and metering operations; they cannot claim portal
customers by email or business identifier. Archival blocks new service admission
and keeps existing financial history. Verified human linking and the remaining
embedded key, checkout, evidence and notification surfaces remain under #747.

The owner portal exposes stable application namespace creation and explicit
integration credential grants. It supports application-only or named-customer
authority, distinct scopes, expiry and reason. It retains an unchanged submitted
operation for retry, displays secrets once and explains metadata-only recovery.
List state distinguishes expiry from revocation; no owner credential is delegated
to an external application's runtime.


The Bills portal retains customer-specific signed-head trust inputs and executes
the existing WASM signature/consistency verifier before reporting continuity
(issue #858). The initial operator key is a server observation, explicitly not an
independently authenticated identity. The original key/origin/head is preserved;
empty-head checks report signature-only evidence. Exports support separate offline
checking and do not expand entry, arithmetic, completeness or metering proof scope.


The customer Evidence portal (issue #863) exposes existing owner/admin-scoped
period and issued-plan-statement exports. Creation explicitly freezes the chosen
scope; retries preserve the immutable period identity. Raw bundle downloads and
separate expected inputs survive link expiration. Local checks distinguish
retained-key, bundle-supplied-key and unsigned evidence, bind the signed head to
the authenticated customer's ledger origin and preserve the existing proof scope.

### Actual non-plan refund cash (#841)

Wallet, bundle and legacy CreditLine-repayment refunds retain the full verified
provider cash return once. Only retained original purchased Wallet funding is
taken back; consumed, held, repayment-origin or uncertain amounts need finance
source review. Later top-ups cannot pay for an earlier order's refund. Original
CreditLine/receivable debt and entitlement are unchanged.

A refund's approval state and its actual cash effect are separate facts. A late
success after decline/failure/release retains the old terminal state and shows
confirmed cash plus review status. Unapproved returns do not become approvals.
Once real cash is recorded, the same refund cannot authorize another payout and
its amount remains consumed in the order's refundable bound. Confirmed returns
before fulfillment prevent later grants and remain in reconciliation.

Historical recovery requires current owner/finance authority, exact original
provider receipt and verified ledger source. It previews first and only applies
an unchanged reviewed source revision. A wrong-source old Wallet debit can be
restored without repeating already-recorded Cash; unsupported source is refused.

### Independent financial reconciliation (#848)

The Business reports page retains each comparison attempt and shows both source
assertions and independently verified ledger amounts. Operators can select an
older attempt, inspect its exact differences and see coverage and exclusions.
A source that is missing, corrupt, unsequenced, attached to the wrong original
order/period or otherwise unsupported cannot produce a clean comparison. Each
difference names the customer, source and particular fact; resolving or waiving
an operational exception does not change prior evidence or conceal a new drift.

The comparison covers plan billed, earned, deferred, collected and receivable
amounts plus confirmed refund Cash. Legacy v1 uses original-posting journal
cohorts; v2 uses signed movements under booking, service or payment clocks.
Statement lines and outstanding debt are checked against their original journal
and dispute sources, including mixed-policy statements. Payment lines must retain
their exact original order, refund-note or usage-correction allocation. A missing
historical ordering boundary between zero-money recognition and a refund or void
leaves the period's recognition schedule explicitly unverified and coverage
incomplete, even if the visible totals agree. Refund payable and
cash-loss suspense are retained separately and do not become income or debt.
Usage, entitlement, funding, costs and actual service delivery remain outside
this comparison and are explicitly listed as such. The result is a bounded
financial-source check, not complete business accounting or a claim about every
filtered number in the reports dashboard.

Application credentials may carry `customerKeysWrite` only when bound to a stable
application namespace. They can issue/list/revoke only that application's own
keys for its own provisioned customer accounts. Issuance requires explicit finite
cumulative spend, model allowlist, RPM/concurrency and an expiry no later than the
management credential and one year. No human membership, funds or pricing authority
is implied. The consumption key remains subject to ordinary customer/gateway
restrictions; archive/suspension blocks consumption and new issuance.

Creation returns a plaintext secret once. Exact application-scoped replay after
credential rotation returns the original metadata without the secret; changed
request/customer/action reuse conflicts. An ambiguous secret response is recovered
by retrieving the receipt, explicitly revoking that key and creating a new operation.
Management-credential revocation prevents future management, while issued keys
keep their separately recorded expiry and revocation. The bridge retains original
application/customer/issuer/key ownership, and operation/audit receipts contain no
secret. Applications cannot adopt or manage unrelated portal-issued keys.
### Gateway evidence capacity (#875)

The effective guardrail set is limited to eight distinct policies. A complete
settlement must fit its documented encoded-byte envelope before a call reserves
funds or executes. Long reasons remain auditable through an explicit digest and
byte length with separately retained full text; no shortened reason claims to
be the full provider/policy output. See [the evidence contract](gateway-evidence-budget.md).
### Historical non-plan chargeback compensation (#842)

An exact reviewed historical wrong-direction chargeback may be compensated with
an appended entry. It cancels the old false cash receipt and records the full
confirmed loss, which can exceed the old clamped credit repayment. Only proven
retained original Wallet funds the actual loss; other amounts remain finance
review suspense. The old credit benefit and debt/statement history stay intact.
Historical bundle promo remains unchanged with explicit `bundlePromoRetained`
review status; this correction does not cancel or reopen past entitlement.
The original lost outcome, audit and entry are retained beside `cashCorrection`.
Operators preview an exact dispute and authenticated receipt, review the source
revision and explicitly apply it under current owner/finance authority. Unsupported
or conflicting history refuses; no generic recovery grants new credit or debt.


### Guardrail refund and request fees (#899)

A whole-response guardrail refusal, including a cache hit, charges zero customer usage and zero fixed request fee and releases the entire wallet reservation and subscription draw. Streamed content already delivered remains billed; a successful zero-token answer still owes its configured request fee. A customer refund does not imply zero provider cost: retained provider usage remains on the actual attempt and prices operator cost; absent provider counts remain untracked. A refused cache hit makes no new provider attempt. Exact request replay retains the same receipt and does not repeat a charge or refund.
## Immediate first-call evidence

A customer's successful visible call can satisfy the request and bill steps
immediately, even with a zero wallet charge. The checklist uses scoped canonical
receipt evidence rather than the last ten operational requests or a report share
link. Missing, incomplete and failed checks never mark the steps complete.
The verification action opens the actual entry's prefilled proof; it does not
claim independent trust merely because the server supplied a matching hash.
Closed-period exports remain a separate workflow with their own authority and
UTC boundary. Model/key/funding readiness remains tracked in #901.
### Application entitlement and customer sale reads (#898)

An owner can explicitly grant application-only `entitlementsRead`. Existing
credentials gain no permission. `GET /api/v1/integration/customers/{organization_id}/entitlement`
returns recorded customer restrictions, sold-plan lifecycle, quota window/counters
and shared plan debt. Recorded remainder includes pending draws and is never a
promise of admission after cancellation, expiry or a restriction.

`GET /api/v1/integration/customers/{organization_id}/models` filters configured
public models by current customer, alias-target and optional application-issued
key/group permissions before pagination (`limit` 1–50, default 20; `after` is the
last public name). `keyId` must be live and belong to this exact application and
customer. Effective published customer sale terms use override, assigned-book,
then default precedence. `saleTerms: null` means unavailable, without substituting
wholesale channel prices. Provider names, destinations, secrets and costs stay
private. Every page chooses its own database snapshot.

These reads never reserve, roll quota, refresh dunning, publish a sale digest,
write a bill or contact a provider. They show current recorded/configured facts;
actual request admission remains authoritative. Personal sessions and consumption
keys cannot use these application management routes.

### Verified personal ownership of application accounts (#920)

An application may offer portal ownership of its own unclaimed account using an
explicit `customerLinkWrite` grant. The recipient must already have a personal
identity, independently verify the exact intended email, satisfy current second
factor policy, review the named application/account and explicitly accept.
Acceptance adds the account's first owner without moving its ledger or funds,
creating keys/staff, changing selected session context or sharing other accounts.
The application's explicitly scoped account and own-key permissions remain;
the consent page discloses this before acceptance. Archived/claimed accounts
refuse a new invitation. Terminal consent is history, never a membership repair.

### Application evidence reads (#924)

An owner may explicitly grant application-only `evidenceRead` for retained evidence
of accounts created in that application's namespace. Listing descriptors and
downloading customer-safe frozen version-2 bundles are read-only. The application
cannot mint a bundle, share token or new ledger entry with this grant. Archived
accounts retain historical reads. Credentials are rechecked on every request.
The original bytes, manifest scope and trust limitations are preserved. Exports
containing opted-in supplier costs, malformed identities/proofs or more than
16 MiB refuse with an explicit recovery error; the download never redacts signed
content or treats an internal consistency check as independently trusted evidence.
## Named plan-statement repayment (#777)

The personal customer can review a named issued or past-due plan statement and
pay part or all of its available debt. Exact verified journal slices determine
the credit-minor amount. Fiat rounds upward once under pinned collection terms;
inverse conversion never changes the debt allocation. A quote does not reserve
or call a provider. Creating an order reserves the amount atomically, and the
quote lists existing orders for recovery after refresh. Uncertain provider
results keep the reservation; authenticated terminal failure/expiry releases it.

`statementRepayment` settles Cash/Receivables only. Legacy `invoiceRepayment`
continues settling drawn CreditLine and does not pay a named plan statement.
Repayment never grants wallet credit, quota, a renewal or income. Clearing debt
recomputes only debt-based suspension; manual restrictions, archive and plan
cancellation retain their meaning. A late payment after release retains Cash
as source-linked RefundPayable with an explicit review result.

Refunds and lost disputes reverse only the original repayment allocation on
the exact statement. They retain full observed Cash; amounts exceeding retained
original funds become reconciliation suspense. A financial void reserves each
repayment-funded payable for that original payment so old plan-order returns
cannot consume it. Source-exclusive original/follow-up documents can void
together. Mixed statements use a source-specific journal void after verified
source review; the whole-statement void remains a refusal. Mixed-source repayments
and their refunds/disputes are supported through exact journal slices.

### Source-specific mixed statement corrections

Finance may void a verified original journal even when its chargebacks share a
statement with other journals. Only that journal's charge and payment lines get
exact opposite lines; other sources keep their balances, payment eligibility and
dunning. The original lines, repayment effects and prior exports remain retained.
Cash never moves during void; collected funds become source-owned refund payable.
A live payment intent anywhere on an affected document blocks correction until
its provider result is resolved. Invalid or reissued history still needs explicit
financial review. This does not authorize new replacement charges or entitlement.

### Application wallet and bundle checkout (#930)

An owner may explicitly grant application-only `checkoutWrite`. The application
quotes a fiat wallet top-up or published bundle for its own customer, shows the
server's exact purchased/promo amount, merchant name and test/live mode, and
creates the reviewed checkout. Money is credited only by verified payment and
normal fulfillment. No staff, manual-receipt, refund-approval or merchant-secret
authority accompanies the grant. Assigned plan activation is described below;
named statement debt checkout remains a separate required N21 follow-up.

Every application order retains its original quote and return/cancel URLs in the
order transaction. Identical retries and rotated credentials recover the same
order/provider identity. Application operation keys cannot collide with portal
keys, and checkout reads include only that application's created orders. Archive
blocks new/resumed checkout while retaining historical reads and valid late
provider callbacks. Revocation/expiry and namespace are rechecked per operation.

### Application plan-period checkout (#931)

An explicitly granted application's `checkoutWrite` can purchase the customer's
already assigned payable plan. It cannot assign a plan or change its price. The
quote includes the exact sold fee, version and period; fiat rounding changes only
the collected currency amount. Purchased/promotional wallet amounts are zero.
Verified payment alone authorizes the existing plan activation path. Cancellation,
scheduled changes, early renewal and complimentary or foreign plans refuse.
One outstanding intent per period is shared with portal checkout.

### Application evidence publication (#934)

The owner may explicitly grant application-only `evidenceWrite` to request one
ended span (at most 31 days) or an owned issued statement. `evidenceRead` remains
a separate download/list permission. An immutable publication request records
current active-customer authority before evidence work; identical retries and
rotated credentials recover the original deterministic scope. Later revocation
blocks responses and future requests but cannot retract already accepted work.
Supplier costs are excluded. Private or oversized existing captures refuse;
original signed bytes and ledger markers are never rewritten. No share token or
financial operation is granted.
### Application payment of named debt (#935)

An explicitly granted application can quote and pay an existing statement belonging
to its own customer. It may select a partial exact debt amount or all unreserved
verified debt, without assigning prices or altering original source allocations.
This is the existing statement repayment purpose: it neither adds wallet credits
nor activates/renews a plan. Paid/refund/lost outcomes keep the same journal and
statement evidence, including mixed historical/daily policy sources. Checkout
recovery and archive boundaries are identical to the application order lifecycle.

### Application customer statement history

An application with explicit billingRead can discover and display its own customers'
issued, paid, past-due and void plan statements, with complete customer-visible
source-linked lines. Draft/open statements stay private. Historical reads remain
available after account archive under current authority. Named repayment still
requires its own checkoutWrite grant and a fresh server quote; reading a balance
or statement does not authorize collection or activate service. Oversized retained
documents need operator recovery instead of a silently incomplete history.
## Immediate first-call evidence

A customer's successful visible call can satisfy the request and bill steps
immediately, even with a zero wallet charge. The checklist uses scoped canonical
receipt evidence rather than the last ten operational requests or a report share
link. Missing, incomplete and failed checks never mark the steps complete.
The verification action opens the actual entry's prefilled proof; it does not
claim independent trust merely because the server supplied a matching hash.
Closed-period exports remain a separate workflow with their own authority and
UTC boundary. Model/key/funding readiness remains tracked in #901.

## Personal customer getting started

The customer checklist reads visible live keys, permitted configured chat
models, effective customer sale terms and a bounded sample funding snapshot.
It requires no deployment token. Free and fully quota-funded samples can pass
with zero wallet funds; pending reservations, current key/member/team caps and
manual/account/plan restrictions still apply. Sample selection, limits and
uncertainty are visible. Missing sale terms, a missing ledger or a period/expiry
transition never masquerade as readiness. First-call evidence remains a separate
actual successful receipt; no readiness read sends a provider request.
### Signed lifecycle signals for external applications (#937)

The owner configures and rotates an application's receiving endpoints in Business
application access. Runtime needs only separately granted machine reads; an
application cannot choose an internal destination or recover a signing secret.
Public HTTPS is the default. An owner may explicitly approve exact internal IPs
for one fixed endpoint, including isolated development receivers.

Committed customer/key/plan/payment/statement/evidence changes enqueue a durable
identity signal in the same transaction. Foreign portal order identities are not
disclosed: shared-balance changes name the customer instead. Notifications do not
assert ordered state or financial completion; duplicate/out-of-order receivers
read current authoritative facts. Disabling an endpoint stops new and unclaimed
work; already-claimed sends may finish. Secret rotation uses a replacement.
Credential rotation/revocation is separate from owner-authorized endpoint delivery.
A new endpoint has no historical replay guarantee; initial reads and time-based
state rechecks remain necessary. Local fixtures do not certify public delivery.

### Standalone application adoption example

The bundled external example owns a separate SQLite application store/process and
uses the owner's existing scoped grants for signup, key lifecycle, checkout,
current statement state and evidence. The customer UI in that example is a local
command interface plus a private loopback checkout page, with a separate signed
notification receiver. It demonstrates exact operation recovery and separately
retained trust; it is not a public multiuser authentication server. Genuine PSP
certification, browser acceptance and deployment of an exact candidate remain
separate from its isolated local acceptance fixture.

## Manual receipt refund boundary

A manually recorded payment never passed through a payment provider. Its order
therefore offers no automated provider refund: contact the business to reconcile
an out-of-band return. New requests and approvals without complete pinned
provider coordinates return `409` before reserving a refund or issuing a note.
Retained manual requests remain readable and may be declined while requested;
old pending records stay unresolved for financial review, without provider retry
calls. Their approval/note state does not confirm that cash was returned.


Monthly statement issuance (#1009) commits the proof anchor, original issued
figures, lines and finalized notification together. A failed issuance leaves the
draft and no new anchor; repeating a successful issuance returns the original
statement. A retained legacy anchor that disagrees with current source inputs
requires explicit recovery and cannot silently become a different bill.

Sold-plan period consumption reads the matching quota-engine window, including outstanding reservations and released unused amounts. Closed windows retain their own consumption; new periods do not replace historical counters. The read does not post money or rewrite authorization history.

The Business dispute resolution dialog requires an explicit provider outcome: evidence due, won, lost or closed. Each outcome explains its financial effect. The exact decision, reason and idempotency identity remain fixed across ambiguous retries; the dialog closes only after a successful response.


Business owner/finance can generate and issue monthly credit statements from the
customer's separate Monthly statements card (#1011). Generation is a working
copy for a closed UTC month. Issuance confirms the displayed usage, credit draw,
lines and payment terms; changed data or UTC review day requires renewed review.
The reason and actor are audited atomically with issuance. Exact retries retain
their original result, including empty months. Verified monthly repayment uses
the separate #1015 payment action; issuance does not record incoming money.


## Atomic recorded monthly repayments (#1013)

A deployment-recorded monthly payment names one issued statement and preserves
FIFO credit repayment, including earlier unbilled months. It cannot fund a wallet
or exceed unpaid credit draws through that month. Cash, credit restoration,
statement standing, events and source-bound audit commit together. Exact retries
remain valid after full payment; a changed amount or statement conflicts. Staff
portal repayment and explicit bank-reference collection remain tracked by #1006.




portal repayment and explicit bank-reference collection use the #1015 workflow.

## Staff monthly repayment (#1015)

Owner/finance can record verified incoming money from an issued customer's
monthly statement. Confirmation shows the customer, month, exact credit amount,
bank/payment reference and reason. This operation repays the oldest credit draws
through that month; it never pays a sold-plan invoice or grants wallet funds.
References share a global uniqueness domain with deployment manual receipts and
staff manual payment orders, so one source cannot fund another workflow.

A lost response retains the exact intent for retry. The original receipt remains
recoverable after full repayment or archival with current funds authority.
Customer statement reads reflect the updated FIFO standing; original usage
lines, terms and proof anchor remain unchanged. No external collection is initiated.

Chat retains a browser key only for its exact customer organization and signed-in user. The portal checks its live key identity and credential organization before reuse, ignores the legacy unscoped browser key, and isolates pending initialization across account switches. A changed session requires reloading Chat before sending.

The Business journal list permits FundsWrite staff to correct posted or paid journals. Paid corrections retain original payment ownership as refund payable; they do not move Cash or initiate a provider refund. The confirmation retains the exact journal, reason and idempotency identity through an uncertain response and row refresh.

Business staff explicitly confirms an initial complimentary plan grant without entering internal identifiers (#988). The portal retains the selected authorization and reconciles an uncertain response with the recorded customer plan and original period before retrying. Paid plan binding remains pending payment.

Sold-plan period consumption reads the matching quota-engine window, including outstanding reservations and released unused amounts. Closed windows retain their own consumption; new periods do not replace historical counters. The read does not post money or rewrite authorization history.

### Exact plan dispute cash (#1028)

A plan-payment dispute records the authenticated contested amount using the
original order's fiat-to-credit conversion, including partial cases. It never
substitutes the whole order amount. Missing case IDs, missing or mismatched
currency, out-of-range amounts and fractions of a credit minor unit require
review. Repeated case notifications do not increase the contested balance again;
changed case terms cannot overwrite the original. A lost case affects only its
recorded cash amount through the existing original-source accounting policy.
Historical incorrect cases remain visible for explicit review; no stored
financial history is rewritten by this fix.

### Source-linked invoice correction (#1027)

A personal Owner/Finance session can preview and atomically replace a proven
plan invoice with reduced or equal original base-fee lines. Existing credits,
sold version, service interval, currency and original due date are retained;
no new entitlement or payment is created. The immediate predecessor caps each
correction, and each invoice has at most one successor. Old collected funds stay
owned by their original refund-payable sources; the successor starts unpaid
(or paid at zero). Refund and collection are explicit separate operations.

Pending source payments, refunds/disputes, stale previews or unproven history
refuse. Exact actor-scoped retries return the accepted immutable result even
after subsequent payments or another correction. The portal retains unknown
outcomes across dialog closure/reload and links both document generations.
See [the specification](invoice-corrections.md) for supported accounting scope.
