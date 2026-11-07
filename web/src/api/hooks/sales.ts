import { useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";
export type SaleChange = components["schemas"]["SaleChange"];
export type SaleHistory = components["schemas"]["SaleBookHistory"];
export type SaleSchedule = components["schemas"]["SaleSchedule"];
export type SalePreviewRequest = components["schemas"]["SalePreviewRequest"];

/** Retries of the identical intent retain their identity, including a lost response. */
export function useSaleChange() {
  const client = useQueryClient();
  const intent = useRef<{ body: string; key: string } | null>(null);
  const mutation = useMutation({ mutationFn: (body: components["schemas"]["SaleChangeRequest"]) =>
    api<components["schemas"]["SaleMutationResult"]>("/business/sale-books/changes", { method: "POST", body }),
    onSuccess: () => {
      for (const key of ["biz-sale-books", "biz-sale-book", "biz-customer-sale"]) client.invalidateQueries({ queryKey: [key] });
    },
  });
  return { ...mutation, submit: async (change: SaleChange, reason: string) => {
    const body = JSON.stringify({ change, reason });
    if (intent.current?.body !== body) intent.current = { body, key: crypto.randomUUID() };
    return mutation.mutateAsync({ change, reason, idempotencyKey: intent.current.key });
  } };
}
export function useCustomerSaleTerms(id: string) {
  return useQuery({ queryKey: ["biz-customer-sale", id], enabled: Boolean(id),
    queryFn: () => api<SaleSchedule[]>(`/business/customers/${encodeURIComponent(id)}/sale-terms`) });
}
export function useSalePreview() {
  return useMutation({ mutationFn: (body: SalePreviewRequest) =>
    api<components["schemas"]["SalePricePreview"]>("/business/sale-price-preview", { method: "POST", body }) });
}

/** Load the complete book directory so replacing a default can name another book. */
export function useSaleDirectory(enabled = true) {
  return useQuery({ enabled, queryKey: ["biz-sale-books", "directory"], queryFn: async () => {
    const books: components["schemas"]["SaleBook"][] = [];
    const seen = new Set<string>();
    let after: string | null = null;
    do {
      const page: components["schemas"]["SaleBookPage"] = await api(`/business/sale-books?limit=100${after ? `&after=${encodeURIComponent(after)}` : ""}`);
      books.push(...page.books);
      after = page.nextAfter;
      if (after && seen.has(after)) throw new Error("Sale directory cursor repeated");
      if (after) seen.add(after);
    } while (after);
    return books;
  } });
}
