import { useEffect, useRef, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { useKeys, useCreateKey } from "../api/hooks";
import { Banner, Card, Field, TextInput } from "../components/ui";
import { Button } from "../components/ui";
import {
  SelectRoot,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui";
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "../components/ui";
import { CodeBlock } from "../components/ui";
import { Typewriter } from "../components/ui";
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
  const { t } = useTranslation();
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
      .catch(() => setError(t("chat.keyMintError")));
  }, [keys.data, t]);

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
      .catch(() => setError(t("chat.modelsError")));
  }, [secret, t]);

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
        setError(t("chat.gatewayError", { status: res.status }));
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
        setError(t("chat.streamError"));
      }
    } finally {
      setStreaming(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-bold">{t("chat.title")}</h1>
      <p className="text-sm text-muted">
        <Trans i18nKey="chat.intro" components={{ code: <code className="mono" /> }} />
      </p>
      {error ? <Banner tone="error">{error}</Banner> : null}
      {createKey.isPending || !secret ? (
        <p className="text-sm text-muted">{t("chat.mintingKey")}</p>
      ) : (
        <>
          <SelectRoot value={model} onValueChange={(v) => setModel(String(v))}>
            <SelectTrigger aria-label={t("chat.model")} className="w-72">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {models.map((m) => (
                <SelectItem key={m} value={m}>
                  {m}
                </SelectItem>
              ))}
            </SelectContent>
          </SelectRoot>
          <Tabs defaultValue="chat">
            <div className="flex items-center justify-between gap-3">
              <TabsList>
                <TabsTrigger value="chat">{t("chat.title")}</TabsTrigger>
                <TabsTrigger value="messages">{t("chat.messages")}</TabsTrigger>
                <TabsTrigger value="embeddings">{t("chat.embeddings")}</TabsTrigger>
                <TabsTrigger value="rerank">{t("chat.rerank")}</TabsTrigger>
              </TabsList>
              <p className="hidden text-xs text-muted sm:block">{t("chat.probeNote")}</p>
            </div>
            <TabsContent value="chat">
          <Card>
            <div className="flex min-h-64 flex-col gap-3">
              {messages.map((m, i) => (
                <div
                  key={i}
                  className={`rounded-lg px-3 py-2 text-sm whitespace-pre-wrap ${
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
                    texts={[t("chat.hint1"), t("chat.hint2"), t("chat.hint3")]}
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
              placeholder={t("chat.placeholder")}
              disabled={streaming}
            />
            <Button
              type="submit"
              isLoading={streaming}
              loadingText={t("chat.streaming")}
              disabled={!input.trim()}
            >
              {t("chat.send")}
            </Button>
          </form>
            </TabsContent>
            <TabsContent value="messages">
              <Probe
                path="/v1/messages"
                secret={secret}
                build={(fields) => ({
                  model,
                  messages: [{ role: "user", content: fields.text ?? "" }],
                })}
                fields={[{ name: "text", label: t("chat.userMessage"), kind: "textarea" }]}
              />
            </TabsContent>
            <TabsContent value="embeddings">
              <Probe
                path="/v1/embeddings"
                secret={secret}
                build={(fields) => ({ model, input: fields.text ?? "" })}
                fields={[{ name: "text", label: t("chat.inputText"), kind: "textarea" }]}
              />
            </TabsContent>
            <TabsContent value="rerank">
              <Probe
                path="/v1/rerank"
                secret={secret}
                build={(fields) => ({
                  model,
                  query: fields.query ?? "",
                  documents: (fields.documents ?? "")
                    .split("\n")
                    .map((d) => d.trim())
                    .filter(Boolean),
                })}
                fields={[
                  { name: "query", label: t("chat.query") },
                  { name: "documents", label: t("chat.documents"), kind: "textarea" },
                ]}
              />
            </TabsContent>
          </Tabs>
        </>
      )}
    </div>
  );
}

interface ProbeField {
  name: string;
  label: string;
  kind?: "text" | "textarea";
}

/** The non-chat gateway routes — a billed request the page POSTs against the
 *  same `chat` key, answered as pretty JSON. `x-oxsum-charged-minor` on the
 *  response headers shows what the turn actually settled for. */
function Probe(props: {
  path: string;
  secret: string;
  fields: ProbeField[];
  build: (fields: Record<string, string>) => Record<string, unknown>;
}) {
  const { t } = useTranslation();
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [charge, setCharge] = useState<string | null>(null);

  return (
    <Card title={props.path}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setResult(null);
          setCharge(null);
          try {
            const res = await fetch(props.path, {
              method: "POST",
              headers: {
                "Content-Type": "application/json",
                Authorization: `Bearer ${props.secret}`,
              },
              body: JSON.stringify(props.build(values)),
            });
            setCharge(res.headers.get("x-oxsum-charged-minor"));
            const text = await res.text();
            try {
              setResult(JSON.stringify(JSON.parse(text), null, 2));
            } catch {
              setResult(text);
            }
          } catch {
            setResult(t("chat.requestFailed"));
          } finally {
            setBusy(false);
          }
        }}
      >
        {props.fields.map((f) => (
          <Field key={f.name} label={f.label}>
            {f.kind === "textarea" ? (
              <textarea
                className="mono w-full rounded-lg border border-line bg-surface px-2.5 py-1.5 text-sm"
                rows={3}
                value={values[f.name] ?? ""}
                onChange={(e) => setValues((v) => ({ ...v, [f.name]: e.target.value }))}
                required
              />
            ) : (
              <TextInput
                value={values[f.name] ?? ""}
                onChange={(e) => setValues((v) => ({ ...v, [f.name]: e.target.value }))}
                required
              />
            )}
          </Field>
        ))}
        <div className="flex items-center gap-3">
          <Button type="submit" isLoading={busy} loadingText={t("chat.sending")}>
            {t("chat.send")}
          </Button>
          {charge != null ? (
            <p className="text-xs text-muted">
              {t("chat.chargedMinor")} <code className="mono">{charge}</code> minor
            </p>
          ) : null}
        </div>
        {result ? <CodeBlock code={result} language="json" /> : null}
      </form>
    </Card>
  );
}
