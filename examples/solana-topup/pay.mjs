#!/usr/bin/env node
/**
 * Pay one Solana top-up intent on chain, the Solana Pay way: a transfer to
 * `payTo` that carries the intent's `reference` account as a read-only
 * non-signer key — the marker the rail's `findReference` + `validateTransfer`
 * check locates and proves.
 *
 *   node pay.mjs payer.json <payTo> <reference> <amount> [--rpc URL] [--spl <mint>]
 *
 * `amount` is lamports for a native-SOL intent, or base units of `<mint>`
 * for an SPL intent (the same number the intent's `amountLamports` answers
 * either way). Prints the signature on success.
 *
 * Native SOL sends one `SystemProgram.transfer`; `--spl` sends an SPL
 * `TransferChecked` between the payer's and recipient's associated token
 * accounts — both carry `reference` as a read-only non-signer key, the
 * marker `getSignaturesForAddress` indexes either way. A wallet built on
 * `@solana/pay` produces the same shape.
 */
import { Connection, Keypair, PublicKey, SystemProgram, Transaction } from '@solana/web3.js';
import {
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
  getAssociatedTokenAddress,
  getMint,
} from '@solana/spl-token';
import fs from 'node:fs';

const args = process.argv.slice(2);
const positional = args.filter((a) => !a.startsWith('--'));
const [skFile, payToAddr, refAddr, amountStr] = positional;
const flag = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const RPC = flag('--rpc') ?? 'http://127.0.0.1:8899';
const SPL_MINT = flag('--spl');
if (!skFile || !payToAddr || !refAddr || !amountStr) {
  console.error(
    'usage: node pay.mjs payer.json <payTo> <reference> <amount> [--rpc URL] [--spl <mint>]');
  process.exit(2);
}

const conn = new Connection(RPC, 'confirmed');
const payer = Keypair.fromSecretKey(
  Uint8Array.from(JSON.parse(fs.readFileSync(skFile, 'utf8'))));
const payTo = new PublicKey(payToAddr);
const reference = new PublicKey(refAddr);
const amount = BigInt(amountStr);

const tx = new Transaction();
if (SPL_MINT) {
  const mint = new PublicKey(SPL_MINT);
  const { decimals } = await getMint(conn, mint);
  const fromAta = await getAssociatedTokenAddress(mint, payer.publicKey);
  const toAta = await getAssociatedTokenAddress(mint, payTo);
  // The recipient's ATA may not exist yet on a first deposit — create it
  // idempotently in the same transaction (the payer funds its rent), which is
  // what Solana Pay wallets do for a first-time SPL recipient.
  tx.add(createAssociatedTokenAccountIdempotentInstruction(
    payer.publicKey, toAta, payTo, mint));
  const ix = createTransferCheckedInstruction(
    fromAta, mint, toAta, payer.publicKey, amount, decimals);
  // The intent marker on the token transfer itself — `accountKeys` indexes it
  // the same way the system-transfer form does.
  ix.keys.push({ pubkey: reference, isSigner: false, isWritable: false });
  tx.add(ix);
} else {
  const ix = SystemProgram.transfer({
    fromPubkey: payer.publicKey, toPubkey: payTo, lamports: Number(amount),
  });
  ix.keys.push({ pubkey: reference, isSigner: false, isWritable: false });
  tx.add(ix);
}

const sig = await conn.sendTransaction(tx, [payer]);
for (let i = 0; i < 40; i++) {
  const st = (await conn.getSignatureStatuses([sig])).value[0];
  if (st && (st.confirmationStatus === 'confirmed' || st.confirmationStatus === 'finalized')) break;
  await new Promise((r) => setTimeout(r, 400));
}
const got = await conn.getTransaction(sig, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 });
console.log(`SIGNATURE=${sig}`);
console.log(`ONCHAIN_ERR=${JSON.stringify(got && got.meta.err)}`);
if (SPL_MINT && got) {
  console.log(`POST_TOKEN=${JSON.stringify(got.meta.postTokenBalances?.map((r) => r.uiTokenAmount?.amount))}`);
}
if (got && got.meta.err) process.exitCode = 1;
