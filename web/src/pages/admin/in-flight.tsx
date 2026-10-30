import { Banner, Button, EmptyRows, TableShell, Td, credits } from "../../components/ui";
import { useAdmin } from "./api";

export function InFlight({ token }: { token: string }) {
  const holds = useAdmin<
    { organization: string; requestId: string; model: string; channel: string; freezeMinor: number; openedAt: string; sweepAttempts?: number; lastError?: string | null }[]
  >(token, "/holds");
  return (
    <div className="flex flex-col gap-3">
      <Button tone="ghost" onClick={holds.refresh} className="self-start">
        Refresh
      </Button>
      {holds.error ? (
        <Banner tone="error">{holds.error}</Banner>
      ) : (
        <TableShell
          head={[
            "Organization",
            { label: "Request", truncate: true },
            "Model",
            "Channel",
            { label: "Frozen", align: "right" },
            "Opened",
            "Sweeps",
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
            <EmptyRows colSpan={7}>Nothing in flight.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
