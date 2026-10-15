import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { api, ApiError } from "../api/client";
import { useDeleteAccount, useHealth, useOrg, useSession } from "../api/hooks";
import { HaloButton } from "../components/cult/halo-button";
import { HaloBadge } from "../components/cult/halo-badge";
import { Banner, Card, Field, PageState, TextInput } from "../components/ui";

function DeleteAccount() {
  const del = useDeleteAccount();
  const navigate = useNavigate();
  const [password, setPassword] = useState("");
  const [confirming, setConfirming] = useState(false);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    del.mutate(password, {
      onSuccess: () => navigate("/login", { replace: true }),
    });
  };

  return (
    <Card title="Danger zone">
      <div className="flex flex-col gap-3">
        <p className="text-sm text-muted">
          Deleting the account is irreversible: the email is tombstoned, sessions and API keys are
          revoked, memberships end — except that an organization's last owner is refused. Bills keep
          verifying; the ledger never carries identity. OAuth-created accounts need a real password
          (set one through the reset flow) to confirm this.
        </p>
        {confirming ? (
          <form className="flex items-end gap-2" onSubmit={submit}>
            <Field label="Your password confirms">
              <TextInput
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required
                autoComplete="current-password"
              />
            </Field>
            <HaloButton type="submit" isLoading={del.isPending} loadingText="Deleting…">
              Delete my account
            </HaloButton>
            <button
              type="button"
              className="rounded-md border border-line px-3 py-1.5 text-sm hover:bg-canvas"
              onClick={() => setConfirming(false)}
            >
              Cancel
            </button>
          </form>
        ) : (
          <div>
            <button
              type="button"
              className="rounded-md border border-danger bg-danger px-3 py-1.5 text-sm font-medium text-white hover:bg-danger-hover"
              onClick={() => setConfirming(true)}
            >
              Delete account…
            </button>
          </div>
        )}
        {del.error instanceof ApiError ? (
          <Banner tone="error">{del.error.message}</Banner>
        ) : null}
      </div>
    </Card>
  );
}

export function Settings() {
  const session = useSession();
  const org = useOrg();
  const health = useHealth();
  const [verifyMsg, setVerifyMsg] = useState<string | null>(null);

  if (session.isLoading) return <PageState>Loading…</PageState>;
  if (!session.data) return null;
  const { user, organization, role } = session.data;

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Settings</h1>
      <Card title="Account">
        <dl className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div>
            <dt className="text-xs font-medium text-muted">Email</dt>
            <dd className="text-sm">{user.email}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">Email status</dt>
            <dd>
              {user.emailVerified ? (
                <HaloBadge variant="default">verified</HaloBadge>
              ) : (
                <HaloBadge variant="destructive">unverified</HaloBadge>
              )}
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">Organization</dt>
            <dd className="text-sm">
              {organization.name} <span className="text-muted">({organization.kind})</span>
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">Your role</dt>
            <dd>
              <HaloBadge variant="secondary">{role}</HaloBadge>
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">Tenant id</dt>
            <dd className="mono text-sm">{org.data?.tenantId ?? "…"}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">Organization since</dt>
            <dd className="text-sm">{org.data ? org.data.createdAt.slice(0, 10) : "…"}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">Server health</dt>
            <dd>
              {health.data === "ok" ? (
                <HaloBadge variant="default">ok</HaloBadge>
              ) : (
                <HaloBadge variant="outline">{health.data ?? "checking…"}</HaloBadge>
              )}
            </dd>
          </div>
        </dl>
        {!user.emailVerified ? (
          <div className="mt-3 flex items-center gap-3">
            <HaloButton
              type="button"
              isLoading={false}
              onClick={() =>
                api("/auth/verify/request", { method: "POST" })
                  .then(() => setVerifyMsg("Verification email sent — check your inbox."))
                  .catch((e) =>
                    setVerifyMsg(e instanceof ApiError ? e.message : "failed to send"),
                  )
              }
            >
              Resend verification email
            </HaloButton>
            {verifyMsg ? <span className="text-sm text-muted">{verifyMsg}</span> : null}
          </div>
        ) : null}
      </Card>
      <DeleteAccount />
    </div>
  );
}
