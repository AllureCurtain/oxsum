/** Parse decimal text without rounding or floating-point money multiplication. */
export function decimalMinor(raw: string, digits: number, allowZero = false): number | null {
  const match = /^(\d+)(?:\.(\d+))?$/.exec(raw.trim());
  if (!match || (match[2]?.length ?? 0) > digits || raw.length > 40) return null;
  const value = BigInt(match[1]) * 10n ** BigInt(digits) + BigInt((match[2] ?? "").padEnd(digits, "0") || "0");
  if (value > BigInt(Number.MAX_SAFE_INTEGER) || value < (allowZero ? 0n : 1n)) return null;
  return Number(value);
}

/** Quote sufficient two-digit provider minor units to cover a credit debt. */
export function collectionMinor(credits: number, rate: string): number | null {
  if (!Number.isSafeInteger(credits) || credits <= 0 || !/^\d+(?:\.\d{1,8})?$/.test(rate)) return null;
  const [whole, fraction = ""] = rate.split(".");
  const numerator = BigInt(credits) * BigInt(whole + fraction) * 100n;
  const denominator = 10n ** BigInt(6 + fraction.length);
  const value = (numerator + denominator - 1n) / denominator;
  return value > 0n && value <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(value) : null;
}
