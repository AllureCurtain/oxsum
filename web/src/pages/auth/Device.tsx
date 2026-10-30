import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useMutation, useQuery } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Banner, Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function Device() {
  const [params] = useSearchParams();
  const [code, setCode] = useState(params.get("code") ?? "");
  const [done, setDone] = useState<"approved" | "denied" | null>(null);
  const pending = useQuery({
    queryKey: ["device-request", code],
    queryFn: () =>
      api<{ userCode: string; createdAt: string; expiresAt: string }>(
        `/device/request?code=${encodeURIComponent(code)}`,
      ),
    enabled: code.trim().length >= 8,
    retry: false,
  });
  const authorize = useMutation({
    mutationFn: (body: { userCode: string; approve: boolean }) =>
      api("/device/authorize", { method: "POST", body }),
  });

  return (
    <AuthCard title="Authorize a device">
      {done ? (
        <Banner tone="success">
          {done === "approved" ? "The device may poll for its key." : "The request was denied."}
        </Banner>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            authorize.mutate({ userCode: code, approve: true }, { onSuccess: () => setDone("approved") });
          }}
        >
          <Field label="User code" hint="the code the tool shows">
            <TextInput
              required
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="XXXX-XXXX"
            />
          </Field>
          {pending.data ? (
            <Banner tone="info">
              Request <code className="mono">{pending.data.userCode}</code>, minted{" "}
              {new Date(pending.data.createdAt).toLocaleString()}, expires{" "}
              {new Date(pending.data.expiresAt).toLocaleString()}.
            </Banner>
          ) : null}
          <ErrorText error={authorize.error} />
          <div className="flex gap-2">
            <Button type="submit" disabled={authorize.isPending}>
              Approve
            </Button>
            <Button
              tone="danger"
              type="button"
              disabled={authorize.isPending}
              onClick={() =>
                authorize.mutate({ userCode: code, approve: false }, { onSuccess: () => setDone("denied") })
              }
            >
              Deny
            </Button>
          </div>
        </form>
      )}
    </AuthCard>
  );
}
