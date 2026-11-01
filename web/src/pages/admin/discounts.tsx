import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card, Field, NumberInput, TableShell, Td, TextInput } from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

export function Discounts({ token }: { token: string }) {
  const { t } = useTranslation();
  const discounts = useAdmin<
    {
      discountId: string;
      percent: number;
      organization?: string | null;
      model?: string | null;
      label?: string | null;
      validUntil?: string | null;
    }[]
  >(token, "/discounts");
  const [percent, setPercent] = useState("");
  const [orgId, setOrgId] = useState("");
  const [model, setModel] = useState("");
  const [label, setLabel] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const rows = discounts.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      <Card title={t("admin.discounts.createTitle")}>
        <form
          className="grid grid-cols-1 gap-3 sm:grid-cols-4"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await adminApi(token, "/discounts", {
                method: "POST",
                body: {
                  percent: Number(percent),
                  organizationId: orgId || null,
                  model: model || null,
                  label: label || null,
                  idempotencyKey: `discount-${Date.now()}`,
                },
              });
              setMsg(t("admin.discounts.created"));
              discounts.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label={t("admin.discounts.percentOff")}>
            <NumberInput required min={1} max={100} value={percent} onChange={(e) => setPercent(e.target.value)} />
          </Field>
          <Field label={t("admin.discounts.organizationId")}>
            <TextInput value={orgId} onChange={(e) => setOrgId(e.target.value)} placeholder={t("admin.discounts.all")} />
          </Field>
          <Field label={t("admin.discounts.model")}>
            <TextInput value={model} onChange={(e) => setModel(e.target.value)} placeholder={t("admin.discounts.all")} />
          </Field>
          <Field label={t("admin.discounts.label")}>
            <TextInput value={label} onChange={(e) => setLabel(e.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">{t("common.create")}</Button>
          </div>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell
          head={[{ label: t("admin.discounts.colPercent"), align: "right" }, t("admin.discounts.colOrganization"), t("admin.discounts.colModel"), t("admin.discounts.colLabel"), t("admin.discounts.colUntil"), ""]}
          density="compact"
          sticky={false}
        >
          {rows.map((d) => (
            <tr key={d.discountId}>
              <Td mono align="right">{d.percent}%</Td>
              <td>{d.organization ?? t("admin.discounts.all")}</td>
              <td>{d.model ?? t("admin.discounts.all")}</td>
              <td>{d.label ?? "—"}</td>
              <td>{d.validUntil ? String(d.validUntil).slice(0, 10) : "—"}</td>
              <td>
                <Button
                  tone="danger"
                  onClick={async () => {
                    try {
                      await adminApi(token, `/discounts/${d.discountId}`, { method: "DELETE" });
                      discounts.refresh();
                    } catch (err) {
                      setMsg(err instanceof ApiError ? err.message : "failed");
                    }
                  }}
                >
                  {t("admin.discounts.end")}
                </Button>
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}
