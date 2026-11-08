import type { components } from "../api/schema";

type Schemas = components["schemas"];
export type MerchantRail = Schemas["MerchantRail"];
export type MerchantDraft = {
  apiUrl: string; apiKey: string; webhookSecret: string; clientId: string;
  publishableKey: string; sdkUrl: string; checkoutUrl: string;
  network: "public" | "internal"; allowedIps: string;
};

/** Redacted versions can seed public fields, never placeholder credentials. */
export function merchantDraft(version?: Schemas["MerchantVersion"]): MerchantDraft {
  return {
    apiUrl: version?.apiUrl ?? "", apiKey: "", webhookSecret: "", clientId: "",
    publishableKey: version?.publishableKey ?? "", sdkUrl: version?.sdkUrl ?? "",
    checkoutUrl: version?.checkoutUrl ?? "", network: version?.destinationPolicy.network ?? "public",
    allowedIps: version?.destinationPolicy.allowedIps?.join("\n") ?? "",
  };
}

/** Match the complete rail-specific contract without transmitting hidden fields. */
export function merchantConfig(rail: MerchantRail, draft: MerchantDraft): Schemas["MerchantVersionInput"] {
  return {
    apiUrl: draft.apiUrl.trim(), apiKey: draft.apiKey, webhookSecret: draft.webhookSecret,
    destinationPolicy: draft.network === "public" ? { network: "public" } : {
      network: "internal", allowedIps: draft.allowedIps.split(/[\s,]+/).filter(Boolean),
    },
    ...(rail === "hyperswitch" ? { publishableKey: draft.publishableKey.trim(), sdkUrl: draft.sdkUrl.trim() } : {}),
    ...(rail === "airwallex" ? { clientId: draft.clientId, checkoutUrl: draft.checkoutUrl.trim() } : {}),
  };
}
