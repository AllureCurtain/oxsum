import { useState } from "react";
import { Button, Card, Field, NumberInput } from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi } from "./api";

export function Redemptions({ token }: { token: string }) {
  const [amount, setAmount] = useState("");
  const [count, setCount] = useState("1");
  const [batch, setBatch] = useState<{ batchId: string; codes: string[] } | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  return (
    <div className="flex flex-col gap-4">
      <Card title="Mint redemption codes">
        <form
          className="flex items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            try {
              const res = await adminApi<{ batchId: string; codes: string[] }>(
                token,
                "/redemption-codes",
                {
                  method: "POST",
                  body: { amountMinor: Math.round(Number(amount) * 1_000_000), count: Number(count) },
                },
              );
              setBatch(res);
              setMsg(`${res.codes.length} minted`);
            } catch (err) {
              setMsg(err instanceof ApiError ? err.message : "failed");
            }
          }}
        >
          <Field label="Amount each (credits)">
            <NumberInput required min={0} value={amount} onChange={(e) => setAmount(e.target.value)} />
          </Field>
          <Field label="Count">
            <NumberInput required min={1} max={50} value={count} onChange={(e) => setCount(e.target.value)} />
          </Field>
          <Button type="submit">Mint</Button>
        </form>
        {batch ? (
          <div className="mt-3">
            <p className="mb-1 text-xs text-muted">
              Batch {batch.batchId.slice(0, 8)}… — the secrets, shown once:
            </p>
            <ul className="max-h-60 overflow-y-auto space-y-1">
              {batch.codes.map((c) => (
                <li key={c} className="mono text-xs">
                  {c}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        {msg ? <p className="mt-2 text-xs text-muted">{msg}</p> : null}
      </Card>
      <p className="text-xs text-muted">
        The codes' secrets are answered here and nowhere else — the database keeps only their
        hashes. Redeemed codes land on the wallet through `POST /api/v1/redemptions`.
      </p>
    </div>
  );
}
