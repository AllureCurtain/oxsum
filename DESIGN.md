# Design rules

The UI is the React dashboard under `web/` (issue #256 retired the Topcoat crate); its concrete token values are mapped into Tailwind's `@theme` in `web/src/index.css`, including the semantic aliases (`background`, `foreground`, `muted-foreground`, `input`, `ring`, …) the component layer resolves against. This file only records usage rules. The one server-rendered page left, `/verify`, keeps the shared palette through `crates/web/style/main.css`, the same tokens in plain CSS.

Issue #448 widened the vocabulary to the shadcn semantic set so vendored components land on the same palette — surface ladder, shadow levels, a finer type scale, font features, radius scale, and a named z-order now have token definitions alongside the original color list.

## Component vocabulary

There is exactly one component layer: `web/src/components/ui/`. Interactive primitives are built on `@base-ui/react` for behavior and styled with this palette — no second component library, no copy-in theme sources. Pages never import from anything else: when a shared piece is missing, it is added to `ui/` rather than vendored per page. Component names are the plain words (`Button`, `Badge`, `Card`, …); decorative or one-off effects do not become components. (Recorded for issue #412: the `cult/` vendor directory and the `ui/` shims were removed in favor of this single layer; the galaxy catalog is a reference, not a dependency.)

## Colors

### Surfaces (light → lifted → dimmed)

| token | used for |
| --- | --- |
| `canvas` | page background, the lowest surface |
| `canvas-inset` | recessed fills — input underlays, zebra rows, code wells |
| `surface` | cards, panels, the default raised surface |
| `surface-raised` | floating surfaces — dropdowns, popovers, hover cards |
| `surface-overlay` | scrim behind modal dialogs (translucent ink) |
| `rail` / `rail-2` / `rail-3` | sidebar chrome ladder, dark-tinted to the palette hue |

### Text

| token | used for |
| --- | --- |
| `ink` | body copy, headings |
| `ink-secondary` | secondary labels, card subheadings |
| `muted` | helper text, captions, table labels — never body copy |
| `muted-2` | placeholders, finest print |

### Actions & states

| token | used for |
| --- | --- |
| `primary` | primary buttons, links, selected states |
| `danger` | deletion, errors, insufficient balance |
| `success` | verification passed |
| `warning` / `pending` | caution, frozen (held) amounts |
| `accent` | soft fills behind selected/active states |

Text-to-background contrast is at least 4.5:1. Verification results must never rely on color alone; pair them with an icon and text.

### Do / Don't

- **Do** use `surface-raised` + `shadow-2` for dropdowns/popovers; **don't** lift them with a plain `surface` — a floating panel on a flat surface reads as part of the page.
- **Do** put modal dialogs on `surface-overlay` scrim; **don't** show a dialog over the bare page.
- **Do** use `canvas-inset` for recessed content (input wells, zebra); **don't** use `canvas` — inset needs to read as *below* the card surface.
- **Don't** use `primary` for body text or general decoration — it's reserved for CTAs, links, and selection.
- **Don't** stack `muted` on `muted` — if text needs to be quieter than `muted`, use `muted-2`, never `muted/60` opacity.

## Elevation & depth

| level | token | used for |
| --- | --- | --- |
| 0 | (none) | default surface — borders carry the structure |
| 1 | `shadow-1` | card hover lift, subtle raise |
| 2 | `shadow-2` | dropdowns, popovers, floating panels |
| 3 | `shadow-3` | modal dialogs, sheets, drawers |

- Shadows are cool-tinted to the palette hue, never neutral black.
- A component uses at most one elevation level; a card inside a dropdown does not double-lift.

## Type and spacing

- Type scale: **10 / 11 / 12 / 13 / 14 / 16 / 18 / 20 / 24 / 28**; body text is 14
  - `micro` (10) — all-caps eyebrows, finest print
  - `caption` (11) — table labels, helper text
  - `small` (13) — dense UI, badges, compact buttons
  - `body` (14) — default body and UI text
  - `body-lg` (16) — lead paragraphs, section intros
  - `subhead` (18) — card titles, sub-section headings
  - `heading` (20) — section headings
  - `title` (24) — page titles
  - `display` (28) — KPI values, hero numbers
- Amounts and hashes always use a monospace face; amounts are right-aligned
- Numeric figures that must align but shouldn't go full mono use `font-feature-settings: "tnum"` (the `.tnum` utility) — KPI values, counts, table numbers
- `font-feature-settings: "ss01"` is on globally for the body text face
- Spacing comes in multiples of 4 only: 4 / 8 / 12 / 16 / 24 / 32

## Radius

| token | used for |
| --- | --- |
| `radius-xs` | hairline tags, table chrome |
| `radius-sm` | form inputs, compact elements |
| `radius-md` | buttons, standard cards |
| `radius-lg` | feature cards, dialogs |
| `radius-xl` | dashboard mockups, large containers |

- One control vocabulary: an input, a button, and a card never disagree on radius.
- Pill shape (`9999px`) is reserved for tag/badge pills and the tabs indicator, not general buttons.

## Component states

Every interactive component has: default, hover, focus (a keyboard-visible focus ring), disabled, loading.

## Page states

Every data page handles: loading, empty, error, and normal.

## Z-index order

| layer | token | used for |
| --- | --- | --- |
| 30 | `z-sticky` | sticky table headers, in-page sticky chrome |
| 40 | `z-topbar` | the app topbar |
| 50 | `z-dropdown` | dropdowns, popovers, tooltips |
| 60 | `z-modal` | dialogs, sheets, drawers |
| 70 | `z-toast` | toasts, transient notifications |

- Never use ad-hoc `z-[999]`; pick the named layer.
- A dropdown inside a modal stays at `z-dropdown` — the modal's own stacking context contains it.

## Responsive

Breakpoints: 640 / 1024, mobile first.

## Accessibility

- Icon-only buttons must carry an `aria-label`
- Form controls must have an associated label
- Every action is reachable with the keyboard alone
- Verification results must never rely on color alone; pair them with an icon and text
