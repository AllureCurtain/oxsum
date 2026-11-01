import type { components } from "../schema";

// The response types come from the generated schema — `npm run codegen`
// keeps `api/schema.d.ts` authoritative over `openapi.yaml`, so a hook's
// return type can never drift from the contract. The four types with no
// named schema (Balance, SignedHead, WitnessCheckpoint, PriceEstimate)
// stay hand-typed at the bottom of this block.

export type SessionInfo = components["schemas"]["SessionInfo"];
export type ApiKey = components["schemas"]["ApiKey"];
export type Entitlement = components["schemas"]["Entitlement"];
export type UsageDay = components["schemas"]["UsageDay"];
export type Member = components["schemas"]["Member"];
export type WebhookEndpoint = components["schemas"]["WebhookEndpoint"];
export type WebhookDelivery = components["schemas"]["WebhookDelivery"];
export type BudgetSnapshot = components["schemas"]["BudgetSnapshot"];
export type BillingRecord = components["schemas"]["BillingRecord"];
export type RequestLogRow = components["schemas"]["RequestLogRow"];
export type Statement = components["schemas"]["Statement"];
export type StatementLine = components["schemas"]["StatementLine"];
export type UserOrganization = components["schemas"]["UserOrganization"];
export type AuthMethods = components["schemas"]["AuthMethods"];
export type TotpRequired = components["schemas"]["TotpRequired"];
export type TotpLoginRequest = components["schemas"]["TotpLoginRequest"];
export type TotpCodeRequest = components["schemas"]["TotpCodeRequest"];
export type TotpEnrollResponse = components["schemas"]["TotpEnrollResponse"];
export type TotpConfirmResponse = components["schemas"]["TotpConfirmResponse"];
export type Receipt = components["schemas"]["Receipt"];
export type BillRow = components["schemas"]["BillRow"];
export type VerifyingKey = components["schemas"]["VerifyingKey"];
export type DisplayCurrencyType = components["schemas"]["DisplayCurrency"];

/** Renamed to the schema's names — `OpenHold`/`LogEntryRow`/`PricedModel`
 * were the scaffold's hand labels; the wire types are `OpenHoldView`,
 * `LogEntryView` and `PricedModel` (which is `ModelPrice & {channel,protocol}`,
 * the pricing page's row). */
export type OpenHold = components["schemas"]["OpenHoldView"];
export type LogEntryRow = components["schemas"]["LogEntryView"];
export type PricedModel = components["schemas"]["PricedModel"];

// No named schema — these endpoints return an inline `data` shape the
// codegen doesn't lift into components["schemas"], so they stay hand-typed.
export interface Balance {
  // no schema counterpart — the `/balance` response is an inline object
  availableMinor: number;
  creditLimitMinor: number;
  creditUsedMinor: number;
  reservedMinor: number;
  spentThisMonthMinor: number;
}

export interface SignedHead {
  // no schema counterpart — `/log/head` and `/log/price-head` return inline shapes
  note: string;
  origin: string;
  size: number;
  root: string;
  keyName: string;
  publicKey: string;
  keyHash: string;
}

export interface WitnessCheckpoint {
  // no schema counterpart — `/log/checkpoint` returns an inline shape
  headSize: number;
  note: string;
  cosignatures: { witnessUrl: string; note: string; firstSeen: string }[];
  /** The deployment's pinned witness keyset (issue #259): which cosignature/v1
   * key a signature line under each URL must verify against. */
  witnessKeys: { witnessUrl: string; keyName: string; publicKey: string; keyHash: string }[];
  updatedAt: string;
}

export interface PriceEstimate {
  // no schema counterpart — `/estimate-price` returns an inline shape
  estimateMinor: number;
  model: string;
  version: number;
}
