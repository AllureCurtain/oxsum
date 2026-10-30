import { useMemo, useState } from "react";
import { KeyRound } from "lucide-react";
import type { ApiKey } from "../api/hooks";
import type { DisplayCurrency } from "../components/ui";
import { useBudget, useCreateKey, useDisplay, useKeys, usePatchKey, useRevokeKey } from "../api/hooks";
import {
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Field,
  NumberInput,
  PageState,
  Select,
  TableShell,
  TextInput,
} from "../components/ui";
import { ApiError } from "../api/client";
import { CopyButton } from "../components/ui";

const DURATIONS = ["daily", "weekly", "monthly"] as const;

export function Keys() {
  const keys = useKeys();
  const budget = useBudget();
  const display = useDisplay();
  const create = useCreateKey();
  const revoke = useRevokeKey();
  const [name, setName] = useState("");
  const [limit, setLimit] = useState("");
  const [duration, setDuration] = useState<string>("");
  const [rpm, setRpm] = useState("");
  const [tpm, setTpm] = useState("");
  const [maxHolds, setMaxHolds] = useState("");
  const [models, setModels] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);

  const committedByKey = useMemo(() => {
    const map = new Map<string, NonNullable<typeof budget.data>["keys"][number]>();
    for (const k of budget.data?.keys ?? []) {
      map.set(k.keyId, k);
    }
    return map;
  }, [budget.data]);

  function submit() {
    const spendLimitMinor = limit === "" ? null : Math.round(Number(limit) * 1_000_000);
    create.mutate(
      {
        name: name.trim() || undefined,
        expiresAt: expiresAt ? new Date(expiresAt).toISOString() : null,
        spendLimitMinor,
        budgetDuration: (duration || null) as "daily" | "weekly" | "monthly" | null,
        requestsPerMinute: rpm === "" ? null : Number(rpm),
        tokensPerMinute: tpm === "" ? null : Number(tpm),
        maxConcurrentHolds: maxHolds === "" ? null : Number(maxHolds),
        modelAllowlist: models.trim() ? models.split(",").map((m) => m.trim()) : null,
      },
      {
        onSuccess: (created) => {
          setSecret(created.secret);
          setName("");
          setLimit("");
          setDuration("");
          setRpm("");
          setTpm("");
          setMaxHolds("");
          setModels("");
          setExpiresAt("");
        },
      },
    );
  }

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">API keys</h1>

      {secret ? (
        <Banner tone="success">
          The key was created. The secret is shown here and nowhere else:
          <code className="mono ml-2">{secret}</code>
          <CopyButton value={secret} aria-label="Copy key secret" />
          <Button tone="ghost" className="ml-3" onClick={() => setSecret(null)}>
            Done
          </Button>
        </Banner>
      ) : null}
      {create.error instanceof ApiError ? <Banner tone="error">{create.error.message}</Banner> : null}

      <Card title="Create a key">
        <form
          className="flex flex-col gap-5"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          {/* Grouped, not a flat 8-field grid — identity, then the budget,
              then the rate limits. */}
          <section className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <p className="col-span-full text-xs font-semibold uppercase tracking-wider text-muted">
              Identity
            </p>
            <Field label="Name">
              <TextInput value={name} onChange={(e) => setName(e.target.value)} maxLength={80} />
            </Field>
            <Field label="Expires at">
              <TextInput type="datetime-local" value={expiresAt} onChange={(e) => setExpiresAt(e.target.value)} />
            </Field>
          </section>

          <section className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <p className="col-span-full text-xs font-semibold uppercase tracking-wider text-muted">
              Budget
            </p>
            <Field label="Spend limit (credits)" hint="empty: unlimited">
              <NumberInput value={limit} min={0} onChange={(e) => setLimit(e.target.value)} />
            </Field>
            <Field label="Budget window" hint="makes the limit periodic (UTC)">
              <Select value={duration} onChange={(e) => setDuration(e.target.value)}>
                <option value="">cumulative</option>
                {DURATIONS.map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))}
              </Select>
            </Field>
          </section>

          <section className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <p className="col-span-full text-xs font-semibold uppercase tracking-wider text-muted">
              Rate limits
            </p>
            <Field label="Requests per minute">
              <NumberInput value={rpm} min={1} onChange={(e) => setRpm(e.target.value)} />
            </Field>
            <Field label="Tokens per minute">
              <NumberInput value={tpm} min={1} onChange={(e) => setTpm(e.target.value)} />
            </Field>
            <Field label="Max concurrent holds">
              <NumberInput value={maxHolds} min={1} onChange={(e) => setMaxHolds(e.target.value)} />
            </Field>
            <Field label="Model allowlist" hint="comma-separated gateway model names">
              <TextInput value={models} onChange={(e) => setModels(e.target.value)} />
            </Field>
          </section>

          <div>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? "Creating…" : "Create key"}
            </Button>
          </div>
        </form>
      </Card>

      {keys.isError ? (
        <PageState>The keys could not be read.</PageState>
      ) : (
        <TableShell
          head={["Name", "Prefix", "Budget", "Limits", "Created", ""]}
          loading={keys.isLoading ? 4 : 0}
        >
          {!keys.isLoading &&
            (keys.data ?? []).map((key) => (
              <KeyRow
                key={key.id}
                apiKey={key}
                committed={committedByKey.get(key.id)}
                display={display}
                editing={editing === key.id}
                onEdit={() => setEditing(editing === key.id ? null : key.id)}
                onRevoke={() => revoke.mutate(key.id)}
              />
            ))}
          {!keys.isLoading && (keys.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={6} icon={<KeyRound />}>
              No keys yet — create one above to start calling the gateway.
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}

function KeyRow(props: {
  apiKey: ApiKey;
  committed?: { committedMinor: number; consumedPercent?: number | null; windowStart?: string | null };
  display?: DisplayCurrency | null;
  editing: boolean;
  onEdit: () => void;
  onRevoke: () => void;
}) {
  const key = props.apiKey;
  const patch = usePatchKey();
  const [limit, setLimit] = useState(
    key.spendLimitMinor != null ? String(key.spendLimitMinor / 1_000_000) : "",
  );
  const [duration, setDuration] = useState(key.budgetDuration ?? "");
  const [rpm, setRpm] = useState(key.requestsPerMinute?.toString() ?? "");
  const [tpm, setTpm] = useState(key.tokensPerMinute?.toString() ?? "");
  const [maxHolds, setMaxHolds] = useState(key.maxConcurrentHolds?.toString() ?? "");
  const [models, setModels] = useState((key.modelAllowlist ?? []).join(", "));

  return (
    <>
      <tr>
        <td>{key.name ?? <span className="text-muted">unnamed</span>}</td>
        <td className="mono">{key.prefix}</td>
        <td className="text-sm">
          {key.spendLimitMinor == null ? (
            <span className="text-muted">unlimited</span>
          ) : (
            <>
              {props.committed?.consumedPercent != null ? `${props.committed.consumedPercent}% of ` : ""}
              <Cr minor={key.spendLimitMinor} currency={props.display} />
              {key.budgetDuration ? ` / ${key.budgetDuration}` : ""}
              {props.committed?.windowStart ? (
                <span className="text-muted"> · since {props.committed.windowStart}</span>
              ) : null}
            </>
          )}
        </td>
        <td className="text-xs text-muted">
          {[
            key.requestsPerMinute != null ? `${key.requestsPerMinute} rpm` : null,
            key.tokensPerMinute != null ? `${key.tokensPerMinute} tpm` : null,
            key.maxConcurrentHolds != null ? `${key.maxConcurrentHolds} holds` : null,
            key.modelAllowlist?.length ? `${key.modelAllowlist.length} models` : null,
          ]
            .filter(Boolean)
            .join(" · ") || "—"}
        </td>
        <td>{key.createdAt.slice(0, 10)}</td>
        <td className="text-right">
          {key.revokedAt ? (
            <span className="text-xs text-danger">revoked</span>
          ) : (
            <span className="inline-flex gap-2">
              <Button tone="ghost" onClick={props.onEdit}>
                Constraints
              </Button>
              <Button tone="danger" onClick={props.onRevoke}>
                Revoke
              </Button>
            </span>
          )}
        </td>
      </tr>
      {props.editing ? (
        <tr>
          <td colSpan={6} className="bg-canvas">
            <form
              className="grid grid-cols-1 gap-3 p-3 sm:grid-cols-3"
              aria-label={`constraints for ${key.name ?? key.prefix}`}
              onSubmit={(e) => {
                e.preventDefault();
                patch.mutate({
                  keyId: key.id,
                  spendLimitMinor: limit === "" ? null : Math.round(Number(limit) * 1_000_000),
                  budgetDuration: (duration || null) as "daily" | "weekly" | "monthly" | null,
                  requestsPerMinute: rpm === "" ? null : Number(rpm),
                  tokensPerMinute: tpm === "" ? null : Number(tpm),
                  maxConcurrentHolds: maxHolds === "" ? null : Number(maxHolds),
                  modelAllowlist: models.trim() ? models.split(",").map((m) => m.trim()) : null,
                });
              }}
            >
              <p className="col-span-full -mb-1 text-xs font-semibold uppercase tracking-wider text-muted">
                Constraints · {key.name ?? key.prefix}
              </p>
              <Field label="Spend limit (credits)">
                <NumberInput value={limit} min={0} onChange={(e) => setLimit(e.target.value)} />
              </Field>
              <Field label="Budget window">
                <Select value={duration} onChange={(e) => setDuration(e.target.value)}>
                  <option value="">cumulative</option>
                  {DURATIONS.map((d) => (
                    <option key={d} value={d}>
                      {d}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label="Requests per minute">
                <NumberInput value={rpm} min={1} onChange={(e) => setRpm(e.target.value)} />
              </Field>
              <Field label="Tokens per minute">
                <NumberInput value={tpm} min={1} onChange={(e) => setTpm(e.target.value)} />
              </Field>
              <Field label="Max concurrent holds">
                <NumberInput value={maxHolds} min={1} onChange={(e) => setMaxHolds(e.target.value)} />
              </Field>
              <Field label="Model allowlist">
                <TextInput value={models} onChange={(e) => setModels(e.target.value)} />
              </Field>
              <div className="flex items-end gap-2">
                <Button type="submit" disabled={patch.isPending}>
                  {patch.isPending ? "Saving…" : "Save"}
                </Button>
                {patch.error instanceof ApiError ? (
                  <span className="text-xs text-danger">{patch.error.message}</span>
                ) : null}
              </div>
            </form>
          </td>
        </tr>
      ) : null}
    </>
  );
}
