import { useBillingSocket } from "../api/ws";
import { credits } from "./ui";

/**
 * The live billing socket, shown in the topbar as a quiet status pill: a dot +
 * "billing live/offline" while connected, swelling with the last settlement
 * when a turn lands. Product chrome — no morphing island, no spring layout.
 */
export function LiveIsland() {
  const { turns, live } = useBillingSocket();
  const lastSettled = turns.find((t) => t.state === "settled" && t.chargedMinor !== undefined);
  const inFlight = turns.filter((t) => t.state !== "settled").length;

  return (
    <div
      className="flex h-9 w-48 flex-col items-start justify-center rounded-lg border border-line bg-canvas px-3"
      role="status"
      aria-live="polite"
    >
      <p className="flex items-center gap-1.5 text-xs font-medium text-ink">
        <span
          className={`inline-block h-1.5 w-1.5 rounded-full ${live ? "bg-success" : "bg-muted"}`}
        />
        {live ? "billing live" : "billing offline"}
      </p>
      <p className="mt-0.5 text-[11px] leading-none text-muted">
        {lastSettled
          ? `settled ${credits(lastSettled.chargedMinor!)} cr · ${lastSettled.kind ?? "usage"}`
          : inFlight > 0
            ? `${inFlight} in flight`
            : "idle"}
      </p>
    </div>
  );
}
