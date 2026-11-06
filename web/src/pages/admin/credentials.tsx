import { useTranslation } from "react-i18next";
import { Banner, Button, EmptyRows, TableShell } from "../../components/ui";
import { useAdmin } from "./api";

export function Credentials({ token }: { token: string }) {
  const { t } = useTranslation();
  const credentials = useAdmin<
    { credentialId: string; name?: string | null; prefix: string; createdAt: string }[]
  >(token, "/service-credentials");
  const rows = credentials.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      <Banner tone="info">{t("admin.credentials.retired")}</Banner>
      {credentials.error ? (
        <Banner tone="error">
          {credentials.error}
          <Button tone="ghost" onClick={credentials.refresh}>{t("admin.credentials.retry")}</Button>
        </Banner>
      ) : (
        <TableShell
          head={[t("admin.credentials.colName"), t("admin.credentials.colPrefix"), t("admin.credentials.colCreated"), t("common.status")]}
          density="compact"
          sticky={false}
          loading={credentials.loading ? 4 : 0}
        >
          {!credentials.loading && rows.map((c) => (
            <tr key={c.credentialId}>
              <td>{c.name ?? "—"}</td>
              <td className="mono">{c.prefix}</td>
              <td>{c.createdAt.slice(0, 10)}</td>
              <td>{t("admin.credentials.disabled")}</td>
            </tr>
          ))}
          {!credentials.loading && rows.length === 0 ? (
            <EmptyRows colSpan={4}>{t("admin.credentials.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
