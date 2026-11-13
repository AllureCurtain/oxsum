#!/usr/bin/env node
/**
 * Solana top-up walkthrough (issue #1117): the customer-facing half of the
 * rail, asserted step by step against a live deployment.
 *
 *   OXSUM_BASE_URL=http://127.0.0.1:3000 \
 *   OXSUM_SOLANA_PAYER=<customer account's email:password> \
 *   node topup.mjs <amountMinor>
 *
 * `OXSUM_SOLANA_PAYER` is the oxsum *customer* login (an `email:password`
 * pair for `/api/v1/auth/login`), not a chain key — the on-chain wallet is
 * `payer.json` beside this script. What it does, in order:
 *
 *   1. logs the customer in (personal session) and reads `/api/v1/balance`
 *   2. opens the intent — `POST /api/v1/topups/solana` answers
 *      `{reference, payTo, amountLamports, splToken, expiresAt}`
 *   3. pays it on chain with `pay.mjs`'s recipe (reference marker attached)
 *   4. settles it — `POST /api/v1/topups/solana/confirm` with the reference;
 *      asserts the wallet's `availableMinor` grew by the asked amount
 *   5. replays the confirm — the documented `404 NOT_FOUND`, since the
 *      deposit is already `credited`; a second intent is left for the 15 s
 *      `solana-deposits` poll to credit on its own, then asserted
 *
 * Env: OXSUM_BASE_URL (required), OXSUM_SOLANA_PAYER (required),
 * SOLANA_RPC_URL (default http://127.0.0.1:8899 — the local validator).
 * Run `node keys.mjs` first to mint payer.json/payto.json.
 */
import { Connection, Keypair, PublicKey, SystemProgram, Transaction } from '@solana/web3.js';
import fs from 'node:fs';
import path from 'node:path';
import url from 'node:url';

const HERE = path.dirname(url.fileURLToPath(import.meta.url));
const BASE = (process.env.OXSUM_BASE_URL ?? '').replace(/\/+$/, '');
const WHO = process.env.OXSUM_SOLANA_PAYER ?? '';
const RPC = process.env.SOLANA_RPC_URL ?? 'http://127.0.0.1:8899';
if (!BASE || !WHO) {
  console.error('usage: OXSUM_BASE_URL=… OXSUM_SOLANA_PAYER=email:password node topup.mjs [amountMinor]');
  process.exit(2);
}
const AMOUNT_MINOR = Number(process.argv[2] ?? 1000);
const [email, password] = WHO.split(':');

const state = { pass: 0, fail: 0 };
const check = (ok, label, detail = '') => {
  state[ok ? 'pass' : 'fail']++;
  console.log(`[${ok ? 'PASS' : 'FAIL'}] ${label}${detail ? ` — ${detail}` : ''}`);
  if (!ok) process.exitCode = 1;
};

/** Minimal client mirroring bootstrap.mjs' shape: personal session only. */
const call = async (p, { method = 'GET', session, body } = {}) => {
  const headers = {};
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (session) headers.cookie = `oxsum_session=${session}`;
  const res = await fetch(BASE + p, {
    method, headers, redirect: 'manual', signal: AbortSignal.timeout(30_000),
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null, setCookie: res.headers.get('set-cookie') };
};

const login = await call('/api/v1/auth/login', { method: 'POST', body: { email, password } });
check(login.status === 200, 'customer login', email);
const session = (login.setCookie ?? '').split(';')[0].replace(/^oxsum_session=/, '');
if (!session) { console.log('[FAIL] no session cookie — finish any second factor separately'); process.exit(1); }
await call('/api/v1/session/context', {
  method: 'POST', session, body: { idempotencyKey: `solana-${crypto.randomUUID()}`, context: 'personal' },
}).catch(() => {});

const balance = async () => (await call('/api/v1/balance', { session })).json.data.availableMinor;
const before = await balance();

// 1 — open the intent.
const intent = await call('/api/v1/topups/solana', {
  method: 'POST', session, body: { amountMinor: AMOUNT_MINOR },
});
check(intent.status === 200, 'open intent', JSON.stringify(intent.json?.data));
if (intent.status !== 200) process.exit(1);
const { reference, payTo, amountLamports, splToken } = intent.json.data;
check(!splToken, 'native SOL intent', splToken ? `unexpected mint ${splToken}` : 'no SPL mint configured');

// 2 — pay it on chain: the transfer plus the read-only reference marker.
const conn = new Connection(RPC, 'confirmed');
const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(fs.readFileSync(path.join(HERE, 'payer.json'), 'utf8'))));
const ix = SystemProgram.transfer({
  fromPubkey: payer.publicKey, toPubkey: new PublicKey(payTo), lamports: Number(amountLamports),
});
ix.keys.push({ pubkey: new PublicKey(reference), isSigner: false, isWritable: false });
const sig = await conn.sendTransaction(new Transaction().add(ix), [payer]);
await conn.confirmTransaction(sig, 'confirmed');
check(true, 'transfer confirmed', `${amountLamports} lamports → ${payTo} (sig ${sig})`);

// 3 — settle now with the reference (never the signature — the rail finds it).
const confirm = await call('/api/v1/topups/solana/confirm', {
  method: 'POST', session, body: { reference },
});
check(confirm.status === 200, 'confirm settles', JSON.stringify(confirm.json?.data));
const mid = await balance();
check(mid === before + AMOUNT_MINOR, 'wallet credited once', `${before} → ${mid}`);

// 4 — a replayed confirm is the documented 404: the deposit is credited, not pending.
const replay = await call('/api/v1/topups/solana/confirm', {
  method: 'POST', session, body: { reference },
});
check(replay.status === 404, 'replayed confirm answers 404', `got ${replay.status}`);
check((await balance()) === mid, 'replay credits nothing');

// 5 — a second intent left alone is credited by the solana-deposits poll.
const second = await call('/api/v1/topups/solana', {
  method: 'POST', session, body: { amountMinor: AMOUNT_MINOR },
});
const r2 = second.json.data;
const ix2 = SystemProgram.transfer({
  fromPubkey: payer.publicKey, toPubkey: new PublicKey(r2.payTo), lamports: Number(r2.amountLamports),
});
ix2.keys.push({ pubkey: new PublicKey(r2.reference), isSigner: false, isWritable: false });
await conn.confirmTransaction(await conn.sendTransaction(new Transaction().add(ix2), [payer]), 'confirmed');
console.log('[INFO] second intent paid; waiting on the 15 s solana-deposits poll…');
const deadline = Date.now() + 45_000;
let after = await balance();
while (after < mid + AMOUNT_MINOR && Date.now() < deadline) {
  await new Promise((r) => setTimeout(r, 3_000));
  after = await balance();
}
check(after === mid + AMOUNT_MINOR, 'poll credits the unpaid intent', `${mid} → ${after}`);

console.log(`DONE pass=${state.pass} fail=${state.fail}`);
