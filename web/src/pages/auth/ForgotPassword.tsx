import { useState } from "react";
import { Link } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Banner, Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function ForgotPassword() {
  const [email, setEmail] = useState("");
  const [sent, setSent] = useState(false);
  const forgot = useMutation({
    mutationFn: (body: { email: string }) =>
      api("/auth/password/forgot", { method: "POST", body }),
  });

  return (
    <AuthCard title="Reset password">
      {sent ? (
        <Banner tone="success">
          If the account exists, a reset mail is on its way.
        </Banner>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            forgot.mutate({ email }, { onSuccess: () => setSent(true) });
          }}
        >
          <Field label="Email">
            <TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
          </Field>
          <ErrorText error={forgot.error} />
          <Button type="submit" disabled={forgot.isPending}>
            Send reset mail
          </Button>
        </form>
      )}
      <p className="mt-4 text-sm">
        <Link to="/login" className="text-primary">
          Back to sign in
        </Link>
      </p>
    </AuthCard>
  );
}
