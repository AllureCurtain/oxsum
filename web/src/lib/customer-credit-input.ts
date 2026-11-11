import { decimalMinor } from "./money.ts";

/** Monthly terms never round money or silently truncate a day count. */
export function creditTermsInput(limit: string, terms: string, grace: string) {
  const creditLimitMinor = decimalMinor(limit, 6, true);
  const days = (value: string) => /^\d{1,4}$/.test(value) && Number(value) <= 3650 ? Number(value) : null;
  const paymentTermsDays = days(terms);
  const dunningGraceDays = grace === "" ? null : days(grace);
  if (creditLimitMinor === null || paymentTermsDays === null || (grace !== "" && dunningGraceDays === null)) return null;
  return { creditLimitMinor, paymentTermsDays, dunningGraceDays };
}

/** Render retained integer money as exact editable decimal text. */
export function creditLimitText(minor: number) {
  if (!Number.isSafeInteger(minor) || minor < 0) return "";
  const value = BigInt(minor);
  return `${value / 1_000_000n}.${String(value % 1_000_000n).padStart(6, "0")}`;
}
