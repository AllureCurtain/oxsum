# Payment worker completion boundary (#850)

Main CI [42498733548](https://github.com/AllureCurtain/oxsum/actions/runs/42498733548)
on `23b6547` failed the real-worker payment test: the balance already contained the
payment while the order still reported `paid`. The fixture stopped the worker as
soon as it saw the independently committed ledger grant, before the following
fulfillment/outbox transaction. This is an early test cancellation, not evidence
that money should be credited again.

The fixture now polls the actual customer fulfillment endpoint until it reports
`fulfilled`, then stops the scheduler. The ten-second bound, real scheduler,
separate paid-before-fulfilled assertion, exact amount and repeat-sweep assertions
are unchanged. Production behavior is unchanged.

Validation on the patched source based on `df4ad4f`:

- Real PostgreSQL `cargo nextest run -p oxsum-server --test payments`: 19 passed,
  zero skipped (4.699 seconds), including the real scheduler case.
- Strict workspace/all-target Clippy, repository formatting and diff checks passed.
- Independent source review passed, including the explicit HTTP-status assertion.

The earlier failure remains recorded and does not count as candidate acceptance.
Main `df4ad4f` subsequently passed CI `42507398254`, but that successful run did
not itself remove this nondeterministic fixture race. This test-only change uses
the focused suite; it does not repeat an unrelated complete workspace run.
