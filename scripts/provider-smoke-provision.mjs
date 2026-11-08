/** Current-authority fixture setup for the explicitly bounded provider exercise. */
import { writeFileSync } from 'node:fs';
import { configureFixtureRoute, createClient, provisionCustomer } from '../examples/consumption/bootstrap.mjs';
const { OXSUM_DEMO_BASE: base, OXSUM_ADMIN_TOKEN: adminToken, OXSUM_DEMO_UPSTREAM: upstream,
  OXSUM_DEMO_STATE: output, OXSUM_SMOKE_RELAY_TOKEN: relayToken } = process.env;
if (!base || !adminToken || !upstream || !output || !relayToken || new URL(base).hostname !== '127.0.0.1') {
  throw new Error('Run only through scripts/bounded-provider-smoke.py');
}
const call = createClient(base);
const state = await provisionCustomer(call, { adminToken, amountMinor: 100_000_000 });
const channel = 'bounded-deepseek';
const sale = await configureFixtureRoute(call, { ownerSession: state.ownerSession,
  organizationId: state.registration.organization.id, channelName: channel, model: 'deepseek-flash', upstream,
  price: { mode: 'chat', inputPricePerMillion: 1_000_000, outputPricePerMillion: 1_000_000, maxOutputTokens: 32 } });
// Replace the synthetic fixture credential with the guard's distinct local token.
// The external provider credential never enters the gateway or this subprocess.
await call('/api/v1/business/channels', { method: 'POST', session: state.ownerSession,
  body: { idempotencyKey: crypto.randomUUID(), reason: 'Bind the private bounded provider guard',
    config: { name: channel, baseUrl: upstream, protocol: 'openai', apiKeys: [relayToken],
      destinationPolicy: { network: 'internal', allowedIps: ['127.0.0.1'] } } } });
writeFileSync(output, JSON.stringify({ ...state, sale }), { mode: 0o600, flag: 'wx' });
console.log('Invited synthetic customer, exact receipt replay, sale terms and private guard prepared.');
