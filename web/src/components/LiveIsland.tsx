import { useEffect, useRef } from "react";
import {
  DynamicDescription,
  DynamicIsland,
  DynamicIslandProvider,
  DynamicTitle,
  useDynamicIslandSize,
} from "./cult/dynamic-island";
import { useBillingSocket } from "../api/ws";
import { credits } from "./ui";

/**
 * The live billing socket as a dynamic island in the shell's top bar: quiet and
 * narrow while the socket is up, swelling to show the last settlement when a
 * turn lands, dark when the socket drops.
 */
function IslandBody() {
  const { turns, live } = useBillingSocket();
  const { setSize } = useDynamicIslandSize();
  const lastSettled = turns.find((t) => t.state === "settled" && t.chargedMinor !== undefined);
  const seenRef = useRef<string | null>(null);

  useEffect(() => {
    if (lastSettled && seenRef.current !== lastSettled.requestId) {
      seenRef.current = lastSettled.requestId;
      setSize("compactLong");
      const timer = setTimeout(() => setSize(live ? "compact" : "default"), 4000);
      return () => clearTimeout(timer);
    }
    setSize(live ? "compact" : "default");
  }, [lastSettled, live, setSize]);

  return (
    <div className="flex w-full flex-col items-start justify-center px-4">
      <DynamicTitle className="flex items-center gap-2 text-xs font-medium">
        <span
          className={`inline-block h-2 w-2 rounded-full ${live ? "bg-green-400" : "bg-zinc-500"}`}
        />
        {live ? "billing live" : "billing offline"}
      </DynamicTitle>
      <DynamicDescription className="text-xs text-zinc-400">
        {lastSettled
          ? `settled ${credits(lastSettled.chargedMinor!)} cr · ${lastSettled.kind ?? "usage"}`
          : `${turns.filter((t) => t.state !== "settled").length} in flight`}
      </DynamicDescription>
    </div>
  );
}

export function LiveIsland() {
  return (
    <DynamicIslandProvider initialSize="compact">
      <DynamicIsland id="billing-live">
        <IslandBody />
      </DynamicIsland>
    </DynamicIslandProvider>
  );
}
