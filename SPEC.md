# SPEC — Semantic rebase of PR #761 (refund-correlation) onto main after #764

## Task
Rebase branch `fix/refund-correlation` onto `origin/main` so its feature lands correctly. A previous mechanical rebase corrupted the `.rs` files (head+incoming concatenation); the current branch tip `40dc419` is BROKEN and must NOT be used as the source of truth. The correct source of intent is the green commit `d709414`.

## Context
- PR #761 "correlate signed refund callbacks within pinned merchant versions" closes #759.
- Hyperswitch refund objects retain their own metadata; historical oxsum refunds omitted payment/customer coordinates and could not settle through the scoped callback. #761 sends the actual order/payment/customer IDs and resolves missing historical coordinates from an existing immutable provider reference inside the exact merchant/version. Supplied conflicts and malformed UUIDs remain errors; original normalized events and payload digests stay immutable.
- **#764 (already merged, commit c7926fe) "recover Hyperswitch operations with durable body identities" rewrote the SAME files**: `crates/core/src/merchant_events.rs`, `crates/integrations/src/merchant_checkout.rs`, `crates/server/src/routes/refunds.rs`, `crates/server/src/jobs.rs`, `crates/server/src/routes/merchant_events.rs`. It changed refund reservation semantics — refunds now use durable body identities, `target.refund` handles, `MerchantCheckoutError`, refund UUID as provider idempotency key.

## What to do
1. `git rebase --abort` any in-progress rebase; check out `fix/refund-correlation`.
2. Rebase the GREEN commit `d709414` onto `origin/main` (`git rebase --onto origin/main <old-base> d709414` is messy because the branch is already rebased; instead create the change fresh): `git checkout origin/main -B fix/refund-correlation` then apply `d709414`'s INTENT, resolving each `.rs` file by understanding both sides.
3. For each `.rs` conflict, the rule is: **#764's durable-identity semantics are the current authority** — keep `target.refund`, `MerchantCheckoutError`, the UUID-as-idempotency-key flow. Port #761's *additions* on top: the order/payment/customer coordinate fields on refund callbacks, the historical-coordinate resolution from the immutable provider reference, the malformed-UUID / supplied-conflict refusals.
4. Non-.rs files (TODO.md, docs/*.md) are tail-appends — concatenate HEAD block then incoming block.
5. Verify: `cargo check -p oxsum-core -p oxsum-integrations -p oxsum-server`, `scripts/fmt-check.sh`, `cargo clippy -p oxsum-core -p oxsum-integrations -p oxsum-server --all-targets`, and `cargo nextest run -p oxsum-core --test merchant_events --test refunds -p oxsum-server --test merchant_events --test refunds` must pass.
6. Commit as the single feature commit "fix: correlate signed refund callbacks with pinned payment references", keep the same PR body intent. Do NOT push — report back.

## Reference files
- Green (intent) version: `git show d709414:<path>`
- Current main (authority) version: `git show origin/main:<path>`
- Dumped for convenience: `/tmp/rc_green_events.rs`, `/tmp/rc_main_events.rs`
