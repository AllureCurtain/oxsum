import { useState } from "react";
import { Button, Card, Field, NumberInput, TableShell, Td, TextInput } from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

export function Tiers({ token }: { token: string }) {
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
      <Card title="Set tier">
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
              setMsg("saved");
              tiers.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Name">
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label="Requests per minute">
            <NumberInput min={1} value={rpm} onChange={(e) => setRpm(e.target.value)} />
          </Field>
          <Field label="Model allowlist">
            <TextInput value={models} onChange={(e) => setModels(e.target.value)} />
          </Field>
          <div className="flex items-end">
            <Button type="submit">Save</Button>
          </div>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell
          head={["Tier", { label: "RPM", align: "right" }, "Models", { label: "Organizations", align: "right" }, ""]}
          density="compact"
          sticky={false}
        >
          {rows.map((t) => (
            <tr key={t.name}>
              <td className="mono">{t.name}</td>
              <Td mono align="right">{t.requestsPerMinute ?? "∞"}</Td>
              <td>{t.modelAllowlist?.join(", ") ?? "all"}</td>
              <Td align="right">{t.organizations}</Td>
              <td>
                <Button
                  tone="danger"
                  onClick={async () => {
                    try {
                      await adminApi(token, `/tiers/${t.name}`, { method: "DELETE" });
                      tiers.refresh();
                    } catch (err) {
                      setMsg(err instanceof ApiError ? err.message : "failed");
                    }
                  }}
                >
                  Delete
                </Button>
              </td>
            </tr>
          ))}
        </TableShell>
      ) : null}
    </div>
  );
}
