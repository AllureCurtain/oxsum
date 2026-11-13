#!/usr/bin/env node
/**
 * Generate the two Solana keypairs the walkthrough uses and airdrop them
 * on a local `solana-test-validator` (or devnet, when one is reachable).
 *
 *   node keys.mjs [--rpc http://127.0.0.1:8899] [--out .] [--mint]
 *
 * Writes `payto.json` (the deployment's receiving address — point
 * `OXSUM_SOLANA_PAY_TO` at its public key) and `payer.json` (the customer's
 * wallet), each a JSON array of the 64 secret-key bytes. The payer needs an
 * airdrop to pay; the payTo needs a rent-exempt minimum before its first
 * deposit or the payer's transfer is refused (`insufficient funds for
 * rent`), so the script airdrops a small amount to it too. Keypairs are
 * throwaway — never reuse them outside a scratch validator/devnet.
 *
 * `--mint` also creates an SPL mint and a payer ATA, and prints `MINT=` for
 * `OXSUM_SOLANA_SPL_TOKEN_MINT` — the SPL half of the walkthrough needs it.
 * The payer keeps the mint authority, so there is no separate mint keypair to
 * save. Devnet already has a public USDC mint
 * (`4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU`) if you'd rather not mint
 * your own; this flag is for a scratch validator where nothing exists yet.
 */
import { Keypair, Connection, LAMPORTS_PER_SOL } from '@solana/web3.js';
import { createMint, getOrCreateAssociatedTokenAccount, mintTo } from '@solana/spl-token';
import fs from 'node:fs';
import path from 'node:path';

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
};
const has = (name) => args.includes(name);
const RPC = flag('--rpc', 'http://127.0.0.1:8899');
const OUT = flag('--out', '.');

const conn = new Connection(RPC, 'confirmed');
const payTo = Keypair.generate();
const payer = Keypair.generate();
const write = (name, kp) => fs.writeFileSync(
  path.join(OUT, name), JSON.stringify(Array.from(kp.secretKey)));
write('payto.json', payTo);
write('payer.json', payer);
console.log(`PAY_TO=${payTo.publicKey.toBase58()}`);
console.log(`PAYER=${payer.publicKey.toBase58()}`);

const airdrop = async (kp, sol) => {
  const sig = await conn.requestAirdrop(kp.publicKey, sol * LAMPORTS_PER_SOL);
  await conn.confirmTransaction(sig, 'confirmed');
};
try {
  await airdrop(payer, 2);
  // ~0.1 SOL clears the ~890880-lamport rent-exempt minimum with room to
  // spare; a fresh payTo cannot receive a bare 5000-lamport deposit.
  await airdrop(payTo, 0.1);
  console.log(`PAYER_BALANCE=${await conn.getBalance(payer.publicKey)}`);
  console.log(`PAYTO_BALANCE=${await conn.getBalance(payTo.publicKey)}`);
} catch (e) {
  // The public devnet faucet answers HTTP 410 to requestAirdrop these days;
  // on devnet fund the payer by hand (https://faucet.solana.com) instead.
  console.log(`AIRDROP_FAIL=${e.message}`);
  console.log('On devnet, fund PAYER at https://faucet.solana.com and retry.');
  process.exitCode = 1;
}

if (has('--mint')) {
  // An SPL mint on a scratch validator: 6 decimals (USDC's convention). The
  // payer keeps the mint authority, so `mintTo` can top up its own ATA later
  // — there is no separate mint keypair file to save.
  const mint = await createMint(conn, payer, payer.publicKey, null, 6);
  const ata = await getOrCreateAssociatedTokenAccount(conn, payer, mint, payer.publicKey);
  await mintTo(conn, payer, mint, ata.address, payer, 1_000_000_000); // 1000 tokens
  console.log(`MINT=${mint.toBase58()}`);
  console.log(`PAYER_ATA=${ata.address.toBase58()}`);
}
