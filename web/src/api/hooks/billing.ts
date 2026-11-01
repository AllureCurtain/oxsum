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

export function useCreateHold() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { idempotencyKey: string; amountMinor: number }) =>
      api<Receipt>("/holds", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["holds"] }),
  });
}

export function useSettleHold() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { holdKey: string; actualMinor: number }) =>
      api<Receipt>("/settlements", { method: "POST", body }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["holds"] });
      client.invalidateQueries({ queryKey: ["balance"] });
    },
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

export function useTopup() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (amountMinor: number) =>
      api<Receipt>("/topups", {
        method: "POST",
        body: { amountMinor, idempotencyKey: crypto.randomUUID() },
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["balance"] });
      client.invalidateQueries({ queryKey: ["bills"] });
    },
  });
}

export function useStripeTopup() {
  return useMutation({
    mutationFn: (amountMinor: number) =>
      api<{ checkoutUrl: string; depositId: string }>("/topups/stripe", {
        method: "POST",
        body: { amountMinor, idempotencyKey: crypto.randomUUID() },
      }),
  });
}

export function useHyperswitchTopup() {
  return useMutation({
    mutationFn: (amountMinor: number) =>
      api<{
        paymentId: string;
        clientSecret: string;
        publishableKey: string;
        depositId: string;
        sdkUrl: string;
      }>("/topups/hyperswitch", {
        method: "POST",
        body: { amountMinor, idempotencyKey: crypto.randomUUID() },
      }),
  });
}

export function useAirwallexTopup() {
  return useMutation({
    mutationFn: (amountMinor: number) =>
      api<{ checkoutUrl: string; intentId: string; clientSecret?: string; depositId: string }>(
        "/topups/airwallex",
        {
          method: "POST",
          body: { amountMinor, idempotencyKey: crypto.randomUUID() },
        },
      ),
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
