import { useTranslation } from "react-i18next";
import { Banner, EmptyRows, TableShell, Td, credits } from "../../components/ui";
import { settlementOutcome } from "../../lib/kinds";
import { useAdmin } from "./api";

export function Anomalies({ token }: { token: string }) {
  const { t } = useTranslation();
  const anomalies = useAdmin<
    { organization: string; requestId: string; model: string; channel: string; kind: string; chargedMinor: number; freezeMinor: number; bookedOn: string; upstreamAttempts: number }[]
  >(token, "/anomalies");
  return (
    <div>
      {anomalies.error ? (
        <Banner tone="error">{anomalies.error}</Banner>
      ) : (
        <TableShell
          head={[
            t("admin.anomalies.colOrganization"),
            { label: t("admin.anomalies.colRequest"), truncate: true },
            t("admin.anomalies.colModel"),
            t("admin.anomalies.colChannel"),
            t("admin.anomalies.colKind"),
            { label: t("admin.anomalies.colFrozen"), align: "right" },
            { label: t("admin.anomalies.colCharged"), align: "right" },
            { label: t("admin.anomalies.colAttempts"), align: "right" },
            t("admin.anomalies.colBooked"),
          ]}
          density="compact"
          loading={anomalies.loading ? 6 : 0}
        >
          {!anomalies.loading &&
            (anomalies.data ?? []).map((a) => (
            <tr key={a.requestId}>
              <td>{a.organization}</td>
              <Td mono truncate>{a.requestId}</Td>
              <td>{a.model}</td>
              <td>{a.channel}</td>
              <td title={settlementOutcome(a.kind, t).detail}>
                {settlementOutcome(a.kind, t).label}
              </td>
              <Td mono align="right">{credits(a.freezeMinor)} cr</Td>
              <Td mono align="right">{credits(a.chargedMinor)} cr</Td>
              <Td align="right">{a.upstreamAttempts}</Td>
              <td>{a.bookedOn}</td>
            </tr>
          ))}
          {!anomalies.loading && (anomalies.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={9}>{t("admin.anomalies.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
