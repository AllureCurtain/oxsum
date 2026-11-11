# J18 candidate rehearsal — 2026-11-11

Isolated rehearsal of the external-application journey against the frozen
candidate listener. Local stubs only — no real provider or PSP spend. Recorded
as rehearsal evidence, NOT J18 certification.

Candidate: ghcr.io/allurecurtain/oxsum:sha-cc16bb8fe5d6875ee2bb054e2c436cc788fc94ef
Verified: /api/v1/version = {gitSha: cc16bb8f…, version 0.1.0, schema 0131_invoice_corrections}

CONTAINER + DB
  docker network create j18-net
  docker run -d --name pg --network j18-net -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=oxsum postgres:17
  operator.env: DATABASE_URL, OXSUM_SECRET_KEY, OXSUM_HEAD_SIGNING_KEY (b64), OXSUM_ADMIN_TOKEN,
    OXSUM_SESSION_COOKIE_SECURE=false, OXSUM_DISPLAY_CURRENCY=USD, OXSUM_DISPLAY_RATE_PER_CREDIT=1,
    OXSUM_PUBLIC_URL=http://127.0.0.1:3001, OXSUM_SMTP_URL=smtp://127.0.0.1:56702, OXSUM_MAIL_FROM=j18@example.local
    (PUBLIC_URL requires the mailer trio; loopback SMTP vars satisfy the all-or-none rule — no mail sent)
  --migrate-only → schema migrated to 0131_invoice_corrections
  docker run -d --name oxsum --platform linux/amd64 --network j18-net --env-file operator.env
    --env OXSUM_ADDR=0.0.0.0:3001 -p 3001:3001 <image> → /readyz 200 ~3s

LOCAL STUBS (no real provider/PSP spend)
  stubs.py: provider POST /v1/chat/completions → fixed whole response;
            payments POST /payments → requires_payment_method + fixture secret, GET /payments/{id} → 404 HE_02
  bound 0.0.0.0 (container reaches host via 192.168.65.254, not 127.0.0.1)

OWNER BOOTSTRAP (one-time owner session only; runtime never uses owner creds)
  POST /api/v1/admin/business/setup-tokens (admin token) → oxsetup-…
  POST /api/v1/business/setup/register (token) → business b5dc292f…
  POST /api/v1/auth/login → session cookie
  POST /api/v1/session/context {idempotencyKey,context:business} → business context selected

OWNER CONFIG over /api/v1/business/*
  POST /business/channels {name:j18stub, baseUrl:http://192.168.65.254:57129/v1, protocol:openai,
    apiKeys, destinationPolicy internal 192.168.65.254} → channel saved (no network on save)
  POST /business/channels/j18stub/prices {model:j18stub, price{in:0,out:0,maxOut:32,costPerRequest:40},weight:100} → v1
  POST /business/collection-terms {expectedRevision:1, currency{USD,ratePerCredit:1}} → revision 2, source owner
  POST /business/merchants {name:j18-test-merchant, rail:hyperswitch, mode:test, accountId:j18-embedded-account,
    config{apiUrl:http://192.168.65.254:57130, apiKey, webhookSecret:j18-fixture-event, publishableKey,
           sdkUrl, destinationPolicy internal 192.168.65.254}} → merchant+version
  POST /business/merchants/{id}/status {enabled:true} → enabled
  POST /business/checkout-routes {rail:hyperswitch, expectedRevision:0, merchantId} → route wired
  POST /business/applications {key:j18-embedded, name, customerLimit:10} → app ae30f86f…
  POST /business/integration-credentials {applicationId, scopes:[10 J18 scopes], expiresAt} → oxs-int-e1a2…
    (v1 secret lost mid-capture → revoked, reissued as v2 — idempotencyKey conflict on changed terms
     is itself an honest refusal: CONFLICT "used for different security terms")
  POST /business/plans {name:J18 evidence plan, priceMinor:1000000, USD, quota:2000000, anniversary} → planVersionId
  PUT  /business/customers/{org}/plan {planVersionId} → customerPlanId pendingPayment
  POST /business/applications/{id}/notification-endpoints {url, destinationPolicy internal, events:[7]}
    → endpoint + one-time whsec- secret (v1 loopback unreachable from container → disabled,
      v2 registered at 192.168.65.254)

APP DRIVE (examples/embedded-application/app.py; OXSUM_APPLICATION_CREDENTIAL + APP_ID + APP_DB + BASE)
  signup alice → org 2452df9b… ; retrieve → mapping; read models → [j18stub]
  key alice-key → keyId 943a9a22… (bounded: 5 rpm, 1 hold, model allowlist, 1-credit limit)
  checkout alice-wallet (walletTopup, USD 100) → order …provider_pending
    (first attempt pinned merchant v1 — apiUrl 127.0.0.1 from container = itself → 503;
     immutable pin by design; new operation created order on v3 — honest fixture artifact)
  checkout alice-wallet-v2 → order 56494bde provider_pending; stub POST /payments logged
  WEBHOOK pay: POST /api/v1/webhooks/merchants/{mId}/versions/{vId}
    body {event_id,event_type:payment_succeeded,merchant_id,livemode:false,
          content.object{payment_id=order,amount:100,currency:USD,merchant_id,metadata{payment_id,order_id,organization_id}}}
    X-Webhook-Signature-512 = hex HMAC-SHA512(body, "j18-fixture-event")
    → 200; inbox applied; order fulfilled; wallet credited
  ask alice-question → 200, x-oxsum-charged-minor 40, x-oxsum-request-id …
    (stub provider logged the prompt; no real provider/budget)
  trust alice → org + origin oxsum/ledgers/<tenant> + operator key {oxsum/tree-heads, pubkey, hash 0f0e8552}
  receipt alice-question → /tmp/j18-candidate/receipt {manifest, settlement-bundle, signed-head, operator-key}
  plan-checkout (planPeriod, customerPlanId) → order ebfae810; webhook pay → fulfilled → statement issued+paid
  read statements → statement 4df94063 state paid
  publish alice-statement-export {kind:statement,statementId} → bundle 4d8b7b1f published
  evidence bundleId → /tmp/j18-candidate/evidence {evidence-manifest, evidence-bundle, operator-key}
  archive alice {reason} → customer state archived; entitlement read → archived
  revoke-key alice-key → fires key.changed → endpoint delivered (204) → receiver reconciled keys snapshot

OFFLINE VERIFY (candidate's own SDK tarball, clean consumer dir)
  npm install verify-oxsum-bill-0.1.0.tgz (from /tmp/oxsum-candidate-cc16bb8f bundle artifacts)
  node verify.mjs receipt/    → PASS: retained receipt hash, charge, catalog bindings and signed origin
  node verify.mjs evidence/   → PASS: retained customer, bundle digest, manifest and signed origin
  node verify-negative.mjs …  → PASS: nine offline tamper, scope and missing-trust rejections

RETRY / RECOVERY
  bob signup → key → checkout → webhook pay → ask via Lost transport (OSError after server settle)
    → attempt1_error retained; exact retry → idempotent-replayed:true, SAME answer, ONE charge
    (usage_records charged_minor=40 count=2: one for alice, one for bob — no double charge)

NEGATIVE GATES
  ask after archive → HTTP 401 refused (archived customer cannot consume)
  wrong-namespace: foreign app credential (billingRead) on alice org → 404 billing / 404 customer
  revoked credential reuse → 401 on every route incl. provisioning
  duplicate/out-of-order notification → receiver dedups by delivery ID; endpoint-1's undelivered
    event cannot reopen archived state; delivered key.changed only refreshed current snapshot

FIXTURE / BLOCKED (recorded honestly)
  merchant v1 pinned at create-time apiUrl — first order is a dead artifact (immutable pin by design)
  endpoint v1 loopback URL unreachable from inside container — disabled; v2 at host gateway used
  owner portal UI approval step for loopback endpoint — bypassed via explicit destinationPolicy
    internal+allowedIps in the HTTP payload (the API's documented internal-approval path)
