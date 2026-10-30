import { useState } from "react";
import { useEntries, useLogHead, useLogKey, useWitnessCheckpoint } from "../api/hooks";
import { CopyButton } from "../components/ui";
import { Button, Card, EmptyRows, PageState, TableShell, Td } from "../components/ui";

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
          {checkpoint.data.cosignatures.map((cosig) => {
            const pin = checkpoint.data?.witnessKeys.find(
              (k) => k.witnessUrl === cosig.witnessUrl,
            );
            return (
              <div key={cosig.witnessUrl} className="mt-1">
                <code className="mono block text-xs">{cosig.witnessUrl}</code>
                <p className="text-xs text-muted">
                  {pin
                    ? `pinned key ${pin.keyName} · keyHash ${pin.keyHash}`
                    : "no pinned key — the deployment publishes no key for this witness"}
                </p>
              </div>
            );
          })}
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

      {entries.isError ? (
        <PageState>The log could not be read.</PageState>
      ) : (
        <>
          <TableShell
            head={["#", { label: "Description", truncate: true }, { label: "Content hash", truncate: true }, ""]}
            loading={entries.isLoading ? 8 : 0}
          >
            {!entries.isLoading &&
              (entries.data?.rows ?? []).map((entry) => (
              <tr key={entry.id}>
                <td className="mono">{entry.index}</td>
                <Td mono truncate>{entry.description}</Td>
                <Td mono truncate>{entry.contentHash}</Td>
                <td>
                  <a href={`/verify?entry=${entry.id}`} className="text-primary">
                    verify
                  </a>
                </td>
              </tr>
            ))}
            {!entries.isLoading && (entries.data?.rows.length ?? 0) === 0 ? (
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
