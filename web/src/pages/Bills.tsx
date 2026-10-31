import { useMemo, useState } from "react";
import { Receipt } from "lucide-react";
import { useBills, useConsistency, useDisplay, useLogHead } from "../api/hooks";
import { entryKind } from "../lib/kinds";
import {
  Badge,
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  PageState,
  StatChip,
  TableShell,
  Td,
  NumberInput,
  signedCredits,
  displayAmount,
  type DisplayCurrency,
} from "../components/ui";
import { ApiError, api } from "../api/client";
import { CodeBlock } from "../components/ui";

// The archived head rides the browser's localStorage, like the Leptos
// dashboard's archive: `decide` says what a served head means against it.
const ARCHIVE_KEY = "oxsum.head-archive";

interface ArchivedHead {
  size: number;
  root: string;
}

function archived(): ArchivedHead | null {
  try {
    const raw = localStorage.getItem(ARCHIVE_KEY);
    return raw ? (JSON.parse(raw) as ArchivedHead) : null;
  } catch {
    return null;
  }
}

/** One bill row with an on-demand proof-bundle drawer (`GET /entries/{id}/proof`).
 *  The fiat figure is the row's `displayAmount` when the export already carried it,
 *  else the org's current display rate converts it for the table. */
function BillRow({ row, display }: { row: { index: number; entryId: string; bookedOn: string; kind: string; amountMinor: number; description: string; contentHash: string }; display: DisplayCurrency | null }) {
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
        setErr(e instanceof ApiError ? e.message : "the proof could not be read");
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
            {entryKind(row.kind)}
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
            verify
          </a>
          <Button tone="ghost" onClick={toggle} disabled={busy}>
            {open ? "hide" : busy ? "…" : "proof"}
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
              The bundle is the raw material <code className="mono">/verify</code> checks: signature,
              path, and content hash.
            </p>
          </td>
        </tr>
      ) : null}
    </>
  );
}

export function Bills() {
  const [before, setBefore] = useState<number | undefined>(undefined);
  const bills = useBills(before);
  const head = useLogHead();
  const display = useDisplay();
  const [archiveSize, setArchiveSize] = useState(() => archived()?.size ?? null);
  const [checkFrom, setCheckFrom] = useState<number | null>(null);
  const consistency = useConsistency(checkFrom);

  function archiveCurrent() {
    if (!head.data) {
      return;
    }
    const stored = { size: head.data.size, root: head.data.root };
    localStorage.setItem(ARCHIVE_KEY, JSON.stringify(stored));
    setArchiveSize(stored.size);
  }

  function checkConsistency() {
    const held = archived();
    if (held && head.data && held.size < head.data.size) {
      setCheckFrom(held.size);
    }
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">Bills</h1>
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
        <PageState>The bills could not be read.</PageState>
      ) : (
        <>
          <WindowSummary rows={bills.data?.rows ?? []} display={display} />
          <TableShell
            head={[
              "#",
              "Booked",
              "Kind",
              { label: "Amount", align: "right" },
              { label: "Description", truncate: true },
              { label: "Hash", truncate: true },
              "",
            ]}
            loading={bills.isLoading ? 8 : 0}
          >
            {!bills.isLoading &&
              (bills.data?.rows ?? []).map((row) => (
                <BillRow key={row.entryId} row={row} display={display} />
              ))}
            {!bills.isLoading && (bills.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={7} icon={<Receipt />}>No transactions yet.</EmptyRows>
            ) : null}
          </TableShell>
          {bills.data?.nextCursor != null ? (
            <p>
              <Button tone="ghost" onClick={() => setBefore(bills.data?.nextCursor)}>
                Older
              </Button>
            </p>
          ) : null}
        </>
      )}

      {head.data ? (
        <Card title="Ledger head">
          <p className="text-sm">
            Size {head.data.size}, root <code className="mono">{head.data.root}</code>
          </p>
          <p className="mt-1 text-xs text-muted">
            Signed by {head.data.keyName}. Archive this head, then check later that the
            served log still extends it.
          </p>
          <div className="mt-3 flex flex-wrap items-end gap-3">
            <Button tone="ghost" onClick={archiveCurrent}>
              Archive this head
            </Button>
            <span className="text-sm text-muted">
              Archived: {archiveSize ?? "nothing"}
            </span>
            <Button tone="ghost" onClick={checkConsistency} disabled={!archiveSize}>
              Check consistency
            </Button>
          </div>
          {checkFrom !== null ? (
            consistency.isLoading ? (
              <p className="mt-2 text-sm text-muted">Checking…</p>
            ) : consistency.error instanceof ApiError ? (
              <Banner tone="error">{consistency.error.message}</Banner>
            ) : consistency.data ? (
              <Banner tone="success">
                The log at size {consistency.data.head.size} extends your archived head at
                size {consistency.data.oldHead.size} — proof carries{" "}
                {consistency.data.proof.path.length} nodes.
              </Banner>
            ) : null
          ) : null}
        </Card>
      ) : null}
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
      <StatChip label="In this window" value={sums.count} hint="entries" />
      <StatChip
        label="Top-ups"
        value={<span className="text-success">+{signedCredits(sums.topUp)}</span>}
        hint={display ? `≈ ${displayAmount(sums.topUp, display)} ${display.code}` : undefined}
      />
      <StatChip
        label="Adjustments"
        value={signedCredits(sums.adjustment)}
        hint={display ? `≈ ${displayAmount(sums.adjustment, display)} ${display.code}` : undefined}
      />
      <StatChip
        label="Settled out"
        value={signedCredits(sums.settlement)}
        hint={display ? `≈ ${displayAmount(sums.settlement, display)} ${display.code}` : undefined}
      />
      <StatChip
        label="Net"
        value={signedCredits(sums.topUp + sums.adjustment + sums.settlement)}
      />
    </div>
  );
}

export function ConsistencyForm() {
  const [size, setSize] = useState("");
  const [from, setFrom] = useState<number | null>(null);
  useConsistency(from);
  return (
    <form
      className="flex items-end gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        setFrom(Number(size));
      }}
    >
      <Field label="Archived size">
        <NumberInput value={size} min={1} onChange={(e) => setSize(e.target.value)} required />
      </Field>
      <Button tone="ghost" type="submit">
        Check
      </Button>
    </form>
  );
}
