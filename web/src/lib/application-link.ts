/** A fragment-only secret may survive sign-in in this tab for at most seven days. */
export const applicationLinkKey = "oxsum.application-link";
export function applicationLinkToken(fragment: string, retained: string | null, now: number): string {
  if (fragment) return /^#oxs-lnk-[a-f0-9]{64}$/.test(fragment) ? fragment.slice(1) : "";
  try {
    const entry: unknown = JSON.parse(retained ?? "null");
    if (!entry || typeof entry !== "object" || !("token" in entry) || !("until" in entry)) return "";
    return typeof entry.token === "string" && /^oxs-lnk-[a-f0-9]{64}$/.test(entry.token) && typeof entry.until === "number" && entry.until > now && entry.until <= now + 7 * 86_400_000 ? entry.token : "";
  } catch { return ""; }
}
