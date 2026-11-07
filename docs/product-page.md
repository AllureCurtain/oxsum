# Oxsum: verifiable billing for AI services

Oxsum lets an AI-service business operate billing in its own infrastructure and
hand customers evidence they can verify. One Business owns upstream connections,
customer sale terms and collection. Customer accounts hold their own purchased
credit, promotional credit, plan entitlements and approved receivables. Oxsum
takes no platform fee in this self-hosted model.

The product serves two workflows:

- An API-service operator uses the Business portal to configure supply and sale
  terms, enroll customers, collect payments and reconcile the books. Customers
  use `/app` to manage keys, calls, orders and evidence.
- An AI application or Agent SaaS developer embeds gateway and scoped billing
  operations in an existing application. The application owns its end-user
  experience; credentials and grants restrict the customer accounts and actions
  the integration can access. The complete embedded lifecycle is part of the
  current [operator-launch program](https://github.com/AllureCurtain/oxsum/issues/725),
  not a claim that every planned endpoint is already available.

## From request to evidence

An admitted request pins its route and sale context before reserving the upper
bound. Concurrent admission checks and ledger writes enforce the applicable
limits. A provider failure before output can release the customer hold; that
customer result does not prove the supplier incurred no cost. Partial responses,
missing usage, estimation and capped charges retain explicit settlement outcomes.
Supplier costs and customer sale revenue are different facts.

Settlement records the charged amount, usage, terms and outcome. Purchased
balances, quota and promotions are not interchangeable cash. Payment collection
is separate from revenue recognition; a provider checkout response or return URL
cannot establish payment success. Signed provider events and purpose-specific
fulfillment establish what the order funded. A local reversal does not by itself
prove the PSP returned money.

Customers can retain the settlement entry, content hash and proof. The verifier
recomputes the entry hash and its inclusion in a particular log head. Supported
settlement formats permit a separate arithmetic check. Signed price/sale
bindings, operator signatures, consistency proofs and witness cosignatures each
answer a different question; missing inputs retain their weaker verdicts.
An independently trusted key and an earlier archived head matter more than a
key supplied beside the very bundle being checked.

## Choose the evidence for the question

| Customer question | Evidence and boundary |
| --- | --- |
| Was this entry changed? | Entry proof against the hash the customer retained; a fresh self-supplied hash is a weaker anchor |
| Does the charge recompute? | Supported settlement arithmetic and price/sale bindings; not independent measurement of the provider's tokens |
| Did the history extend my earlier view? | Consistency proof against the previously retained head and expected origin |
| Are these the committed report rows? | Usage report subset digest and inclusion; anchored completeness only when supplied; no automatic per-row arithmetic |
| Is this the statement that was finalized? | Statement anchor and its check payload; not integrity of an edited PDF rendering |
| Are all committed export artifacts present? | Version-2 evidence manifest, artifact digests and entry proofs for the named scope; excluded supplier costs and in-flight activity remain excluded |

[Proof scope](proofs.md) and [the customer verification guide](verify-your-bill.md)
describe the actual checks. Signatures authenticate attestations under selected
keys; they cannot prove honest token counting. A witness controlled by the same
operator is a protocol demonstration, not independent corroboration.

## Operate it

The established architecture is a Rust binary with an embedded React portal,
PostgreSQL 17 and the vendored doubleentry engine. The operator retains database
backups, sealing/signing keys and release artifacts; customers retain their own
verification inputs. Start with the [installation runbook](self-hosted-install.md),
then enter `/app/business` using the initialized owner's session. The deployment
token is a bootstrap and technical operations credential.

Current software capabilities, fixture coverage, genuine PSP sandbox evidence
and live-money evidence are separate in [the rail matrix](rails.md). Historical
release results apply to the source and artifacts they name. The ongoing launch
work retains unmet browser, capacity, provider and independent-pilot gates; it
does not turn a local fixture into a certification or authorize publication.
