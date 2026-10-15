import { useState } from "react";
import { useEntries, useLogHead, useLogKey, useWitnessCheckpoint } from "../api/hooks";
import { CopyButton } from "../components/cult/copy-button";
import { Button, Card, EmptyRows, PageState, TableShell } from "../components/ui";

export function Log() {
  const [before, setBefore] = useState<number | undefined>(undefined);
  const entries = useEntries(100, before);
  const head = useLogHead();
  const checkpoint = useWitnessCheckpoint();
  const logKey = useLogKey();

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

      {logKey.data ? (
        <Card title="Operator verifying key">
          <p className="text-xs text-muted">
            The public half of the key that signs tree heads and price checkpoints. Fetching it here
            is convenience — a verifier pins it through a channel the operator does not control.
          </p>
          <div className="mt-2 flex items-center gap-2">
            <code className="mono text-sm">{logKey.data.publicKey}</code>
            <CopyButton value={logKey.data.publicKey} aria-label="Copy verifying key" />
          </div>
          <p className="mt-1 text-xs text-muted">
            {logKey.data.keyName} · keyHash {logKey.data.keyHash}
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
