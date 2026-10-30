import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../api/client";
import { Banner, Button } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function VerifyEmail() {
  const [params] = useSearchParams();
  const verify = useMutation({
    mutationFn: (token: string) => api("/auth/verify", { method: "POST", body: { token } }),
  });
  const [asked, setAsked] = useState(false);
  const token = params.get("token") ?? "";

  return (
    <AuthCard title="Verify email">
      {verify.isSuccess ? (
        <Banner tone="success">Your email is verified.</Banner>
      ) : (
        <>
          {token ? (
            <Button onClick={() => verify.mutate(token)} disabled={verify.isPending}>
              Verify this address
            </Button>
          ) : (
            <>
              <p className="text-sm text-muted">
                No token in the link? Request a fresh verification mail.
              </p>
              <Button
                className="mt-3"
                disabled={asked}
                onClick={() => {
                  api("/auth/verify/request", { method: "POST" }).then(() => setAsked(true));
                }}
              >
                {asked ? "Mail sent" : "Send verification mail"}
              </Button>
            </>
          )}
          <ErrorText error={verify.error} />
        </>
      )}
    </AuthCard>
  );
}
