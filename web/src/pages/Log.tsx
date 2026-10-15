import { useState } from "react";
import { useEntries, useLogHead, useWitnessCheckpoint } from "../api/hooks";
import { Button, Card, EmptyRows, PageState, TableShell } from "../components/ui";

export function Log() {
  const [before, setBefore] = useState<number | undefined>(undefined);
  const entries = useEntries(100, before);
  const head = useLogHead();
  const checkpoint = useWitnessCheckpoint();

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Transaction log</h1>

      {checkpoint.data ? (
        <Card title="Anchored checkpoint">
          <p className="text-sm">
            Head #{checkpoint.data.headSize}, published {checkpoint.data.updatedAt}, cosigned
            by {checkpoint.data.cosignatures.length} witness
            {checkpoint.data.cosignatures.length === 1 ? "" : "es"} — the "this log cannot be
            rewritten" evidence.
          </p>
          {checkpoint.data.cosignatures.map((cosig) => (
            <code key={cosig.witnessUrl} className="mono mt-1 block text-xs">
              {cosig.witnessUrl}
            </code>
          ))}
        </Card>
      ) : null}

      {head.data ? (
        <Card title="Signed head">
          <p className="text-sm">
            Size {head.data.size}, root <code className="mono">{head.data.root}</code>
          </p>
          <p className="mt-1 text-xs text-muted">
            Signed by {head.data.keyName} ({head.data.keyHash})
          </p>
        </Card>
      ) : null}

      {entries.isLoading ? (
        <PageState>Loading…</PageState>
      ) : entries.isError ? (
        <PageState>The log could not be read.</PageState>
      ) : (
        <>
          <TableShell head={["#", "Description", "Content hash", ""]}>
            {(entries.data?.rows ?? []).map((entry) => (
              <tr key={entry.id}>
                <td className="mono">{entry.index}</td>
                <td className="mono">{entry.description}</td>
                <td className="mono">{entry.contentHash.slice(0, 16)}…</td>
                <td>
                  <a href={`/verify?entry=${entry.id}`} className="text-primary">
                    verify
                  </a>
                </td>
              </tr>
            ))}
            {(entries.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={4}>The log is empty.</EmptyRows>
            ) : null}
          </TableShell>
          {entries.data?.nextCursor != null ? (
            <p>
              <Button tone="ghost" onClick={() => setBefore(entries.data?.nextCursor)}>
                Older
              </Button>
            </p>
          ) : null}
        </>
      )}
    </div>
  );
}
