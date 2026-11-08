import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

export type CollectionTerms = components["schemas"]["CollectionTerms"];
export type CollectionTermsRequest = components["schemas"]["CollectionTermsRequest"];

export function useCollectionTerms(business = false) {
  return useQuery({
    queryKey: [business ? "biz-collection" : "collection"],
    queryFn: () => api<CollectionTerms>(business ? "/business/collection-terms" : "/collection-terms"),
  });
}

export function useSetCollectionTerms() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: CollectionTermsRequest) =>
      api<components["schemas"]["CollectionTermsMutation"]>("/business/collection-terms", { method: "POST", body }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["biz-collection"] });
      client.invalidateQueries({ queryKey: ["collection"] });
    },
  });
}
