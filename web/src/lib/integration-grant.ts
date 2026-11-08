import type { components } from "../api/schema";

type Scope = components["schemas"]["IntegrationScope"];
export type CredentialRequest = components["schemas"]["IntegrationCredentialRequest"];
export type GrantMode = "application" | "customers" | "provision";
export interface GrantDraft {
  mode: GrantMode;
  applicationId: string;
  customerIds: string[];
  scopes: Scope[];
  name: string;
  days: string;
  reason: string;
}

export const grantScopes: Scope[] = ["customersRead", "customersProvision", "meteringWrite", "billingRead", "customersLifecycleWrite", "customerKeysWrite"];

/** Freeze absolute expiry and authority once; retries send this exact object. */
export function credentialRequest(draft: GrantDraft, idempotencyKey: string, now: number): CredentialRequest {
  if (!/^[1-9][0-9]{0,2}$/.test(draft.days) || Number(draft.days) > 365) throw new Error("expiry");
  if (!draft.reason.trim() || draft.reason.trim().length > 500 || draft.name.trim().length > 80) throw new Error("details");
  const scopes = draft.mode === "provision" ? ["customersProvision" as const] : [...draft.scopes].sort();
  if (!scopes.length || new Set(scopes).size !== scopes.length || scopes.some((scope) => !grantScopes.includes(scope))) throw new Error("scopes");
  if (draft.mode !== "application" && (scopes.includes("customersLifecycleWrite") || scopes.includes("customerKeysWrite"))) throw new Error("scopes");
  const customerIds = draft.mode === "customers" ? [...new Set(draft.customerIds)].sort() : [];
  if (draft.mode === "customers" && (!customerIds.length || customerIds.length > 1000)) throw new Error("customers");
  if (draft.mode === "application" && !draft.applicationId) throw new Error("application");
  return {
    idempotencyKey, name: draft.name.trim() || null, scopes, customerIds,
    ...(draft.mode === "application" ? { applicationId: draft.applicationId } : {}),
    expiresAt: new Date(now + Number(draft.days) * 86_400_000).toISOString(),
    reason: draft.reason.trim(),
  };
}
