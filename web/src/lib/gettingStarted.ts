// The "Get started" checklist's derived state (issue #559). Every step reads the
// same server data its own screen reads — no local checkmarks, so a reload after
// adding the missing thing flips the step by itself. The page and the Overview
// card share this hook, which also means React Query serves both from one fetch.

import { useBalance, useKeys } from "../api/hooks";
import { useCustomerReceiptReadiness } from "../api/hooks/readiness";
import { receiptProofHref, receiptReadinessState } from "./receipt-readiness";
import { useAdmin, useAdminToken } from "../pages/admin/api";

/** One step's outcome. `blocked` means the read was skipped because the operator
 *  token is not stored in this browser — not that the step is unmet. */
export type StepState = "loading" | "done" | "todo" | "error" | "blocked";

export interface GettingStartedStep {
  key: "channel" | "credit" | "key" | "request" | "bill";
  state: StepState;
  /** The counts the page's copy names when a step is unmet. */
  detail?: { channels?: number; pricedChannels?: number };
}

export interface GettingStarted {
  steps: GettingStartedStep[];
  done: number;
  total: number;
  loading: boolean;
  ready: boolean;
  proofHref?: string;
}

interface AdminChannel {
  name: string;
  models: { model: string; version: number }[];
}

export function useGettingStarted(): GettingStarted {
  const [adminToken] = useAdminToken();
  const channels = useAdmin<AdminChannel[]>(adminToken ?? "", "/channels", [], Boolean(adminToken));
  const balance = useBalance();
  const keys = useKeys();
  const receipt = useCustomerReceiptReadiness();
  const receiptState = receiptReadinessState({ ...receipt, error: receipt.error as { status?: number } | null });

  let channelStep: GettingStartedStep;
  if (!adminToken) {
    channelStep = { key: "channel", state: "blocked" };
  } else if (channels.loading) {
    channelStep = { key: "channel", state: "loading" };
  } else if (channels.error) {
    channelStep = { key: "channel", state: "error" };
  } else {
    const all = channels.data ?? [];
    const priced = all.filter((channel) => channel.models.length > 0);
    channelStep = {
      key: "channel",
      state: priced.length > 0 ? "done" : "todo",
      detail: { channels: all.length, pricedChannels: priced.length },
    };
  }

  const steps: GettingStartedStep[] = [
    channelStep,
    {
      key: "credit",
      state: balance.isPending
        ? "loading"
        : balance.isError
          ? "error"
          : (balance.data?.availableMinor ?? 0) > 0
            ? "done"
            : "todo",
    },
    {
      key: "key",
      state: keys.isPending
        ? "loading"
        : keys.isError
          ? "error"
          : (keys.data?.length ?? 0) > 0
            ? "done"
            : "todo",
    },
    { key: "request", state: receiptState },
    { key: "bill", state: receiptState },
  ];

  const done = steps.filter((step) => step.state === "done").length;
  return {
    steps,
    done,
    total: steps.length,
    loading: steps.some((step) => step.state === "loading"),
    ready: done === steps.length,
    proofHref: receiptProofHref(receipt.data),
  };
}
