import { useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { Banner, Button, Card, Field, TableShell, TextInput } from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

export function Credentials({ token }: { token: string }) {
  const { t } = useTranslation();
  const credentials = useAdmin<
    { credentialId: string; name?: string | null; prefix: string; createdAt: string; revokedAt?: string | null }[]
  >(token, "/service-credentials");
  const [name, setName] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const rows = credentials.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      {secret ? (
        <Banner tone="success">
          <Trans
            i18nKey="admin.credentials.minted"
            values={{ secret }}
            components={{ code: <code className="mono break-all" /> }}
          />
        </Banner>
      ) : null}
      <Card title={t("admin.credentials.mintTitle")}>
        <form
          className="flex items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              const res = await adminApi<{ secret: string }>(token, "/service-credentials", {
                method: "POST",
                body: { name },
              });
              setSecret(res.secret);
              credentials.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label={t("admin.credentials.name")}>
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Button type="submit">{t("admin.credentials.mint")}</Button>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell head={[t("admin.credentials.colName"), t("admin.credentials.colPrefix"), t("admin.credentials.colCreated"), ""]} density="compact" sticky={false}>
          {rows.map((c) => (
            <tr key={c.credentialId}>
              <td>{c.name ?? "—"}</td>
              <td className="mono">{c.prefix}</td>
              <td>{c.createdAt.slice(0, 10)}</td>
              <td>
                {c.revokedAt ? (
                  <span className="text-xs text-danger">{t("admin.credentials.revoked")}</span>
                ) : (
                  <Button
                    tone="danger"
                    onClick={async () => {
                      try {
                        await adminApi(token, `/service-credentials/${c.credentialId}`, {
                          method: "DELETE",
                        });
                      } catch (err) {
                        setMsg(err instanceof ApiError ? err.message : "failed");
                      }
                      credentials.refresh();
                    }}
                  >
                    {t("admin.credentials.revoke")}
                  </Button>
                )}
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}
