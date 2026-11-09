# Application integrator walkthrough

Status: prepared, unexecuted by an independent participant. Complete the
[shared intake](README.md) and keep a separate [result sheet](participant-result.md).
This path covers J18 and the embedded half of J17, including the financial and
evidence prerequisites from J01/J05/J08–J16.

The target is a separate application process and store that owns its own user,
customer mapping and retry state. Its runtime uses scoped integration/customer
credentials. An owner session may issue the initial grants once; the application
must not store the owner password or log in as the owner on every request.

## Availability check before handoff

Confirm each item against the selected candidate's OpenAPI and execution ledger.
Do not borrow an unrestricted owner/deployment token to fill a missing surface.

| Capability | Current preparation status | Pilot requirement |
| --- | --- | --- |
| Application namespaces and bounded service-managed customer provision/read/archive | Integrated #855; owner grant UI #857 | Stable business/application/external-customer identity; exact replay across credential rotation; no fabricated human login |
| Explicit customer billing reads and external metering | Integrated #791 / existing metering contract | Current grant and customer checks; immutable operation IDs and bill/source evidence |
| Bounded application customer key lifecycle | #866 implementation prepared, not yet integrated at this kit checkpoint | Finite model/budget/rate/concurrency/expiry, one-time secret recovery, revoke and foreign-customer refusal |
| Application entitlement/model reads, scoped checkout/evidence | N21 remaining | Narrow actual contract endpoints; no wildcard customer access or self-credit |
| Verified human consent/linking | N21 remaining | Explicit verified consent before linking an app-managed account to a person; contact email alone never links |
| Signed lifecycle notifications and complete external sample | N21 remaining | Signature/dedup/order/retry recovery, separate durable application store and a complete J18 run |

The complete external pilot is blocked until these required surfaces are
integrated and exercised against the candidate. Existing small examples are
useful components, not a claim that J18 is complete.

## Participant sequence

1. Install the exact candidate using the [install guide](../../../self-hosted-install.md),
   then configure one local stub model, immutable customer sale terms and local
   captured mail. Record the provider/collection tier. Follow the portal kit for
   initial owner consent and infrastructure only.
2. Owner creates a stable application namespace with a finite lifetime customer
   limit and grants only required scopes and expiry. Keep the one-time secret in
   the application's private credential store. Record its public ID, namespace,
   scope set and expiry without the secret.
3. Build or use the candidate's complete external sample with a separate store.
   Persist the application external-user ID, stable oxsum customer UUID, exact
   operation body and idempotency key before provisioning. Concurrent/retried
   signup must create one mapping. Same external identity with changed terms
   must not create a second billable customer silently.
4. Create a bounded customer gateway key through the actual scoped endpoint.
   Save the secret once; after a lost response, replay must not invent another
   key or expose its old secret. Exercise the documented revoke/new-operation
   recovery. Keep customer keys separate from management credentials.
5. Read permitted models/entitlement and initiate a correctly scoped quoted
   checkout using only supported application scopes. Demonstrate that unpaid
   checkout cannot fund the account. Use the declared synthetic receipt or
   separately authorized actual sandbox confirmation. Never mint credit from
   an application-provided “payment succeeded” assertion.
6. Make one fixed bounded call under the customer credential, or report external
   work through explicit metering authority. Persist request/hold/settlement
   identities and exact bodies. Record actual usage/charge/reservation and
   demonstrate that an interrupted response followed by exact retry cannot
   perform a second inference or charge. Observe at least one real configured
   billing cycle; initial usage alone is insufficient.
7. Display the customer's own balance/statement/payment/bill state using scoped
   reads. Fetch evidence and verify it in a clean consumer using the original
   package and explicit expected customer/origin/key/hash inputs. Preserve the
   trust provenance; a key supplied by the same response is not independently
   authenticated operator identity.
8. Exercise signed lifecycle notifications: original event, exact duplicate,
   later event arriving first, invalid signature, failed delivery/retry and
   application restart with retained dedup state. No stale event may reopen a
   cancelled/archived state or duplicate effects. Record this as pending if the
   candidate lacks the promised contract; local fabricated messages cannot
   claim real delivery.
9. Rotate/revoke the application credential and attempt the old one. Retry an
   existing permitted operation with the new grant. Attempt a foreign namespace,
   foreign customer and insufficient scope. Archive the account and verify new
   admission stops while historical billing/evidence remains within the
   documented grants. Application archive must not erase ledger history.
10. Stop/restart the application with its store preserved, then run the documented
    isolated service restore. Existing mappings and pending operation identity
    must recover. If the customer chooses a portal account, perform the verified
    consent link; an application must never manufacture that human approval.
11. Explain the runtime privilege boundary, monetary model, billing evidence and
    recovery procedure without owner credentials. Record time, assistance,
    defects and willingness to keep using the integration.

## Current component references

- [Supported integrations](../../../integrations.md) and
  [OpenAPI source](../../../../crates/server/openapi.yaml) define the actual
  routes, fields, credentials and refusal semantics.
- [Consumption](../../../../examples/consumption/README.md) shows current setup,
  synthetic funding and a deterministic local model; it deliberately performs
  privileged bootstrap and is not the final application's runtime architecture.
- [Scoped management](../../../../examples/scoped-management/README.md) and
  [Python metering client](../../../../examples/scoped-integration/README.md)
  demonstrate selected-grant metering and idempotency. The management walkthrough
  logs in for bootstrap; do not copy that into every application request.
- [Webhook tools](../../../../examples/webhook-tools/README.md) exercise signed
  replay fixtures, not a substitute for unimplemented business/customer lifecycle
  delivery or actual PSP callbacks.
- [Independent verifier consumer](../../../../examples/verify-consumer/README.md)
  demonstrates the packed SDK and altered-input checks. Retain actual candidate
  customer output rather than silently substituting its bundled fixtures.
