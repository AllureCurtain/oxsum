# Reconciliation period dates

Issue #983. The Finance comparison run returned `[2026, 314]` and `[2026, 315]`
for its inclusive period, despite the existing OpenAPI date-string contract.
React concatenated these into `2026314 → 2026315`.

The core response now serializes and deserializes `YYYY-MM-DD`. Twelve focused
core/server financial-fact tests passed (run
`96f7b5d9-aca5-4a16-8c0e-d29cf6276137`), including create/list/detail date assertions
and a typed round trip. Strict core/server all-target Clippy and pinned formatting
passed. The prior 2,256-pass workspace evidence from #975 is reused; this isolated
serialization change does not claim a new full candidate run.

Actual Finance browser recheck on source
`ca835528c5001434ae24256c177f1adb50bed274`, schema 0128, returned and displayed
`2026-11-10 → 2026-11-11` for the same run and retained comparison. List/detail,
English/Chinese, refresh, keyboard activation and a 390-pixel mobile detail were
checked. Screenshots were visually inspected. The frontend is unchanged from
`3b5b55be4b54cf4bf9ba35c87d6119bd4b108ff7`; runtime metadata identifies the backend.

The stored snapshot is semantically identical before/after: 15 sources, 20
verified entries, six equal monetary totals and four open missing-cost exceptions.
No comparison was re-executed and no financial data was edited for this diagnosis.
This is local isolated development evidence, not PSP or final candidate acceptance.
