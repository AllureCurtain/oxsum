# Current checkout authority descriptions (#799)

The review uses integrated baseline `04e4bec`. It corrects current descriptions
of existing behavior; it does not establish payment, browser or release
acceptance.

## Source review

| Current description | Existing authority checked | Correction |
| --- | --- | --- |
| Two Payment order rows in the product table | [`create_payment_order`](../../../../crates/core/src/payment_orders.rs) and [`plan_payments`](../../../../crates/core/src/plan_payments.rs) | Keep one row covering published bundle quantities, the exact sold plan fee, the upward fiat quote rounding and Business collection conversion for other purposes. Retain replay, webhook and fulfillment rules. |
| Three top-up rails and code redemption accept any organization credential | [`customer_route_capability`](../../../../crates/server/src/auth.rs), [`require_customer`](../../../../crates/core/src/customer_authority.rs) and their [OpenAPI](../../../../crates/server/openapi.yaml) operations | State that checkout and redemption require a customer personal session. The route allowlist classifies these POSTs as `Personal`; key principals do not satisfy it. |
| The Orders page takes its currency from organization display preferences | [`OrderNew`](../../../../web/src/pages/Orders.tsx) | State that top-ups use Business collection currency and bundles use their published price/currency. The form reads collection terms and the selected offer. |
| OpenAPI manual orders use a customer display rate, and every order amount is derived by fiat conversion | [`create_manual_payment_order`](../../../../crates/core/src/payment_orders.rs), order creation and plan quote calculation | Match the persisted Business collection rate and the separate bundle/sold-plan authority already implemented. No request fields, security requirements or response shapes change. |

[API conventions](../../../api.md) already state the same payment authority and
need no further edit. Historical decision entries remain unchanged. The N11
decision's inconsistent day-one/period-close wording and activation boundary
were reported to the financial-policy work; this correction does not choose
new revenue-recognition policy.

## Validation

- `npm --prefix web run codegen` passed. Regeneration updates the two generated
  order descriptions and removes a second, identical `InvitationDeliveryState`
  declaration left in the baseline generated file. The OpenAPI source has one
  declaration. After removing comments and that duplicate, the generated type
  content matches the baseline exactly.
- `./node_modules/.bin/tsc -b` passed from `web/`.
- The YAML parses successfully. Comparing parsed documents with `description`
  values removed finds no structural change.
- Local Markdown links in the changed documents and the new issue links were
  checked; `git diff --check` passed. The final diff was reviewed against the
  source references above.
- No Rust, database or browser test was run for these documentation changes.

The independent OpenAPI defect that references an undefined `apiKey` security
scheme is tracked by [#800](https://github.com/AllureCurtain/oxsum/issues/800).
Those requirements are unchanged here; its own repair must audit the actual
credential class of every affected route.
