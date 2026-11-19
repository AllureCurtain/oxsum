# Billing workflow pilot protocol

Tracks #745 (two independent operators) and #740 (authentic provider evidence).
This is a runnable protocol, not a report of completed pilots. No operator has
been contacted or enrolled by this development program.

## Preparation

Use a dedicated restored/sanitized environment with the merged billing program,
migrations 0140–0143 and current terms. Do not deploy or promote a candidate as
part of this protocol. Each operator must have personally authorized access;
provide owner/finance/customer roles appropriate to each step. Keep customer
content and provider credentials out of shared artifacts. Retain original raw
Costs numeric values and pagination metadata when sanitizing.

For each of two independent operators, prepare at least 10 genuine, sanitized
cases across the supported scope: ordinary purchased-credit debit, disputed
amount, legacy estimation, accepted provider-reported missing/partial usage,
reservation release, unsupported mixed-funding correction, and a supplier daily
scope/coverage discrepancy. Record actual sample composition and exclusions;
synthetic rehearsals must be separate rows and cannot count as real pilots.

## Portal scenario

1. Start timing when handed an exact request/provider/ledger ID. In Business
   Customers find the charge, identify customer, original terms/rates, evidence
   basis, debit, released reservation and net correction. Explain uncertainty.
2. Export the readable receipt and existing proof bundle. Explain what each
   does and does not prove without SQL or raw proof JSON as the main interface.
3. Preview a bounded purchased-wallet correction, enter a reason and apply.
   Retry the same intent after a simulated lost response. Confirm a single
   source-linked credit, original charge intact and final customer net effect.
4. Publish a new policy version, verify existing acceptance is unchanged,
   explicitly accept as customer and make one supported synchronous text call.
   Record admitted policy/digest and absence of retrospective repricing.
5. For missing usage, observe pending→closed or documented early closure,
   unknown quantities/waiver/released reservation and the operator recovery
   queue. Never force provider storage without the caller's actual consent.
6. Import an authentic complete OpenAI Costs UTC day with account/project and
   channel mapping evidence plus cost-unit conversion. Review scope, known
   amounts, unknown attempts and residual. Save an initial comparison, later
   capture another, and export both exact versions. Aggregate equality must
   never be described as complete per-request matching.

## Per-case evidence template

| Field | Record |
| --- | --- |
| Operator pseudonym / independent organization | |
| Case ID / sanitized source identity | |
| Genuine / synthetic / excluded reason | |
| Funding / route / policy version | |
| Started / correctly explained UTC timestamps | |
| Elapsed seconds / independent assessor | |
| Explanation correct? What was missing? | |
| SQL/raw-JSON help required? | |
| Correction amount / completion time / duplicate check | |
| Missing usage? Waived amount? Unknown supplier exposure? | |
| Supplier scope / amount difference / unknown attempt count | |
| Receipt/export hashes and trusted storage location | |
| Operator feedback and reproduction steps | |

Target: at least 90% of supported sampled cases located and correctly explained,
or precisely diagnosed as missing evidence, within 120 seconds without SQL/raw
proof JSON. Report numerator, denominator, sample sizes per operator, excluded
cases and correctness assessment. Also report missing-usage rate, unresolved
exposure, correction latency and waived-cost burden. These are targets, not
measured results; do not generalize beyond the actual sample.

## External OpenAI evidence checklist

- Original `GET /v1/organization/costs` day page(s), query and full cursor chain;
  authorized account identity and fetch time; no admin credential in artifacts.
- Account/project ↔ oxsum channel mapping valid over that entire UTC day,
  including outside-oxsum traffic or shared credentials that invalidate scope.
- Explicit basis for converting local supplier-cost credit units to USD.
- Original source version, exact comparison export and known/unknown coverage;
  revised observations retained separately.
- For stored-chat recovery certification: caller-authorized `store:true`, exact
  returned completion ID, original canonical endpoint/credential, sanitized GET
  report, terminal/deadline and final financial outcome. Never substitute the
  local test-only lookup trigger for this evidence.

Stop certification for missing authorization, incomplete pages/mapping or an
unsupported format. Record the exact gap under #740/#745; completed engineering
checks do not override it.
