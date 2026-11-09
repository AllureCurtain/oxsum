# Independent operator pilot kit

Prepared under #872 / N19. **No participant has run this kit.** It is a reusable
handoff package, not pilot evidence, certification or permission to contact
anyone. The final launch needs two distinct external operators: one using the
built-in portal and one integrating an application. Internal agents and the
project author do not count as participants.

Use [Portal operator](portal-operator.md) and [Application integrator](application-integrator.md)
with one separate copy of [the result template](participant-result.md) per
participant. The [execution ledger](../EXECUTION.md) and [contract](../CONTRACT.md)
remain authoritative for N00–N21 and J01–J18.

## Before an invitation or installation

The coordinator completes this intake for each participant before handoff:

| Required input | Record or criterion |
| --- | --- |
| Participant | A distinct external person/team, their selected path, consent and a private contact reference; never publish contact details in the result |
| Environment | Explicit disposable target; CPU architecture/count, memory, storage, OS, Docker/Compose/PostgreSQL versions, network and browser; no ordinary live-service upgrade |
| Candidate | Exact merged source SHA, successful executed full CI including engine/process-shared suites, candidate run/attempt, validated manifest, image/archive/config/platform identity and original verifier package hashes |
| Provenance | [Candidate validation](../../../self-hosted-release.md) passes; the observed `/api/v1/version` matches the supplied artifact |
| Scope | Local stub/synthetic receipt or a separately authorized real sandbox, named provider/model/scenarios and explicit budget; never infer permission from a stored credential |
| Product prerequisites | Customer readiness, explicit statement repayment and the selected N21 operations are integrated and pass the candidate journeys; consult live issue status, not this prepared kit alone |
| Assistance | Agreed support channel and observation method; observer records interventions but does not silently perform participant steps |
| Data handling | Fictional customer identities; local mail capture; private credential files; sanitized screenshots, request IDs and errors; no secrets or raw database dump in feedback |

There is currently no frozen operator-launch candidate, supplied participant,
real PSP sandbox credential, external contact authority or live-money allowance.
These inputs must be supplied for the affected pilot steps. A manual-receipt
pilot can exercise billing software, but cannot close PSP capture/refund gates.
The shared DeepSeek budget belongs to the separate bounded exercise; it is not
an automatic budget for two pilots.

For a private candidate rehearsal, load the validated archive and select its
exact image reference; do not substitute `latest`, build a moving branch, or
publish a new image to make the kit work. If an external participant cannot
receive the candidate through an authorized channel, retain the pending status.

## Observation and evidence

Start a monotonic timer when the participant first opens the supplied instructions.
Record wall-clock UTC timestamps too, so logs can be correlated. Pause the active
work timer for unavailable external credentials or scheduled waiting, and record
that time separately; never remove it from total elapsed time. Keep both
install-to-first-settled-bill and install-to-first-independent-verification times.
Record each intervention, the step and minutes spent. Distinguish independent
completion, completion with assistance, failed, blocked and not attempted.

Capture the exact artifact/environment first, then the inputs and retained output
for each step. A successful HTTP status or page alone is insufficient for a
financial step: name its order/request/statement, actual ledger effect and
verified evidence. Mark browser execution separately from scripts/domain tests.
Retain the original failed result before retrying or applying a fix. New code
requires a new candidate identity and affected reruns.

Each participant must complete one actual plan/billing cycle. A first-call bill
or an empty closed-day export is not a completed cycle. Do not backdate a ledger,
change historical timestamps, or accelerate a real plan by SQL. If a documented
isolated clock fixture is later supplied, identify its code, technique and scope;
otherwise observe the configured cycle in real time and leave it pending.

The result must include what the participant understood about pricing, refunds,
statements, proof limitations and routine recovery, plus whether they would
continue using the product and why. A successful script does not answer those
questions. File concrete defects with expected/observed behavior, reproducible
steps, sanitized identities and the first affected artifact. Never include API
keys, login cookies, setup/share tokens, signing/sealing keys or private contact
information.

## Completion and cleanup

The coordinator checks every verdict against retained evidence and links the two
actual results from N19. A missing workflow is an implementation defect; a missing
PSP credential or participant is a named external dependency. Do not call both
cases “passed with limitations.”

Stop only the participant's task-owned services. Keep the retained database,
backups and operator keys until the agreed recovery window ends; never run volume
deletion as an unreviewed cleanup step. Revoke temporary customer/integration
credentials and close any task-owned public endpoint. Public publication and
existing-service deployment remain separately authorized operations.
