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

// Reconnect policy: bounded exponential backoff with full jitter —
// 1s → 2s → 4s … capped at 30s, each attempt rolled in [0, delay] so a
// flapping server doesn't get a thundering-herd reconnect wave. After
// `DEAD_AFTER` consecutive failures the hook surfaces `dead` so the UI can
// say "connection lost" rather than silently retrying forever; a manual
// `reconnect()` (or the next successful open) clears it.
const BACKOFF_MIN_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;
const DEAD_AFTER = 8;

export function useBillingSocket(enabled = true) {
  const [turns, setTurns] = useState<LiveTurn[]>([]);
  const [live, setLive] = useState(false);
  const [dead, setDead] = useState(false);
  const client = useQueryClient();
  const clientRef = useRef(client);
  clientRef.current = client;
  // Bumping this forces the effect to re-run and reopen the socket — the
  // manual reconnect path.
  const [attempt, setAttempt] = useState(0);
  const reconnect = () => setAttempt((a) => a + 1);

  useEffect(() => {
    if (!enabled) {
      return;
    }
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    const url = `${proto}://${window.location.host}/ws/billing`;

    let socket: WebSocket | null = null;
    let retryTimer: number | null = null;
    let failures = 0;
    let disposed = false;

    const apply = (message: Wire) => {
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

    const connect = () => {
      if (disposed) return;
      socket = new WebSocket(url);
      socket.onopen = () => {
        failures = 0;
        setDead(false);
        setLive(true);
        // The server pushes a fresh `snapshot` on open, so a reopened socket
        // re-seeds `turns` with the current in-flight set — no extra handshake.
      };
      socket.onmessage = (frame) => {
        let message: Wire;
        try {
          message = JSON.parse(frame.data as string) as Wire;
        } catch {
          return;
        }
        apply(message);
      };
      socket.onerror = () => {
        // onerror always precedes onclose for a failed socket; the close path
        // owns state + retry so we don't double-count.
      };
      socket.onclose = () => {
        setLive(false);
        socket = null;
        if (disposed) return;
        failures += 1;
        if (failures >= DEAD_AFTER) {
          setDead(true);
          return; // sustained failure — stop, surface it, wait for a manual reconnect
        }
        const exp = Math.min(BACKOFF_MAX_MS, BACKOFF_MIN_MS * 2 ** (failures - 1));
        const delay = Math.floor(Math.random() * exp); // full jitter in [0, exp)
        retryTimer = window.setTimeout(connect, delay);
      };
    };

    connect();

    return () => {
      disposed = true;
      if (retryTimer !== null) {
        window.clearTimeout(retryTimer);
      }
      // Closing an OPEN/CONNECTING socket fires onclose; `disposed` gates the
      // retry there. Swallow the close event entirely.
      if (socket) {
        socket.onclose = null;
        socket.close();
      }
    };
  }, [enabled, attempt]);

  return { turns, live, dead, reconnect };
}
