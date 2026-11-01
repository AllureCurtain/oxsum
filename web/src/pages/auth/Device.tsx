import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSearchParams } from "react-router-dom";
import { useMutation, useQuery } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Banner, Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function Device() {
  const { t } = useTranslation();
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
    <AuthCard title={t("auth.authorizeDevice")}>
      {done ? (
        <Banner tone="success">
          {done === "approved" ? t("auth.deviceApproved") : t("auth.deviceDenied")}
        </Banner>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            authorize.mutate({ userCode: code, approve: true }, { onSuccess: () => setDone("approved") });
          }}
        >
          <Field label={t("auth.userCode")} hint={t("auth.userCodeHint")}>
            <TextInput
              required
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="XXXX-XXXX"
            />
          </Field>
          {pending.data ? (
            <Banner tone="info">
              {t("auth.requestLine",{code:pending.data.userCode,created:new Date(pending.data.createdAt).toLocaleString(),expires:new Date(pending.data.expiresAt).toLocaleString()})}
            </Banner>
          ) : null}
          <ErrorText error={authorize.error} />
          <div className="flex gap-2">
            <Button type="submit" disabled={authorize.isPending}>
              {t("auth.approve")}
            </Button>
            <Button
              tone="danger"
              type="button"
              disabled={authorize.isPending}
              onClick={() =>
                authorize.mutate({ userCode: code, approve: false }, { onSuccess: () => setDone("denied") })
              }
            >
              {t("auth.deny")}
            </Button>
          </div>
        </form>
      )}
    </AuthCard>
  );
}
