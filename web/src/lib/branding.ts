// Seller branding (issue #614, T23): `GET /api/v1/branding` answers the
// deployment's display name, optional logo and optional `#rrggbb` accent. The
// accent maps onto the `primary` token family — the palette's action hue —
// through the CSS custom properties `@theme` in `index.css` declares, so the
// whole component layer follows without a prop thread. A deployment that
// configures nothing leaves the tokens at their compiled-in values.

import type { components } from "../api/schema";

export type Branding = components["schemas"]["Branding"];

/** `#rrggbb` → `[r, g, b]` channel triple, or `null` on a malformed value the
 * server's validation would already have refused. */
function parseHex(raw: string): [number, number, number] | null {
  const m = /^#([0-9a-fA-F]{6})$/.exec(raw);
  if (!m) return null;
  const n = Number.parseInt(m[1], 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function toHex(r: number, g: number, b: number): string {
  const c = (v: number) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, "0");
  return `#${c(r)}${c(g)}${c(b)}`;
}

/** Darken a channel triple toward black by `f` (0 = unchanged, 1 = black). */
function shade([r, g, b]: [number, number, number], f: number): string {
  return toHex(r * (1 - f), g * (1 - f), b * (1 - f));
}

/** Mix a channel triple toward white by `f` (0 = unchanged, 1 = white). */
function tint([r, g, b]: [number, number, number], f: number): string {
  return toHex(r + (255 - r) * f, g + (255 - g) * f, b + (255 - b) * f);
}

/**
 * Applies the seller's accent to the `primary` token family: the base sets
 * `--color-primary` and the foregrounds that read against it; a fixed hover
 * darkening and a soft tint derive the states every `bg-primary`/`text-primary`
 * consumer shares. Called once per branding read — idempotent, and a `null`
 * accent removes the override so a redeploy reverts cleanly.
 */
export function applyBranding(branding: Branding | undefined): void {
  const root = document.documentElement;
  const accent = branding?.accentColor ? parseHex(branding.accentColor) : null;
  if (!accent) {
    // No configured accent (or a read still in flight): the compiled-in
    // tokens stand — clear any override a previous application left.
    for (const token of [
      "--color-primary",
      "--color-primary-hover",
      "--color-primary-soft",
      "--color-accent",
      "--color-accent-foreground",
    ]) {
      root.style.removeProperty(token);
    }
    return;
  }
  root.style.setProperty("--color-primary", branding!.accentColor!);
  root.style.setProperty("--color-primary-hover", shade(accent, 0.12));
  root.style.setProperty("--color-primary-soft", tint(accent, 0.88));
  root.style.setProperty("--color-accent", tint(accent, 0.88));
  root.style.setProperty("--color-accent-foreground", branding!.accentColor!);
}
