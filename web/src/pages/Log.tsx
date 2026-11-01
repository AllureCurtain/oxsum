import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ScrollText } from "lucide-react";
import { useEntries, useLogHead, useLogKey, useWitnessCheckpoint } from "../api/hooks";
import { CopyButton } from "../components/ui";
import { Button, Card, EmptyRows, PageState, TableShell, Td } from "../components/ui";

export function Log() {
  const { t } = useTranslation();
  const [before, setBefore] = useState<number | undefined>(undefined);
  const entries = useEntries(100, before);
  const head = useLogHead();
  const checkpoint = useWitnessCheckpoint();
  const logKey = useLogKey();

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">{t("log.title")}</h1>

      {checkpoint.data ? (
        <Card title={t("log.anchoredCheckpoint")}>
          <p className="text-sm">
            {t("log.headPublished", {
              size: checkpoint.data.headSize,
              updatedAt: checkpoint.data.updatedAt,
              count: checkpoint.data.cosignatures.length,
            })}
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
                    ? t("log.pinnedKey", { keyName: pin.keyName, keyHash: pin.keyHash })
                    : t("log.noPinnedKey")}
                </p>
              </div>
            );
          })}
        </Card>
      ) : null}

      {head.data ? (
        <Card title={t("log.signedHead")}>
          <p className="text-sm">
            Size {head.data.size}, root <code className="mono">{head.data.root}</code>
          </p>
          <p className="mt-1 text-xs text-muted">
            {t("log.signedBy", { keyName: head.data.keyName, keyHash: head.data.keyHash })}
          </p>
        </Card>
      ) : null}

      {logKey.data ? (
        <Card title={t("log.operatorKey")}>
          <p className="text-xs text-muted">{t("log.keyNote")}</p>
          <div className="mt-2 flex items-center gap-2">
            <code className="mono text-sm">{logKey.data.publicKey}</code>
            <CopyButton value={logKey.data.publicKey} aria-label={t("log.copyKey")} />
          </div>
          <p className="mt-1 text-xs text-muted">
            {logKey.data.keyName} · keyHash {logKey.data.keyHash}
          </p>
        </Card>
      ) : null}

      {entries.isError ? (
        <PageState>{t("log.readError")}</PageState>
      ) : (
        <>
          <TableShell
            head={["#", { label: t("log.colDescription"), truncate: true }, { label: t("log.colHash"), truncate: true }, ""]}
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
              <EmptyRows colSpan={4} icon={<ScrollText />}>{t("log.empty")}</EmptyRows>
            ) : null}
          </TableShell>
          {entries.data?.nextCursor != null ? (
            <p>
              <Button tone="ghost" onClick={() => setBefore(entries.data?.nextCursor)}>
                {t("common.older")}
              </Button>
            </p>
          ) : null}
        </>
      )}
    </div>
  );
}
