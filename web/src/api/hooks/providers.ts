import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "../client";
import type { components } from "../schema";

type Schemas = components["schemas"];
type Config<T extends { config: unknown }> = T["config"];

/** Every owner write uses the existing business contract, never an admin token. */
export type ProviderIntent =
  | { operation: "connection"; config: Config<Schemas["BusinessChannelRequest"]> }
  | { operation: "credential"; channel: string; config: Config<Schemas["BusinessProviderCredentialRequest"]> }
  | { operation: "billing"; channel: string; config: Config<Schemas["BusinessProviderBillKeyRequest"]> }
  | { operation: "status"; channel: string; credential: string; config: Config<Schemas["BusinessProviderStatusRequest"]> }
  | { operation: "remove"; channel: string; credential: string; config: Config<Schemas["BusinessProviderRemoveRequest"]> };

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
      if (intent.operation !== "connection") {
        path += `/${encodeURIComponent(intent.channel)}`;
        switch (intent.operation) {
          case "credential": path += "/credentials"; break;
          case "billing": path += "/billing-credential"; break;
          case "status":
          case "remove":
            path += `/credentials/${encodeURIComponent(intent.credential)}/${intent.operation}`;
            break;
        }
      }
      return api<Schemas["ProviderMutationResult"]>(path, {
        method: "POST", body: { idempotencyKey, reason, config: intent.config },
      });
    },
    onSuccess: () => client.invalidateQueries({ queryKey: ["biz-channels"] }),
  });
}
