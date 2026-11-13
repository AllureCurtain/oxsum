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
import type { SolanaCluster } from "./solanaPay";

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

/** The public endpoint a `VITE_SOLANA_RPC_URL`-less build may use — only
 * ever mainnet-beta. A devnet/testnet rail cannot safely default to the
 * public cluster: the deployment's endpoints may be a private provider or a
 * local validator the public network cannot see, so a silent public-devnet
 * send would land on a network the rail's verifier never reads (issue
 * #1181). Non-mainnet without a configured endpoint refuses and prompts for
 * configuration — the QR, whose `cluster=` carries the right network, stays
 * the payment path. */
const MAINNET_PUBLIC_ENDPOINT = "https://api.mainnet-beta.solana.com";

/**
 * The cluster an endpoint's *shape* implies — the same heuristic the server
 * applies to `OXSUM_SOLANA_RPC_URLS` (issue #1181), so a `VITE_SOLANA_RPC_URL`
 * an operator builds the bundle with is checked against the intent's own
 * `cluster`, never assumed. A URL naming `devnet`/`testnet` or pointing at a
 * localhost-style host (a local validator is a dev network by construction)
 * implies that cluster; anything else implies `mainnet-beta` — the
 * production direction is the dangerous one, so an unannotated endpoint is
 * never read as test money.
 */
export function solanaEndpointCluster(endpoint: string): SolanaCluster {
  const lower = endpoint.toLowerCase();
  if (
    lower.includes("devnet") ||
    lower.includes("localhost") ||
    lower.includes("127.0.0.1") ||
    lower.includes("[::1]")
  ) {
    return "devnet";
  }
  if (lower.includes("testnet")) return "testnet";
  return "mainnet-beta";
}

/** The wallet-adapter send path's endpoint decision (issue #1181). `ok` is
 * the URL the `Connection` may point at; `error` is a user-facing string the
 * page shows *instead of sending* — a mismatch must refuse, because sending
 * to the wrong cluster would move real money against the intent's terms
 * while the rail's own verifier waits on another network for a transfer
 * that will never land. */
export type SolanaEndpointResolution =
  | { ok: string }
  | { error: "mismatch" | "unconfigured"; cluster: SolanaCluster };

/**
 * Resolves the RPC endpoint the browser sends through for `cluster` (issues
 * #1147, #1181). `VITE_SOLANA_RPC_URL` at build time wins when it names the
 * intent's cluster — an authenticated provider the operator pinned. When it
 * names a *different* cluster (a mainnet URL baked into a bundle now serving
 * a devnet rail), the send refuses: the endpoint is the operator's own word,
 * and honoring it would send on the wrong network. With no `VITE_` variable,
 * `mainnet-beta` keeps the public endpoint it always had; `devnet`/`testnet`
 * refuse with `unconfigured` — a non-mainnet rail is usually a private
 * provider or a local validator the public cluster cannot see, so the page
 * prompts for `VITE_SOLANA_RPC_URL` and the QR (its `cluster=` carries the
 * right network) stays the payment path.
 */
export function solanaRpcEndpoint(cluster: SolanaCluster): SolanaEndpointResolution {
  const configured = import.meta.env?.VITE_SOLANA_RPC_URL;
  if (typeof configured === "string" && configured.trim() !== "") {
    const endpoint = configured.trim();
    const implied = solanaEndpointCluster(endpoint);
    if (implied !== cluster) {
      return { error: "mismatch", cluster: implied };
    }
    return { ok: endpoint };
  }
  if (cluster === "mainnet-beta") return { ok: MAINNET_PUBLIC_ENDPOINT };
  return { error: "unconfigured", cluster };
}
