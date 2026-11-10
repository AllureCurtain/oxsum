import assert from "node:assert/strict";
import test from "node:test";
import { creditLimitText, creditTermsInput } from "../src/lib/customer-credit-input.ts";

test("credit forms preserve minor units and reject rounding or ambiguous day counts", () => {
  assert.deepEqual(creditTermsInput("1.000001", "0", ""), { creditLimitMinor: 1_000_001, paymentTermsDays: 0, dunningGraceDays: null });
  assert.equal(creditLimitText(9_007_199_254_740_991), "9007199254.740991");
  assert.equal(creditTermsInput(creditLimitText(9_007_199_254_740_991), "3650", "0")?.creditLimitMinor, 9_007_199_254_740_991);
  for (const value of ["0.0000001", "1e3", "-1", "9007199254.740992"]) assert.equal(creditTermsInput(value, "14", "3"), null);
  for (const value of ["", "-1", "1.5", "1e2", "3651"]) assert.equal(creditTermsInput("1", value, "3"), null);
  assert.equal(creditTermsInput("1", "14", "3651"), null);
});
