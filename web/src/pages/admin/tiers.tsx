import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card, Field, NumberInput, TableShell, Td, TextInput } from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

export function Tiers({ token }: { token: string }) {
  const { t } = useTranslation();
  const tiers = useAdmin<
    { name: string; requestsPerMinute?: number | null; modelAllowlist?: string[] | null; organizations: number }[]
  >(token, "/tiers");
  const [name, setName] = useState("");
  const [rpm, setRpm] = useState("");
  const [models, setModels] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const rows = tiers.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      <Card title={t("admin.tiers.setTitle")}>
        <form
          className="grid grid-cols-1 gap-3 sm:grid-cols-3"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await adminApi(token, `/tiers/${name}`, {
                method: "PUT",
                body: {
                  requestsPerMinute: rpm === "" ? null : Number(rpm),
                  modelAllowlist: models.trim() ? models.split(",").map((m) => m.trim()) : null,
                },
              });
              setMsg(t("admin.tiers.saved"));
              tiers.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label={t("admin.tiers.name")}>
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label={t("admin.tiers.rpm")}>
            <NumberInput min={1} value={rpm} onChange={(e) => setRpm(e.target.value)} />
          </Field>
          <Field label={t("admin.tiers.modelAllowlist")}>
            <TextInput value={models} onChange={(e) => setModels(e.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">{t("common.save")}</Button>
          </div>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell
          head={[t("admin.tiers.colTier"), { label: t("admin.tiers.colRpm"), align: "right" }, t("admin.tiers.colModels"), { label: t("admin.tiers.colOrganizations"), align: "right" }, ""]}
          density="compact"
          sticky={false}
        >
          {rows.map((tier) => (
            <tr key={tier.name}>
              <td className="mono">{tier.name}</td>
              <Td mono align="right">{tier.requestsPerMinute ?? "∞"}</Td>
              <td>{tier.modelAllowlist?.join(", ") ?? t("admin.tiers.all")}</td>
              <Td align="right">{tier.organizations}</Td>
              <td>
                <Button
                  tone="danger"
                  onClick={async () => {
                    try {
                      await adminApi(token, `/tiers/${tier.name}`, { method: "DELETE" });
                      tiers.refresh();
                    } catch (err) {
                      setMsg(err instanceof ApiError ? err.message : "failed");
                    }
                  }}
                >
                  {t("common.delete")}
                </Button>
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}
