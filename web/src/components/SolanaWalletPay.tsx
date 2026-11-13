/**
 * The "Pay with wallet" control on an open Solana intent (issue #1147). Loaded
 * as a lazy chunk — `@solana/web3.js` plus the adapter stack only reach a
 * browser that actually opens the Solana rail, so a top-up on Stripe or
 * Hyperswitch never downloads the wallet code. The whole Wallet Standard set
 * (Phantom, Solflare, Backpack, …) enumerates through
 * `useStandardWalletAdapters([])` with no per-wallet adapter package; a
 * browser with none installed simply renders nothing here and the QR/manual
 * path below stays the answer.
 *
 * The flow: pick (or auto-pick) a usable wallet → `connect()` → build the
 * intent's transfer (`buildSolanaTransfer`, reference marker appended
 * read-only) → `adapter.sendTransaction` signs and submits through the
 * public/`VITE_SOLANA_RPC_URL` endpoint → the parent re-asks the confirm
 * endpoint the same way "I've sent it" does, so credit rides the existing
 * poll/status path rather than a parallel one.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useStandardWalletAdapters } from "@solana/wallet-standard-wallet-adapter-react";
import { type Adapter } from "@solana/wallet-adapter-base";
import { Connection } from "@solana/web3.js";

import type { SolanaIntent } from "../api/hooks";
import { buildSolanaTransfer, intentTransferTerms, solanaRpcEndpoint } from "../lib/solanaTx";
import { usableSolanaWallets } from "../lib/solanaWallets";
import { Banner, Button } from "./ui";

type WalletPhase =
  | "pick" // several wallets detected — choose one
  | "ready" // one wallet usable (or chosen), awaiting the click
  | "connecting" // connect() in flight
  | "sending" // sign + sendTransaction in flight
  | "sent"; // a signature came back — confirm re-ask owns the status now

export function SolanaWalletPay({
  intent,
  onSent,
}: {
  intent: SolanaIntent;
  /** Fired once with the chain signature when the wallet's send lands. */
  onSent: (signature: string) => void;
}) {
  const { t } = useTranslation();
  // `[]`: no legacy adapters — every injected wallet arrives through the
  // standard registry on its own.
  const adapters = useStandardWalletAdapters([]);
  const usable = useMemo(() => usableSolanaWallets(adapters), [adapters]);
  const [chosen, setChosen] = useState<Adapter | null>(null);
  const [phase, setPhase] = useState<WalletPhase>("pick");
  const [error, setError] = useState<string | null>(null);
  const active: Adapter | null = chosen ?? (usable.length === 1 ? usable[0] : null);

  // A wallet appearing or disappearing re-derives the picker; keep the choice
  // only while the chosen adapter still reports usable.
  useEffect(() => {
    if (chosen && !usable.includes(chosen)) setChosen(null);
  }, [chosen, usable]);

  // The intent's terms parsed once per intent+payer; `null` means the button
  // cannot honestly build the transfer and stays hidden — the QR/manual path
  // carries the payment instead.
  const payer = active?.publicKey ?? null;
  const terms = useMemo(
    () => intentTransferTerms(intent, payer),
    [intent, payer],
  );
  const connectionRef = useRef<Connection | null>(null);
  if (connectionRef.current === null) {
    connectionRef.current = new Connection(solanaRpcEndpoint(), "confirmed");
  }

  if (usable.length === 0) {
    // No compatible wallet installed — the graceful fallback the issue asks
    // for: nothing here, the QR/manual block below is the payment path. A one
    // line hint names the option so a user who *could* install a wallet knows
    // the shortcut exists.
    return (
      <p className="text-xs text-muted">{t("topup.solanaWalletNone")}</p>
    );
  }

  const pay = async (adapter: Adapter) => {
    setError(null);
    try {
      setPhase("connecting");
      if (adapter.publicKey === null) await adapter.connect();
      const connected = adapter.publicKey;
      if (connected === null) {
        throw new Error(t("topup.solanaWalletRejected"));
      }
      const built = intentTransferTerms(intent, connected);
      if (built === null) throw new Error(t("topup.solanaWalletBadIntent"));
      setPhase("sending");
      const tx = buildSolanaTransfer(built);
      tx.feePayer = connected;
      const { blockhash } = await connectionRef.current!.getLatestBlockhash();
      tx.recentBlockhash = blockhash;
      const signature = await adapter.sendTransaction(tx, connectionRef.current!);
      setPhase("sent");
      onSent(signature);
    } catch (e) {
      // A user closing the wallet prompt surfaces as a rejected promise —
      // read it as a soft decline, not a rail failure.
      const message = e instanceof Error ? e.message : "";
      setPhase(active ? "ready" : "pick");
      setError(
        /reject|declin|cancel/i.test(message)
          ? t("topup.solanaWalletRejected")
          : t("topup.solanaWalletFailed"),
      );
    }
  };

  const busy = phase === "connecting" || phase === "sending";
  return (
    <div className="flex flex-col gap-2">
      {active === null ? (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs font-medium text-ink">{t("topup.solanaWalletPick")}</span>
          {usable.map((adapter) => (
            <Button
              key={String(adapter.name)}
              type="button"
              tone="ghost"
              onClick={() => setChosen(adapter)}
            >
              {adapter.icon ? (
                <img src={adapter.icon} alt="" className="size-4 rounded-sm" aria-hidden />
              ) : null}
              {String(adapter.name)}
            </Button>
          ))}
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-3">
          <Button
            type="button"
            onClick={() => void pay(active)}
            isLoading={busy}
            loadingText={
              phase === "connecting"
                ? t("topup.solanaWalletConnecting")
                : t("topup.solanaWalletSending")
            }
            disabled={phase === "sent" || usable.length === 0}
          >
            {t("topup.solanaWalletPay", { wallet: String(active.name) })}
          </Button>
          {usable.length > 1 && !busy && phase !== "sent" ? (
            <button
              type="button"
              className="text-xs text-muted underline underline-offset-2"
              onClick={() => setChosen(null)}
            >
              {t("topup.solanaWalletChange")}
            </button>
          ) : null}
        </div>
      )}
      {phase === "sent" ? (
        <p className="text-xs text-muted" role="status">{t("topup.solanaWalletSent")}</p>
      ) : null}
      {error ? <Banner tone="warn">{error}</Banner> : null}
      {/*
        `terms` is read to keep the intent's terms honest: a malformed intent
        (unparseable `payTo`/`reference`, a non-positive amount) disables the
        send but still renders, so the user sees the button exist yet cannot
        fire a bad transaction — the QR path remains.
      */}
      {active !== null && payer !== null && terms === null ? (
        <Banner tone="warn">{t("topup.solanaWalletBadIntent")}</Banner>
      ) : null}
    </div>
  );
}

export default SolanaWalletPay;
