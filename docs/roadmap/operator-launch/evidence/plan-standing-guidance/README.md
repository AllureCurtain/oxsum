# Plan renewal and debt guidance

Issue #1018 was found during #1017's actual browser boundary acceptance.
The expired customer's original policy-2 invoice was fully paid, its outstanding
balance was zero, and its `nextDueAt` was null. Both Plans and Billing previously
claimed an overdue statement; the Plans baseline is `expiry-no-renewal.png`.

The revised shared notice derives renewal from the latest authorized period's
end and overdue debt from positive credit standing with a reached due date.
Neither reason is inferred from `plan.state` alone. Both may be shown together.
Plans and Billing label the zero-debt state **Renewal due / 待续费**; Billing's
link reaches Plans and does not instruct the customer to repay absent debt.

Four focused checks cover zero-debt expiry, active entitlement with overdue
debt, combined reasons/future debt and cancellation/unbound/initial states.
Five existing OpenAPI contract checks, TypeScript/Vite build, pinned formatting
and diff validation passed. No Rust or API behavior changed.

Actual browser reacceptance used the same isolated customer and database, an
ordinary debug server, and the built revised frontend. English/Chinese guidance
and the Billing-to-Plans link passed; the notice fit a 390×844 viewport without
document overflow. The unchanged Billing table's narrow amount wrapping was
observed separately and is tracked as a separate repair. Some pre-existing
plan-payment strings remain English; this change localizes the new guidance.
`manifest.json` records source and artifact hashes. This is development evidence,
not genuine PSP, elapsed-month or final-candidate certification.
