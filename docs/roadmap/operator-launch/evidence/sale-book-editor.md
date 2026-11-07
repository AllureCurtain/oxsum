# Sale book editor development evidence (#731)

The branch starts from `0314cbd`. Existing API fields are reused unchanged. The
editor separates mutable draft input, exact revision publication and activation;
all writes retain unchanged-intent retry identity and require an audit reason.

## Checks executed

- Frontend typecheck and production build pass; exact decimal/rule round-trip and
  UTC-boundary tests pass. The tests include the largest browser-safe integer,
  one microcredit, explicit free rates, conditional rate preservation, excessive
  fractional precision, overflow, duplicate models, mode bounds, invalid calendar
  dates and a one-microsecond active window. UTC text never adopts local time.
- Actual listener journey ran against an isolated PostgreSQL database using
  frozen development binary SHA-256
  `d1ee30d74c6d5d5a8c13cf4b04f6e3f955d23f6278f52ae781ff0fc9cf069fe6`
  and packed SDK SHA-256
  `ecc6e8d33cfecb1fad60d591376777ed939e583ff36d452a35f9135f481baaf9`.
  The binary was the collection-authority development build; it is not a final
  release candidate or proof that the new UI was exercised in a browser.
- Two freshly invited customers resolve different intended terms: default price
  previews 23 minor; customer model override previews 46. Each actual local
  request settles that exact amount, purchased balances decrease accordingly,
  and reservations end at zero. Exactly two local upstream attempts occurred.
- During the first customer's paused stream, a new draft is saved/published and
  atomically replaces the default. New preview becomes 69; the already-admitted
  stream still settles 23 under its original snapshot. Publication replay returns
  the same immutable result.
- Both immediate bill bundles, customer-held entry hashes, signed heads and keys
  are retained. After the listener stops and the scratch database is removed,
  the packed independent verifier passes inclusion/arithmetic, explicit sale and
  price binding, and head signature. Changed posting bytes are rejected. Fetch
  is disabled in that verifier.
- The first local fixture used too short a prompt, causing the correct freeze
  cap to bind before its declared usage. It was corrected to reserve enough for
  the declared usage; neither the accounting cap nor performance thresholds were
  weakened. All attempted runs removed their own server/database.

Retained ignored evidence: `local-notes/operator-launch/sales-listener-three/`.
This is local provider simulation, not inference or payment certification.
Strict workspace Clippy and 11 focused PostgreSQL sale-book/signed-head/router tests pass. CI, actual browser save/conflict/mobile/keyboard flows and genuine reviewer approval remain pending at this checkpoint.

CI follow-up: the Node sale-input tests were initially inserted in the WASM job,
whose path filter does not cover all UI-only edits. They now run in the frontend
job under its frontend filter. Local Node checks and actionlint passed; the
updated workflow requires a new PR CI result. No product code changed.
