# Personal gateway funding preview (#912)

Parent: #901 / #735 / #725. Base: `5285fc0b`.

This core-only slice supplies `Wallet::preview_gateway_funding` for one live
visible key, permitted model and server-priced integer bound. It adds no public
endpoint and does not change the customer checklist. The model/sale selection
and UI remain #901; supplier price existence is only a route check here, never
the source of the customer amount.

## Behavior

- Fresh personal customer authority and tenant/key visibility are required;
  consumption credentials, foreign keys, members' sibling keys and inactive
  keys cannot obtain a funding snapshot.
- The repeatable-read transaction is PostgreSQL `READ ONLY`. No ledger
  bootstrap, sale digest, quota rollover/draw, operation, watch, hold, accounting
  write, provider I/O or row/advisory lock occurs.
- The actual admission path shares its current key, member and team cap checks
  plus manual/account suspension, plan overage suspension and concurrency with
  the snapshot. Its existing lock order and write behavior are retained.
- Zero-priced and fully quota-funded turns need no wallet balance. Recorded
  pending quota draws reduce quota headroom; caps apply to the wallet remainder.
  Existing excessive caps still refuse a new zero-wallet turn. Partial quota,
  overflow policy, plan debt limits, pending pool reservations and promotions
  are accounted for separately.
- Retained uncancelled past-due quota is usable only when the boundary state is
  already recorded. An unprocessed/cancelled/future boundary reports
  `quotaTransitionRequired`. Due-but-unburned promotional funding reports
  `promoExpiryPending`: actual admission currently pins those lots before the
  expiry worker, so this is conservative uncertainty rather than a claim that
  the live request would be refused. These states cause no transition writes.
- Configured route existence respects customer/key model scope, group channels,
  alias targets and alias-price shadowing. No provider credentials are opened.

The caller must open an existing wallet and resolve the exact sale bound. This
is a funding snapshot, not an execution guarantee: rates, policy outcomes,
provider health, mandates and future changes remain outside this primitive.
Actual admission remains authoritative.

## Validation

- Initial focused core selection: 54 passed (funding, subscriptions, key limits,
  teams and serving access).
- Complete core suite: 777 passed, zero skips (265.793 s).
- Final funding suite after two additional lock/alias regressions: 11 passed.
- Actual HTTP/coordinated admission regressions: 35 passed, zero skips, including
  funded/free admission, boundary rollback and team deletion/reassignment races.
- Strict core all-target Clippy, pinned formatting and whitespace checks passed.

The complete core run used unchanged production code; only two test cases and
this evidence were added afterward. No cross-crate production interface or
migration changed, so the focused server run covers shared admission without
repeating the entire workspace. Independent review and protected integration
remain pending. Real provider, browser, PSP and candidate acceptance remain
separate; no external budget was consumed.
