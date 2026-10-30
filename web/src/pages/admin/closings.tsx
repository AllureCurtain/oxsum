import { useState } from "react";
import {
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

export function Closings({ token }: { token: string }) {
  const closings = useAdmin<
    { organization: string; period: string; entryCount: number; treeSize: number; treeRoot: string; sealHash: string }[]
  >(token, "/closings");
  const [month, setMonth] = useState("");
  const [msg, setMsg] = useState<string | null>(null);

  return (
    <div className="flex flex-col gap-4">
      <Card title="Seal a month">
        <form
          className="flex items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              await adminApi(token, "/closings", { method: "POST", body: { month } });
              setMsg("sealed");
              closings.refresh();
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Month" hint="YYYY-MM, must have fully ended">
            <TextInput required pattern="\d{4}-\d{2}" value={month} onChange={(e) => setMonth(e.target.value)} placeholder="2026-04" />
          </Field>
          <Button type="submit">Seal</Button>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {closings.error ? (
        <Banner tone="error">{closings.error}</Banner>
      ) : (
        <TableShell
          head={[
            "Month",
            "Organization",
            { label: "Entries", align: "right" },
            { label: "Tree size", align: "right" },
            { label: "Seal", truncate: true },
          ]}
          density="compact"
          loading={closings.loading ? 5 : 0}
        >
          {!closings.loading &&
            (closings.data ?? []).map((r) => (
            <tr key={`${r.organization}-${r.period}`}>
              <td className="mono">{r.period}</td>
              <td>{r.organization}</td>
              <Td align="right">{r.entryCount}</Td>
              <Td align="right">{r.treeSize}</Td>
              <Td mono truncate>{r.sealHash}</Td>
            </tr>
          ))}
          {!closings.loading && (closings.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>No sealed months.</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
