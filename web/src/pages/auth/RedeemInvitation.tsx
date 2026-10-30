import { useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function RedeemInvitation() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const redeem = useMutation({
    mutationFn: (body: { token: string; email: string; password: string }) =>
      api("/invitations/redeem", { method: "POST", body }),
  });
  const token = params.get("token") ?? "";

  return (
    <AuthCard title="Join the organization">
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          redeem.mutate({ token, email, password }, { onSuccess: () => navigate("/login") });
        }}
      >
        <Field label="Email">
          <TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Field label="Password" hint="at least 12 characters">
          <TextInput
            type="password"
            required
            minLength={12}
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>
        <ErrorText error={redeem.error} />
        <Button type="submit" disabled={redeem.isPending || !token}>
          Register into the organization
        </Button>
      </form>
    </AuthCard>
  );
}
