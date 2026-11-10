# Original-window plan consumption (#973)

Date: 2026-11-10. Development browser acceptance, local payment/model fixtures.

The same customer and exact plan period read **0** consumed before the repair,
although the quota engine had recorded **80/100** minor after two actual browser
SSE calls. The repaired binary and Plans page both read **80** (0.00008 credits).
No new model call, money write or quota repair was needed. Cancellation remains
scheduled at period end; the newly opened dispute debt is separate from usage.

The read matches the customer plan's original subscription, organization and
period start; a closed period retains its own usage and late returns. An absent
historical engine window falls back to the retained authorization snapshot.
Reservation admission and money behavior do not change.

Baseline backend: `ac7bb191ee7a2b52af4a23f2ce54cd9c6f9d82e5`.
Repaired backend/frontend source: `3b5b55be4b54cf4bf9ba35c87d6119bd4b108ff7`.
Binary SHA-256: `4f56cc7735524163b08fe2718ad03450609569c389c978e5dadd07a88bc4bef8`.
Schema: `0128_application_notifications`. Exact public responses, visually
inspected screenshots and frontend byte inventory accompany this note.

Validation: all **17 core and HTTP plan checks** passed, covering current
reservations, net settlement, late returns, cancellation and old/new renewal
window isolation; strict core/server all-target Clippy, pinned formatting,
five contract checks, code generation and TypeScript/Vite build passed.
The unchanged cross-crate baseline has the separate 2,256-pass workspace run
recorded in [manual refund evidence](../manual-refund-route/README.md); the
read-only follow-up was checked with the affected plan suites.

This is development acceptance, not an exact release candidate, external PSP,
month-end, capacity or independent pilot claim. No secrets are included.
