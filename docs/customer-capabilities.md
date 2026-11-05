# Customer capability and evidence inventory

Contract: OpenAPI 0.109.0, issues #637/#638 (T05) and #644 (T07.a). The exact method/MatchedPath matrix in `crates/server/src/auth.rs` fails closed for every unclassified customer route or method. Authentication resolves the current membership for each request. Business and deployment credentials use separate gates; customer roles grant no business authority.

## Record policy

A consumption key means the exact authenticated key. A member's **scoped history** includes records attributed to keys they created, including revoked/expired keys, plus shared unattributed history. Owner/admin sessions read the selected customer's full history. Restricted readers cannot read any ledger entry carrying a document payload: statements, usage reports, day anchors and future unknown document kinds fail closed even through generic raw entries, proofs, bills and exports. A future key-scoped document type needs an explicit policy before becoming readable.

Out-of-scope key, entry, proof and task IDs return 404. Capability-denied customer routes return 403 before body parsing. Explicit Authorization, x-api-key or x-goog-api-key headers prevent cookie fallback even when malformed; the first present header wins in that order. Separate personal handlers reject explicit machine credentials themselves; malformed JSON may be rejected by their body extractor first.

## Customer and personal methods

Every row below names an actual OpenAPI operation; path variables use the contract's spelling. Personal sessions must select customer context for customer routes. Personal identity/TOTP/context operations use their own session checks and may work in either context. Existing owner/admin/member rules remain additional constraints.

| Method and path | Consumption key | Member session | Owner/admin session |
| --- | --- | --- | --- |
| `POST /api/v1/org/invitations/{invitationId}/revoke` | 403 | 403 | Owner/admin role rules |
| `GET /api/v1/device/request` | Denied | Personal session | Personal session |
| `POST /api/v1/device/authorize` | Denied | Personal session | Personal session |
| `POST /api/v1/auth/totp/enroll` | Denied | Personal session | Personal session |
| `POST /api/v1/auth/totp/confirm` | Denied | Personal session | Personal session |
| `POST /api/v1/auth/totp/disable` | Denied | Personal session | Personal session |
| `GET /api/v1/session` | Denied | Personal session | Personal session |
| `POST /api/v1/session/organization` | Denied | Personal session | Personal session |
| `DELETE /api/v1/users/me` | Denied | Personal session | Personal session |
| `GET /api/v1/orgs` | Denied | Personal session | Personal session |
| `POST /api/v1/orgs` | Denied | Personal session | Personal session |
| `GET /api/v1/org` | Metadata/estimate | Metadata/estimate | Metadata/estimate |
| `PATCH /api/v1/org` | 403 | 403 | Owner/admin role rules |
| `GET /api/v1/org/keys` | Own key | Own created keys | All customer keys |
| `POST /api/v1/org/keys` | 403 | Own keys; group assignment denied | All customer keys |
| `PATCH /api/v1/org/keys/{keyId}` | 403 | Own keys; group assignment denied | All customer keys |
| `DELETE /api/v1/org/keys/{keyId}` | 403 | Own keys; group assignment denied | All customer keys |
| `GET /api/v1/org/keys/{keyId}/mandates` | 403 | Own keys; group assignment denied | All customer keys |
| `POST /api/v1/org/keys/{keyId}/mandates` | 403 | Own keys; group assignment denied | All customer keys |
| `GET /api/v1/org/keys/{keyId}/entitlement` | Own key | Own created keys | All customer keys |
| `POST /api/v1/org/invitations` | 403 | 403 | Owner/admin role rules |
| `GET /api/v1/org/members` | Denied | Personal session | Personal session |
| `POST /api/v1/org/members` | 403 | 403 (retired) | 403 (retired) |
| `PATCH /api/v1/org/members/{userId}` | 403 | 403 | Owner/admin role rules |
| `DELETE /api/v1/org/members/{userId}` | 403 | 403 | Owner/admin role rules |
| `GET /api/v1/org/teams` | Denied | Personal session | Personal session |
| `POST /api/v1/org/teams` | 403 | 403 | Owner/admin role rules |
| `PATCH /api/v1/org/teams/{teamId}` | 403 | 403 | Owner/admin role rules |
| `DELETE /api/v1/org/teams/{teamId}` | 403 | 403 | Owner/admin role rules |
| `GET /api/v1/org/experiments` | 403 | 403 | Allowed |
| `PUT /api/v1/org/experiments/{name}` | 403 | 403 | Allowed |
| `POST /api/v1/org/experiments/{name}/end` | 403 | 403 | Allowed |
| `GET /api/v1/org/experiments/{experimentId}/results` | 403 | 403 | Allowed |
| `POST /api/v1/org/ownership` | 403 | 403 | Owner only |
| `GET /api/v1/org/subscription` | Denied | Personal session | Personal session |
| `POST /api/v1/org/subscription/checkout` | Denied | Personal session | Personal session |
| `GET /api/v1/org/subscription/periods` | Denied | Personal session | Personal session |
| `GET /api/v1/webhooks` | 403 | 403 | Allowed |
| `POST /api/v1/webhooks` | 403 | 403 | Allowed |
| `DELETE /api/v1/webhooks/{endpointId}` | 403 | 403 | Allowed |
| `GET /api/v1/webhooks/{endpointId}/deliveries` | 403 | 403 | Allowed |
| `POST /api/v1/topups` | 403 | 403 (retired) | 403 (retired) |
| `POST /api/v1/topups/stripe` | Denied | Personal session | Personal session |
| `POST /api/v1/topups/airwallex` | Denied | Personal session | Personal session |
| `POST /api/v1/topups/x402` | Denied | Personal session | Personal session |
| `POST /api/v1/topups/hyperswitch` | Denied | Personal session | Personal session |
| `POST /api/v1/redemptions` | Denied | Personal session | Personal session |
| `GET /api/v1/holds` | Own attributed records | Scoped history | Full customer history |
| `POST /api/v1/holds` | 403 | 403 (retired) | 403 (retired) |
| `POST /api/v1/settlements` | 403 | 403 (retired) | 403 (retired) |
| `GET /api/v1/bills` | Own attributed records | Scoped history | Full customer history |
| `GET /api/v1/statements` | 403 | 403 | Allowed |
| `GET /api/v1/statements/{statementId}` | 403 | 403 | Allowed |
| `GET /api/v1/statements/{statementId}/proof` | 403 | 403 | Allowed |
| `GET /api/v1/statements/{statementId}/export.pdf` | 403 | 403 | Allowed |
| `GET /api/v1/balance` | Denied | Personal session | Personal session |
| `GET /api/v1/usage` | Own attributed records | Scoped history | Full customer history |
| `GET /api/v1/budget` | Denied | Personal session | Personal session |
| `GET /api/v1/billing-records` | Own attributed records | Scoped history | Full customer history |
| `GET /api/v1/requests` | Own attributed records | Scoped history | Full customer history |
| `GET /api/v1/org/usage-reports` | 403 | 403 | Allowed |
| `POST /api/v1/org/usage-reports` | 403 | 403 | Allowed |
| `GET /api/v1/org/usage-reports/{reportId}` | 403 | 403 | Allowed |
| `POST /api/v1/org/usage-reports/{reportId}/links` | 403 | 403 | Allowed |
| `GET /api/v1/org/usage-reports/links` | 403 | 403 | Allowed |
| `POST /api/v1/org/usage-reports/links` | 403 | 403 | Allowed |
| `DELETE /api/v1/org/usage-reports/links/{linkId}` | 403 | 403 | Allowed |
| `POST /api/v1/estimate-price` | Metadata/estimate | Metadata/estimate | Metadata/estimate |
| `GET /api/v1/pricing` | Metadata/estimate | Metadata/estimate | Metadata/estimate |
| `GET /api/v1/entries/{entryId}/proof` | Own attributed records | Scoped history | Full customer history |
| `GET /api/v1/entries` | Own attributed records | Scoped history | Full customer history |
| `GET /api/v1/log/head` | Metadata/estimate | Metadata/estimate | Metadata/estimate |
| `GET /api/v1/log/consistency` | Metadata/estimate | Metadata/estimate | Metadata/estimate |
| `GET /api/v1/log/price-head` | Metadata/estimate | Metadata/estimate | Metadata/estimate |
| `GET /api/v1/log/checkpoint` | Metadata/estimate | Metadata/estimate | Metadata/estimate |
| `GET /api/v1/session/context` | Denied | Personal session | Personal session |
| `POST /api/v1/session/context` | Denied | Personal session | Personal session |
| `GET /api/v1/org/groups` | Denied | Personal session | Personal session |

## Separate trust boundaries

The following inventory covers every remaining OpenAPI operation. A listed token/signature/role is required independently of any consumption key or customer role. Public here means no customer credential; token, signature, bot check and registration-policy validation still apply where described.

| Method and path | Required authority |
| --- | --- |
| `GET /healthz` | Public metadata |
| `GET /metrics` | Deployment operator token; no customer/session authority |
| `POST /add-checkpoint` | Signed checkpoint and configured witness trust; no customer credential |
| `POST /api/v1/auth/register` | Public identity flow; endpoint-specific password/OAuth/token verification |
| `POST /api/v1/invitations/redeem` | Live invitation token; redeem requires new-user consent and signup checks |
| `POST /api/v1/invitations/preview` | Live invitation token; reads only the offered identity/role/expiry |
| `POST /api/v1/invitations/accept` | Bound recipient personal session plus live invitation |
| `POST /api/v1/business/customer-invitations` | Current business-context session and explicit business capability |
| `POST /api/v1/business/customer-invitations/{invitationId}/revoke` | Current business-context session and explicit business capability |
| `POST /api/v1/auth/login` | Public identity flow; endpoint-specific password/OAuth/token verification |
| `POST /api/v1/auth/logout` | Idempotent logout of the presented session cookie; no customer data returned |
| `GET /api/v1/auth/methods` | Public authentication-method metadata |
| `POST /api/v1/device/code` | Public device challenge issuance; no credential is granted yet |
| `POST /api/v1/device/token` | Device code plus personal approval; expiry and poll limits apply |
| `GET /api/v1/auth/oauth/{provider}` | Public identity flow; endpoint-specific password/OAuth/token verification |
| `GET /api/v1/auth/oauth/{provider}/callback` | Public identity flow; endpoint-specific password/OAuth/token verification |
| `POST /api/v1/auth/totp` | Unexpired login challenge plus second factor |
| `POST /api/v1/auth/verify/request` | Personal session in either context |
| `POST /api/v1/auth/verify` | Public identity flow; endpoint-specific password/OAuth/token verification |
| `POST /api/v1/auth/password/forgot` | Public identity flow; endpoint-specific password/OAuth/token verification |
| `POST /api/v1/auth/password/reset` | Public identity flow; endpoint-specific password/OAuth/token verification |
| `POST /api/v1/admin/organizations/{organizationId}/manual-receipts` | Deployment operator token; no customer/session authority |
| `POST /api/v1/webhooks/stripe` | Configured provider signature; callback never trusts customer claims |
| `POST /api/v1/webhooks/airwallex` | Configured provider signature; callback never trusts customer claims |
| `POST /api/v1/webhooks/hyperswitch` | Configured provider signature; callback never trusts customer claims |
| `GET /api/v1/log/key` | Public metadata |
| `POST /api/v1/metering/holds` | Owner-issued meteringWrite integration credential; selected customer and exact originating credential/hold/body binding |
| `POST /api/v1/metering/settle` | Owner-issued meteringWrite integration credential; selected customer and exact originating credential/hold/body binding |
| `POST /api/v1/metering/release` | Owner-issued meteringWrite integration credential; selected customer and exact originating credential/hold/body binding |
| `POST /api/v1/metering/settlements` | Owner-issued meteringWrite integration credential; selected customer and exact originating credential/hold/body binding |
| `GET /api/v1/business` | Current business-context session with overviewRead |
| `GET /api/v1/business/customers` | Staff customersRead, or integration customersRead filtered to selected customers |
| `GET /api/v1/business/customers/{organizationId}` | Staff customersRead, or integration customersRead for this selected customer |
| `PATCH /api/v1/business/customers/{organizationId}` | Current business-context session and explicit business capability |
| `POST /api/v1/business/customers/{organizationId}/lifecycle` | Current business-context session and explicit business capability |
| `POST /api/v1/business/customers/{organizationId}/suspension` | Current business-context session and explicit business capability |
| `GET /api/v1/business/staff` | Current business-context session and explicit business capability |
| `POST /api/v1/business/staff` | Current business-context session and explicit business capability |
| `POST /api/v1/business/staff/revoke` | Current business-context session and explicit business capability |
| `POST /api/v1/business/ownership` | Current business-context session and explicit business capability |
| `POST /api/v1/business/registration` | Current business-context session and explicit business capability |
| `POST /api/v1/admin/business/setup-tokens` | Deployment operator token; no customer/session authority |
| `POST /api/v1/business/setup` | One-time setup token plus bound personal session |
| `POST /api/v1/business/setup/register` | One-time email-bound setup token plus new owner consent |
| `GET /api/v1/admin/business` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/business/customers/{organizationId}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/channels` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/channels` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/channels/{channelName}/prices` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/channels/{channelName}/prices` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/channels/{channelName}/aliases` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/channels/{channelName}/aliases` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/channels/{channelName}/aliases/{alias}` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/channels/{channelName}/credentials` | Deployment operator token; no customer/session authority |
| `PATCH /api/v1/admin/channels/{channelName}/credentials/{credentialId}` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/channels/{channelName}/credentials/{credentialId}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/organizations` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/requests` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/organizations/{organizationId}/adjustments` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/organizations/{organizationId}/credit-notes` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/organizations/{organizationId}/credit-notes` | Deployment operator token; no customer/session authority |
| `PATCH /api/v1/admin/organizations/{organizationId}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/subscription-plans` | Deployment operator token; no customer/session authority |
| `PUT /api/v1/admin/subscription-plans/{name}` | Deployment operator token; no customer/session authority |
| `PUT /api/v1/admin/organizations/{organizationId}/subscription` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/organizations/{organizationId}/subscription` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/guardrail-policies` | Deployment operator token; no customer/session authority |
| `PUT /api/v1/admin/guardrail-policies/{name}` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/guardrail-policies/{name}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/guardrail-bindings` | Deployment operator token; no customer/session authority |
| `PUT /api/v1/admin/guardrail-bindings/{scope}/{target}` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/guardrail-bindings/{scope}/{target}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/model-fallbacks/{model}` | Deployment operator token; no customer/session authority |
| `PUT /api/v1/admin/model-fallbacks/{model}` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/model-fallbacks/{model}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/groups` | Deployment operator token; no customer/session authority |
| `PUT /api/v1/admin/organizations/{organizationId}/groups/{name}` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/organizations/{organizationId}/groups/{name}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/holds` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/anomalies` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/deposits` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/deposits/{depositId}/reverse` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/margin` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/reconciliation` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/redemption-codes` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/closings` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/closings` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/statements` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/statements` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/statements/{statementId}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/statements/{statementId}/export.pdf` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/statements/{statementId}/finalize` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/statements/{statementId}/payments` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/statements/{statementId}/suspend` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/users/{userId}/password-reset` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/tiers` | Deployment operator token; no customer/session authority |
| `PUT /api/v1/admin/tiers/{tier}` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/tiers/{tier}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/discounts` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/discounts` | Deployment operator token; no customer/session authority |
| `DELETE /api/v1/admin/discounts/{discountId}` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/audit` | Deployment operator token; no customer/session authority |
| `GET /api/v1/admin/service-credentials` | Deployment operator token; no customer/session authority |
| `POST /api/v1/admin/service-credentials` | Retired: 403 after deployment authentication |
| `DELETE /api/v1/admin/service-credentials/{credentialId}` | Deployment operator token; no customer/session authority |
| `GET /v1/models` | Consumption key only; model/admission constraints apply |
| `POST /v1/chat/completions` | Consumption key only; model/admission constraints apply |
| `POST /v1/responses` | Consumption key only; model/admission constraints apply |
| `POST /v1/tasks` | Consumption key only; model/admission constraints apply |
| `GET /v1/tasks/{taskId}` | Exact submitting consumption key; sibling/foreign IDs return 404 |
| `POST /v1/tasks/{taskId}/cancel` | Exact submitting consumption key; sibling/foreign IDs return 404 |
| `POST /v1/messages` | Consumption key only; model/admission constraints apply |
| `POST /v1/embeddings` | Consumption key only; model/admission constraints apply |
| `POST /v1/rerank` | Consumption key only; model/admission constraints apply |
| `POST /v1beta/models/{model}:generateContent` | Consumption key only; model/admission constraints apply |
| `POST /v1beta/models/{model}:streamGenerateContent` | Consumption key only; model/admission constraints apply |

## Auxiliary surfaces and private response state

| Surface | Authority and visibility |
| --- | --- |
| GET /dashboard/bills/export.csv and /dashboard/bills/export.json | Personal customer session; member scoped history or full owner/admin history. Explicit machine credential returns 401 even with a valid cookie. |
| GET /ws/billing | Personal customer session; scoped initial snapshot and every live event. Explicit machine credential returns 401. Current membership, customer and context are rechecked before delivery and every 30 seconds while idle; changed context/customer, logout or removal closes the old socket. |
| GET /bill/{token} | Explicit owner/admin-issued report share capability, checked for expiry/revocation; the capability authorizes either one report or the named end-user report history. Customer/key identity adds no authority. |
| /verify and static verifier assets | Public verification of caller-supplied evidence; ?sample uses a fixed public fixture without any database access. No private customer data is selected by an unauthenticated ID or display name. |
| /app and legacy dashboard/auth/admin redirects | Public app shell/redirects; data and mutations retain the API gates above. |
| Gateway/task Idempotency-Key | Fingerprint binds key, protocol/path model and request body. Mismatch returns 422 without releasing or overwriting the original claim. Legacy unbound records conflict until their original 24-hour expiry. Task replays preserve the exact original response body. |
| Response cache | Digests bind the exact key plus customer/channel/model/prompt; sibling keys and legacy unbound digests miss. No historical ledger row is rewritten. Full serving-context evolution remains T07/T13. |

Ledger scans and SQL request-log queries filter immutable provenance before pagination. Internal WebSocket key IDs are not serialized. Gateway responses and insufficient-funds errors expose the request bound/charge, never the customer's wallet balance. Own entitlement preview retains effective key/member/team admission constraints.

Evidence: `crates/core/tests/customer_authority.rs`, `crates/server/tests/consumption_scope.rs`, `crates/core/tests/idempotency.rs`, `crates/server/tests/tasks.rs`, `cache.rs`, `statements.rs`, and the existing key/mandate/session/export suites. [T05.a evidence](roadmap/self-hosted-v1/evidence/T05-a.md) records exact candidate/check results; this inventory alone is not a validation claim.


## Business integration credential additions (#638)

| Method and path | Required authority |
| --- | --- |
| GET /api/v1/business/integration-credentials | Current business owner session; metadata pagination only |
| POST /api/v1/business/integration-credentials | Current business owner session/TOTP/context; audited explicit scopes/customers/expiry; secret once |
| POST /api/v1/business/integration-credentials/{credentialId}/revoke | Current business owner session/TOTP/context; atomic receipt/audit |
| GET /api/v1/business/customers and /customers/{organizationId} | Existing staff customersRead, or integration customersRead with selected customer filtering before pagination |

No other business/customer/personal/gateway/deployment route accepts integration authority. Explicit bearer prevents cookie fallback; unsupported header credentials fail closed. Legacy oxs-svc- credentials never authenticate. Metering binds the current integration grant to original and terminal requests in core. External eventId/endUser is attribution, never customer selection or global operation identity.


## Business provider additions (#641)

All these routes require a current personal business-context session/TOTP. Explicit bearer/API-key headers prevent cookie fallback. Integration/consumption/deployment bearers have no authority here. Core checks current role/ownership in the mutation transaction before replay.

| Method and path | Required authority |
| --- | --- |
| GET /api/v1/business/channels | Owner/admin; owned metadata page |
| POST /api/v1/business/channels | Owner; connection and provider pool |
| POST /api/v1/business/channels/{channelName}/credentials | Owner |
| POST /api/v1/business/channels/{channelName}/billing-credential | Owner |
| POST /api/v1/business/channels/{channelName}/credentials/{credentialId}/status | Owner, exact channel/credential |
| POST /api/v1/business/channels/{channelName}/credentials/{credentialId}/remove | Owner, exact channel/credential |
| POST /api/v1/business/channels/{channelName}/cache | Owner/admin routingWrite |
| GET /api/v1/business/channels/{channelName}/prices | Owner/admin, owned channel |
| POST /api/v1/business/channels/{channelName}/prices | Owner/admin catalogWrite |
| GET /api/v1/business/channels/{channelName}/aliases | Owner/admin, owned channel |
| POST /api/v1/business/channels/{channelName}/aliases | Owner/admin catalogWrite |
| POST /api/v1/business/channels/{channelName}/aliases/{alias}/remove | Owner/admin, exact channel/alias |
| GET /api/v1/business/model-fallbacks/{model} | Owner/admin, owned served model |
| PUT /api/v1/business/model-fallbacks/{model} | Owner/admin routingWrite, all targets owned |

Secrets are returned only as tails; audit binds full requests through a keyed fingerprint. Foreign identifiers return 404. No save triggers a network connection; DNS/redirect/internal endpoint policy and explicit probes remain #642.


Customer model metadata (`/v1/models`, pricing, estimate and direct signed-price checkpoints) intersects customer tier and business ownership with a consumption key's model and group-channel grants. Personal sessions read their tier's catalog. Public alias and resolved target must both pass. Experiment configuration accepts only customer-visible variants; runtime keys still narrow them. Task status/cancel remains an original-key read/recovery action after later model/grant edits.
