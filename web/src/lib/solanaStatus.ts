/**
 * The opened Solana intent's lifecycle as the Topup page reads it (issue
 * #1148): a countdown to `expiresAt` and a status line that maps every
 * answer the confirm endpoint can give — including the exceptions issue
 * #1142's deposits queue names — onto one tone + translation key pair. Pure
 * functions so the page stays markup and the mapping stays testable.
 *
 * The seam to #1142: `solanaStatus` accepts either a confirm answer's HTTP
 * status (`409` → the exception the error's `code`/`message` names) or —
 * once the deposits list ships — a queue row's literal `status` string
 * (`"underpaid"`, `"late"`, `"overpaid"`, `"flagged"`, `"refund_pending"`,
 * `"refunded"`, `"expired"`, `"credited"`). The page passes `{status}` from
 * an ApiError today; a deposits row passes `{status: row.status}` tomorrow
 * and the same line answers.
 */

/** Where the intent stands as far as the customer cares. */
export type SolanaPhase =
  /** The wallet has not paid yet — or the rail has not found the transfer. */
  | "waiting"
  /** A terminal exception the mapping could not name — the server's own
   * message carries the detail. */
  | "exception"
  /** The transfer credited the wallet (confirm's 200, or a balance bump). */
  | "credited"
  /** The transfer underpaid the intent — the deposit is queued for review. */
  | "underpaid"
  /** The transfer landed at/after `expiresAt` — queued the same way. */
  | "late"
  /** The transfer over-delivered; the opened amount still credited. */
  | "overpaid"
  /** Screening parked the payment for operator review. */
  | "flagged"
  /** The clock ran out before any transfer landed — open a new intent. */
  | "expired"
  /** confirm's 404: the intent no longer sits `pending` and no balance bump
   * was seen — per the issue this reads as *still waiting*, never an error. */
  | "settling";

/** The tone a status banner takes. */
export type SolanaTone = "info" | "warn" | "success" | "error";

export interface SolanaStatusLine {
  phase: SolanaPhase;
  tone: SolanaTone;
  /** The `topup.*` i18n key — present in en + zh-CN together. */
  key: string;
}

/** The countdown text: `m:ss`, clamped at zero — `null` when the intent's
 * `expiresAt` cannot parse (the page then shows the raw timestamp instead). */
export function solanaCountdown(expiresAt: string, now: number): string | null {
  const expiry = Date.parse(expiresAt);
  if (!Number.isFinite(expiry)) return null;
  const remaining = Math.max(0, Math.floor((expiry - now) / 1000));
  const minutes = Math.floor(remaining / 60);
  const seconds = remaining % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/** `true` once `expiresAt` has passed — the intent lapsed unpaid. */
export function solanaExpired(expiresAt: string, now: number): boolean {
  const expiry = Date.parse(expiresAt);
  return Number.isFinite(expiry) && expiry <= now;
}

/** The seconds left under which the countdown turns urgent (danger). */
export const SOLANA_URGENT_SECS = 60;

export function solanaUrgent(expiresAt: string, now: number): boolean {
  const expiry = Date.parse(expiresAt);
  if (!Number.isFinite(expiry)) return false;
  const remaining = (expiry - now) / 1000;
  return remaining > 0 && remaining <= SOLANA_URGENT_SECS;
}

/**
 * Maps a confirm answer — HTTP status plus the error's machine code and human
 * message — onto the line the status block shows.
 *
 * - `200` / receipt → `credited` (the page reads the receipt itself).
 * - `403` → still `waiting`: no honoring transfer has landed yet.
 * - `404` → `settling`: no `pending` intent answers the reference. Per issue
 *   #1148 that is informational — the poll may have just credited it (the
 *   balance watcher confirms that case) or it is still finalizing.
 * - `409` → the exception queue's verdict: `underpaid` or `late` from the
 *   error's `code` when the server names it (`UNDERPAID`, `DEPOSIT_LATE`, …),
 *   else parsed from the message the route sends; unknown 409s read `warn`
 *   with the server's own message.
 * - Anything else → `error` with the server's message.
 */
export function solanaStatus(input: {
  status?: number;
  code?: string;
  message?: string;
  expired?: boolean;
}): SolanaStatusLine {
  const { status, code, message, expired } = input;
  if (expired === true) {
    return { phase: "expired", tone: "warn", key: "topup.solanaStatusExpired" };
  }
  // A named exception status — the deposits-queue row's word, or the wire
  // code when one lands — reads the same whatever surface delivered it.
  const named = (code ?? "").toLowerCase();
  if (named === "underpaid" || named === "solana_underpaid") {
    return { phase: "underpaid", tone: "error", key: "topup.solanaStatusUnderpaid" };
  }
  if (named === "late" || named === "solana_late") {
    return { phase: "late", tone: "error", key: "topup.solanaStatusLate" };
  }
  if (named === "overpaid" || named === "solana_overpaid") {
    return { phase: "overpaid", tone: "warn", key: "topup.solanaStatusOverpaid" };
  }
  if (named === "flagged" || named === "solana_flagged") {
    return { phase: "flagged", tone: "warn", key: "topup.solanaStatusFlagged" };
  }
  switch (status) {
    case 200:
      return { phase: "credited", tone: "success", key: "topup.solanaStatusCredited" };
    case 403:
      return { phase: "waiting", tone: "info", key: "topup.solanaStatusWaiting" };
    case 404:
      return { phase: "settling", tone: "info", key: "topup.solanaStatusSettling" };
    case 409: {
      // #1142's 409 names the exception in its message ("underpaid",
      // "after the intent expired", "flagged by screening"); a code may do
      // so too. Match the word, never the whole sentence.
      const text = `${code ?? ""} ${message ?? ""}`.toLowerCase();
      if (text.includes("underpaid")) {
        return { phase: "underpaid", tone: "error", key: "topup.solanaStatusUnderpaid" };
      }
      if (text.includes("late") || text.includes("expired")) {
        return { phase: "late", tone: "error", key: "topup.solanaStatusLate" };
      }
      // #1184's flagged confirm: the parked deposit reads held-for-review,
      // never the "settling" a 404 would pretend.
      if (text.includes("flagged") || text.includes("held for review")) {
        return { phase: "flagged", tone: "warn", key: "topup.solanaStatusFlagged" };
      }
      return { phase: "exception", tone: "error", key: "topup.solanaStatusException" };
    }
    default:
      if (status === undefined) {
        return { phase: "waiting", tone: "info", key: "topup.solanaStatusWaiting" };
      }
      return { phase: "exception", tone: "error", key: "topup.solanaStatusError" };
  }
}
