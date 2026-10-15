import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";

// `/ws/billing` pushes the organization's live billing: a `snapshot` of
// in-flight holds first, then `turnStarted` / `turnProgress` / `turnSettled`
// as turns run. The requests page renders the socket's events beside the
// settled list; the overview's holds table listens too.
export interface LiveTurn {
  requestId: string;
  model: string;
  state: "started" | "streaming" | "settled";
  freezeMinor?: number;
  outputChars?: number;
  chargedMinor?: number;
  kind?: string;
}

type Wire =
  | { type: "snapshot"; holds: { requestId: string; model: string; freezeMinor: number }[] }
  | { type: "turnStarted"; requestId: string; model: string; freezeMinor: number }
  | { type: "turnProgress"; requestId: string; outputChars: number }
  | { type: "turnSettled"; requestId: string; chargedMinor: number; kind: string };

export function useBillingSocket(enabled = true) {
  const [turns, setTurns] = useState<LiveTurn[]>([]);
  const [live, setLive] = useState(false);
  const client = useQueryClient();
  const clientRef = useRef(client);
  clientRef.current = client;

  useEffect(() => {
    if (!enabled) {
      return;
    }
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    const socket = new WebSocket(`${proto}://${window.location.host}/ws/billing`);
    socket.onopen = () => setLive(true);
    socket.onclose = () => setLive(false);
    socket.onerror = () => setLive(false);
    socket.onmessage = (frame) => {
      let message: Wire;
      try {
        message = JSON.parse(frame.data as string) as Wire;
      } catch {
        return;
      }
      setTurns((current) => {
        const next = current.slice();
        switch (message.type) {
          case "snapshot":
            return message.holds.map((h) => ({
              requestId: h.requestId,
              model: h.model,
              state: "started" as const,
              freezeMinor: h.freezeMinor,
            }));
          case "turnStarted":
            next.unshift({
              requestId: message.requestId,
              model: message.model,
              state: "started",
              freezeMinor: message.freezeMinor,
            });
            break;
          case "turnProgress": {
            const i = next.findIndex((t) => t.requestId === message.requestId);
            if (i >= 0) {
              next[i] = { ...next[i], state: "streaming", outputChars: message.outputChars };
            }
            break;
          }
          case "turnSettled": {
            const i = next.findIndex((t) => t.requestId === message.requestId);
            if (i >= 0) {
              next[i] = {
                ...next[i],
                state: "settled",
                chargedMinor: message.chargedMinor,
                kind: message.kind,
              };
            }
            break;
          }
        }
        return next.slice(0, 50);
      });
      // A settled turn is the settled lists' signal to re-read: billing records,
      // balance and holds all move with it.
      if (message.type === "turnSettled") {
        clientRef.current.invalidateQueries({ queryKey: ["billing-records"] });
        clientRef.current.invalidateQueries({ queryKey: ["balance"] });
        clientRef.current.invalidateQueries({ queryKey: ["holds"] });
        clientRef.current.invalidateQueries({ queryKey: ["bills"] });
      }
    };
    return () => socket.close();
  }, [enabled]);

  return { turns, live };
}
