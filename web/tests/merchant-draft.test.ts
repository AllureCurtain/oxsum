import assert from "node:assert/strict";
import test from "node:test";
import { merchantConfig, merchantDraft } from "../src/lib/merchant-draft.ts";

test("redacted versions never synthesize reusable secret placeholders", () => {
  const draft = merchantDraft({ id: "v", merchantId: "m", revision: 2, apiUrl: "https://api.example.com", publishableKey: "pk_test_fixture", sdkUrl: "https://sdk.example.com/sdk.js", checkoutUrl: null, destinationPolicy: { network: "internal", allowedIps: ["127.0.0.1"] }, source: "owner", validationStatus: "locallyValidated", createdAt: "" });
  assert.equal(draft.apiKey, ""); assert.equal(draft.webhookSecret, ""); assert.equal(draft.clientId, "");
  assert.equal(draft.publishableKey, "pk_test_fixture"); assert.equal(draft.allowedIps, "127.0.0.1");
});

test("complete merchant versions include only their rail's fields and exact secrets", () => {
  const draft = { ...merchantDraft(), apiUrl: " https://api.example.com ", apiKey: " exact secret ", webhookSecret: " webhook ", clientId: "private client", publishableKey: "pk_test_fixture", sdkUrl: "https://sdk.example.com/sdk.js", checkoutUrl: "https://checkout.example.com", allowedIps: "127.0.0.1" };
  const stripe = merchantConfig("stripe", draft);
  assert.deepEqual(stripe, { apiUrl: "https://api.example.com", apiKey: " exact secret ", webhookSecret: " webhook ", destinationPolicy: { network: "public" } });
  assert.deepEqual(merchantConfig("hyperswitch", draft), { ...stripe, publishableKey: draft.publishableKey, sdkUrl: draft.sdkUrl });
  assert.deepEqual(merchantConfig("airwallex", draft), { ...stripe, clientId: draft.clientId, checkoutUrl: draft.checkoutUrl });
  assert.deepEqual(merchantConfig("stripe", { ...draft, network: "internal", allowedIps: "127.0.0.1, ::1\n10.0.0.2" }).destinationPolicy, { network: "internal", allowedIps: ["127.0.0.1", "::1", "10.0.0.2"] });
});
