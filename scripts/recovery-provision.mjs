/** Two synthetic customers and immutable sale terms for the isolated recovery runner. */
import { writeFileSync } from "node:fs";
import { configureFixtureRoute, createClient, provisionCustomer } from "../examples/consumption/bootstrap.mjs";
const { OXSUM_DEMO_BASE: base, OXSUM_ADMIN_TOKEN: adminToken, OXSUM_DEMO_UPSTREAM: upstream, OXSUM_DEMO_STATE: output } = process.env;
if (!base || !adminToken || !upstream || !output || new URL(base).hostname !== "127.0.0.1") throw new Error("Use the isolated recovery runner");
const call = createClient(base);
const first = await provisionCustomer(call, { adminToken, amountMinor: 100_000_000 });
const second = await provisionCustomer(call, { adminToken, amountMinor: 100_000_000, ownerEmail: first.ownerEmail, ownerPassword: first.ownerPassword });
for (const [index, customer] of [first, second].entries()) {
  customer.sale = await configureFixtureRoute(call, { ownerSession: first.ownerSession,
    organizationId: customer.registration.organization.id, channelName: `recovery-local-${index}`, model: "recovery-chat", upstream,
    price: { mode: "chat", inputPricePerMillion: 1_000_000, outputPricePerMillion: 1_000_000, maxOutputTokens: 64 } });
}
writeFileSync(output, JSON.stringify([first,second]), { mode: 0o600, flag: "wx" });
console.log("Two synthetic funded customers and sale books prepared.");
