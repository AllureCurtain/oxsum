// ---------------------------------------------------------------------------
// Money helpers — integer minor units, 1 credit = 1_000_000, never float math.
// ---------------------------------------------------------------------------

export function credits(minor: number): string {
  const sign = minor < 0 ? "-" : "";
  const abs = Math.abs(minor);
  const whole = Math.trunc(abs / 1_000_000);
  const frac = String(abs % 1_000_000).padStart(6, "0").replace(/0+$/, "");
  return `${sign}${frac === "" ? String(whole) : `${whole}.${frac}`}`;
}

/** Minor units with the sign made explicit — the bills page's signed amounts. */
export function signedCredits(minor: number): string {
  const text = credits(minor);
  return minor > 0 ? `+${text}` : text;
}

/**
 * The org's display currency as the API answered it (issue #257). `null` —
 * or a missing org read — renders credits alone. The conversion is a pure
 * presentation: minor units are still the ledger's truth everywhere.
 */
export type DisplayCurrency = { code: string; ratePerCredit: string; decimals: number };

/**
 * What `minor` displays as in the currency, or `null` when the org renders
 * credits only. Decimal-string math (never a float): the rate is mantissa /
 * 10^scale, the render rounds half-up to `decimals` places.
 */
export function displayAmount(minor: number, currency: DisplayCurrency): string {
  const [whole, frac = ""] = currency.ratePerCredit.split(".");
  const mantissa = BigInt(whole + frac);
  const rateScale = frac.length;
  const renderScale = currency.decimals;
  const numerator = BigInt(minor) * mantissa * 10n ** BigInt(renderScale);
  const denominator = 10n ** BigInt(6 + rateScale); // 1_000_000 minor × rate scale
  const negative = numerator < 0n;
  const magnitude = negative ? -numerator : numerator;
  const rounded = (magnitude + denominator / 2n) / denominator;
  const unit = 10n ** BigInt(renderScale);
  const w = rounded / unit;
  const f = rounded % unit;
  const sign = negative && rounded > 0n ? "-" : "";
  return renderScale === 0
    ? `${sign}${w}`
    : `${sign}${w}.${String(f).padStart(renderScale, "0")}`;
}

/**
 * The same amount in both units: `"1.5 cr ≈ 0.01 USD"`, or `"1.5 cr"` when
 * the org renders credits only. The fiat half is presentational — a bill's
 * proof covers the credits, never the conversion.
 */
export function creditsWithDisplay(minor: number, currency: DisplayCurrency | null | undefined): string {
  const cr = credits(minor);
  return currency ? `${cr} cr ≈ ${displayAmount(minor, currency)} ${currency.code}` : `${cr} cr`;
}

/**
 * One money amount, rendered as the page shows it: the credits figure, with
 * the fiat conversion as a quiet second line when the org displays in one.
 * `minor` is the ledger's integer; the conversion never feeds a charge.
 */
export function Cr(props: { minor: number; currency?: DisplayCurrency | null }) {
  return (
    <span className="tabular-nums">
      {credits(props.minor)} cr
      {props.currency ? (
        <span className="text-muted">
          {" "}
          ≈ {displayAmount(props.minor, props.currency)} {props.currency.code}
        </span>
      ) : null}
    </span>
  );
}
