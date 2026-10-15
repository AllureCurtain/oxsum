import { useState } from "react";
import { useBills, useConsistency, useLogHead } from "../api/hooks";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  PageState,
  TableShell,
  NumberInput,
  signedCredits,
} from "../components/ui";
import { ApiError } from "../api/client";

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

export function Bills() {
  const [before, setBefore] = useState<number | undefined>(undefined);
  const bills = useBills(before);
  const head = useLogHead();
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

      {bills.isLoading ? (
        <PageState>Loading…</PageState>
      ) : bills.isError ? (
        <PageState>The bills could not be read.</PageState>
      ) : (
        <>
          <TableShell head={["#", "Booked", "Kind", "Amount", "Description", "Hash", ""]}>
            {(bills.data?.rows ?? []).map((row) => (
              <tr key={row.entryId}>
                <td className="mono">{row.index}</td>
                <td>{row.bookedOn}</td>
                <td>{row.kind}</td>
                <td className="mono">{signedCredits(row.amountMinor)}</td>
                <td className="mono">{row.description}</td>
                <td className="mono">{row.contentHash.slice(0, 12)}…</td>
                <td>
                  <a href={`/verify?entry=${row.entryId}`} className="text-primary">
                    verify
                  </a>
                </td>
              </tr>
            ))}
            {(bills.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={7}>No transactions yet.</EmptyRows>
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
