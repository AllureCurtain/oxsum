#!/usr/bin/env node
/**
 * Solana top-up walkthrough (issue #1117, SPL path #1121): the customer-facing
 * half of the rail, asserted step by step against a live deployment.
 *
 *   OXSUM_BASE_URL=http://127.0.0.1:3000 \
 *   OXSUM_SOLANA_PAYER=<customer account's email:password> \
 *   node topup.mjs [amountMinor]
 *
 * `OXSUM_SOLANA_PAYER` is the oxsum *customer* login (an `email:password`
 * pair for `/api/v1/auth/login`), not a chain key — the on-chain wallet is
 * `payer.json` beside this script. What it does, in order:
 *
 *   1. logs the customer in (personal session) and reads `/api/v1/balance`
 *   2. opens the intent — `POST /api/v1/topups/solana` answers
 *      `{reference, payTo, amountLamports, splToken, expiresAt}`
 *   3. pays it on chain — native `SystemProgram.transfer`, or an SPL
 *      `TransferChecked` when `splToken` names a mint — carrying the
 *      `reference` marker
 *   4. settles it — `POST /api/v1/topups/solana/confirm` with the reference;
 *      asserts the wallet's `availableMinor` grew by the asked amount
 *   5. replays the confirm — the documented `404 NOT_FOUND`; a second intent
 *      is left for the 15 s `solana-deposits` poll to credit on its own
 *
 * Env: OXSUM_BASE_URL (required), OXSUM_SOLANA_PAYER (required),
 * SOLANA_RPC_URL (default http://127.0.0.1:8899 — the local validator).
 * Run `node keys.mjs` first to mint payer.json/payto.json; add `--mint`
 * for the SPL half (mints a token, prints `MINT=` for
 * `OXSUM_SOLANA_SPL_TOKEN_MINT`).
 */
import { Connection, Keypair, PublicKey, SystemProgram, Transaction } from '@solana/web3.js';
import {
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
  getAssociatedTokenAddress,
  getMint,
} from '@solana/spl-token';
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

const intent = await call('/api/v1/topups/solana', {
  method: 'POST', session, body: { amountMinor: AMOUNT_MINOR },
});
check(intent.status === 200, 'open intent', JSON.stringify(intent.json?.data));
if (intent.status !== 200) process.exit(1);
const { reference, payTo, amountLamports, splToken } = intent.json.data;

const conn = new Connection(RPC, 'confirmed');
const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(fs.readFileSync(path.join(HERE, 'payer.json'), 'utf8'))));

/** Build and send the reference-marked payment; native SOL or SPL. */
const pay = async ({ payTo: to, amountLamports: amt, reference: ref, splToken: mint }) => {
  const tx = new Transaction();
  if (mint) {
    const m = new PublicKey(mint);
    const { decimals } = await getMint(conn, m);
    const fromAta = await getAssociatedTokenAddress(m, payer.publicKey);
    const toAta = await getAssociatedTokenAddress(m, new PublicKey(to));
    // The recipient's ATA may not exist yet on a first SPL deposit — create
    // it idempotently in the same transaction (the payer funds its rent).
    tx.add(createAssociatedTokenAccountIdempotentInstruction(
      payer.publicKey, toAta, new PublicKey(to), m));
    const ix = createTransferCheckedInstruction(fromAta, m, toAta, payer.publicKey, BigInt(amt), decimals);
    ix.keys.push({ pubkey: new PublicKey(ref), isSigner: false, isWritable: false });
    tx.add(ix);
  } else {
    const ix = SystemProgram.transfer({
      fromPubkey: payer.publicKey, toPubkey: new PublicKey(to), lamports: Number(amt),
    });
    ix.keys.push({ pubkey: new PublicKey(ref), isSigner: false, isWritable: false });
    tx.add(ix);
  }
  const sig = await conn.sendTransaction(tx, [payer]);
  await conn.confirmTransaction(sig, 'confirmed');
  return sig;
};

const sig = await pay(intent.json.data);
check(true, 'transfer confirmed', `${amountLamports} ${splToken ? 'base units' : 'lamports'} → ${payTo} (sig ${sig})`);

const confirm = await call('/api/v1/topups/solana/confirm', {
  method: 'POST', session, body: { reference },
});
check(confirm.status === 200, 'confirm settles', JSON.stringify(confirm.json?.data));
const mid = await balance();
check(mid === before + AMOUNT_MINOR, 'wallet credited once', `${before} → ${mid}`);

const replay = await call('/api/v1/topups/solana/confirm', {
  method: 'POST', session, body: { reference },
});
check(replay.status === 404, 'replayed confirm answers 404', `got ${replay.status}`);
check((await balance()) === mid, 'replay credits nothing');

const second = await call('/api/v1/topups/solana', {
  method: 'POST', session, body: { amountMinor: AMOUNT_MINOR },
});
await pay(second.json.data);
console.log('[INFO] second intent paid; waiting on the 15 s solana-deposits poll…');
const deadline = Date.now() + 45_000;
let after = await balance();
while (after < mid + AMOUNT_MINOR && Date.now() < deadline) {
  await new Promise((r) => setTimeout(r, 3_000));
  after = await balance();
}
check(after === mid + AMOUNT_MINOR, 'poll credits the unpaid intent', `${mid} → ${after}`);

console.log(`DONE pass=${state.pass} fail=${state.fail}`);
