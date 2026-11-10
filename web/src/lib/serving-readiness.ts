import type { components } from "../api/schema";

export type ServingReadiness = components["schemas"]["CustomerServingReadiness"];
export type ServingStepState = "loading" | "done" | "todo" | "error" | "blocked";

export function servingReadinessStates(query: {
  isPending: boolean;
  isError: boolean;
  error?: { status?: number } | null;
  data?: ServingReadiness;
}): { channel: ServingStepState; credit: ServingStepState; key: ServingStepState } {
  const uniform = (state: ServingStepState) => ({ channel: state, credit: state, key: state });
  if (query.isPending) return uniform("loading");
  if (query.isError) return uniform(query.error?.status === 403 ? "blocked" : "error");
  if (!query.data) return uniform("error");
  if (query.data.reason === "restricted") return uniform("blocked");
  return {
    channel: query.data.modelReady && query.data.sample ? "done" : "todo",
    credit: query.data.fundingReady && query.data.sample && query.data.reason === null ? "done" : "todo",
    key: query.data.keyReady ? "done" : "todo",
  };
}
