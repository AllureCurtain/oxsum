/**
 * Wallet detection for the "pay with wallet" button (issue #1147), split off
 * the component so the pick rules stay testable. The adapter layer already
 * enumerates injected wallets — Phantom, Solflare, Backpack and every other
 * Wallet Standard implementation arrive through `useStandardWalletAdapters`
 * with no per-wallet package — so the only question this layer answers is
 * which of those are actually usable right now and how to label the button.
 */

import { WalletReadyState, type Adapter } from "@solana/wallet-adapter-base";

/**
 * The adapters worth offering: a standard wallet that reports `Installed`
 * (or the always-available `Loadable`) is one the user can pay with — its
 * extension/app is present. `NotDetected` (a named adapter whose injection
 * never landed) and `Unsupported` (a mobile-only wallet on desktop) are
 * dropped so the button never offers a wallet the click would fail on.
 */
export function usableSolanaWallets(adapters: Adapter[]): Adapter[] {
  const seen = new Set<string>();
  const usable: Adapter[] = [];
  for (const adapter of adapters) {
    if (adapter.readyState !== WalletReadyState.Installed &&
        adapter.readyState !== WalletReadyState.Loadable) {
      continue;
    }
    // The same injected wallet can surface twice (a standard registration and
    // a legacy adapter); dedupe on the adapter's own name so the picker never
    // lists "Phantom" twice.
    const name = String(adapter.name);
    if (seen.has(name)) continue;
    seen.add(name);
    usable.push(adapter);
  }
  return usable;
}
