# Concurrent evidence publication (#891)

Main CI [42550669522](https://github.com/AllureCurtain/oxsum/actions/runs/42550669522)
on `134a4aaef7ca03f8c8203a5bfd3c1217e11271ec` exposed PostgreSQL error 23505 in
`concurrent_exports_during_refund_return_one_consistent_committed_snapshot`.
The primary-key arbiter did not cover the simultaneous partial unique-index
check on `(organization_id, statement_id)`. Period scopes have the same shape.

Publication now uses `ON CONFLICT DO NOTHING` across all applicable indexes.
It still reads only the expected deterministic marker id afterward; an unrelated
stored identity cannot be adopted as this publication's result. No existing
bundle bytes or captured inputs change, and no connection is held across wallet
calls. The scope and API contract are unchanged.

A controlled regression holds both actual publishers at the final INSERT using a
PostgreSQL advisory-lock trigger. It covers statement and period scopes,
identical retained descriptors and replay bytes. A separate altered-identity
trigger checks refusal to adopt another marker id. Existing concurrent
refund/export, single-connection publication and orphan-marker tests remain in
the focused selection. All five selected core tests passed (1.928 seconds after
compilation); the other 144 library tests were filtered out. Core-only strict
Clippy passed in 1m20. Formatting/diff checks and independent source review
passed on `ec713918`. Logs: `/tmp/oxsum-891-focused.log` and
`/tmp/oxsum-891-clippy.log`. The prior main CI provides the original failing
production evidence. This core-only correction ran no workspace test suite.
