import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError } from "./client";

// The response bodies, typed by hand for the endpoints the scaffold serves —
// `npm run codegen` keeps `api/schema.d.ts` as the authoritative full surface
// (these stay deliberately small; the generated schema is what later pages
// should consume).

export interface SessionInfo {
  user: { id: string; email: string; emailVerified: boolean };
  organization: { id: string; name: string; kind: string; tenantId: string };
  role: "owner" | "admin" | "member";
}

export interface Balance {
  availableMinor: number;
  creditLimitMinor: number;
  creditUsedMinor: number;
  reservedMinor: number;
  spentThisMonthMinor: number;
}

export interface BillingRecord {
  bookedOn: string;
  requestId: string;
  entryId: string;
  model: string;
  kind: string;
  status: string;
  inputTokens: number;
  outputTokens: number;
  chargedMinor: number;
  discountPercent?: number;
  upstreamAttempts: number;
  keyId?: string;
  failureReason?: string;
}

export interface ApiKey {
  id: string;
  name?: string | null;
  prefix: string;
  createdBy?: string | null;
  createdAt: string;
  expiresAt?: string | null;
  revokedAt?: string | null;
  spendLimitMinor?: number | null;
  budgetDuration?: "daily" | "weekly" | "monthly" | null;
  modelAllowlist?: string[] | null;
  requestsPerMinute?: number | null;
  maxConcurrentHolds?: number | null;
  tokensPerMinute?: number | null;
  tokensPerMinuteScopes?: Record<string, unknown> | null;
}

export interface UsageDay {
  day: string;
  keyId?: string | null;
  channel: string;
  model: string;
  turns: number;
  inputTokens: number;
  outputTokens: number;
  cachedTokens: number;
  reasoningTokens: number;
  chargedMinor: number;
}

export interface Member {
  userId: string;
  email: string;
  role: "owner" | "admin" | "member";
  budgetLimitMinor?: number | null;
  joinedAt: string;
}

export interface OpenHold {
  requestId: string;
  model: string;
  freezeMinor: number;
  keyPrefix?: string;
}

export interface LogEntryRow {
  index: number;
  id: string;
  description: string;
  contentHash: string;
}

export interface BillRow {
  index: number;
  entryId: string;
  bookedOn: string;
  kind: "topUp" | "adjustment" | "settlement";
  amountMinor: number;
  description: string;
  contentHash: string;
}

export interface Statement {
  id: string;
  organizationId: string;
  period: string;
  status: "draft" | "finalized";
  paymentStatus: "pending" | "paid" | "overdue" | "suspended";
  totalMinor: number;
  creditDrawnMinor: number;
  paidMinor: number;
  outstandingMinor: number;
  paymentTermsDays: number;
  dueDate?: string | null;
  finalizedAt?: string | null;
  createdAt: string;
}

export interface StatementLine {
  channel: string;
  model: string;
  turns: number;
  inputTokens: number;
  outputTokens: number;
  cachedTokens: number;
  reasoningTokens: number;
  amountMinor: number;
}

export interface WebhookEndpoint {
  id: string;
  url: string;
  events: string[];
  enabled: boolean;
  balanceLowMinor?: number | null;
  budgetThresholdsPercent?: number[];
  secretLast4?: string;
  createdAt: string;
}

export interface WebhookDelivery {
  id: string;
  endpointId: string;
  event: string;
  status: string;
  attempts: number;
  lastError?: string | null;
  createdAt: string;
}

export interface BudgetSnapshot {
  organization: {
    availableMinor: number;
    reservedMinor: number;
    creditLimitMinor?: number | null;
    creditUsedMinor?: number | null;
    consumedPercent?: number | null;
  };
  member?: {
    budgetLimitMinor: number;
    committedMinor: number;
    consumedPercent?: number | null;
  } | null;
  keys: {
    keyId: string;
    name?: string | null;
    prefix: string;
    spendLimitMinor?: number | null;
    budgetDuration?: string | null;
    windowStart?: string | null;
    committedMinor: number;
    consumedPercent?: number | null;
  }[];
}

export interface SignedHead {
  note: string;
  origin: string;
  size: number;
  root: string;
  keyName: string;
  publicKey: string;
  keyHash: string;
}

export interface WitnessCheckpoint {
  headSize: number;
  note: string;
  cosignatures: { witnessUrl: string; note: string; firstSeen: string }[];
  /** The deployment's pinned witness keyset (issue #259): which cosignature/v1
   * key a signature line under each URL must verify against. */
  witnessKeys: { witnessUrl: string; keyName: string; publicKey: string; keyHash: string }[];
  updatedAt: string;
}

export interface UserOrganization {
  organization: { id: string; name: string; kind: string; tenantId: string };
  role: "owner" | "admin" | "member";
}

export interface AuthMethods {
  oauthGithub: boolean;
  turnstileSiteKey?: string | null;
}

export interface Receipt {
  entryId: string;
  logIndex?: number | null;
  contentHash: string;
  isNew: boolean;
}

export interface PricedModel {
  model: string;
  channel: string;
  protocol: string;
  version: number;
  weight: number;
  createdAt: string;
  inputPricePerMillion: number;
  outputPricePerMillion?: number;
  maxOutputTokens?: number;
  cacheReadPricePerMillion?: number;
  cacheWrite5mPricePerMillion?: number;
  cacheWrite1hPricePerMillion?: number;
  reasoningPricePerMillion?: number;
  costPerRequest?: number;
}

export interface PriceEstimate {
  estimateMinor: number;
  model: string;
  version: number;
}

export interface VerifyingKey {
  keyName: string;
  publicKey: string;
  keyHash: string;
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

export function useBalance() {
  return useQuery({ queryKey: ["balance"], queryFn: () => api<Balance>("/balance") });
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
      api<ApiKey & { secret: string }>("/org/keys", { method: "POST", body }),
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
        displayCurrency: { code: string; ratePerCredit: string; decimals: number } | null;
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

export { ApiError };
