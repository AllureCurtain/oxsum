import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  NumberInput,
  Switch,
  TableShell,
  Td,
  TextInput,
  credits,
} from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

export interface AdminOrg {
  id: string;
  name: string;
  kind: string;
  members: number;
  availableMinor: number;
  reservedMinor: number;
  creditLimitMinor: number;
  creditUsedMinor: number;
  suspended?: boolean;
}

export function Organizations({ token }: { token: string }) {
  const { t } = useTranslation();
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const orgs = useAdmin<{ organizations?: AdminOrg[]; orgs?: AdminOrg[]; nextCursor?: string } | AdminOrg[]>(
    token,
    `/organizations${cursor ? `?cursor=${cursor}` : ""}`,
    [cursor],
  );
  const [adjustId, setAdjustId] = useState<string | null>(null);
  const rows = Array.isArray(orgs.data) ? orgs.data : (orgs.data?.organizations ?? orgs.data?.orgs ?? []);
  const nextCursor = Array.isArray(orgs.data) ? undefined : orgs.data?.nextCursor;

  return (
    <div className="flex flex-col gap-4">
      {orgs.error ? (
        <Banner tone="error">{orgs.error}</Banner>
      ) : (
        <>
          <TableShell
            head={[
              t("admin.organizations.colName"),
              t("admin.organizations.colKind"),
              { label: t("admin.organizations.colMembers"), align: "right" },
              { label: t("admin.organizations.colAvailable"), align: "right" },
              { label: t("admin.organizations.colReserved"), align: "right" },
              { label: t("admin.organizations.colCredit"), align: "right" },
              t("admin.organizations.colState"),
              "",
            ]}
            density="compact"
            loading={orgs.loading ? 6 : 0}
          >
            {!orgs.loading &&
              rows.map((o) => (
              <tr key={o.id}>
                <td>{o.name}</td>
                <td>{o.kind}</td>
                <Td align="right">{o.members}</Td>
                <Td mono align="right">{credits(o.availableMinor)} cr</Td>
                <Td mono align="right">{credits(o.reservedMinor)} cr</Td>
                <Td mono align="right">
                  {credits(o.creditUsedMinor)} / {credits(o.creditLimitMinor)} cr
                </Td>
                <td>{o.suspended ? <span className="text-danger">{t("admin.organizations.suspended")}</span> : t("admin.organizations.active")}</td>
                <td>
                  <Button tone="ghost" onClick={() => setAdjustId(adjustId === o.id ? null : o.id)}>
                    {t("admin.organizations.adjust")}
                  </Button>
                </td>
              </tr>
            ))}
            {!orgs.loading && rows.length === 0 ? (
              <EmptyRows colSpan={8}>{t("admin.organizations.empty")}</EmptyRows>
            ) : null}
          </TableShell>
          {nextCursor ? (
            <Button tone="ghost" onClick={() => setCursor(nextCursor)}>
              {t("common.older")}
            </Button>
          ) : null}
        </>
      )}
      {adjustId ? <AdjustOrg token={token} id={adjustId} onDone={() => { setAdjustId(null); orgs.refresh(); }} /> : null}
    </div>
  );
}

function AdjustOrg({ token, id, onDone }: { token: string; id: string; onDone: () => void }) {
  const { t } = useTranslation();
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState("");
  const [credit, setCredit] = useState("");
  const [terms, setTerms] = useState("");
  const [tier, setTier] = useState("");
  const [displayCode, setDisplayCode] = useState("");
  const [displayRate, setDisplayRate] = useState("");
  const [suspend, setSuspend] = useState<boolean | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  return (
    <Card title={t("admin.organizations.manageTitle", { id: id.slice(0, 8) })}>
      <form
        className="grid grid-cols-1 gap-3 sm:grid-cols-3"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            if (amount !== "") {
              await adminApi(token, `/organizations/${id}/adjustments`, {
                method: "POST",
                body: {
                  amountMinor: Math.round(Number(amount) * 1_000_000),
                  reason,
                  idempotencyKey: `adm-${Date.now()}`,
                },
              });
            }
            if (
              credit !== "" ||
              terms !== "" ||
              tier !== "" ||
              suspend !== null ||
              displayCode !== "" ||
              displayRate !== ""
            ) {
              await adminApi(token, `/organizations/${id}`, {
                method: "PATCH",
                body: {
                  idempotencyKey: `adm-${Date.now()}`,
                  ...(credit !== "" ? { creditLimitMinor: Math.round(Number(credit) * 1_000_000) } : {}),
                  ...(terms !== "" ? { paymentTermsDays: Number(terms) } : {}),
                  ...(tier !== "" ? { tier } : {}),
                  ...(suspend !== null ? { suspended: suspend } : {}),
                  ...(displayCode !== ""
                    ? {
                        displayCurrency: {
                          code: displayCode.trim().toUpperCase(),
                          ratePerCredit: displayRate.trim(),
                          decimals: 2,
                        },
                      }
                    : {}),
                },
              });
            }
            setMsg(t("admin.organizations.saved"));
            onDone();
          } catch (err) {
            setMsg(err instanceof ApiError ? err.message : "failed");
          }
        }}
      >
        <Field label={t("admin.organizations.adjustAmount")}>
          <NumberInput value={amount} onChange={(e) => setAmount(e.target.value)} />
        </Field>
        <Field label={t("admin.organizations.reason")} hint={t("admin.organizations.reasonHint")}>
          <TextInput value={reason} maxLength={200} onChange={(e) => setReason(e.target.value)} />
        </Field>
        <Field label={t("admin.organizations.creditLimit")}>
          <NumberInput min={0} value={credit} onChange={(e) => setCredit(e.target.value)} />
        </Field>
        <Field label={t("admin.organizations.paymentTerms")}>
          <NumberInput min={0} value={terms} onChange={(e) => setTerms(e.target.value)} />
        </Field>
        <Field label={t("admin.organizations.tier")}>
          <TextInput value={tier} onChange={(e) => setTier(e.target.value)} />
        </Field>
        <Field label={t("admin.organizations.displayCurrency")} hint={t("admin.organizations.displayCurrencyHint")}>
          <div className="flex gap-2">
            <TextInput
              value={displayCode}
              onChange={(e) => setDisplayCode(e.target.value)}
              placeholder="USD"
              maxLength={3}
              className="w-20 uppercase"
            />
            <TextInput
              value={displayRate}
              onChange={(e) => setDisplayRate(e.target.value)}
              placeholder="0.0014"
            />
          </div>
        </Field>
        <Field label={t("admin.organizations.suspendSpend")} hint={t("admin.organizations.suspendSpendHint")}>
          <div className="flex items-center gap-2 pt-2">
            <Switch
              checked={suspend ?? false}
              onCheckedChange={(v) => setSuspend(v)}
            />
            <span className="text-xs text-muted">
              {suspend === null ? t("admin.organizations.leaveAsIs") : suspend ? t("admin.organizations.suspend") : t("admin.organizations.reinstate")}
            </span>
          </div>
        </Field>
        <div className="flex items-end gap-2">
          <Button type="submit">{t("admin.organizations.apply")}</Button>
          {msg ? <span className="text-xs text-muted">{msg}</span> : null}
        </div>
      </form>
      <CreditNotes token={token} id={id} />
    </Card>
  );
}

/// One credit note the admin surface lists (issue #352).
type CreditNote = {
  creditNoteId: string;
  entryId: string;
  organizationId: string;
  amountMinor: number;
  reason: string;
  createdAt: string;
};

/// The credit-note half of the per-organization card: issue a reasoned credit
/// (a new ledger entry, Revenue → Wallet, no money moved) and read back what
/// was issued. Deliberately beside the adjustment form, not merged into it.
function CreditNotes({ token, id }: { token: string; id: string }) {
  const { t } = useTranslation();
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState("");
  const [notes, setNotes] = useState<CreditNote[]>([]);
  const [msg, setMsg] = useState<string | null>(null);
  const [version, setVersion] = useState(0);

  useEffect(() => {
    adminApi<{ rows: CreditNote[] }>(token, `/organizations/${id}/credit-notes`)
      .then((data) => setNotes(data.rows))
      .catch(() => setNotes([]));
  }, [token, id, version]);

  return (
    <div className="mt-4 border-t border-line pt-3">
      <h3 className="text-sm font-semibold">{t("admin.organizations.creditNotes")}</h3>
      <p className="text-xs text-muted">
        {t("admin.organizations.creditNotesNote")}
      </p>
      <form
        className="mt-2 flex flex-wrap items-end gap-2"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await adminApi(token, `/organizations/${id}/credit-notes`, {
              method: "POST",
              body: {
                amountMinor: Math.round(Number(amount) * 1_000_000),
                reason,
                idempotencyKey: `cn-${crypto.randomUUID()}`,
              },
            });
            setMsg(t("admin.organizations.issued"));
            setAmount("");
            setReason("");
            setVersion((v) => v + 1);
          } catch (err) {
            setMsg(err instanceof ApiError ? err.message : "failed");
          }
        }}
      >
        <Field label={t("admin.organizations.amount")}>
          <NumberInput
            required
            min={0}
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
          />
        </Field>
        <Field label={t("admin.organizations.reasonRequired")} hint={t("admin.organizations.reasonRequiredHint")}>
          <TextInput value={reason} maxLength={200} onChange={(e) => setReason(e.target.value)} />
        </Field>
        <Button type="submit">{t("admin.organizations.issueNote")}</Button>
        {msg ? <span className="text-xs text-muted">{msg}</span> : null}
      </form>
      {notes.length > 0 ? (
        <div className="mt-2">
          <TableShell
            head={[t("admin.organizations.colIssued"), { label: t("admin.organizations.colAmount"), align: "right" }, t("admin.organizations.colReason"), { label: t("admin.organizations.colEntry"), truncate: true }]}
            density="compact"
            sticky={false}
          >
            {notes.map((note) => (
              <tr key={note.creditNoteId}>
                <td className="mono text-xs">{note.createdAt.slice(0, 19).replace("T", " ")}</td>
                <Td mono align="right">{credits(note.amountMinor)} cr</Td>
                <td className="text-xs">{note.reason}</td>
                <Td mono truncate className="text-xs text-muted">
                  {note.entryId}
                </Td>
              </tr>
            ))}
          </TableShell>
        </div>
      ) : (
        <p className="mt-2 text-xs text-muted">{t("admin.organizations.noCreditNotes")}</p>
      )}
    </div>
  );
}
