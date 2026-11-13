#!/usr/bin/env node
/**
 * Pay one Solana top-up intent on chain, the Solana Pay way: a native-SOL
 * transfer to `payTo` that carries the intent's `reference` account as a
 * read-only non-signer key — the marker the rail's `findReference` +
 * `validateTransfer` check locates and proves.
 *
 *   node pay.mjs payer.json <payTo> <reference> <lamports> [--rpc http://127.0.0.1:8899]
 *
 * `payer.json` is the keypair file `keys.mjs` wrote. Prints the signature
 * on success. The same transaction shape is what a wallet built on
 * `@solana/pay` produces; an SPL-token intent needs a token transfer
 * carrying the reference instead (not covered here — the rail's
 * `postTokenBalances` check is what it feeds).
 */
import { Connection, Keypair, PublicKey, SystemProgram, Transaction } from '@solana/web3.js';
import fs from 'node:fs';

const args = process.argv.slice(2);
const [skFile, payToAddr, refAddr, lamportsStr] = args.filter((a) => !a.startsWith('--'));
const rpcAt = args.indexOf('--rpc');
const RPC = rpcAt >= 0 ? args[rpcAt + 1] : 'http://127.0.0.1:8899';
if (!skFile || !payToAddr || !refAddr || !lamportsStr) {
  console.error('usage: node pay.mjs payer.json <payTo> <reference> <lamports> [--rpc URL]');
  process.exit(2);
}

const conn = new Connection(RPC, 'confirmed');
const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(fs.readFileSync(skFile, 'utf8'))));

const ix = SystemProgram.transfer({
  fromPubkey: payer.publicKey,
  toPubkey: new PublicKey(payToAddr),
  lamports: Number(lamportsStr),
});
// The intent marker: a fresh per-intent account key, present but never
// signing and never mutated — exactly what getSignaturesForAddress indexes.
ix.keys.push({ pubkey: new PublicKey(refAddr), isSigner: false, isWritable: false });

const sig = await conn.sendTransaction(new Transaction().add(ix), [payer]);
for (let i = 0; i < 40; i++) {
  const st = (await conn.getSignatureStatuses([sig])).value[0];
  if (st && (st.confirmationStatus === 'confirmed' || st.confirmationStatus === 'finalized')) break;
  await new Promise((r) => setTimeout(r, 400));
}
const tx = await conn.getTransaction(sig, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
console.log(`SIGNATURE=${sig}`);
console.log(`ONCHAIN_ERR=${JSON.stringify(tx && tx.meta.err)}`);
if (tx && tx.meta.err) process.exitCode = 1;
