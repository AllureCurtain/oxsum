import { useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";
export type BundleOffer = components["schemas"]["BundleOffer"];
export type BundlePublish = components["schemas"]["BundlePublishRequest"];
export function useBundles(business = false) {
  return useQuery({
    queryKey: [business ? "biz-bundles" : "bundles"],
    queryFn: () => api<BundleOffer[]>(business ? "/business/bundles" : "/bundles"),
  });
}
export function useBundleMutation() {
  const client = useQueryClient();
  const intent = useRef<{ body: string; key: string } | null>(null);
  const mutation = useMutation({
    mutationFn: ({ path, body }: { path: string; body: Record<string, unknown> }) =>
      api<components["schemas"]["BundleMutation"]>(path, { method: "POST", body }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["biz-bundles"] });
      client.invalidateQueries({ queryKey: ["bundles"] });
    },
  });
  return {
    ...mutation,
    submit: (path: string, body: Record<string, unknown>) => {
      const value = JSON.stringify({ path, body });
      if (intent.current?.body !== value) intent.current = { body: value, key: crypto.randomUUID() };
      return mutation.mutateAsync({ path, body: { ...body, idempotencyKey: intent.current.key } });
    },
  };
}
