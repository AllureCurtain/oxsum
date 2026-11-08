# Complete integrated regression repair (#805)

The initial complete local run used exact integrated source
`c97e5f79541040494753f1148121c47191ae5785`, PostgreSQL scratch databases and
`cargo nextest run --workspace --no-fail-fast`: **1,871 passed, five failed,
nine skipped**, in 251 seconds after compilation. This confirmed the #793 and
#795 repaired paths while exposing all five residual failures instead of
stopping at the first shard failure. Main CI also failed on that source:
[run 42451016182](https://github.com/AllureCurtain/oxsum/actions/runs/42451016182).

The repair starts from `465061d` (checkout documentation #804 integrated).

## Findings and preserved checks

| Finding | Repair and retained invariant |
| --- | --- |
| Deposit upgrade stops at obsolete 0100; current fixture setup needs collection terms | Stop before the actual 0107 refund migration. Preserve all original deposit JSON and ledger references; only a matching pinned historical handle is due for read-only recovery. |
| Plan upgrade stops at obsolete 0103 and sees an unrelated new bundle column | Stop before actual 0109; assert `plan_payment` is null and compare every historical field, including the already-existing bundle column. Historical activation replays while pending unquoted orders remain fenced. |
| Related bundle fixture still uses obsolete 0102 | Stop before actual 0108 and keep both newly added authority snapshots explicitly null. |
| Concurrent/repeated migration inventory omits `staff_invitation_mail` | Include the existing staff mail table; retain exact schema inventory and eight concurrent migration calls. |
| Historical Hyperswitch refund fixture names `pay_test_123`, but the payment stub now pins its supplied UUID | Use that exact payment identity. Assert one read attempt, no new refund POST, no body/create authority, wrong-payment rejection, missing-handle refusal and original metadata preservation. HTTP success still means pending, never confirmed external success. |
| HTTP journal test assumes base fee sorts before another line created in the same transaction | Compare the complete typed amount set. Read order is creation time plus UUID tie breaking; it never promised billing-kind ordering. |
| Embedded migration registry drifted from file order | Sort existing full names without changing SQL or renaming history. A source invariant checks unique ordered registration and exact SQL contents against every migration file. |

## Validation

- Focused PostgreSQL/core registry selection: **42 passed**, covering every
  original failure and related bundle/collection/identity/journal/refund paths.
  The other 1,219 tests in that selection were filtered out, not reported as
  passing.
- `scripts/fmt.sh`, `scripts/fmt-check.sh`, `git diff --check` and strict
  `cargo clippy --workspace --all-targets -- -D warnings` passed. Initial Clippy
  correctly refused test-only `expect` calls until the standard test-module
  allowance was scoped to that module.
- Complete combined workspace validation: **1,877 passed, zero failed, nine
  skipped**, across 180 binaries in 271 seconds after compilation. This includes
  engine conformance and the repaired core/server paths. The nine skips remain
  six existing opt-in suites plus the three separately tracked #779 financial
  reproductions; they are not certified by this pass.
- Independent source review found no blocking issue and confirmed that the
  registry has the same 104 full version names and SQL bodies before/after.
  Log files are retained locally under
  `local-notes/operator-launch/repair-validation/integrated-regressions-*`.

No real provider, payment, public publication, browser acceptance or final
candidate was exercised. The three separate #779 financial reproductions remain
explicitly ignored until their own corrections land. This repair does not
reinterpret the historical financial policy or mark N10/N11 complete.
