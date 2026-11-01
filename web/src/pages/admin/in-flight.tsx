import { useTranslation } from "react-i18next";
import { Banner, Button, EmptyRows, TableShell, Td, credits } from "../../components/ui";
import { useAdmin } from "./api";

export function InFlight({ token }: { token: string }) {
  const { t } = useTranslation();
  const holds = useAdmin<
    { organization: string; requestId: string; model: string; channel: string; freezeMinor: number; openedAt: string; sweepAttempts?: number; lastError?: string | null }[]
  >(token, "/holds");
  return (
    <div className="flex flex-col gap-3">
      <Button tone="ghost" onClick={holds.refresh} className="self-start">
        {t("admin.inFlight.refresh")}
      </Button>
      {holds.error ? (
        <Banner tone="error">{holds.error}</Banner>
      ) : (
        <TableShell
          head={[
            t("admin.inFlight.colOrganization"),
            { label: t("admin.inFlight.colRequest"), truncate: true },
            t("admin.inFlight.colModel"),
            t("admin.inFlight.colChannel"),
            { label: t("admin.inFlight.colFrozen"), align: "right" },
            t("admin.inFlight.colOpened"),
            t("admin.inFlight.colSweeps"),
          ]}
          density="compact"
          loading={holds.loading ? 6 : 0}
        >
          {!holds.loading &&
            (holds.data ?? []).map((h) => (
            <tr key={h.requestId}>
              <td>{h.organization}</td>
              <Td mono truncate>{h.requestId}</Td>
              <td>{h.model}</td>
              <td>{h.channel}</td>
              <Td mono align="right">{credits(h.freezeMinor)} cr</Td>
              <td>{String(h.openedAt).slice(0, 19).replace("T", " ")}</td>
              <td className="text-xs">
                {h.sweepAttempts ?? 0}
                {h.lastError ? <span className="block text-danger">{h.lastError}</span> : null}
              </td>
            </tr>
          ))}
          {!holds.loading && (holds.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={7}>{t("admin.inFlight.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
