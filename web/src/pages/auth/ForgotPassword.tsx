import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Banner, Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function ForgotPassword() {
  const { t } = useTranslation();
  const [email, setEmail] = useState("");
  const [sent, setSent] = useState(false);
  const forgot = useMutation({
    mutationFn: (body: { email: string }) =>
      api("/auth/password/forgot", { method: "POST", body }),
  });

  return (
    <AuthCard title={t("auth.resetPassword")}>
      {sent ? (
        <Banner tone="success">
          {t("auth.resetMailSent")}
        </Banner>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            forgot.mutate({ email }, { onSuccess: () => setSent(true) });
          }}
        >
          <Field label={t("auth.email")}>
            <TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
          </Field>
          <ErrorText error={forgot.error} />
          <Button type="submit" disabled={forgot.isPending}>
            {t("auth.sendResetMail")}
          </Button>
        </form>
      )}
      <p className="mt-4 text-sm">
        <Link to="/login" className="text-primary">
          {t("auth.backToSignInLink")}
        </Link>
      </p>
    </AuthCard>
  );
}
