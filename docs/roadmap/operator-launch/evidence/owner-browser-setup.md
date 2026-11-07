# Initial owner browser acceptance (#748)

Date: 2026-11-08. Base: `0314cbd3d5439adbc904a327a8e16704b344a0af`.
Scope: N03.a, partial J02. This evidence covers the initial owner ceremony;
profile, currency, provider, sale and merchant setup remain separate tasks.
It is development evidence, not an exact release-candidate acceptance.

The frontend was built with Node 24.22.1/npm 11.19.1 and served by the actual
debug server using two task-owned PostgreSQL databases. The first started
empty. The second used the existing demo seed to supply a pre-existing
customer identity; that is a labeled fixture, not a fresh-install result.
No external model, payment or mail request was made.

## Executed checks

- `npm --prefix web run build`: passed. Existing bundle-size advisory remains.
- `scripts/fmt-check.sh`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo nextest run -p oxsum-server --test business_setup`: 1 passed, 0 skipped;
  deployment authorization, session consent and completed-setup replay.
- `cargo nextest run -p oxsum-server --test business_setup_fresh`: 1 passed,
  0 skipped; new invite-only installation without provider configuration.
- `cargo nextest run -p oxsum-core --test business_setup`: 1 passed, 0 skipped;
  email binding, expiry, hashed secret, concurrent one-time acceptance,
  immutable audit and reopen behavior.
- English and Chinese setup resources have matching keys; diff whitespace
  check passed.

## Browser observations

The documented deployment endpoint minted the token. All ownership acceptance,
login, context selection and recovery checks below used the browser UI.

| Scenario | Observed outcome |
| --- | --- |
| Signed-out empty installation | Invited email, password, business name and explicit consent required; no public signup ownership inference |
| Wrong invited email, submitted with keyboard | Rejected; token masked and fields retained |
| Correct new identity | Business initialized invite-only; login link returns to setup; personal consumption key labeled separately |
| Login, reload, open console | Current staff access restored without a second token; explicit context switch reaches owner setup |
| Replay after initialization | Conflict with recovery instructions; no new owner or business |
| Pre-existing customer identity | Normal login returns to setup; current email shown and registration fields absent; acceptance succeeds |
| Local network-failure injection | Request rejected before transport; inputs retained and lost-response guidance shown; no success claim |
| Refresh after failure | Token and draft cleared |
| Desktop English / 390px Chinese | No horizontal overflow; error and consent remain readable; checkbox/submission reachable by keyboard |

A browser CDP offline toggle did not actually interrupt the request in this
browser session; that attempt is not counted as offline evidence. The table's
network case uses an explicit local `fetch` rejection, removed on reload.
The ordinary existing-user acceptance still succeeded through the server.

Private task records and screenshots live under the ignored
`local-notes/operator-launch/owner-ui/` directory. Tokens/passwords are not
included in this evidence. The existing TOTP login return branch is preserved;
a separately enrolled authenticator was not exercised in this browser pass.
