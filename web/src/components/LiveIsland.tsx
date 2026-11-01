import { useTranslation } from "react-i18next";
import { useBillingSocket } from "../api/ws";
import { credits } from "./ui";

/**
 * The live billing socket, shown in the topbar as a quiet status pill: a dot +
 * "billing live/offline" while connected, swelling with the last settlement
 * when a turn lands. A socket that has given up retrying (`dead`) flips the
 * pill into a reconnect control — clicking it forces a fresh attempt.
 */
export function LiveIsland() {
  const { t } = useTranslation();
  const { turns, live, dead, reconnect } = useBillingSocket();
  const lastSettled = turns.find((t) => t.state === "settled" && t.chargedMinor !== undefined);
  const inFlight = turns.filter((t) => t.state !== "settled").length;

  const dot = live ? "bg-success" : dead ? "bg-danger" : "bg-muted";
  const status = live ? t("live.billingLive") : dead ? t("live.connectionLost") : t("live.billingOffline");

  const detail = dead
    ? t("live.retry")
    : lastSettled
      ? t("live.settled", { amount: credits(lastSettled.chargedMinor!), kind: lastSettled.kind ?? "usage" })
      : inFlight > 0
        ? t("live.inFlight", { count: inFlight })
        : t("live.idle");

  const body = (
    <>
      <p className="flex items-center gap-1.5 text-xs font-medium text-ink">
        <span className={`inline-block h-1.5 w-1.5 rounded-full ${dot}`} />
        {status}
      </p>
      <p className={`mt-0.5 text-[11px] leading-none ${dead ? "text-primary" : "text-muted"}`}>
        {detail}
      </p>
    </>
  );

  return dead ? (
    <button
      type="button"
      onClick={reconnect}
      title={t("live.reconnect")}
      className="flex h-9 w-48 flex-col items-start justify-center rounded-lg border border-line bg-canvas px-3 hover:bg-canvas/60"
      role="status"
      aria-live="polite"
    >
      {body}
    </button>
  ) : (
    <div
      className="flex h-9 w-48 flex-col items-start justify-center rounded-lg border border-line bg-canvas px-3"
      role="status"
      aria-live="polite"
    >
      {body}
    </div>
  );
}
