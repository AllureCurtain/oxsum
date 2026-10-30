import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError } from "./client";
import type { components } from "./schema";

// The response types come from the generated schema — `npm run codegen`
// keeps `api/schema.d.ts` authoritative over `openapi.yaml`, so a hook's
// return type can never drift from the contract. The four types with no
// named schema (Balance, SignedHead, WitnessCheckpoint, PriceEstimate)
// stay hand-typed at the bottom of this block.

export type SessionInfo = components["schemas"]["SessionInfo"];
export type ApiKey = components["schemas"]["ApiKey"];
export type UsageDay = components["schemas"]["UsageDay"];
export type Member = components["schemas"]["Member"];
export type WebhookEndpoint = components["schemas"]["WebhookEndpoint"];
export type WebhookDelivery = components["schemas"]["WebhookDelivery"];
export type BudgetSnapshot = components["schemas"]["BudgetSnapshot"];
export type BillingRecord = components["schemas"]["BillingRecord"];
export type Statement = components["schemas"]["Statement"];
export type StatementLine = components["schemas"]["StatementLine"];
export type UserOrganization = components["schemas"]["UserOrganization"];
export type AuthMethods = components["schemas"]["AuthMethods"];
export type Receipt = components["schemas"]["Receipt"];
export type BillRow = components["schemas"]["BillRow"];
export type VerifyingKey = components["schemas"]["VerifyingKey"];
export type DisplayCurrencyType = components["schemas"]["DisplayCurrency"];

/** Renamed to the schema's names — `OpenHold`/`LogEntryRow`/`PricedModel`
 * were the scaffold's hand labels; the wire types are `OpenHoldView`,
 * `LogEntryView` and `PricedModel` (which is `ModelPrice & {channel,protocol}`,
 * the pricing page's row). */
export type OpenHold = components["schemas"]["OpenHoldView"];
export type LogEntryRow = components["schemas"]["LogEntryView"];
export type PricedModel = components["schemas"]["PricedModel"];

// No named schema — these endpoints return an inline `data` shape the
// codegen doesn't lift into components["schemas"], so they stay hand-typed.
export interface Balance {
  // no schema counterpart — the `/balance` response is an inline object
  availableMinor: number;
  creditLimitMinor: number;
  creditUsedMinor: number;
  reservedMinor: number;
  spentThisMonthMinor: number;
}

export interface SignedHead {
  // no schema counterpart — `/log/head` and `/log/price-head` return inline shapes
  note: string;
  origin: string;
  size: number;
  root: string;
  keyName: string;
  publicKey: string;
  keyHash: string;
}

export interface WitnessCheckpoint {
  // no schema counterpart — `/log/checkpoint` returns an inline shape
  headSize: number;
  note: string;
  cosignatures: { witnessUrl: string; note: string; firstSeen: string }[];
  /** The deployment's pinned witness keyset (issue #259): which cosignature/v1
   * key a signature line under each URL must verify against. */
  witnessKeys: { witnessUrl: string; keyName: string; publicKey: string; keyHash: string }[];
  updatedAt: string;
}

export interface PriceEstimate {
  // no schema counterpart — `/estimate-price` returns an inline shape
  estimateMinor: number;
  model: string;
  version: number;
}

export function useSession() {
  return useQuery({
    queryKey: ["session"],
    queryFn: () => api<SessionInfo>("/session"),
    retry: false,
    staleTime: 60_000,
  });
}

export function useLogin() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { email: string; password: string }) =>
      api<SessionInfo>("/auth/login", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries(),
  });
}

export function useLogout() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: () => api("/auth/logout", { method: "POST" }),
    onSettled: () => client.clear(),
  });
}

export function useAuthMethods() {
  return useQuery({
    queryKey: ["auth-methods"],
    queryFn: () => api<AuthMethods>("/auth/methods"),
    staleTime: 300_000,
    retry: false,
  });
}

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

export function useKeys() {
  return useQuery({
    queryKey: ["keys"],
    queryFn: () => api<ApiKey[]>("/org/keys"),
  });
}

export function useCreateKey() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: Partial<ApiKey> & { name?: string }) =>
      api<components["schemas"]["CreatedApiKey"]>("/org/keys", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["keys"] }),
  });
}

export function usePatchKey() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ keyId, ...body }: { keyId: string } & Record<string, unknown>) =>
      api<ApiKey>(`/org/keys/${keyId}`, { method: "PATCH", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["keys"] }),
  });
}

export function useRevokeKey() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (keyId: string) => api<ApiKey>(`/org/keys/${keyId}`, { method: "DELETE" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["keys"] }),
  });
}

export function useUsage(from: string, to: string) {
  return useQuery({
    queryKey: ["usage", from, to],
    queryFn: () => api<{ days: UsageDay[] }>(`/usage?from=${from}&to=${to}`),
  });
}

export function useMembers() {
  return useQuery({
    queryKey: ["members"],
    queryFn: () => api<{ members: Member[] }>("/org/members"),
  });
}

export function useAddMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (email: string) =>
      api<Member>("/org/members", { method: "POST", body: { email } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["members"] }),
  });
}

export function useUpdateMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ userId, ...body }: { userId: string; role?: string; budgetLimitMinor?: number | null }) =>
      api<Member>(`/org/members/${userId}`, { method: "PATCH", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["members"] }),
  });
}

export function useRemoveMember() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (userId: string) =>
      api(`/org/members/${userId}`, { method: "DELETE" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["members"] }),
  });
}

export function useTransferOwnership() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (userId: string) =>
      api("/org/ownership", { method: "POST", body: { userId } }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["members"] });
      client.invalidateQueries({ queryKey: ["session"] });
    },
  });
}

export function useCreateInvitation() {
  return useMutation({
    mutationFn: () =>
      api<{ url: string }>("/org/invitations", { method: "POST" }),
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

export function useOrganizations() {
  return useQuery({
    queryKey: ["orgs"],
    queryFn: () => api<UserOrganization[]>("/orgs"),
  });
}

export function useCreateOrganization() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      api("/orgs", { method: "POST", body: { name } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["orgs"] }),
  });
}

export function useSwitchOrganization() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (organizationId: string) =>
      api("/session/organization", { method: "POST", body: { organizationId } }),
    onSettled: () => client.invalidateQueries(),
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

export function useDeleteAccount() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (password: string) =>
      api("/users/me", { method: "DELETE", body: { password } }),
    onSettled: () => client.clear(),
  });
}

export function useOrg() {
  return useQuery({
    queryKey: ["org"],
    queryFn: () =>
      api<{
        id: string;
        name: string;
        tenantId: string;
        kind: string;
        createdAt: string;
        // The fiat unit this org renders in (issue #257); null = credits only.
        displayCurrency: DisplayCurrencyType | null;
      }>("/org"),
  });
}

/** The organization's display currency — `null` renders credits only. Every
 * money cell that wants the conversion reads the same cached org row. */
export function useDisplay() {
  const org = useOrg();
  return org.data?.displayCurrency ?? null;
}

/** `PATCH /api/v1/org` — the owner-or-admin write that sets (or, with `null`,
 * clears) the display currency. The rate is a decimal string so the wire
 * never carries a float. */
export function useUpdateOrgDisplay() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (displayCurrency: { code: string; ratePerCredit: string; decimals: number } | null) =>
      api("/org", { method: "PATCH", body: { displayCurrency } }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["org"] }),
  });
}

export function useHealth() {
  return useQuery({
    queryKey: ["healthz"],
    queryFn: async () => {
      const res = await fetch("/healthz", { credentials: "same-origin" });
      return res.ok ? ("ok" as const) : (`unhealthy: ${res.status}` as const);
    },
    refetchInterval: 30_000,
    retry: false,
  });
}

// End-user usage reports and their share links (issues #261/#300). The wire
// shapes come from the generated schema: openapi.yaml is the contract.
export type UsageReportSummary = components["schemas"]["UsageReportSummary"];
export type UsageReport = components["schemas"]["UsageReport"];
export type ReportLink = components["schemas"]["ReportLink"];
export type CreatedReportLink = components["schemas"]["CreatedReportLink"];

export function useUsageReports() {
  return useQuery({
    queryKey: ["usage-reports"],
    queryFn: () => api<{ rows: UsageReportSummary[] }>("/org/usage-reports"),
  });
}

export function useMintUsageReport() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { endUser: string; from: string; to: string }) =>
      api<UsageReport>("/org/usage-reports", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["usage-reports"] }),
  });
}

export function useReportLinks() {
  return useQuery({
    queryKey: ["report-links"],
    queryFn: () => api<{ rows: ReportLink[] }>("/org/usage-reports/links"),
  });
}

export function useCreateReportLink() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({
      reportId,
      ...body
    }: { reportId: string; label?: string; expiresAt?: string; email?: string }) =>
      api<CreatedReportLink>(`/org/usage-reports/${reportId}/links`, {
        method: "POST",
        body,
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["report-links"] }),
  });
}

export function useCreateCustomerLink() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { endUser: string; label?: string; expiresAt?: string; email?: string }) =>
      api<CreatedReportLink>("/org/usage-reports/links", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["report-links"] }),
  });
}

export function useRevokeReportLink() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (linkId: string) =>
      api<ReportLink>(`/org/usage-reports/links/${linkId}`, { method: "DELETE" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["report-links"] }),
  });
}

export { ApiError };
