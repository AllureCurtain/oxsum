// Personal customer setup facts share one funding snapshot and scoped receipt evidence.
import { useCustomerReceiptReadiness, useCustomerServingReadiness } from "../api/hooks/readiness";
import { receiptProofHref, receiptReadinessState } from "./receipt-readiness";
import { servingReadinessStates, type ServingReadiness } from "./serving-readiness";

export type StepState = "loading" | "done" | "todo" | "error" | "blocked";

export interface GettingStartedStep {
  key: "channel" | "credit" | "key" | "request" | "bill";
  state: StepState;
}
export interface GettingStarted {
  steps: GettingStartedStep[];
  done: number;
  total: number;
  loading: boolean;
  ready: boolean;
  proofHref?: string;
  serving?: ServingReadiness;
}

export function useGettingStarted(): GettingStarted {
  const serving = useCustomerServingReadiness();
  const states = servingReadinessStates({ ...serving, error: serving.error as { status?: number } | null });
  const receipt = useCustomerReceiptReadiness();
  const receiptState = receiptReadinessState({ ...receipt, error: receipt.error as { status?: number } | null });
  const steps: GettingStartedStep[] = [
    { key: "channel", state: states.channel },
    { key: "credit", state: states.credit },
    { key: "key", state: states.key },
    { key: "request", state: receiptState },
    { key: "bill", state: receiptState },
  ];
  const done = steps.filter((step) => step.state === "done").length;
  return {
    steps, done, total: steps.length,
    loading: steps.some((step) => step.state === "loading"),
    ready: done === steps.length,
    proofHref: receiptProofHref(receipt.data),
    serving: serving.data,
  };
}
