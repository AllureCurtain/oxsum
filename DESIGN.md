# Design rules

The UI is built in `crates/web` (Leptos); the concrete token values live in `crates/web/style/main.css`, and this file only records usage rules.

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
