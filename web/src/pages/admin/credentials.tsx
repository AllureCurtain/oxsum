import { useState } from "react";
import { Banner, Button, Card, Field, TableShell, TextInput } from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi, useAdmin } from "./api";

export function Credentials({ token }: { token: string }) {
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
          Credential minted — shown once: <code className="mono break-all">{secret}</code>
        </Banner>
      ) : null}
      <Card title="Mint a metering credential">
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
          <Field label="Name">
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Button type="submit">Mint</Button>
        </form>
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      {rows.length ? (
        <TableShell head={["Name", "Prefix", "Created", ""]} density="compact" sticky={false}>
          {rows.map((c) => (
            <tr key={c.credentialId}>
              <td>{c.name ?? "—"}</td>
              <td className="mono">{c.prefix}</td>
              <td>{c.createdAt.slice(0, 10)}</td>
              <td>
                {c.revokedAt ? (
                  <span className="text-xs text-danger">revoked</span>
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
                    Revoke
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
