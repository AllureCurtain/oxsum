# Portal operator walkthrough

Status: prepared, unexecuted by an independent participant. Complete the
[shared intake](README.md) and use [one result sheet](participant-result.md).
This path samples the portal adoption journey across J01–J14 and J16,
and supplies the portal participant result for J17. J15 capacity/soak requires
its separate reference-environment run and is not measured by this pilot. Exact acceptance
requirements remain in [the contract](../CONTRACT.md); this checklist does not
replace the full scenario matrix.

Use separate owner, customer and finance/support browser identities so that a
successful action cannot accidentally borrow the owner's authority. Keep setup,
login, key and evidence-sharing secrets out of URLs and screenshots.

| Step | Participant action | Required observation/evidence |
| --- | --- | --- |
| 1. Install | Follow [self-hosted installation](../../../self-hosted-install.md) with the exact candidate and a private deployment configuration. Run its documented secret/config checks, database, migration and start commands. | Installation timing, environment, container/platform identity, health/readiness and exact version/schema response; no substituted image |
| 2. Accept ownership | Deployer mints a one-time setup token; intended owner completes `/app/setup`, signs in and enters Business context. Subsequent ordinary setup uses the owner session. | Intended account/business and lost-response recovery; setup secret never exposed; deployment token absent from browser storage |
| 3. Set up supply and prices | In `/app/business`, configure business identity/collection, an explicitly authorized provider connection, the selected model and supply price. Publish sale terms and assign them to the invited customer. | Provider/merchant secrets remain masked; pinned model/sale versions and rate units can be explained; connection check is not mistaken for an inference |
| 4. Invite people | Owner invites one fictional customer through local mail capture. Customer accepts through `/app/invite` using a separate browser identity. Invite finance/support staff if the chosen scenario needs them. | Token expiry/replay and email binding; customer enters its own account; support cannot perform finance-only actions |
| 5. Establish funding | In a synthetic pilot, the deployer records an explicitly labeled manual receipt through the documented operator API; customer cannot credit itself. In a separately authorized sandbox, customer uses `/app/orders/new` and completes actual processor checkout. | Correct currency/rational conversion, original order/merchant version, funding once on duplicate/retry, unpaid order grants nothing. Record synthetic, local signed fixture and real PSP tiers separately |
| 6. First call | Customer checks `/app/pricing`, creates a finite restricted key in `/app/keys`, and submits one bounded request through `/app/chat` or the selected SDK. | Exact request ID, permitted model, usage, settled charge, released reservation and balance. Revoked/wrong-model/unfunded key fails without an upstream call. Record first-bill time |
| 7. Understand and verify | Inspect `/app/requests` and `/app/bills`. Retain the operator key/head before a later call, download the bill and verify with the supplied independent package. | Customer explains price × recorded usage, sale version and proof scope; genuine proof and altered amount/hash/key/origin rejection; record first-verification time |
| 8. Refund/dispute | Customer requests a refund in `/app/orders`; authorized finance reviews it in the business console. Drive only the selected synthetic or actual sandbox event mechanism. Inspect credit note, source allocation and review suspense when present. | Approval alone is not provider success; duplicate events cannot refund twice; refunded cash, remaining Wallet/receivable and evidence match. No forced real dispute or live payment |
| 9. Complete a plan cycle | Select a versioned plan through `/app/plans`; retain the quoted period/order. Observe activation, daily recognition, quota/dunning and the actual period close/statement. Use manual renewal and explicit statement repayment as separate operations when available in the candidate. | One actual configured cycle, original prices/service window, statement identity/amount, repayment allocation and once-only effects; renewal does not silently repay old debt. Any missing repayment implementation blocks this row |
| 10. Export a closed period | Use `/app/evidence` for an ended UTC period, or open its plan statement and export its captured bundle. Keep manifest, original bytes and explicit key/origin/trust inputs. | A newly created account's empty previous-day bundle does not verify today's call; current open period is refused; retained statement export remains the same after later operations |
| 11. Routine operations | Read business reports/reconciliation and queue diagnostics. Use [sanitized diagnostics](../../../development.md) and [upgrade/restore](../../../self-hosted-upgrade.md) on the designated disposable backup/restore targets. | Unmatched/partial evidence is visible, unknown state is not zero; drained upgrade, independent restore and compatible rollback preserve old proofs/keys and permit a new call |
| 12. Explain and decide | Without hints, explain the last bill, a pending refund, a statement and what the proof can/cannot establish. Describe routine daily operations and willingness to continue. | Participant's own answers, friction points, assistance minutes, priorities and recorded defects |

The [user guide](../../../user-guide.md), [bill verification guide](../../../verify-your-bill.md),
[proof scope](../../../proofs.md), [payment support](../../../rails.md) and
[refused operations](../../../unsupported.md) are participant references. Use
[the original package consumer](../../../../examples/verify-consumer/README.md)
for independent verification. A successful built-in verifier is a different
observation from a clean external consumer.

When an action fails, retain the exact sanitized response, request identity,
version and time. Retry the original operation with its original immutable body
and idempotency key where supported; do not mint another payment or inference
just to see whether it succeeds. Ask for assistance only through the agreed
channel and record it. No pilot step authorizes public publication or destructive
changes to a pre-existing service.
