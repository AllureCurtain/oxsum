/**
 * The wallet-adapter pay path's transaction builder (issue #1147): given an
 * opened intent's terms — `payTo`, `amountLamports`, `assetDecimals`, the
 * `reference` marker, and `splToken` when the rail priced an SPL asset —
 * construct the unsigned `Transaction` a connected wallet signs and sends.
 *
 * The instruction mirrors the Solana Pay recipe the `solana:` URL encodes:
 * `reference` rides as a *read-only, non-signer* account key appended to the
 * transfer instruction, so the rail's `getSignaturesForAddress(reference)`
 * finds the paying transaction exactly as it does for a scanned QR. A SOL
 * intent builds a `SystemProgram.transfer`; an SPL intent derives the sender's
 * and recipient's associated token accounts for the locked mint and builds a
 * `TransferChecked`, whose on-chain decimals re-check the mint's own precision
 * — `assetDecimals` here is informational for the instruction, never trusted
 * to reprice.
 *
 * Kept free of React and the adapter so the node test can assert the account
 * metas; the component layer feeds in the connected `PublicKey` and a fresh
 * `blockhash`/`feePayer`.
 */

import {
  getAssociatedTokenAddressSync,
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
} from "@solana/spl-token";
import {
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";

import type { SolanaIntent } from "../api/hooks";

/**
 * The terms the transfer needs, read off the opened intent plus the wallet's
 * connected account. `payer` is the wallet's own `publicKey` — the SOL debit
 * source, or the SPL source ATA's owner.
 */
export interface SolanaTransferTerms {
  payer: PublicKey;
  payTo: PublicKey;
  /** lamports for SOL, the mint's base units for SPL — `amountLamports` */
  baseUnits: number;
  /** the locked asset's decimals (9 for SOL, the mint's own for SPL) */
  assetDecimals: number;
  /** the per-intent marker account — appended read-only non-signer */
  reference: PublicKey;
  /** the SPL mint when the intent priced a token; `null` for native SOL */
  splToken: PublicKey | null;
}

/** Parses a base58 string into a `PublicKey`, `null` on malformed input. */
export function parsePublicKey(text: string | null | undefined): PublicKey | null {
  if (!text) return null;
  try {
    return new PublicKey(text.trim());
  } catch {
    return null;
  }
}

/** Reads the intent's string terms into the keys the builder needs; `null`
 * when any address or the amount cannot pin an honest transaction — the caller
 * then falls back to the QR/manual path rather than send a malformed tx. */
export function intentTransferTerms(
  intent: Pick<SolanaIntent, "payTo" | "amountLamports" | "assetDecimals" | "reference" | "splToken">,
  payer: PublicKey | null,
): SolanaTransferTerms | null {
  const payTo = parsePublicKey(intent.payTo);
  const reference = parsePublicKey(intent.reference);
  const splToken = intent.splToken ? parsePublicKey(intent.splToken) : null;
  if (payer === null || payTo === null || reference === null) return null;
  if (intent.splToken && splToken === null) return null;
  if (!Number.isSafeInteger(intent.amountLamports) || intent.amountLamports <= 0) return null;
  if (!Number.isSafeInteger(intent.assetDecimals) || intent.assetDecimals < 0) return null;
  return {
    payer,
    payTo,
    baseUnits: intent.amountLamports,
    assetDecimals: intent.assetDecimals,
    reference,
    splToken,
  };
}

/** Appends `reference` to an instruction's keys as a read-only non-signer —
 * the exact marker shape the Solana Pay recipe pins and the rail scans for. */
function withReference(ix: TransactionInstruction, reference: PublicKey): TransactionInstruction {
  // A reference already present (shouldn't happen — it is freshly minted per
  // intent) stays singular so the transfer never names the marker twice.
  if (!ix.keys.some((k) => k.pubkey.equals(reference))) {
    ix.keys.push({ pubkey: reference, isSigner: false, isWritable: false });
  }
  return ix;
}

/**
 * Builds the unsigned transfer `Transaction`. For SOL: one
 * `SystemProgram.transfer` carrying the marker. For SPL: the recipient's ATA
 * for the locked mint is derived (a `createAssociatedTokenAccountIdempotent`
 * prepended covers a vault whose ATA does not exist yet — idempotent so an
 * existing ATA is a no-op), the sender's ATA debits, and `TransferChecked`
 * moves `baseUnits` at `assetDecimals`. `feePayer`/`recentBlockhash` stay
 * unset — the wallet fills the payer on `signTransaction` and the component
 * sets the blockhash before send.
 */
export function buildSolanaTransfer(terms: SolanaTransferTerms): Transaction {
  const tx = new Transaction();
  if (terms.splToken === null) {
    tx.add(
      withReference(
        SystemProgram.transfer({
          fromPubkey: terms.payer,
          toPubkey: terms.payTo,
          lamports: terms.baseUnits,
        }),
        terms.reference,
      ),
    );
    return tx;
  }
  const mint = terms.splToken;
  // `payTo` is the vault's owner key; its ATA for this mint is what the
  // transfer must credit. The idempotent create covers a never-funded vault
  // ATA without failing on one that already exists.
  const sourceAta = getAssociatedTokenAddressSync(mint, terms.payer);
  const destAta = getAssociatedTokenAddressSync(mint, terms.payTo);
  tx.add(
    createAssociatedTokenAccountIdempotentInstruction(
      terms.payer,
      destAta,
      terms.payTo,
      mint,
    ),
  );
  tx.add(
    withReference(
      createTransferCheckedInstruction(
        sourceAta,
        mint,
        destAta,
        terms.payer,
        BigInt(terms.baseUnits),
        terms.assetDecimals,
      ),
      terms.reference,
    ),
  );
  return tx;
}

/**
 * The RPC endpoint the browser sends through (issue #1147):
 * `VITE_SOLANA_RPC_URL` at build time, defaulting to the public mainnet-beta
 * cluster. The send only ever carries the signed transaction — the rail's own
 * configured RPC pool does the authoritative verification server-side, so a
 * public or rate-limited endpoint here degrades the send path, never the
 * credit decision.
 */
export function solanaRpcEndpoint(): string {
  const configured = import.meta.env?.VITE_SOLANA_RPC_URL;
  if (typeof configured === "string" && configured.trim() !== "") {
    return configured.trim();
  }
  return "https://api.mainnet-beta.solana.com";
}
