import { useEffect, useRef, useState } from "react";
import { useKeys, useCreateKey } from "../api/hooks";
import { Banner, Card, TextInput } from "../components/ui";
import { HaloButton } from "../components/cult/halo-button";
import {
  HaloSelect,
  HaloSelectContent,
  HaloSelectItem,
  HaloSelectTrigger,
  HaloSelectValue,
} from "../components/cult/halo-select";
import { Typewriter } from "../components/cult/typewriter";
import { api } from "../api/client";

// The chat page, moved from the server-rendered `/dashboard/chat`: the browser
// mints a `chat`-named API key once and keeps its secret in localStorage (the
// same trust model — the secret never leaves the browser), then streams
// `/v1/chat/completions` by hand over the gateway's OpenAI-shaped SSE.
const KEY_STORAGE = "oxsum.chat-key";

interface Message {
  role: "user" | "assistant";
  text: string;
}

async function ensureChatKey(existing: ReturnType<typeof useKeys>["data"]): Promise<string> {
  const stored = localStorage.getItem(KEY_STORAGE);
  if (stored && existing?.some((k) => k.name === "chat" && !k.revokedAt)) {
    return stored;
  }
  const created = await api<{ secret: string }>("/org/keys", {
    method: "POST",
    body: { name: "chat" },
  });
  localStorage.setItem(KEY_STORAGE, created.secret);
  return created.secret;
}

export function Chat() {
  const keys = useKeys();
  const createKey = useCreateKey();
  const [secret, setSecret] = useState<string | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState("");
  const [input, setInput] = useState("");
  const [messages, setMessages] = useState<Message[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const abort = useRef<AbortController | null>(null);

  useEffect(() => {
    if (!keys.data) {
      return;
    }
    ensureChatKey(keys.data)
      .then(setSecret)
      .catch(() => setError("the chat key could not be minted"));
  }, [keys.data]);

  useEffect(() => {
    if (!secret) {
      return;
    }
    fetch("/v1/models", { headers: { Authorization: `Bearer ${secret}` } })
      .then((r) => r.json())
      .then((data) => {
        const ids = (data?.data ?? data ?? [])
          .map((m: { id?: string }) => m.id)
          .filter(Boolean) as string[];
        setModels(ids);
        if (ids[0]) {
          setModel(ids[0]);
        }
      })
      .catch(() => setError("the model list could not be read"));
  }, [secret]);

  async function send() {
    if (!secret || !input.trim() || streaming) {
      return;
    }
    const next = [...messages, { role: "user" as const, text: input.trim() }];
    setMessages(next);
    setInput("");
    setStreaming(true);
    setError(null);
    abort.current = new AbortController();
    try {
      const res = await fetch("/v1/chat/completions", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${secret}`,
        },
        body: JSON.stringify({
          model,
          stream: true,
          messages: next.map((m) => ({ role: m.role, content: m.text })),
        }),
        signal: abort.current.signal,
      });
      if (!res.ok || !res.body) {
        setError(`the gateway answered ${res.status}`);
        setStreaming(false);
        return;
      }
      setMessages([...next, { role: "assistant", text: "" }]);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) {
          break;
        }
        buffer += decoder.decode(value, { stream: true });
        const events = buffer.split("\n\n");
        buffer = events.pop() ?? "";
        for (const frame of events) {
          for (const line of frame.split("\n")) {
            if (!line.startsWith("data:")) {
              continue;
            }
            const data = line.slice(5).trim();
            if (data === "[DONE]") {
              continue;
            }
            try {
              const delta = JSON.parse(data).choices?.[0]?.delta?.content;
              if (typeof delta === "string") {
                setMessages((current) => {
                  const copy = [...current];
                  copy[copy.length - 1] = {
                    role: "assistant",
                    text: copy[copy.length - 1].text + delta,
                  };
                  return copy;
                });
              }
            } catch {
              // a partial JSON line waits for the rest of its bytes
            }
          }
        }
      }
    } catch {
      if (!abort.current?.signal.aborted) {
        setError("the stream failed");
      }
    } finally {
      setStreaming(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-bold">Chat</h1>
      <p className="text-sm text-muted">
        Streams against the gateway with the <code className="mono">chat</code> key this browser
        minted — every turn is a billed request on the organization's wallet.
      </p>
      {error ? <Banner tone="error">{error}</Banner> : null}
      {createKey.isPending || !secret ? (
        <p className="text-sm text-muted">Minting the chat key…</p>
      ) : (
        <>
          <HaloSelect value={model} onValueChange={(v) => setModel(String(v))} className="w-72">
            <HaloSelectTrigger aria-label="model">
              <HaloSelectValue />
            </HaloSelectTrigger>
            <HaloSelectContent>
              {models.map((m) => (
                <HaloSelectItem key={m} value={m}>
                  {m}
                </HaloSelectItem>
              ))}
            </HaloSelectContent>
          </HaloSelect>
          <Card>
            <div className="flex min-h-64 flex-col gap-3">
              {messages.map((m, i) => (
                <div
                  key={i}
                  className={`rounded-md px-3 py-2 text-sm whitespace-pre-wrap ${
                    m.role === "user" ? "ml-8 bg-primary-soft" : "mr-8 bg-canvas"
                  }`}
                >
                  <p className="mb-1 text-xs font-semibold text-muted">{m.role}</p>
                  {m.text || "…"}
                </div>
              ))}
              {messages.length === 0 ? (
                <p className="text-sm text-muted">
                  <Typewriter
                    delay={80}
                    texts={[
                      "Ask anything — every turn is a billed request.",
                      "The wallet freezes an upper bound before the stream starts…",
                      "…and settles against real usage after it ends.",
                    ]}
                  />
                </p>
              ) : null}
            </div>
          </Card>
          <form
            className="flex gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              send();
            }}
          >
            <TextInput
              className="flex-1"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              placeholder="Say something…"
              disabled={streaming}
            />
            <HaloButton
              type="submit"
              isLoading={streaming}
              loadingText="Streaming…"
              disabled={!input.trim()}
            >
              Send
            </HaloButton>
          </form>
        </>
      )}
    </div>
  );
}
