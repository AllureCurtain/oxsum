import { useMemo, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { Receipt } from "lucide-react";
import { useBills, useDisplay } from "../api/hooks";
import { entryKind } from "../lib/kinds";
import {
  Badge,
  Banner,
  Button,
  EmptyRows,
  PageState,
  StatChip,
  TableShell,
  Td,
  signedCredits,
  displayAmount,
  type DisplayCurrency,
} from "../components/ui";
import { ApiError, api } from "../api/client";
import { CodeBlock } from "../components/ui";

import { BillsTrust } from "./bills-trust";

/** One bill row with an on-demand proof-bundle drawer (`GET /entries/{id}/proof`).
 *  The fiat figure is the row's `displayAmount` when the export already carried it,
 *  else the org's current display rate converts it for the table. */
function BillRow({ row, display }: { row: { index: number; entryId: string; bookedOn: string; kind: string; amountMinor: number; description: string; contentHash: string }; display: DisplayCurrency | null }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [proof, setProof] = useState<unknown>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function toggle() {
    if (!open && proof === null) {
      setBusy(true);
      try {
        setProof(await api(`/entries/${row.entryId}/proof`));
      } catch (e) {
        setErr(e instanceof ApiError ? e.message : t("bills.proofError"));
      } finally {
        setBusy(false);
      }
    }
    setOpen(!open);
  }

  return (
    <>
      <tr key={row.entryId}>
        <td className="mono">{row.index}</td>
        <td>{row.bookedOn}</td>
        <td>
          <Badge
            variant={
              row.kind === "topUp" ? "success" : row.kind === "settlement" ? "secondary" : "outline"
            }
          >
            {entryKind(row.kind, t)}
          </Badge>
        </td>
        <Td mono align="right">
          {signedCredits(row.amountMinor)}
          {display ? (
            <span className="block text-xs text-muted">
              ≈ {displayAmount(row.amountMinor, display)} {display.code}
            </span>
          ) : null}
        </Td>
        <Td mono truncate>{row.description}</Td>
        <Td mono truncate>{row.contentHash}</Td>
        <td className="flex items-center gap-2">
          <a href={`/verify?entry=${row.entryId}`} className="text-primary">
            {t("common.verify")}
          </a>
          <Button tone="ghost" onClick={toggle} disabled={busy}>
            {open ? t("bills.hide") : busy ? "…" : t("bills.proof")}
          </Button>
        </td>
      </tr>
      {open ? (
        <tr>
          <td colSpan={7} className="bg-canvas">
            {err ? (
              <Banner tone="error">{err}</Banner>
            ) : (
              <CodeBlock language="json" code={JSON.stringify(proof, null, 2)} />
            )}
            <p className="mt-1 text-xs text-muted">
              <Trans
                i18nKey="bills.bundleNote"
                components={{ code: <code className="mono" /> }}
              />
            </p>
          </td>
        </tr>
      ) : null}
    </>
  );
}

export function Bills() {
  const { t } = useTranslation();
  const [before, setBefore] = useState<number | undefined>(undefined);
  const bills = useBills(before);
  const display = useDisplay();
  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t("bills.title")}</h1>
        <span className="flex gap-3 text-sm">
          <a href="/dashboard/bills/export.csv" className="text-primary">
            CSV
          </a>
          <a href="/dashboard/bills/export.json" className="text-primary">
            JSON
          </a>
        </span>
      </div>

      {bills.isError ? (
        <PageState>{t("bills.readError")}</PageState>
      ) : (
        <>
          <WindowSummary rows={bills.data?.rows ?? []} display={display} />
          <TableShell
            head={[
              "#",
              t("bills.colBooked"),
              t("bills.colKind"),
              { label: t("bills.colAmount"), align: "right" },
              { label: t("log.colDescription"), truncate: true },
              { label: t("bills.colHash"), truncate: true },
              "",
            ]}
            loading={bills.isLoading ? 8 : 0}
          >
            {!bills.isLoading &&
              (bills.data?.rows ?? []).map((row) => (
                <BillRow key={row.entryId} row={row} display={display} />
              ))}
            {!bills.isLoading && (bills.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={7} icon={<Receipt />}>{t("bills.empty")}</EmptyRows>
            ) : null}
          </TableShell>
          {bills.data?.nextCursor != null ? (
            <p>
              <Button tone="ghost" onClick={() => setBefore(bills.data?.nextCursor)}>
                {t("common.older")}
              </Button>
            </p>
          ) : null}
        </>
      )}

      <BillsTrust />
    </div>
  );
}

/** The loaded window's shape summed per kind: top-ups in, adjustments booked,
 *  settlements out — so the table opens with the answer "what does this page of
 *  the ledger add up to" before the rows walk it line by line. The window is
 *  whatever `useBills` returned (the newest page, or the page `before` paged
 *  back to); the band never pretends to be the whole account. */
function WindowSummary({
  rows,
  display,
}: {
  rows: { kind: string; amountMinor: number }[];
  display: DisplayCurrency | null;
}) {
  const { t } = useTranslation();
  const sums = useMemo(() => {
    const acc = { topUp: 0, adjustment: 0, settlement: 0, count: rows.length };
    for (const row of rows) {
      if (row.kind === "topUp") acc.topUp += row.amountMinor;
      else if (row.kind === "adjustment") acc.adjustment += row.amountMinor;
      else if (row.kind === "settlement") acc.settlement += row.amountMinor;
    }
    return acc;
  }, [rows]);
  if (sums.count === 0) return null;
  return (
    <div className="flex flex-wrap gap-x-10 gap-y-4 rounded-xl border border-line bg-surface px-5 py-4 shadow-[0_1px_3px_rgba(20,34,64,.05)]">
      <StatChip label={t("bills.inThisWindow")} value={sums.count} hint={t("bills.entries")} />
      <StatChip
        label={t("bills.topups")}
        value={<span className="text-success">+{signedCredits(sums.topUp)}</span>}
        hint={display ? `≈ ${displayAmount(sums.topUp, display)} ${display.code}` : undefined}
      />
      <StatChip
        label={t("bills.adjustments")}
        value={signedCredits(sums.adjustment)}
        hint={display ? `≈ ${displayAmount(sums.adjustment, display)} ${display.code}` : undefined}
      />
      <StatChip
        label={t("bills.settledOut")}
        value={signedCredits(sums.settlement)}
        hint={display ? `≈ ${displayAmount(sums.settlement, display)} ${display.code}` : undefined}
      />
      <StatChip
        label={t("bills.net")}
        value={signedCredits(sums.topUp + sums.adjustment + sums.settlement)}
      />
    </div>
  );
}

