# Design rules

The UI is the React dashboard under `web/` (issue #256 retired the Topcoat crate); its concrete token values are mapped into Tailwind's `@theme` in `web/src/index.css`, including the semantic aliases (`background`, `foreground`, `muted-foreground`, `input`, `ring`, …) the component layer resolves against. This file only records usage rules. The one server-rendered page left, `/verify`, keeps the shared palette through `crates/web/style/main.css`, the same tokens in plain CSS.

## Component vocabulary

There is exactly one component layer: `web/src/components/ui.tsx`. Interactive primitives (select, tabs, switch, progress) are built on `@base-ui/react` for behavior and styled with this palette — no second component library, no copy-in theme sources. Pages never import from anything else: when a shared piece is missing, it is added to `ui.tsx` rather than vendored per page. Component names are the plain words (`Button`, `Badge`, `Card`, …); decorative or one-off effects do not become components. (Recorded for issue #412: the `cult/` vendor directory and the `ui/` shims were removed in favor of this single layer; the galaxy catalog is a reference, not a dependency.)

## Colors

| token | used for |
| --- | --- |
| `primary` | primary buttons, links, selected states |
| `danger` | deletion, errors, insufficient balance |
| `success` | verification passed |
| `pending` | frozen (held) amounts |
| `text-muted` | secondary text, never body copy |

Text-to-background contrast is at least 4.5:1. Verification results must never rely on color alone; pair them with an icon and text.

## Type and spacing

- Type scale: 12 / 14 / 16 / 20 / 24; body text is 14
- Amounts and hashes always use a monospace face; amounts are right-aligned
- Spacing comes in multiples of 4 only: 4 / 8 / 12 / 16 / 24 / 32

## Component states

Every interactive component has: default, hover, focus (a keyboard-visible focus ring), disabled, loading.

## Page states

Every data page handles: loading, empty, error, and normal.

## Responsive

Breakpoints: 640 / 1024, mobile first.

## Accessibility

- Icon-only buttons must carry an `aria-label`
- Form controls must have an associated label
- Every action is reachable with the keyboard alone
