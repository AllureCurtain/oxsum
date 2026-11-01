import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { api, ApiError } from "../api/client";
import {
  useDeleteAccount,
  useHealth,
  useOrg,
  useSession,
  useTotpConfirm,
  useTotpDisable,
  useTotpEnroll,
  useUpdateOrgDisplay,
} from "../api/hooks";
import { Button } from "../components/ui";
import { Badge } from "../components/ui";
import { Banner, Card, Field, NumberInput, PageState, TextInput, displayAmount } from "../components/ui";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "../components/ui";

function DeleteAccount() {
  const del = useDeleteAccount();
  const navigate = useNavigate();
  const [password, setPassword] = useState("");
  const [open, setOpen] = useState(false);

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
        <AlertDialog open={open} onOpenChange={setOpen}>
          <AlertDialogTrigger asChild>
            <div>
              <Button tone="danger">Delete account…</Button>
            </div>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Permanently delete your account?</AlertDialogTitle>
              <AlertDialogDescription>
                This cannot be undone. Your password confirms the deletion.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <form id="delete-account-form" className="flex flex-col gap-3" onSubmit={submit}>
              <Field label="Your password confirms">
                <TextInput
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  required
                  autoComplete="current-password"
                />
              </Field>
            </form>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <Button
                type="submit"
                form="delete-account-form"
                tone="danger"
                isLoading={del.isPending}
                loadingText="Deleting…"
              >
                Delete my account
              </Button>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
        {del.error instanceof ApiError ? (
          <Banner tone="error">{del.error.message}</Banner>
        ) : null}
      </div>
    </Card>
  );
}

/** The display-currency form (issue #257). Only an owner or an admin may
 * PATCH /api/v1/org — a member sees the current value read-only, and a key
 * never reaches this page at all. The rate is a decimal string ("0.0014"),
 * never a float input, so the wire carries the exact text the ledger later
 * parses. */
function DisplayCurrencyCard({ role }: { role: "owner" | "admin" | "member" }) {
  const org = useOrg();
  const update = useUpdateOrgDisplay();
  const current = org.data?.displayCurrency ?? null;
  const [code, setCode] = useState(current?.code ?? "");
  const [rate, setRate] = useState(current?.ratePerCredit ?? "");
  const [decimals, setDecimals] = useState(String(current?.decimals ?? 2));
  const [msg, setMsg] = useState<string | null>(null);
  const canWrite = role === "owner" || role === "admin";

  // The org row can arrive after the form mounted — refresh the fields once.
  const [hydrated, setHydrated] = useState(false);
  if (!hydrated && current) {
    setCode(current.code);
    setRate(current.ratePerCredit);
    setDecimals(String(current.decimals));
    setHydrated(true);
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    setMsg(null);
    const value =
      code.trim() === ""
        ? null
        : { code: code.trim().toUpperCase(), ratePerCredit: rate.trim(), decimals: Number(decimals) };
    update.mutate(value, {
      onSuccess: () => setMsg(value ? "Display currency saved." : "Display currency cleared."),
      onError: (err) => setMsg(err instanceof ApiError ? err.message : "could not save"),
    });
  }

  return (
    <Card title="Display currency">
      <form className="flex flex-col gap-3" onSubmit={submit}>
        <p className="text-sm text-muted">
          The ledger always keeps credits — this is only how the dashboard and the
          statement PDF <em>render</em> them. Set a code and a rate per credit
          (e.g. <code className="mono">USD</code> at <code className="mono">0.0014</code>
          {" "}≈ $1.40 per 1,000 credits); leave the code empty to render credits alone.
          The conversion is presentational — a bill's proof covers the credits,
          never the fiat figure.
        </p>
        {canWrite ? (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-4">
            <Field label="Currency code" hint="ISO 4217, e.g. USD">
              <TextInput
                value={code}
                onChange={(e) => setCode(e.target.value)}
                placeholder="—"
                maxLength={3}
                className="w-24 uppercase"
              />
            </Field>
            <Field label="Rate per credit" hint="decimal, e.g. 0.0014">
              <TextInput
                value={rate}
                onChange={(e) => setRate(e.target.value)}
                placeholder="0.0014"
                disabled={code.trim() === ""}
              />
            </Field>
            <Field label="Decimals">
              <NumberInput
                value={decimals}
                min={0}
                max={6}
                onChange={(e) => setDecimals(e.target.value)}
                disabled={code.trim() === ""}
              />
            </Field>
            <div className="flex items-end gap-2">
              <Button type="submit" isLoading={update.isPending} loadingText="Saving…">
                Save
              </Button>
            </div>
            {/* Live preview — what the typed rate actually renders, before the
                commit changes how every bill displays. */}
            <p className="col-span-full text-sm text-muted" aria-live="polite">
              {code.trim() && rate.trim() && Number.isFinite(Number(rate)) && Number(rate) > 0 ? (
                <>
                  Preview: <code className="mono">1.5 cr</code> renders as{" "}
                  <code className="mono">
                    ≈ {displayAmount(1_500_000, { code: code.trim().toUpperCase(), ratePerCredit: rate.trim(), decimals: Number(decimals) || 0 })}{" "}
                    {code.trim().toUpperCase()}
                  </code>
                </>
              ) : (
                "Set a code and rate to preview the conversion."
              )}
            </p>
          </div>
        ) : (
          <p className="text-sm">
            {current ? (
              <>
                This organization renders credits as{" "}
                <code className="mono">
                  ≈ {current.ratePerCredit} {current.code}
                </code>{" "}
                per credit, {current.decimals} decimals.
              </>
            ) : (
              "This organization renders credits alone."
            )}{" "}
            <span className="text-muted">(only an owner or an admin may change it)</span>
          </p>
        )}
        {msg ? (
          <Banner tone={msg.includes("could not") || msg.includes("error") ? "error" : "info"}>
            {msg}
          </Banner>
        ) : null}
      </form>
    </Card>
  );
}

/** Two-factor authentication (issue #504). The `User` wire carries no TOTP
 * status flag, so the card can't read enabled-vs-not off the session — it
 * drives purely off the enrollment flow: enroll mints a pending secret,
 * confirm proves a live code against it and answers the ten backup codes
 * (shown once), disable needs a live code. When nothing is in flight the
 * card offers both doors, each confirming before it fires. */
function TwoFactorCard() {
  const enroll = useTotpEnroll();
  const confirm = useTotpConfirm();
  const disable = useTotpDisable();
  const [code, setCode] = useState("");
  const [disableCode, setDisableCode] = useState("");
  const [msg, setMsg] = useState<string | null>(null);

  function onConfirm(e: FormEvent) {
    e.preventDefault();
    setMsg(null);
    confirm.mutate(
      { code: code.trim() },
      {
        onError: (err) => setMsg(err instanceof ApiError ? err.message : "could not confirm"),
      },
    );
  }

  function onDisable(e: FormEvent) {
    e.preventDefault();
    setMsg(null);
    disable.mutate(
      { code: disableCode.trim() },
      {
        onSuccess: () => setMsg("Two-factor authentication disabled."),
        onError: (err) => setMsg(err instanceof ApiError ? err.message : "could not disable"),
      },
    );
  }

  const pending = enroll.data;
  const confirmed = confirm.data;

  return (
    <Card title="Two-factor authentication">
      <div className="flex flex-col gap-4">
        <p className="text-sm text-muted">
          A confirmed authenticator gates every password login behind a live
          six-digit code — or a single-use backup code — so a stolen password
          alone cannot open the session.
        </p>

        {confirmed ? (
          <div className="flex flex-col gap-3">
            <Banner tone="info">
              Two-factor authentication is on. Save these backup codes — they
              are shown this once, each works a single time.
            </Banner>
            <ul className="grid grid-cols-2 gap-1 sm:grid-cols-3">
              {confirmed.backupCodes.map((c) => (
                <li key={c} className="mono rounded border border-line px-2 py-1 text-center text-sm">
                  {c}
                </li>
              ))}
            </ul>
          </div>
        ) : pending ? (
          <form className="flex flex-col gap-3" onSubmit={onConfirm}>
            <p className="text-sm">
              Add this secret to your authenticator — scan the link or type the
              base32 — then prove it with a live code.
            </p>
            <dl className="grid grid-cols-1 gap-2">
              <div>
                <dt className="text-xs font-medium text-muted">Secret (base32)</dt>
                <dd className="mono break-all rounded border border-line bg-canvas px-2 py-1 text-sm">
                  {pending.secret}
                </dd>
              </div>
              <div>
                <dt className="text-xs font-medium text-muted">otpauth URL</dt>
                <dd className="mono break-all rounded border border-line bg-canvas px-2 py-1 text-xs">
                  {pending.otpauthUrl}
                </dd>
              </div>
            </dl>
            <Field label="Live code" hint="the six digits the authenticator now shows">
              <TextInput
                inputMode="numeric"
                autoComplete="one-time-code"
                required
                value={code}
                onChange={(e) => setCode(e.target.value)}
                placeholder="123456"
              />
            </Field>
            <div className="flex gap-2">
              <Button type="submit" isLoading={confirm.isPending} loadingText="Confirming…">
                Confirm & enable
              </Button>
              <Button type="button" onClick={() => enroll.reset()}>
                Cancel
              </Button>
            </div>
          </form>
        ) : (
          <div className="flex flex-col gap-3">
            <div>
              <Button
                type="button"
                isLoading={enroll.isPending}
                loadingText="Starting…"
                onClick={() => {
                  setMsg(null);
                  enroll.mutate(undefined, {
                    onError: (err) =>
                      setMsg(
                        err instanceof ApiError ? err.message : "could not start enrollment",
                      ),
                  });
                }}
              >
                Set up an authenticator
              </Button>
            </div>
            <form className="flex items-end gap-2 border-t border-line pt-3" onSubmit={onDisable}>
              <Field label="Turn it off" hint="a live code disables the confirmed factor">
                <TextInput
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  value={disableCode}
                  onChange={(e) => setDisableCode(e.target.value)}
                  placeholder="123456"
                />
              </Field>
              <Button type="submit" tone="danger" isLoading={disable.isPending} loadingText="Disabling…">
                Disable
              </Button>
            </form>
          </div>
        )}
        {msg ? (
          <Banner tone={msg.includes("disabled") && !msg.includes("could not") ? "info" : "error"}>
            {msg}
          </Banner>
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
                <Badge variant="default">verified</Badge>
              ) : (
                <Badge variant="destructive">unverified</Badge>
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
              <Badge variant="secondary">{role}</Badge>
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
                <Badge variant="default">ok</Badge>
              ) : (
                <Badge variant="outline">{health.data ?? "checking…"}</Badge>
              )}
            </dd>
          </div>
        </dl>
        {!user.emailVerified ? (
          <div className="mt-3 flex items-center gap-3">
            <Button
              type="button"
              isLoading={false}
              onClick={() =>
                api("/auth/verify/request", { method: "POST" })
                  .then(() => setVerifyMsg("Verification email sent; check your inbox."))
                  .catch((e) =>
                    setVerifyMsg(e instanceof ApiError ? e.message : "failed to send"),
                  )
              }
            >
              Resend verification email
            </Button>
            {verifyMsg ? <span className="text-sm text-muted">{verifyMsg}</span> : null}
          </div>
        ) : null}
      </Card>
      <DisplayCurrencyCard role={role} />
      <TwoFactorCard />
      <DeleteAccount />
    </div>
  );
}
