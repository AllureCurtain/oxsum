import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useSearchParams } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Banner, Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function ResetPassword() {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const [password, setPassword] = useState("");
  const [done, setDone] = useState(false);
  const reset = useMutation({
    mutationFn: (body: { token: string; password: string }) =>
      api("/auth/password/reset", { method: "POST", body }),
  });
  const token = params.get("token") ?? "";

  return (
    <AuthCard title={t("auth.chooseNewPassword")}>
      {done ? (
        <>
          <Banner tone="success">{t("auth.passwordChanged")}</Banner>
          <Button className="mt-4 w-full" onClick={() => undefined}>
            <Link to="/login" className="block w-full text-center text-white">
              {t("auth.signInLink")}
            </Link>
          </Button>
        </>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            reset.mutate({ token, password }, { onSuccess: () => setDone(true) });
          }}
        >
          <Field label={t("auth.newPassword")} hint={t("auth.passwordHint")}>
            <TextInput
              type="password"
              required
              minLength={12}
              autoComplete="new-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </Field>
          <ErrorText error={reset.error} />
          <Button type="submit" disabled={reset.isPending || !token}>
            {t("auth.resetPassword")}
          </Button>
        </form>
      )}
    </AuthCard>
  );
}
