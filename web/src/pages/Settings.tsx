import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { Trans, useTranslation } from "react-i18next";
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
  const { t } = useTranslation();
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
    <Card title={t("settings.dangerZone")}>
      <div className="flex flex-col gap-3">
        <p className="text-sm text-muted">{t("settings.deleteExplain")}</p>
        <AlertDialog open={open} onOpenChange={setOpen}>
          <AlertDialogTrigger asChild>
            <div>
              <Button tone="danger">{t("settings.deleteAccount")}</Button>
            </div>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>{t("settings.deleteConfirm")}</AlertDialogTitle>
              <AlertDialogDescription>{t("settings.deleteNote")}</AlertDialogDescription>
            </AlertDialogHeader>
            <form id="delete-account-form" className="flex flex-col gap-3" onSubmit={submit}>
              <Field label={t("settings.passwordConfirms")}>
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
              <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
              <Button
                type="submit"
                form="delete-account-form"
                tone="danger"
                isLoading={del.isPending}
                loadingText={t("common.deleting")}
              >
                {t("settings.deleteMyAccount")}
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
  const { t } = useTranslation();
  const org = useOrg();
  const update = useUpdateOrgDisplay();
  const current = org.data?.displayCurrency ?? null;
  const [code, setCode] = useState(current?.code ?? "");
  const [rate, setRate] = useState(current?.ratePerCredit ?? "");
  const [decimals, setDecimals] = useState(String(current?.decimals ?? 2));
  const [msg, setMsg] = useState<{ text: string; failed: boolean } | null>(null);
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
      onSuccess: () =>
        setMsg({ text: value ? t("settings.saved") : t("settings.cleared"), failed: false }),
      onError: (err) =>
        setMsg({
          text: err instanceof ApiError ? err.message : t("settings.saveError"),
          failed: true,
        }),
    });
  }

  return (
    <Card title={t("settings.displayCurrency")}>
      <form className="flex flex-col gap-3" onSubmit={submit}>
        <p className="text-sm text-muted">
          <Trans
            i18nKey="settings.displayNote"
            components={{ em: <em />, code: <code className="mono" /> }}
          />
        </p>
        {canWrite ? (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-4">
            <Field label={t("settings.currencyCode")} hint={t("settings.currencyCodeHint")}>
              <TextInput
                value={code}
                onChange={(e) => setCode(e.target.value)}
                placeholder="—"
                maxLength={3}
                className="w-24 uppercase"
              />
            </Field>
            <Field label={t("settings.ratePerCredit")} hint={t("settings.rateHint")}>
              <TextInput
                value={rate}
                onChange={(e) => setRate(e.target.value)}
                placeholder="0.0014"
                disabled={code.trim() === ""}
              />
            </Field>
            <Field label={t("settings.decimals")}>
              <NumberInput
                value={decimals}
                min={0}
                max={6}
                onChange={(e) => setDecimals(e.target.value)}
                disabled={code.trim() === ""}
              />
            </Field>
            <div className="flex items-end gap-2">
              <Button type="submit" isLoading={update.isPending} loadingText={t("common.saving")}>
                {t("common.save")}
              </Button>
            </div>
            {/* Live preview — what the typed rate actually renders, before the
                commit changes how every bill displays. */}
            <p className="col-span-full text-sm text-muted" aria-live="polite">
              {code.trim() && rate.trim() && Number.isFinite(Number(rate)) && Number(rate) > 0 ? (
                <Trans
                  i18nKey="settings.preview"
                  values={{
                    rendered: `≈ ${displayAmount(1_500_000, { code: code.trim().toUpperCase(), ratePerCredit: rate.trim(), decimals: Number(decimals) || 0 })} ${code.trim().toUpperCase()}`,
                  }}
                  components={{ code: <code className="mono" /> }}
                />
              ) : (
                t("settings.previewPrompt")
              )}
            </p>
          </div>
        ) : (
          <p className="text-sm">
            {current ? (
              <Trans
                i18nKey="settings.rendersAs"
                values={{
                  rate: current.ratePerCredit,
                  code: current.code,
                  decimals: current.decimals,
                }}
                components={{ code: <code className="mono" /> }}
              />
            ) : (
              t("settings.rendersCredits")
            )}{" "}
            <span className="text-muted">{t("settings.ownerOnly")}</span>
          </p>
        )}
        {msg ? <Banner tone={msg.failed ? "error" : "info"}>{msg.text}</Banner> : null}
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
  const { t } = useTranslation();
  const enroll = useTotpEnroll();
  const confirm = useTotpConfirm();
  const disable = useTotpDisable();
  const [code, setCode] = useState("");
  const [disableCode, setDisableCode] = useState("");
  const [msg, setMsg] = useState<{ text: string; failed: boolean } | null>(null);

  function onConfirm(e: FormEvent) {
    e.preventDefault();
    setMsg(null);
    confirm.mutate(
      { code: code.trim() },
      {
        onError: (err) =>
          setMsg({
            text: err instanceof ApiError ? err.message : t("settings.confirmError"),
            failed: true,
          }),
      },
    );
  }

  function onDisable(e: FormEvent) {
    e.preventDefault();
    setMsg(null);
    disable.mutate(
      { code: disableCode.trim() },
      {
        onSuccess: () => setMsg({ text: t("settings.disabled"), failed: false }),
        onError: (err) =>
          setMsg({
            text: err instanceof ApiError ? err.message : t("settings.disableError"),
            failed: true,
          }),
      },
    );
  }

  const pending = enroll.data;
  const confirmed = confirm.data;

  return (
    <Card title={t("settings.twoFactor")}>
      <div className="flex flex-col gap-4">
        <p className="text-sm text-muted">{t("settings.twoFactorNote")}</p>

        {confirmed ? (
          <div className="flex flex-col gap-3">
            <Banner tone="info">{t("settings.twoFactorOn")}</Banner>
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
            <p className="text-sm">{t("settings.addSecretNote")}</p>
            <dl className="grid grid-cols-1 gap-2">
              <div>
                <dt className="text-xs font-medium text-muted">{t("settings.secretBase32")}</dt>
                <dd className="mono break-all rounded border border-line bg-canvas px-2 py-1 text-sm">
                  {pending.secret}
                </dd>
              </div>
              <div>
                <dt className="text-xs font-medium text-muted">{t("settings.otpauthUrl")}</dt>
                <dd className="mono break-all rounded border border-line bg-canvas px-2 py-1 text-xs">
                  {pending.otpauthUrl}
                </dd>
              </div>
            </dl>
            <Field label={t("settings.liveCode")} hint={t("settings.liveCodeHint")}>
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
              <Button type="submit" isLoading={confirm.isPending} loadingText={t("common.confirming")}>
                {t("settings.confirmEnable")}
              </Button>
              <Button type="button" onClick={() => enroll.reset()}>
                {t("common.cancel")}
              </Button>
            </div>
          </form>
        ) : (
          <div className="flex flex-col gap-3">
            <div>
              <Button
                type="button"
                isLoading={enroll.isPending}
                loadingText={t("settings.starting")}
                onClick={() => {
                  setMsg(null);
                  enroll.mutate(undefined, {
                    onError: (err) =>
                      setMsg({
                        text:
                          err instanceof ApiError
                            ? err.message
                            : t("settings.enrollError"),
                        failed: true,
                      }),
                  });
                }}
              >
                {t("settings.setupAuthenticator")}
              </Button>
            </div>
            <form className="flex items-end gap-2 border-t border-line pt-3" onSubmit={onDisable}>
              <Field label={t("settings.turnOff")} hint={t("settings.turnOffHint")}>
                <TextInput
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  value={disableCode}
                  onChange={(e) => setDisableCode(e.target.value)}
                  placeholder="123456"
                />
              </Field>
              <Button
                type="submit"
                tone="danger"
                isLoading={disable.isPending}
                loadingText={t("settings.disabling")}
              >
                {t("settings.disable")}
              </Button>
            </form>
          </div>
        )}
        {msg ? <Banner tone={msg.failed ? "error" : "info"}>{msg.text}</Banner> : null}
      </div>
    </Card>
  );
}

export function Settings() {
  const { t } = useTranslation();
  const session = useSession();
  const org = useOrg();
  const health = useHealth();
  const [verifyMsg, setVerifyMsg] = useState<string | null>(null);

  if (session.isLoading) return <PageState>{t("common.loading")}</PageState>;
  if (!session.data) return null;
  const { user, organization, role } = session.data;

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">{t("settings.title")}</h1>
      <Card title={t("settings.account")}>
        <dl className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div>
            <dt className="text-xs font-medium text-muted">{t("settings.email")}</dt>
            <dd className="text-sm">{user.email}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("settings.emailStatus")}</dt>
            <dd>
              {user.emailVerified ? (
                <Badge variant="default">{t("settings.verified")}</Badge>
              ) : (
                <Badge variant="destructive">{t("settings.unverified")}</Badge>
              )}
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("settings.organization")}</dt>
            <dd className="text-sm">
              {organization.name} <span className="text-muted">({organization.kind})</span>
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("settings.yourRole")}</dt>
            <dd>
              <Badge variant="secondary">{role}</Badge>
            </dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("settings.tenantId")}</dt>
            <dd className="mono text-sm">{org.data?.tenantId ?? "…"}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("settings.orgSince")}</dt>
            <dd className="text-sm">{org.data ? org.data.createdAt.slice(0, 10) : "…"}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("settings.serverHealth")}</dt>
            <dd>
              {health.data === "ok" ? (
                <Badge variant="default">ok</Badge>
              ) : (
                <Badge variant="outline">{health.data ?? t("common.checking")}</Badge>
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
                  .then(() => setVerifyMsg(t("settings.verificationSent")))
                  .catch((e) =>
                    setVerifyMsg(
                      e instanceof ApiError ? e.message : t("settings.verifySendError"),
                    ),
                  )
              }
            >
              {t("settings.resendVerification")}
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
