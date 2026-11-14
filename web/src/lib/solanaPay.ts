/**
 * Classic Solana Pay transfer-request URLs (issue #1141): the `solana:` link a
 * wallet scans or opens, pinning the intent's terms — recipient, amount in the
 * asset's own decimal units, the `reference` marker the rail finds the paying
 * transaction by, plus a human label and memo. This is the transfer-request
 * spec, not the newer solana-foundation/pay flow.
 *
 *     solana:<recipient>?amount=<decimal>&spl-token=<mint>&reference=<marker>
 *         &label=<label>&memo=<memo>
 *
 * `spl-token` is omitted for native SOL; every other value is URL-encoded so a
 * label or memo cannot inject a second `reference` or `amount` into the
 * request — the wallet, not the page, decides what the transfer carries.
 */

/** The Solana cluster an intent pays on — the open-intent `cluster` field's
 * own union (issue #1181), spelled out here so the pure URL builder stays
 * free of the API layer. `mainnet-beta` is the production default a bare
 * Solana Pay URL already means; `devnet` and `testnet` are the values the
 * `cluster` parameter must carry. */
export type SolanaCluster = "mainnet-beta" | "devnet" | "testnet";

/** The cluster the page names beside an intent's terms (issue #1181): the
 * `cluster` value itself — wallets and the spec already spell the names the
 * customer needs to see, so the display string is the value verbatim, kept
 * behind one helper so the page never drops a stray literal. `undefined`/
 * an unknown value answers `mainnet-beta`: an intent that does not declare
 * a network is production-shaped by the same rule the endpoint resolution
 * takes — the dangerous direction is never rendered as test money. */
export function solanaClusterName(cluster: SolanaCluster | string | null | undefined): SolanaCluster {
  if (cluster === "devnet" || cluster === "testnet") return cluster;
  return "mainnet-beta";
}

/** The terms the transfer-request URL pins. `assetDecimals` converts
 * `baseUnits` (lamports for SOL, the mint's smallest unit for an SPL token)
 * into the decimal `amount` wallets display; `splToken` stays `null`/`undefined`
 * for native SOL so no `spl-token` parameter is emitted. */
export interface SolanaPayRequest {
  /** The deposit address the transfer pays (`payTo`). */
  recipient: string;
  /** The base units the payment must deliver (`amountLamports`). */
  baseUnits: number;
  /** The asset's decimals — 9 for SOL, 6 for USDC; `baseUnits` is divided by
   * `10^assetDecimals` to write the `amount` parameter. */
  assetDecimals: number;
  /** The per-intent marker account the paying transaction must carry. */
  reference: string;
  /** The SPL mint when the intent prices a token; omitted for native SOL. */
  splToken?: string | null;
  /** The network the payment must land on (issue #1181). `mainnet-beta` (or
   * unset) writes no `cluster` parameter — Solana Pay's own spec treats it
   * as the default; `devnet`/`testnet` write `cluster=` so a scanning wallet
   * sends on the same network the rail verifies. */
  cluster?: SolanaCluster;
  /** The wallet-facing label (e.g. "oxsum top-up"). */
  label?: string;
  /** The wallet-facing memo (e.g. the deposit id). */
  memo?: string;
}

/**
 * Writes `baseUnits` as a plain decimal string at `decimals` places — exact,
 * never floating point, so 5_000 lamports at 9 decimals is "0.000005", not a
 * rounded or exponent form a wallet might reject. Mirrors `decimalMinor` in
 * `money.ts`: integer math, padded fraction, no scientific notation.
 */
export function baseUnitsDecimal(baseUnits: number, decimals: number): string | null {
  if (!Number.isSafeInteger(baseUnits) || baseUnits < 0) return null;
  if (!Number.isSafeInteger(decimals) || decimals < 0 || decimals > 30) return null;
  const units = BigInt(baseUnits);
  if (decimals === 0) return units.toString();
  const scale = 10n ** BigInt(decimals);
  const whole = units / scale;
  const fraction = (units % scale).toString().padStart(decimals, "0");
  // Trailing zeros carry no value; strip them, then a now-empty fraction with
  // its point, so "1.230000000" reads "1.23" and "5.000000000" reads "5".
  return `${whole}.${fraction}`.replace(/\.?0+$/, "");
}

/**
 * Builds the `solana:` transfer-request URL for an opened intent, or `null`
 * when the terms cannot pin an honest request (a negative or non-integer
 * amount, a missing recipient or reference) — the page then falls back to the
 * manual fields rather than render a QR that names nothing. Every parameter is
 * URL-encoded: Solana keys are base58 and need none, but the label and memo
 * are free text, and an unencoded `&` or `reference=` in either would smuggle
 * a second value into the request.
 */
export function solanaPayUrl(request: SolanaPayRequest): string | null {
  const { recipient, baseUnits, assetDecimals, reference, splToken, cluster, label, memo } = request;
  if (recipient.trim() === "" || reference.trim() === "") return null;
  const amount = baseUnitsDecimal(baseUnits, assetDecimals);
  if (amount === null) return null;
  const params = new URLSearchParams();
  params.set("amount", amount);
  // `spl-token` names the mint an SPL transfer must move; native SOL leaves
  // the parameter off entirely so a wallet never treats the URL as a token
  // request.
  if (splToken) params.set("spl-token", splToken);
  params.set("reference", reference);
  // `cluster` names the network the transfer must land on (issue #1181):
  // Solana addresses are identical across clusters, so a devnet/testnet
  // intent must pin it or the wallet defaults to mainnet — real money to the
  // same `payTo`. `mainnet-beta` omits it, matching the spec's own default.
  if (cluster !== undefined && cluster !== "mainnet-beta") params.set("cluster", cluster);
  if (label !== undefined && label !== "") params.set("label", label);
  if (memo !== undefined && memo !== "") params.set("memo", memo);
  return `solana:${encodeURIComponent(recipient)}?${params.toString()}`;
}
