import test from "node:test";
import assert from "node:assert/strict";
import { decimalMinor, collectionMinor } from "../src/lib/money.ts";

test("money text is exact, precision bounded and never rounded into another intent", () => {
  assert.equal(decimalMinor("0.29", 2), 29);
  assert.equal(decimalMinor("90071992547409.91", 2), Number.MAX_SAFE_INTEGER);
  assert.equal(decimalMinor("90071992547409.92", 2), null);
  assert.equal(decimalMinor("0.001", 2), null);
  assert.equal(decimalMinor("1e3", 2), null);
  assert.equal(decimalMinor("-1", 2), null);
  assert.equal(decimalMinor("0", 6, true), 0);
  assert.equal(decimalMinor("1.000001", 6), 1000001);
  assert.equal(collectionMinor(1500000000, "0.0014"), 210);
  assert.equal(collectionMinor(1, "1"), 1);
  assert.equal(collectionMinor(Number.MAX_SAFE_INTEGER + 1, "1"), null);
});
