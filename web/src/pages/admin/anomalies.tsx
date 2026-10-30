import { Banner, EmptyRows, TableShell, Td, credits } from "../../components/ui";
import { settlementOutcome } from "../../lib/kinds";
import { useAdmin } from "./api";

export function Anomalies({ token }: { token: string }) {
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
            "Organization",
            { label: "Request", truncate: true },
            "Model",
            "Channel",
            "Kind",
            { label: "Frozen", align: "right" },
            { label: "Charged", align: "right" },
            { label: "Attempts", align: "right" },
            "Booked",
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
              <td title={settlementOutcome(a.kind).detail}>
                {settlementOutcome(a.kind).label}
              </td>
              <Td mono align="right">{credits(a.freezeMinor)} cr</Td>
              <Td mono align="right">{credits(a.chargedMinor)} cr</Td>
              <Td align="right">{a.upstreamAttempts}</Td>
              <td>{a.bookedOn}</td>
            </tr>
          ))}
          {!anomalies.loading && (anomalies.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={9}>No anomalies.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
