import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSearchParams } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Banner, Button } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function VerifyEmail() {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const verify = useMutation({
    mutationFn: (token: string) => api("/auth/verify", { method: "POST", body: { token } }),
  });
  const [asked, setAsked] = useState(false);
  const token = params.get("token") ?? "";

  return (
    <AuthCard title={t("auth.verifyEmail")}>
      {verify.isSuccess ? (
        <Banner tone="success">{t("auth.emailVerified")}</Banner>
      ) : (
        <>
          {token ? (
            <Button onClick={() => verify.mutate(token)} disabled={verify.isPending}>
              {t("auth.verifyThisAddress")}
            </Button>
          ) : (
            <>
              <p className="text-sm text-muted">
                {t("auth.noToken")}
              </p>
              <Button
                className="mt-3"
                disabled={asked}
                onClick={() => {
                  api("/auth/verify/request", { method: "POST" }).then(() => setAsked(true));
                }}
              >
                {asked ? t("auth.mailSent") : t("auth.sendVerificationMail")}
              </Button>
            </>
          )}
          <ErrorText error={verify.error} />
        </>
      )}
    </AuthCard>
  );
}
