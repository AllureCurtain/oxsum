/** Prepare the isolated Python SDK demo through the shared current-authority flow. */
import { writeFileSync } from "node:fs";
import { configureFixtureRoute, createClient, provisionCustomer } from "../examples/consumption/bootstrap.mjs";

const { OXSUM_DEMO_BASE: base, OXSUM_ADMIN_TOKEN: adminToken,
  OXSUM_DEMO_UPSTREAM: upstream, OXSUM_DEMO_STATE: output } = process.env;
if (!base || !adminToken || !upstream || !output) throw new Error("Run through demo/demo.py");
if (new URL(base).hostname !== "127.0.0.1") throw new Error("Demo server must be loopback");
const call = createClient(base);
const setup = await provisionCustomer(call, { adminToken, amountMinor: 100_000_000 });
const sale = await configureFixtureRoute(call, {
  ownerSession: setup.ownerSession, organizationId: setup.registration.organization.id,
  channelName: "demo-local", model: "demo-chat", upstream,
  price: { mode: "chat", inputPricePerMillion: 1_000_000,
    outputPricePerMillion: 1_000_000, maxOutputTokens: 64 },
});
writeFileSync(output, JSON.stringify({ ...setup, sale }), { mode: 0o600, flag: "wx" });
console.log("[PASS] Owner consent, invited customer, receipt replay and explicit sale terms");
console.log("Local synthetic funding: 100 credits; no PSP or real payment involved.");
