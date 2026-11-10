# Explicit complimentary plan binding

Issue #988. The browser baseline required an operator-entered grant UUID without
explaining its format. A descriptive value produced the server's raw UUID parse
error. The portal now asks the operator to confirm the named customer and plan,
generates the authorization internally, and explains whether the initial period
is complimentary or requires payment. It retains an attempted selection until
its outcome is established. Customer detail state is keyed by customer identity.

Actual browser acceptance on frontend `e0b1ca60bc2155440c05117329c666aef4f037ef`
and backend `ca835528c5001434ae24256c177f1adb50bed274`:

- Paid-plan confirmation showed four credits and pending-payment semantics;
  cancelling that review produced no binding request.
- Free-plan confirmation showed zero credits and the explicit initial-period
  grant, without an identifier input.
- A page-local fetch fixture let the real PUT commit (HTTP 200), discarded its
  response and temporarily failed subsequent plan reads. The visible error and
  disabled selection survived closing/reopening the confirmation dialog.
- Switching to Chinese, 390px mobile and keyboard recovery preserved that same
  intent. After reads resumed, the portal matched the original period's exact
  complimentary authorization. It made **one PUT total**, retained **one period**,
  and closed the dialog only after establishing success. `recovery.json` and
  `recovered-plan.json` retain the matching request and original period.
- The customer session's direct Business navigation was denied before owner
  login. No provider call, payment order or wallet funding was used for the grant.

Five contract checks, TypeScript/production build, pinned formatting and diff
checks passed. Screenshots were visually inspected. Core/API behavior is unchanged;
the existing binding contract rejects a second live plan, so recovery reads are
required before retry. The fixture is local isolated development acceptance,
not final candidate or PSP evidence. A new page reload reads the standing binding;
unsubmitted choices are local form state.
