import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";
import type { SessionContextInfo } from "./types";
import type {
  SoldPlan,
  CustomerPlan,
  OrgPlanView,
  PlanStatementDetail,
  PlanStatementPage,
} from "./plans";
import type { OrderRefund } from "./orders";

// ---------------------------------------------------------------------------
// The staff business console (issue #615, T24): every hook the `/app/business/*`
// surfaces read and write through. Wire types are generated — a field rename in
// openapi.yaml breaks this file at `tsc`, never at runtime. Every staff write
// carries an `idempotencyKey` (the contract's audit idempotency identity); the
// caller mints it once per form submission so a replay answers the standing
// record rather than a second action.
// ---------------------------------------------------------------------------

export type BusinessPrincipal = components["schemas"]["BusinessPrincipal"];
export type BusinessCapability = components["schemas"]["BusinessCapability"];
export type BusinessRole = components["schemas"]["BusinessRole"];
export type BusinessStaff = components["schemas"]["BusinessStaff"];
export type BusinessMutationResult = components["schemas"]["BusinessMutationResult"];
export type BusinessCustomer = components["schemas"]["BusinessCustomer"];
export type BusinessCustomerPage = components["schemas"]["BusinessCustomerPage"];
export type InvoiceJournal = components["schemas"]["InvoiceJournal"];
export type InvoiceJournalDetail = components["schemas"]["InvoiceJournalDetail"];
export type InvoiceJournalPage = components["schemas"]["InvoiceJournalPage"];
export type PaymentOrderPage = components["schemas"]["PaymentOrderPage"];
export type PaymentReconciliation = components["schemas"]["PaymentReconciliation"];
export type ManualOrderResult = components["schemas"]["ManualOrderResult"];
export type OrderRefundPage = components["schemas"]["OrderRefundPage"];
export type OrderCreditNote = components["schemas"]["OrderCreditNote"];
export type OrderCreditNotePage = components["schemas"]["OrderCreditNotePage"];
export type OrderDispute = components["schemas"]["OrderDispute"];
export type OrderDisputePage = components["schemas"]["OrderDisputePage"];
export type FinancialFacts = components["schemas"]["FinancialFacts"];
export type CostCoverage = components["schemas"]["CostCoverage"];
export type ReconciliationRun = components["schemas"]["ReconciliationRun"];
export type ReconciliationException = components["schemas"]["ReconciliationException"];
export type ReconciliationRepair = components["schemas"]["ReconciliationRepair"];
export type FinanceAuditEntry = components["schemas"]["FinanceAuditEntry"];
export type FinanceAuditPage = components["schemas"]["FinanceAuditPage"];
export type SaleBook = components["schemas"]["SaleBook"];
export type SaleBookPage = components["schemas"]["SaleBookPage"];
export type SaleBookHistory = components["schemas"]["SaleBookHistory"];
export type Channel = components["schemas"]["Channel"];
export type BusinessChannelPage = components["schemas"]["BusinessChannelPage"];
export type Merchant = components["schemas"]["Merchant"];
export type MerchantPage = components["schemas"]["MerchantPage"];
export type MerchantEvent = components["schemas"]["MerchantEvent"];
export type MerchantEventPage = components["schemas"]["MerchantEventPage"];
export type IntegrationCredential = components["schemas"]["IntegrationCredential"];
export type IntegrationCredentialPage = components["schemas"]["IntegrationCredentialPage"];
export type CreatedIntegrationCredential =
  components["schemas"]["CreatedIntegrationCredential"];
export type ProviderConnectionCheck = components["schemas"]["ProviderConnectionCheck"];

/** Mint one idempotency key per logical submission — a uuid the replay's
 * `(business, action, key)` unique index dedupes under. */
export function idem(): string {
  return crypto.randomUUID();
}

function qs(params: Record<string, string | number | undefined | null>): string {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== null && v !== "") sp.set(k, String(v));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

// ---------------------------------------------------------------------------
// Session context + business principal — the console's own identity. The
// customer session selects `business` context explicitly; the principal's
// `capabilities` drive which sections the nav shows.
// ---------------------------------------------------------------------------

/** `GET /api/v1/session/context`: which authority the session currently acts
 * under — `customer` or `business` — plus the business principal when the
 * session holds staff membership. */
export function useSessionContext() {
  return useQuery({
    queryKey: ["session-context"],
    queryFn: () => api<SessionContextInfo>("/session/context"),
    staleTime: 30_000,
    retry: false,
  });
}

/** `POST /api/v1/session/context`: select customer or business authority. */
export function useSwitchContext() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (context: "customer" | "business") =>
      api<SessionContextInfo>("/session/context", {
        method: "POST",
        body: { idempotencyKey: idem(), context },
      }),
    onSuccess: async (data) => {
      // Cancel reads under the previous authority before mounting the new one.
      await client.cancelQueries();
      client.removeQueries({ predicate: (query) => query.queryKey[0] !== "session-context" });
      client.setQueryData(["session-context"], data);
    },
  });
}

/** `GET /api/v1/business`: the active business principal — role + capability
 * set, rechecked on every call. 403 when the session is not in business
 * context or is not staff. */
export function useBusiness(enabled = true) {
  return useQuery({
    queryKey: ["business"],
    queryFn: () => api<BusinessPrincipal>("/business"),
    staleTime: 30_000,
    retry: false,
    enabled,
  });
}

/** Capability gate: does the live principal hold this capability? The nav and
 * every guarded action read the same answer; the backend refuses regardless. */
export function useCapability(
  principal: BusinessPrincipal | undefined,
  cap: BusinessCapability,
): boolean {
  return Boolean(principal?.capabilities.includes(cap));
}

// ---------------------------------------------------------------------------
// Setup checklist — derived from per-section reads (no dedicated endpoint):
// profile, first staff invite, first merchant credential, first plan, first
// customer. Each step reads its own surface so the state is live.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Customers
// ---------------------------------------------------------------------------

export function useBusinessCustomers(after?: string, enabled = true) {
  return useQuery({
    enabled,
    queryKey: ["biz-customers", after ?? null],
    queryFn: () => api<BusinessCustomerPage>(`/business/customers${qs({ after })}`),
  });
}

export function useBusinessCustomer(organizationId: string | undefined) {
  return useQuery({
    queryKey: ["biz-customer", organizationId],
    queryFn: () => api<BusinessCustomer>(`/business/customers/${organizationId}`),
    enabled: Boolean(organizationId),
  });
}

export function useUpdateCustomer(organizationId: string) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { idempotencyKey: string; name: string; contactEmail: string | null; reason: string }) =>
      api<BusinessMutationResult>(`/business/customers/${organizationId}`, {
        method: "PATCH",
        body,
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-customer", organizationId] }),
  });
}

export function useCustomerLifecycle(organizationId: string) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { idempotencyKey: string; state: "active" | "archived"; reason: string }) =>
      api<BusinessMutationResult>(`/business/customers/${organizationId}/lifecycle`, {
        method: "POST",
        body,
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-customer", organizationId] }),
  });
}

export function useCustomerSuspension(organizationId: string) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { idempotencyKey: string; suspended: boolean; reason: string }) =>
      api<BusinessMutationResult>(`/business/customers/${organizationId}/suspension`, {
        method: "POST",
        body,
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-customer", organizationId] }),
  });
}

/** `GET /business/customers/{id}/plan`: the customer's live binding, periods
 * and credit standing — the staff entitlement read. */
export function useCustomerPlan(organizationId: string | undefined) {
  return useQuery({
    queryKey: ["biz-customer-plan", organizationId],
    queryFn: () => api<OrgPlanView>(`/business/customers/${organizationId}/plan`),
    enabled: Boolean(organizationId),
  });
}

/** `PUT /business/customers/{id}/plan`: bind a published sold version. */
export function useBindCustomerPlan(organizationId: string) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { planVersionId: string; complimentaryGrant?: string | null }) =>
      api<CustomerPlan>(`/business/customers/${organizationId}/plan`, {
        method: "PUT",
        body,
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["biz-customer-plan", organizationId] });
      client.invalidateQueries({ queryKey: ["biz-customer", organizationId] });
    },
  });
}

/** `POST /business/customers/{id}/plan/cancel`: boundary-cancel the binding. */
export function useCancelCustomerPlan(organizationId: string) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: () =>
      api<CustomerPlan>(`/business/customers/${organizationId}/plan/cancel`, {
        method: "POST",
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-customer-plan", organizationId] }),
  });
}

// ---------------------------------------------------------------------------
// Plans (sold catalogue)
// ---------------------------------------------------------------------------

export function useBusinessPlans(enabled = true) {
  return useQuery({
    enabled,
    queryKey: ["biz-plans"],
    queryFn: () => api<SoldPlan[]>("/business/plans"),
  });
}

/** `POST /business/plans`: publish a new plan version (draft → immutable). */
export function usePublishPlan() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: {
      planId?: string | null;
      name: string;
      priceMinor: number;
      currency: string;
      quotaMinor: number | null;
      periodKind: "calendarMonth" | "anniversary";
      overageEnabled: boolean;
      sortOrder: number;
    }) => api<SoldPlan>("/business/plans", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-plans"] }),
  });
}

export function useRetirePlan() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (planVersionId: string) =>
      api<BusinessMutationResult>(`/business/plans/${planVersionId}/retire`, { method: "POST" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-plans"] }),
  });
}

// ---------------------------------------------------------------------------
// Payments (orders + reconciliation + manual receipt)
// ---------------------------------------------------------------------------

export function useBusinessOrders(filter: {
  organizationId?: string;
  purpose?: string;
  status?: string;
  after?: string;
}, enabled = true) {
  return useQuery({
    enabled,
    queryKey: ["biz-orders", filter],
    queryFn: () =>
      api<PaymentOrderPage>(
        `/business/orders${qs({
          organizationId: filter.organizationId,
          purpose: filter.purpose,
          status: filter.status,
          after: filter.after,
        })}`,
      ),
  });
}

/** `GET /api/v1/orders/{id}` scoped through the staff orders surface is not a
 * business read; the detail comes off the order's own row in the page. */
export function usePaymentReconciliation() {
  return useQuery({
    queryKey: ["biz-payment-recon"],
    queryFn: () => api<PaymentReconciliation>("/business/payment-reconciliation"),
    refetchInterval: 30_000,
  });
}

/** `POST /business/orders`: a staff-recorded manual wallet-topup receipt. */
export function useManualOrder() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: {
      organizationId: string;
      idempotencyKey: string;
      displayAmountMinor: number;
      displayCurrency: string;
      reference: string;
      reason: string;
    }) => api<ManualOrderResult>("/business/orders", { method: "POST", body }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["biz-orders"] });
      client.invalidateQueries({ queryKey: ["biz-payment-recon"] });
    },
  });
}

// ---------------------------------------------------------------------------
// Refunds, credit notes, disputes
// ---------------------------------------------------------------------------

export function useBusinessRefunds(after?: string) {
  return useQuery({
    queryKey: ["biz-refunds", after ?? null],
    queryFn: () => api<OrderRefundPage>(`/business/refunds${qs({ after })}`),
    refetchInterval: 30_000,
  });
}

export function useBusinessRefund(refundId: string | undefined) {
  return useQuery({
    queryKey: ["biz-refund", refundId],
    queryFn: () => api<OrderRefund>(`/business/refunds/${refundId}`),
    enabled: Boolean(refundId),
  });
}

/** `POST /business/refunds/{id}/resolve`: approve or decline with a reason —
 * the audited staff decision under `RefundsWrite`. */
export function useResolveRefund() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { refundId: string; idempotencyKey: string; decision: "approve" | "decline"; reason?: string | null }) =>
      api<OrderRefund>(`/business/refunds/${v.refundId}/resolve`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, decision: v.decision, reason: v.reason ?? null },
      }),
    onSuccess: (_d, v) => {
      client.invalidateQueries({ queryKey: ["biz-refunds"] });
      client.invalidateQueries({ queryKey: ["biz-refund", v.refundId] });
      client.invalidateQueries({ queryKey: ["biz-credit-notes"] });
    },
  });
}

export function useBusinessCreditNotes(after?: string) {
  return useQuery({
    queryKey: ["biz-credit-notes", after ?? null],
    queryFn: () => api<OrderCreditNotePage>(`/business/credit-notes${qs({ after })}`),
  });
}

export function useBusinessDisputes(after?: string) {
  return useQuery({
    queryKey: ["biz-disputes", after ?? null],
    queryFn: () => api<OrderDisputePage>(`/business/disputes${qs({ after })}`),
    refetchInterval: 30_000,
  });
}

export function useResolveDispute() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { disputeId: string; idempotencyKey: string; resolution: "evidenceDue" | "won" | "lost" | "closed"; reason?: string | null }) =>
      api<OrderDispute>(`/business/disputes/${v.disputeId}/resolve`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, resolution: v.resolution, reason: v.reason ?? null },
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-disputes"] }),
  });
}

// ---------------------------------------------------------------------------
// Statements + journals (receivables)
// ---------------------------------------------------------------------------

export function useBusinessStatements(filter: { organizationId?: string; customerPlanId?: string; after?: string }, enabled = true) {
  return useQuery({
    enabled,
    queryKey: ["biz-statements", filter],
    queryFn: () =>
      api<PlanStatementPage>(
        `/business/statements${qs({ organizationId: filter.organizationId, customerPlanId: filter.customerPlanId, after: filter.after })}`,
      ),
    refetchInterval: 30_000,
  });
}

export function useBusinessStatement(statementId: string | undefined) {
  return useQuery({
    queryKey: ["biz-statement", statementId],
    queryFn: () => api<PlanStatementDetail>(`/business/statements/${statementId}`),
    enabled: Boolean(statementId),
  });
}

/** `POST /business/statements/{id}/release-review`: `extendGrace` restores a
 * past-due receivable, `void` cancels it — both audited. */
export function useReleaseStatement() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { statementId: string; idempotencyKey: string; action: "extendGrace" | "void"; reason: string }) =>
      api<PlanStatementDetail>(`/business/statements/${v.statementId}/release-review`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, action: v.action, reason: v.reason },
      }),
    onSuccess: (_d, v) => {
      client.invalidateQueries({ queryKey: ["biz-statements"] });
      client.invalidateQueries({ queryKey: ["biz-statement", v.statementId] });
    },
  });
}

export function useBusinessJournals(filter: { organizationId?: string; customerPlanId?: string; after?: string }) {
  return useQuery({
    queryKey: ["biz-journals", filter],
    queryFn: () =>
      api<InvoiceJournalPage>(
        `/business/journals${qs({ organizationId: filter.organizationId, customerPlanId: filter.customerPlanId, after: filter.after })}`,
      ),
  });
}

export function useBusinessJournal(journalId: string | undefined) {
  return useQuery({
    queryKey: ["biz-journal", journalId],
    queryFn: () => api<InvoiceJournalDetail>(`/business/journals/${journalId}`),
    enabled: Boolean(journalId),
  });
}

/** `POST /business/journals/{id}/void`: cancel a posted receivable — audited
 * in `finance_audit`, never a delete. */
export function useVoidJournal() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { journalId: string; idempotencyKey: string; reason: string }) =>
      api<InvoiceJournalDetail>(`/business/journals/${v.journalId}/void`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, reason: v.reason },
      }),
    onSuccess: (_d, v) => {
      client.invalidateQueries({ queryKey: ["biz-journals"] });
      client.invalidateQueries({ queryKey: ["biz-journal", v.journalId] });
    },
  });
}

// ---------------------------------------------------------------------------
// Reports: financial facts, cost coverage, reconciliation, audit
// ---------------------------------------------------------------------------

export function useFinancialFacts(filter: { organizationId?: string; model?: string; channel?: string; from?: string; to?: string; timeBasis?: string }) {
  return useQuery({
    queryKey: ["biz-facts", filter],
    queryFn: () =>
      api<FinancialFacts>(
        `/business/financial-facts${qs({ organizationId: filter.organizationId, model: filter.model, channel: filter.channel, from: filter.from, to: filter.to, timeBasis: filter.timeBasis })}`,
      ),
  });
}

export function useCostCoverage(filter: { organizationId?: string; from?: string; to?: string }) {
  return useQuery({
    queryKey: ["biz-coverage", filter],
    queryFn: () =>
      api<CostCoverage>(`/business/cost-coverage${qs({ organizationId: filter.organizationId, from: filter.from, to: filter.to })}`),
  });
}

export function useReconciliationRuns(after?: string) {
  return useQuery({
    queryKey: ["biz-recon-runs", after ?? null],
    queryFn: () => api<ReconciliationRun[]>(`/business/reconciliation-runs${qs({ after })}`),
    refetchInterval: 30_000,
  });
}

export function useReconciliationRun(runId: string | undefined) {
  return useQuery({
    queryKey: ["biz-recon-run", runId],
    queryFn: () => api<ReconciliationRun>(`/business/reconciliation-runs/${runId}`),
    enabled: Boolean(runId),
  });
}

export function useRunExceptions(runId: string | undefined) {
  return useQuery({
    queryKey: ["biz-recon-run-exceptions", runId],
    queryFn: () => api<ReconciliationException[]>(`/business/reconciliation-runs/${runId}/exceptions`),
    enabled: Boolean(runId),
  });
}

export function useReconciliationExceptions(filter: { open?: boolean; after?: string }) {
  return useQuery({
    queryKey: ["biz-exceptions", filter],
    queryFn: () =>
      api<ReconciliationException[]>(`/business/reconciliation-exceptions${qs({ open: filter.open === undefined ? undefined : String(filter.open), after: filter.after })}`),
    refetchInterval: 30_000,
  });
}

export function useRequestRun() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { idempotencyKey: string; periodStart: string; periodEnd: string }) =>
      api<ReconciliationRun>("/business/reconciliation-runs", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-recon-runs"] }),
  });
}

export function useResolveException() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { exceptionId: string; idempotencyKey: string; resolution: string }) =>
      api<ReconciliationException>(`/business/reconciliation-exceptions/${v.exceptionId}/resolve`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, resolution: v.resolution },
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["biz-exceptions"] });
      client.invalidateQueries({ queryKey: ["biz-recon-runs"] });
    },
  });
}

export function useWaiveException() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { exceptionId: string; idempotencyKey: string; resolution: string }) =>
      api<ReconciliationException>(`/business/reconciliation-exceptions/${v.exceptionId}/waive`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, resolution: v.resolution },
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-exceptions"] }),
  });
}

export function useRepairException() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { exceptionId: string; idempotencyKey: string; action: "reclassifyCost" | "adjustJournal" | "linkPayment" | "markDuplicate"; reason: string; patch?: Record<string, unknown> }) =>
      api<ReconciliationRepair>(`/business/reconciliation-exceptions/${v.exceptionId}/repairs`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, action: v.action, reason: v.reason, patch: v.patch },
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-exceptions"] }),
  });
}

/** `GET /business/audit`: the business finance audit — one immutable row per
 * staff financial action. T24 added the read; the writes already landed. */
export function useBusinessAudit(filter: { after?: string; action?: string }) {
  return useQuery({
    queryKey: ["biz-audit", filter],
    queryFn: () => api<FinanceAuditPage>(`/business/audit${qs({ after: filter.after, action: filter.action })}`),
    refetchInterval: 30_000,
  });
}

// ---------------------------------------------------------------------------
// Channels + prices (catalogue)
// ---------------------------------------------------------------------------

export function useBusinessChannels(after?: string) {
  return useQuery({
    queryKey: ["biz-channels", after ?? null],
    queryFn: () => api<BusinessChannelPage>(`/business/channels${qs({ after })}`),
  });
}

export function useChannelConnectionCheck() {
  return useMutation({
    mutationFn: (v: { channelName: string; idempotencyKey: string; reason: string }) =>
      api<ProviderConnectionCheck>(`/business/channels/${v.channelName}/connection-check`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, reason: v.reason },
      }),
  });
}

export function useSaleBooks(after?: string) {
  return useQuery({
    queryKey: ["biz-sale-books", after ?? null],
    queryFn: () => api<SaleBookPage>(`/business/sale-books${qs({ after })}`),
  });
}

export function useSaleBookHistory(bookId: string | undefined) {
  return useQuery({
    queryKey: ["biz-sale-book", bookId],
    queryFn: () => api<SaleBookHistory>(`/business/sale-books/${bookId}`),
    enabled: Boolean(bookId),
  });
}

// ---------------------------------------------------------------------------
// Merchants + integration credentials + staff + merchant events
// ---------------------------------------------------------------------------

export function useMerchants(after?: string, enabled = true) {
  return useQuery({
    enabled,
    queryKey: ["biz-merchants", after ?? null],
    queryFn: () => api<MerchantPage>(`/business/merchants${qs({ after })}`),
  });
}

export function useMerchantEvents(merchantId: string | undefined, after?: string) {
  return useQuery({
    queryKey: ["biz-merchant-events", merchantId, after ?? null],
    queryFn: () => api<MerchantEventPage>(`/business/merchants/${merchantId}/events${qs({ after })}`),
    enabled: Boolean(merchantId),
  });
}

export function useIntegrationCredentials(after?: string) {
  return useQuery({
    queryKey: ["biz-int-credentials", after ?? null],
    queryFn: () => api<IntegrationCredentialPage>(`/business/integration-credentials${qs({ after })}`),
  });
}

export function useCreateIntegrationCredential() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: Record<string, unknown>) =>
      api<CreatedIntegrationCredential>("/business/integration-credentials", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-int-credentials"] }),
  });
}

export function useRevokeIntegrationCredential() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (v: { credentialId: string; idempotencyKey: string; reason: string }) =>
      api<BusinessMutationResult>(`/business/integration-credentials/${v.credentialId}/revoke`, {
        method: "POST",
        body: { idempotencyKey: v.idempotencyKey, reason: v.reason },
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-int-credentials"] }),
  });
}

export function useBusinessStaff(enabled = true) {
  return useQuery({
    enabled,
    queryKey: ["biz-staff"],
    queryFn: () => api<BusinessStaff[]>("/business/staff"),
  });
}

export function useSetStaff() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { idempotencyKey: string; userId: string; role: "admin" | "finance" | "support" | "viewer"; reason: string }) =>
      api<BusinessMutationResult>("/business/staff", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-staff"] }),
  });
}

export function useRevokeStaff() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { idempotencyKey: string; userId: string; reason: string }) =>
      api<BusinessMutationResult>("/business/staff/revoke", { method: "POST", body }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-staff"] }),
  });
}
