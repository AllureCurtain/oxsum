import { useTranslation } from "react-i18next";
import { Banner, EmptyRows, TableShell, Td } from "../../components/ui";
import { useAdmin } from "./api";

export function Audit({ token }: { token: string }) {
  const { t } = useTranslation();
  const audit = useAdmin<{
    entries: { auditId: string; recordedAt: string; actor: string; action: string; target?: string | null; detail?: unknown }[];
    nextCursor?: string;
  }>(token, "/audit");
  const rows = audit.data?.entries ?? [];
  return (
    <div>
      {audit.error ? (
        <Banner tone="error">{audit.error}</Banner>
      ) : (
        <TableShell
          head={[t("admin.audit.colWhen"), t("admin.audit.colActor"), t("admin.audit.colAction"), t("admin.audit.colTarget"), { label: t("admin.audit.colDetail"), truncate: true }]}
          density="compact"
          loading={audit.loading ? 8 : 0}
        >
          {!audit.loading &&
            rows.map((e) => (
            <tr key={e.auditId}>
              <td>{String(e.recordedAt).slice(0, 19).replace("T", " ")}</td>
              <td>{e.actor}</td>
              <td>{e.action}</td>
              <td className="mono">{e.target ?? "—"}</td>
              <Td truncate className="text-xs">
                {e.detail ? JSON.stringify(e.detail) : ""}
              </Td>
            </tr>
          ))}
          {!audit.loading && rows.length === 0 ? (
            <EmptyRows colSpan={5}>{t("admin.audit.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
