import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

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
