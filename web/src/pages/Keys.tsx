import { useMemo, useState } from "react";
import { KeyRound } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import type { ApiKey } from "../api/hooks";
import type { DisplayCurrency } from "../components/ui";
import { useBudget, useCreateKey, useDisplay, useEntitlement, useKeys, usePatchKey, useRevokeKey } from "../api/hooks";
import {
  Badge,
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
  const { t } = useTranslation();
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
      <h1 className="text-2xl font-bold">{t("keys.title")}</h1>

      {secret ? (
        <Banner tone="success">
          {t("keys.createdNote")}
          <code className="mono ml-2">{secret}</code>
          <CopyButton value={secret} aria-label={t("keys.copySecret")} />
          <Button tone="ghost" className="ml-3" onClick={() => setSecret(null)}>
            {t("keys.done")}
          </Button>
        </Banner>
      ) : null}
      {create.error instanceof ApiError ? <Banner tone="error">{create.error.message}</Banner> : null}

      <Card title={t("keys.createKey")}>
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
              {t("keys.identity")}
            </p>
            <Field label={t("keys.name")}>
              <TextInput value={name} onChange={(e) => setName(e.target.value)} maxLength={80} />
            </Field>
            <Field label={t("keys.expiresAt")}>
              <TextInput type="datetime-local" value={expiresAt} onChange={(e) => setExpiresAt(e.target.value)} />
            </Field>
          </section>

          <section className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <p className="col-span-full text-xs font-semibold uppercase tracking-wider text-muted">
              {t("keys.budget")}
            </p>
            <Field label={t("keys.spendLimit")} hint={t("keys.unlimitedHint")}>
              <NumberInput value={limit} min={0} onChange={(e) => setLimit(e.target.value)} />
            </Field>
            <Field label={t("keys.budgetWindow")} hint={t("keys.budgetWindowHint")}>
              <Select value={duration} onChange={(e) => setDuration(e.target.value)}>
                <option value="">{t("keys.cumulative")}</option>
                {DURATIONS.map((d) => (
                  <option key={d} value={d}>
                    {t(`keys.duration.${d}`)}
                  </option>
                ))}
              </Select>
            </Field>
          </section>

          <section className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <p className="col-span-full text-xs font-semibold uppercase tracking-wider text-muted">
              {t("keys.rateLimits")}
            </p>
            <Field label={t("keys.requestsPerMinute")}>
              <NumberInput value={rpm} min={1} onChange={(e) => setRpm(e.target.value)} />
            </Field>
            <Field label={t("keys.tokensPerMinute")}>
              <NumberInput value={tpm} min={1} onChange={(e) => setTpm(e.target.value)} />
            </Field>
            <Field label={t("keys.maxConcurrentHolds")}>
              <NumberInput value={maxHolds} min={1} onChange={(e) => setMaxHolds(e.target.value)} />
            </Field>
            <Field label={t("keys.modelAllowlist")} hint={t("keys.modelAllowlistHint")}>
              <TextInput value={models} onChange={(e) => setModels(e.target.value)} />
            </Field>
          </section>

          <div>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? t("common.creating") : t("keys.createKeyButton")}
            </Button>
          </div>
        </form>
      </Card>

      {keys.isError ? (
        <PageState>{t("keys.readError")}</PageState>
      ) : (
        <TableShell
          head={[
            t("common.name"),
            t("keys.colPrefix"),
            t("keys.colLive"),
            t("keys.colBudget"),
            t("keys.colLimits"),
            t("keys.colCreated"),
            "",
          ]}
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
                t={t}
              />
            ))}
          {!keys.isLoading && (keys.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7} icon={<KeyRound />}>
              {t("keys.empty")}
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
  t: TFunction;
}) {
  const { t } = props;
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
        <td>{key.name ?? <span className="text-muted">{t("keys.unnamed")}</span>}</td>
        <td className="mono">{key.prefix}</td>
        <td>
          <LiveCell keyId={key.id} revoked={key.revokedAt != null} t={t} />
        </td>
        <td className="text-sm">
          {key.spendLimitMinor == null ? (
            <span className="text-muted">{t("keys.unlimited")}</span>
          ) : (
            <>
              {props.committed?.consumedPercent != null
                ? t("keys.percentOf", { percent: props.committed.consumedPercent })
                : ""}
              <Cr minor={key.spendLimitMinor} currency={props.display} />
              {key.budgetDuration ? ` / ${t(`keys.duration.${key.budgetDuration}`)}` : ""}
              {props.committed?.windowStart ? (
                <span className="text-muted">
                  {" "}
                  {t("keys.sinceWindow", { start: props.committed.windowStart })}
                </span>
              ) : null}
            </>
          )}
        </td>
        <td className="text-xs text-muted">
          {[
            key.requestsPerMinute != null ? `${key.requestsPerMinute} rpm` : null,
            key.tokensPerMinute != null ? `${key.tokensPerMinute} tpm` : null,
            key.maxConcurrentHolds != null
              ? t("keys.holdsShort", { count: key.maxConcurrentHolds })
              : null,
            key.modelAllowlist?.length
              ? t("keys.modelsShort", { count: key.modelAllowlist.length })
              : null,
          ]
            .filter(Boolean)
            .join(" · ") || "—"}
        </td>
        <td>{key.createdAt.slice(0, 10)}</td>
        <td className="text-right">
          {key.revokedAt ? (
            <span className="text-xs text-danger">{t("keys.revoked")}</span>
          ) : (
            <span className="inline-flex gap-2">
              <Button tone="ghost" onClick={props.onEdit}>
                {t("keys.constraints")}
              </Button>
              <Button tone="danger" onClick={props.onRevoke}>
                {t("keys.revoke")}
              </Button>
            </span>
          )}
        </td>
      </tr>
      {props.editing ? (
        <tr>
          <td colSpan={7} className="bg-canvas">
            <form
              className="grid grid-cols-1 gap-3 p-3 sm:grid-cols-3"
              aria-label={t("keys.constraintsFor", { name: key.name ?? key.prefix })}
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
                {t("keys.constraintsHeading", { name: key.name ?? key.prefix })}
              </p>
              <Field label={t("keys.spendLimit")}>
                <NumberInput value={limit} min={0} onChange={(e) => setLimit(e.target.value)} />
              </Field>
              <Field label={t("keys.budgetWindow")}>
                <Select value={duration} onChange={(e) => setDuration(e.target.value)}>
                  <option value="">{t("keys.cumulative")}</option>
                  {DURATIONS.map((d) => (
                    <option key={d} value={d}>
                      {t(`keys.duration.${d}`)}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label={t("keys.requestsPerMinute")}>
                <NumberInput value={rpm} min={1} onChange={(e) => setRpm(e.target.value)} />
              </Field>
              <Field label={t("keys.tokensPerMinute")}>
                <NumberInput value={tpm} min={1} onChange={(e) => setTpm(e.target.value)} />
              </Field>
              <Field label={t("keys.maxConcurrentHolds")}>
                <NumberInput value={maxHolds} min={1} onChange={(e) => setMaxHolds(e.target.value)} />
              </Field>
              <Field label={t("keys.modelAllowlist")}>
                <TextInput value={models} onChange={(e) => setModels(e.target.value)} />
              </Field>
              <div className="flex items-end gap-2">
                <Button type="submit" disabled={patch.isPending}>
                  {patch.isPending ? t("common.saving") : t("common.save")}
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

/** The live-availability cell (issue #501): the entitlement endpoint's own
 * answer, refreshed on a slow poll — a revoked key's answer is fixed, so it
 * never asks. */
function LiveCell(props: { keyId: string; revoked: boolean; t: TFunction }) {
  const { t } = props;
  const entitlement = useEntitlement(props.keyId, !props.revoked);
  if (props.revoked) {
    return <Badge tone="secondary">{t("keys.liveOff")}</Badge>;
  }
  if (entitlement.isLoading) {
    return <span className="text-xs text-muted">…</span>;
  }
  const answer = entitlement.data;
  if (!answer) {
    return <span className="text-xs text-muted">—</span>;
  }
  return answer.allowed ? (
    <Badge tone="success" live>
      {t("keys.liveReady")}
    </Badge>
  ) : (
    <Badge
      tone="danger"
      title={
        answer.retryAfterSecs != null
          ? t("keys.freesIn", { secs: answer.retryAfterSecs })
          : undefined
      }
    >
      {answer.reason ?? t("keys.liveRefused")}
    </Badge>
  );
}
