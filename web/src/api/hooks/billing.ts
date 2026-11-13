import { useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type {
  Balance,
  BillRow,
  BillingRecord,
  BudgetSnapshot,
  LogEntryRow,
  OpenHold,
  PriceEstimate,
  PricedModel,
  Receipt,
  RequestLogRow,
  SignedHead,
  Statement,
  StatementLine,
  UsageDay,
  VerifyingKey,
  WebhookDelivery,
  WebhookEndpoint,
  WitnessCheckpoint,
} from "./types";

export function useBalance(options?: { refetchInterval?: number | false }) {
  return useQuery({
    queryKey: ["balance"],
    queryFn: () => api<Balance>("/balance"),
    refetchInterval: options?.refetchInterval,
  });
}

export function useBudget() {
  return useQuery({
    queryKey: ["budget"],
    queryFn: () => api<BudgetSnapshot>("/budget"),
  });
}

export function useBillingRecords(limit = 50, before?: number) {
  return useQuery({
    queryKey: ["billing-records", limit, before ?? null],
    queryFn: () =>
      api<{ rows: BillingRecord[]; nextCursor?: number }>(
        `/billing-records?limit=${limit}${before !== undefined ? `&before=${before}` : ""}`,
      ),
  });
}

export interface RequestLogQuery {
  keyId?: string;
  model?: string;
  channel?: string;
  from?: string;
  to?: string;
  status?: string;
  minCharge?: number;
  limit?: number;
  cursor?: string;
}

/** The request-analytics read (issue #505): the per-request operational
 * record a billing dispute or an SRE query reads — served model, channel,
 * token counts, latency, charge — filtered and cursor-paged, org-scoped by
 * the session. */
export function useRequestLog(query: RequestLogQuery) {
  const params = new URLSearchParams();
  for (const [name, value] of Object.entries(query)) {
    if (value !== undefined && value !== "" && value !== null) {
      params.set(name, String(value));
    }
  }
  const qs = params.toString();
  return useQuery({
    queryKey: ["request-log", qs],
    queryFn: () => api<{ rows: RequestLogRow[]; nextCursor?: string }>(`/requests?${qs}`),
  });
}

export function useUsage(from: string, to: string) {
  return useQuery({
    queryKey: ["usage", from, to],
    queryFn: () => api<{ days: UsageDay[] }>(`/usage?from=${from}&to=${to}`),
  });
}

export function useHolds() {
  return useQuery({
    queryKey: ["holds"],
    queryFn: () => api<{ holds: OpenHold[] }>("/holds"),
    refetchInterval: 15_000,
  });
}

export function useEntries(limit = 100, before?: number) {
  return useQuery({
    queryKey: ["entries", limit, before ?? null],
    queryFn: () =>
      api<{ rows: LogEntryRow[]; nextCursor?: number }>(
        `/entries?limit=${limit}${before !== undefined ? `&before=${before}` : ""}`,
      ),
  });
}

export function useBills(before?: number) {
  return useQuery({
    queryKey: ["bills", before ?? null],
    queryFn: () =>
      api<{ rows: BillRow[]; nextCursor?: number }>(
        `/bills${before !== undefined ? `?before=${before}` : ""}`,
      ),
  });
}

export function useStatements() {
  return useQuery({
    queryKey: ["statements"],
    queryFn: () => api<Statement[]>("/statements"),
  });
}

export function useStatement(id: string) {
  return useQuery({
    queryKey: ["statement", id],
    queryFn: () =>
      api<{ statement: Statement & { [k: string]: unknown }; lines: StatementLine[]; anchor?: unknown }>(
        `/statements/${id}`,
      ),
  });
}

export function useWebhooks() {
  return useQuery({
    queryKey: ["webhooks"],
    queryFn: () => api<WebhookEndpoint[]>("/webhooks"),
  });
}

export function useCreateWebhook() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { url: string; events: string[]; balanceLowMinor?: number | null; budgetThresholdsPercent?: number[] }) =>
      api<WebhookEndpoint & { secret: string }>("/webhooks", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["webhooks"] }),
  });
}

export function useDeleteWebhook() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api(`/webhooks/${id}`, { method: "DELETE" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["webhooks"] }),
  });
}

export function useWebhookDeliveries(endpointId: string) {
  return useQuery({
    queryKey: ["webhook-deliveries", endpointId],
    queryFn: () => api<WebhookDelivery[]>(`/webhooks/${endpointId}/deliveries`),
  });
}

export function useLogHead() {
  return useQuery({
    queryKey: ["log-head"],
    queryFn: () => api<SignedHead>("/log/head"),
    retry: false,
  });
}

export function useConsistency(from: number | null) {
  return useQuery({
    queryKey: ["consistency", from],
    queryFn: () =>
      api<{ oldHead: { size: number; root: string }; head: { size: number; root: string }; proof: { path: string[] } }>(
        `/log/consistency?from=${from}`,
      ),
    enabled: from !== null,
  });
}

export function useWitnessCheckpoint() {
  return useQuery({
    queryKey: ["witness-checkpoint"],
    queryFn: () => api<WitnessCheckpoint | null>("/log/checkpoint"),
    retry: false,
  });
}

/** One idempotency key per amount intent: a `mutate` retry of the same amount
 * replays the key so a response lost mid-flight re-reads the standing top-up
 * instead of minting a second one. A different amount starts a new intent. */
function useKeyedMutation<TData>(path: string) {
  const intent = useRef<{ amount: number; key: string } | null>(null);
  return useMutation({
    mutationFn: (amountMinor: number) => {
      if (intent.current?.amount !== amountMinor) {
        intent.current = { amount: amountMinor, key: crypto.randomUUID() };
      }
      return api<TData>(path, {
        method: "POST",
        body: { amountMinor, idempotencyKey: intent.current.key },
      });
    },
  });
}

export function useStripeTopup() {
  return useKeyedMutation<{ checkoutUrl: string; depositId: string }>("/topups/stripe");
}

export function useHyperswitchTopup() {
  return useKeyedMutation<{
    paymentId: string;
    clientSecret: string;
    publishableKey: string;
    depositId: string;
    sdkUrl: string;
  }>("/topups/hyperswitch");
}

export function useAirwallexTopup() {
  return useKeyedMutation<{
    checkoutUrl: string;
    intentId: string;
    clientSecret?: string;
    depositId: string;
  }>("/topups/airwallex");
}

/** The Solana deposit rail's opened intent (issues #1117, #1140): the
 * per-intent marker the paying transaction must carry, the address and
 * amount to send, and the whitelist `asset` that priced it. `splToken` is
 * the intent's SPL mint or `null` for native SOL; `assetDecimals` converts
 * `amountLamports` into the decimal `amount` a Solana Pay request names
 * (issue #1141), and `depositId` is the memo it pins. */
export interface SolanaIntent {
  reference: string;
  asset?: string;
  payTo: string;
  amountLamports: number;
  splToken?: string | null;
  assetDecimals: number;
  depositId: string;
  expiresAt: string;
}

/** Opens a Solana top-up intent. Unlike the hosted rails the amount rides on
 * `amountMinor` alone — `payment_ref` is the freshly minted `reference`, so no
 * caller idempotency key is needed (the rail re-mints per call). */
export function useSolanaTopup() {
  return useMutation({
    mutationFn: (amountMinor: number) =>
      api<SolanaIntent>("/topups/solana", {
        method: "POST",
        body: { amountMinor },
      }),
  });
}

/** Settles an opened Solana intent now rather than waiting for the poll: the
 * body is the intent's `reference`, never the chain signature — the rail finds
 * and validates the paying transaction itself. 403 means no honoring transfer
 * has landed yet; the balance still catches up on the next poll. */
export function useSolanaConfirm() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (reference: string) =>
      api<Receipt>("/topups/solana/confirm", {
        method: "POST",
        body: { reference },
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["balance"] });
      client.invalidateQueries({ queryKey: ["bills"] });
      client.invalidateQueries({ queryKey: ["solana-deposits"] });
    },
  });
}

/** One solana deposit on the exception queue, as the customer read answers
 * it (issue #1142): `asset` is the whitelist key the intent locked — the
 * denomination `delivered`/`expected` count in — `refundPending` is where
 * the refund-address form writes, `refundSignature` the closed refund's
 * on-chain proof. */
export interface SolanaExceptionDeposit {
  depositId: string;
  organizationId: string;
  organization: string;
  status: string;
  asset: string | null;
  paymentRef: string;
  amountMinor: number;
  receivedMinor: number | null;
  delivered: number | null;
  expected: number | null;
  splToken: string | null;
  signature: string | null;
  entryId: string | null;
  refundAddress: string | null;
  refund: {
    requestedBy?: string;
    requestedAt?: string;
    reason?: string;
    addressedAt?: string;
    approvedBy?: string;
    approvedAt?: string;
    completedAt?: string;
  } | null;
  refundSignature: string | null;
  createdAt: string;
  updatedAt: string;
}

/** The organization's own solana exception deposits — the "your deposit
 * needs attention" list on the Topup surface. Polls lightly while the rail
 * may still walk a deposit onto the queue. */
export function useSolanaDeposits(options?: { refetchInterval?: number | false }) {
  return useQuery({
    queryKey: ["solana-deposits"],
    queryFn: () => api<SolanaExceptionDeposit[]>("/topups/solana/deposits"),
    refetchInterval: options?.refetchInterval ?? 15_000,
    // A deployment without the rail answers 503 — the rail tab already says
    // so; the queue read stays quiet rather than doubling the banner.
    retry: false,
  });
}

/** The customer's leg of the pull-payment refund: names the base58 address
 * the multisig payout sends to — the rail never derives the payer. */
export function useSolanaRefundAddress() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ depositId, address }: { depositId: string; address: string }) =>
      api<SolanaExceptionDeposit>(`/topups/solana/${depositId}/refund-address`, {
        method: "POST",
        body: { address },
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["solana-deposits"] });
    },
  });
}

export function useRedeemCode() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (code: string) =>
      api<Receipt & { amountMinor: number }>("/redemptions", {
        method: "POST",
        body: { code },
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["balance"] });
      client.invalidateQueries({ queryKey: ["bills"] });
    },
  });
}

export function usePricing() {
  return useQuery({
    queryKey: ["pricing"],
    queryFn: () => api<{ models: PricedModel[] }>("/pricing"),
    staleTime: 60_000,
  });
}

export function useEstimatePrice() {
  return useMutation({
    mutationFn: (body: {
      model: string;
      mode?: string;
      inputTokens?: number;
      outputTokens?: number;
      serviceTier?: string;
    }) => api<PriceEstimate>("/estimate-price", { method: "POST", body }),
  });
}

export function usePriceHead(channel: string, model: string, version?: number) {
  return useQuery({
    queryKey: ["price-head", channel, model, version ?? null],
    queryFn: () =>
      api<SignedHead>(
        `/log/price-head?channel=${encodeURIComponent(channel)}&model=${encodeURIComponent(model)}${version !== undefined ? `&version=${version}` : ""}`,
      ),
    enabled: channel !== "" && model !== "",
    retry: false,
  });
}

export function useLogKey() {
  return useQuery({
    queryKey: ["log-key"],
    queryFn: () => api<VerifyingKey>("/log/key"),
    retry: false,
  });
}
