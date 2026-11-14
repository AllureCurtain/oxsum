import assert from "node:assert/strict";
import test from "node:test";

import { PublicKey, SystemProgram } from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";

import {
  buildSolanaTransfer,
  intentTransferTerms,
  parsePublicKey,
  solanaEndpointCluster,
  solanaRpcEndpoint,
} from "../src/lib/solanaTx.ts";
import { usableSolanaWallets } from "../src/lib/solanaWallets.ts";
import { WalletReadyState, type Adapter } from "@solana/wallet-adapter-base";

// Real-shaped keys (the devnet fixtures the solana-pay test reuses) so the
// account metas assert actual addresses, not mocks.
const PAY_TO = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
const PAYER = new PublicKey("4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM");
const REFERENCE = new PublicKey("8opHzTAnfzRpPEx21XtnrVTX28YQuCpAjcn1PczScKh");
const MINT = new PublicKey("Gh9ZwEmdLJ8DscnNT79r5xY69sL2w8hDB2J5r5h1k2tT");
// A real Squads v4 vault PDA — `getVaultPda(multisig, 0)` for multisig
// `C46NEXuBLRJjJhfAsKBUxUYdxSBQUPc3SXQFRBTXfupC`, the known-answer pair the
// `crates/integrations::squads` test pins against the `@sqds/multisig` SDK.
// A vault is a program-derived address, so it is off-curve — the case the
// default ATA derivation refuses.
const SQUADS_VAULT = new PublicKey("65kaU3KWZJCEz11vrCQfUZMc5AWSTzkFybwKD5BckLH8");

const SPL_INTENT = {
  payTo: PAY_TO.toBase58(),
  amountLamports: 1_234_500,
  assetDecimals: 6,
  reference: REFERENCE.toBase58(),
  splToken: MINT.toBase58(),
};
const SOL_INTENT = { ...SPL_INTENT, amountLamports: 5_000, assetDecimals: 9, splToken: null };

test("parsePublicKey accepts base58 and refuses malformed input", () => {
  assert.ok(parsePublicKey(PAY_TO.toBase58())?.equals(PAY_TO));
  assert.equal(parsePublicKey("not-a-key"), null);
  assert.equal(parsePublicKey(""), null);
  assert.equal(parsePublicKey(null), null);
});

test("intentTransferTerms pins the intent's keys and rejects a bad shape", () => {
  const terms = intentTransferTerms(SPL_INTENT, PAYER);
  assert.ok(terms !== null);
  assert.ok(terms.payTo.equals(PAY_TO));
  assert.ok(terms.reference.equals(REFERENCE));
  assert.ok(terms.splToken?.equals(MINT));
  assert.equal(terms.baseUnits, 1_234_500);
  // No payer, a bad recipient, or a non-positive amount pins nothing.
  assert.equal(intentTransferTerms(SPL_INTENT, null), null);
  assert.equal(intentTransferTerms({ ...SPL_INTENT, payTo: "bogus" }, PAYER), null);
  assert.equal(intentTransferTerms({ ...SPL_INTENT, amountLamports: 0 }, PAYER), null);
  // An SPL intent whose mint string is malformed fails rather than send.
  assert.equal(intentTransferTerms({ ...SPL_INTENT, splToken: "bogus" }, PAYER), null);
});

test("a SOL intent builds one transfer carrying the reference read-only", () => {
  const terms = intentTransferTerms(SOL_INTENT, PAYER);
  assert.ok(terms !== null);
  const tx = buildSolanaTransfer(terms);
  assert.equal(tx.instructions.length, 1);
  const ix = tx.instructions[0];
  assert.ok(ix.programId.equals(SystemProgram.programId));
  // SystemProgram.transfer keys: [from(signer,writable), to(writable)] + marker.
  assert.equal(ix.keys.length, 3);
  assert.ok(ix.keys[0].pubkey.equals(PAYER) && ix.keys[0].isSigner);
  assert.ok(ix.keys[1].pubkey.equals(PAY_TO));
  // The marker is the Solana Pay recipe: read-only, non-signer.
  assert.ok(ix.keys[2].pubkey.equals(REFERENCE));
  assert.equal(ix.keys[2].isSigner, false);
  assert.equal(ix.keys[2].isWritable, false);
});

test("an SPL intent derives ATAs and appends the marker on TransferChecked", () => {
  const terms = intentTransferTerms(SPL_INTENT, PAYER);
  assert.ok(terms !== null);
  const tx = buildSolanaTransfer(terms);
  // create-ATA-idempotent + TransferChecked.
  assert.equal(tx.instructions.length, 2);
  const [createAta, transfer] = tx.instructions;
  assert.ok(createAta.programId.equals(ASSOCIATED_TOKEN_PROGRAM_ID));
  assert.ok(transfer.programId.equals(TOKEN_PROGRAM_ID));
  // TransferChecked keys: [source, mint, destination, owner] + marker — the
  // marker lands on the token transfer, the instruction the rail scans.
  const marker = transfer.keys.find((k) => k.pubkey.equals(REFERENCE));
  assert.ok(marker !== undefined);
  assert.equal(marker.isSigner, false);
  assert.equal(marker.isWritable, false);
  // The destination is payTo's ATA, not the owner key itself.
  const destAta = transfer.keys[2].pubkey;
  assert.ok(!destAta.equals(PAY_TO));
});

test("an SPL intent pays a Squads vault — an off-curve payTo (issue #1180)", () => {
  // The vault PDA is off-curve by construction; assert the fixture holds so a
  // test-vector drift fails loudly rather than silently re-testing on-curve.
  assert.equal(PublicKey.isOnCurve(SQUADS_VAULT.toBytes()), false);
  const terms = intentTransferTerms(
    { ...SPL_INTENT, payTo: SQUADS_VAULT.toBase58() },
    PAYER,
  );
  assert.ok(terms !== null);
  // The throw the issue reports happens here, inside the builder.
  const tx = buildSolanaTransfer(terms);
  assert.equal(tx.instructions.length, 2);
  const [createAta, transfer] = tx.instructions;
  // The destination is the Associated Token Program's derivation for the
  // off-curve owner — the account the ATA creation and the transfer name.
  const expectedAta = getAssociatedTokenAddressSync(MINT, SQUADS_VAULT, true);
  assert.ok(createAta.keys[1].pubkey.equals(expectedAta));
  assert.ok(createAta.keys[2].pubkey.equals(SQUADS_VAULT));
  assert.ok(transfer.keys[2].pubkey.equals(expectedAta));
  // The marker still rides the transfer read-only and non-signing.
  const marker = transfer.keys.find((k) => k.pubkey.equals(REFERENCE));
  assert.ok(marker !== undefined);
  assert.equal(marker.isSigner, false);
  assert.equal(marker.isWritable, false);
});

test("usableSolanaWallets keeps installed/loadable, drops the rest, dedupes", () => {
  const fake = (name: string, readyState: WalletReadyState): Adapter =>
    ({ name, readyState }) as unknown as Adapter;
  const list = usableSolanaWallets([
    fake("Phantom", WalletReadyState.Installed),
    fake("Solflare", WalletReadyState.Loadable),
    fake("Phantom", WalletReadyState.Installed), // a second registration
    fake("Backpack", WalletReadyState.NotDetected),
    fake("MobileOnly", WalletReadyState.Unsupported),
  ]);
  assert.deepEqual(
    list.map((a) => String(a.name)),
    ["Phantom", "Solflare"],
  );
  assert.equal(usableSolanaWallets([]).length, 0);
});

test("solanaRpcEndpoint answers the public endpoint for a mainnet intent only", () => {
  // `VITE_SOLANA_RPC_URL` is unset in the test run, so the defaults answer.
  assert.deepEqual(solanaRpcEndpoint("mainnet-beta"), {
    ok: "https://api.mainnet-beta.solana.com",
  });
  // Issue #1181: a non-mainnet intent without a configured endpoint refuses
  // to send — never the old silent public-mainnet fallback, never a guessed
  // private one. The QR carries `cluster=` and stays the payment path.
  assert.deepEqual(solanaRpcEndpoint("devnet"), {
    error: "unconfigured",
    cluster: "devnet",
  });
  assert.deepEqual(solanaRpcEndpoint("testnet"), {
    error: "unconfigured",
    cluster: "testnet",
  });
});

test("solanaEndpointCluster reads devnet/testnet/localhost shapes, else mainnet", () => {
  assert.equal(solanaEndpointCluster("https://api.devnet.solana.com"), "devnet");
  assert.equal(solanaEndpointCluster("http://127.0.0.1:8899"), "devnet");
  assert.equal(solanaEndpointCluster("http://localhost:8899"), "devnet");
  assert.equal(solanaEndpointCluster("https://api.testnet.solana.com"), "testnet");
  assert.equal(solanaEndpointCluster("https://mainnet.helius-rpc.com/?api-key=x"), "mainnet-beta");
  assert.equal(solanaEndpointCluster("https://api.mainnet-beta.solana.com"), "mainnet-beta");
});
