import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

type Schemas = components["schemas"];
type Config<T extends { config: unknown }> = T["config"];

/** Each business write uses its own capability, never an admin token. */
export type ProviderIntent =
  | { operation: "connection"; config: Config<Schemas["BusinessChannelRequest"]> }
  | { operation: "credential"; channel: string; config: Config<Schemas["BusinessProviderCredentialRequest"]> }
  | { operation: "billing"; channel: string; config: Config<Schemas["BusinessProviderBillKeyRequest"]> }
  | { operation: "status"; channel: string; credential: string; config: Config<Schemas["BusinessProviderStatusRequest"]> }
  | { operation: "remove"; channel: string; credential: string; config: Config<Schemas["BusinessProviderRemoveRequest"]> }
  | { operation: "price"; channel: string; config: Config<Schemas["BusinessProviderPriceRequest"]> }
  | { operation: "alias"; channel: string; config: Config<Schemas["BusinessProviderAliasRequest"]> }
  | { operation: "removeAlias"; channel: string; alias: string; config: Config<Schemas["BusinessProviderRemoveRequest"]> }
  | { operation: "cache"; channel: string; config: Config<Schemas["BusinessProviderCacheRequest"]> }
  | { operation: "fallbacks"; model: string; config: Config<Schemas["BusinessProviderFallbackRequest"]> };

export function useProviderAliases(channel: string) {
  return useQuery({ queryKey: ["biz-provider-aliases", channel], retry: false,
    queryFn: () => api<Schemas["ModelAlias"][]>(`/business/channels/${encodeURIComponent(channel)}/aliases`) });
}
export function useProviderPrices(channel: string) {
  return useQuery({ queryKey: ["biz-provider-prices", channel], retry: false,
    queryFn: () => api<Schemas["ModelPrice"][]>(`/business/channels/${encodeURIComponent(channel)}/prices`) });
}
export function useProviderFallbacks(model: string | null) {
  return useQuery({ queryKey: ["biz-provider-fallbacks", model], retry: false, enabled: model !== null,
    gcTime: 0, refetchOnWindowFocus: false,
    queryFn: () => api<Schemas["ModelFallbackChain"]>(`/business/model-fallbacks/${encodeURIComponent(model!)}`) });
}

/** Secret inputs remain in component/mutation memory; they are never persisted. */
export function useProviderWrite() {
  const client = useQueryClient();
  return useMutation({
    retry: false,
    gcTime: 0,
    mutationFn: ({ intent, reason, idempotencyKey }: {
      intent: ProviderIntent; reason: string; idempotencyKey: string;
    }) => {
      let path = "/business/channels";
      if (intent.operation === "fallbacks") path = `/business/model-fallbacks/${encodeURIComponent(intent.model)}`;
      else if (intent.operation !== "connection") {
        path += `/${encodeURIComponent(intent.channel)}`;
        switch (intent.operation) {
          case "credential": path += "/credentials"; break;
          case "billing": path += "/billing-credential"; break;
          case "price": path += "/prices"; break;
          case "alias": path += "/aliases"; break;
          case "removeAlias": path += `/aliases/${encodeURIComponent(intent.alias)}/remove`; break;
          case "cache": path += "/cache"; break;
          case "status":
          case "remove":
            path += `/credentials/${encodeURIComponent(intent.credential)}/${intent.operation}`;
            break;
        }
      }
      return api<Schemas["ProviderMutationResult"]>(path, {
        method: intent.operation === "fallbacks" ? "PUT" : "POST", body: { idempotencyKey, reason, config: intent.config },
      });
    },
    onSuccess: () => Promise.all(["biz-channels", "biz-provider-prices", "biz-provider-aliases", "biz-provider-fallbacks"]
      .map((key) => client.invalidateQueries({ queryKey: [key] }))),
  });
}
