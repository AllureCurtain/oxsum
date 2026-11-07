/** Prepare the explicit event channel/terms used by the isolated example runner. */
import { createClient, sessionOf } from "./consumption/bootstrap.mjs";

const call = createClient(process.env.OXSUM_BASE_URL);
const login = await call("/api/v1/auth/login", { method: "POST", body: {
  email: process.env.OXSUM_OWNER_EMAIL, password: process.env.OXSUM_OWNER_PASSWORD,
} });
const session = sessionOf(login);
await call("/api/v1/session/context", { method: "POST", session, body: {
  idempotencyKey: crypto.randomUUID(), context: "business",
} });
const channel = process.env.OXSUM_EVENT_CHANNEL;
const model = process.env.OXSUM_EVENT_CODE;
const price = { mode: "event", inputPricePerMillion: 4000, outputPricePerMillion: 0, maxOutputTokens: 0 };
const mutate = (path, config) => call(path, { method: "POST", session,
  body: { idempotencyKey: crypto.randomUUID(), reason: "Isolated event example fixture", config },
});
await mutate("/api/v1/business/channels", { name: channel, baseUrl: "http://127.0.0.1:9/v1",
  protocol: "openai", apiKeys: ["local-stub-no-provider-credential"],
  destinationPolicy: { network: "internal", allowedIps: ["127.0.0.1"] },
});
await mutate(`/api/v1/business/channels/${channel}/prices`, { model, price, weight: 100 });
// The consumption example already assigned a book. Add model-specific terms
// instead of replacing or overlapping the customer's existing assignment.
await call("/api/v1/business/sale-books/changes", { method: "POST", session, body: {
  idempotencyKey: crypto.randomUUID(), reason: "Explicit local event sale terms", change: {
    operation: "overrideModel", organizationId: process.env.OXSUM_CUSTOMER_ID,
    model, terms: { price, discountPercent: 0 },
  },
} });
console.log("[PASS] Explicit local event channel and customer sale terms");
