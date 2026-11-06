import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

// The plan surface (issues #610/#612, T19/T21): the published catalogue a
// customer buys from, its own live binding with period history and credit
// standing, and the plan statements a period close issues. The wire types are
// generated — a field rename in openapi.yaml breaks this file at `tsc`, never
// at runtime.

export type SoldPlan = components["schemas"]["SoldPlan"];
export type CustomerPlan = components["schemas"]["CustomerPlan"];
export type PlanPeriod = components["schemas"]["PlanPeriod"];
export type OrgPlanView = components["schemas"]["OrgPlanView"];
export type PlanCreditStanding = components["schemas"]["PlanCreditStanding"];
export type PlanStatement = components["schemas"]["PlanStatement"];
export type PlanStatementDetail = components["schemas"]["PlanStatementDetail"];
export type PlanStatementPage = components["schemas"]["PlanStatementPage"];

/** `GET /api/v1/plans`: every sold version still on sale — the catalogue the
 * plans page browses and a `planPeriod` order's `purposeRef` names. */
export function usePlans() {
  return useQuery({
    queryKey: ["plans"],
    queryFn: () => api<SoldPlan[]>("/plans"),
  });
}

/** `GET /api/v1/org/plan`: the caller's live binding plus every authorized
 * period and its credit standing. `plan` is null while the customer has none. */
export function useOrgPlan() {
  return useQuery({
    queryKey: ["org-plan"],
    queryFn: () => api<OrgPlanView>("/org/plan"),
  });
}

/** `POST /api/v1/org/plan/cancel`: mark `cancelAtPeriodEnd` — the paid period
 * serves to its end, the boundary does not renew. */
export function useCancelPlan() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: () => api<CustomerPlan>("/org/plan/cancel", { method: "POST" }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["org-plan"] }),
  });
}

/** `POST /api/v1/org/plan/change`: book a swap to another sold version,
 * effective at the next period boundary. */
export function useChangePlan() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (planVersionId: string) =>
      api<CustomerPlan>("/org/plan/change", {
        method: "POST",
        body: { planVersionId },
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["org-plan"] }),
  });
}

/** `GET /api/v1/org/statements`: the customer's plan statements, newest
 * first — the billing document a period close issues. */
export function usePlanStatements(after?: string) {
  const params = new URLSearchParams();
  if (after) params.set("after", after);
  const qs = params.toString();
  return useQuery({
    queryKey: ["plan-statements", after ?? null],
    queryFn: () => api<PlanStatementPage>(`/org/statements${qs ? `?${qs}` : ""}`),
  });
}

/** `GET /api/v1/org/statements/{id}`: one statement plus its itemized lines. */
export function usePlanStatement(statementId: string | undefined) {
  return useQuery({
    queryKey: ["plan-statement", statementId],
    queryFn: () => api<PlanStatementDetail>(`/org/statements/${statementId}`),
    enabled: Boolean(statementId),
  });
}
