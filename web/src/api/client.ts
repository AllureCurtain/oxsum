// The API envelope: `{ "data": … }` on success, `{ "error": { code, message } }`
// otherwise — the exact wire shape docs/api.md pins.

export class ApiError extends Error {
  constructor(
    public readonly code: string,
    message: string,
    public readonly status: number,
  ) {
    super(message);
  }
}

type Json = Record<string, unknown> | unknown[] | string | number | boolean | null;

export interface ApiInit extends Omit<RequestInit, "body"> {
  body?: Json;
}

export async function api<T>(path: string, init?: ApiInit): Promise<T> {
  const { body, ...rest } = init ?? {};
  const res = await fetch(`/api/v1${path}`, {
    credentials: "same-origin",
    headers: body !== undefined ? { "content-type": "application/json" } : undefined,
    ...rest,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  const parsed: unknown = text === "" ? null : JSON.parse(text);
  const envelope = parsed as {
    data?: T;
    error?: { code?: string; message?: string };
  } | null;
  if (!res.ok) {
    throw new ApiError(
      envelope?.error?.code ?? "HTTP_" + res.status,
      envelope?.error?.message ?? res.statusText,
      res.status,
    );
  }
  return envelope?.data as T;
}
