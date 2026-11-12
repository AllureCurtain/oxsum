import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ScrollText, Users } from "lucide-react";
import {
  idem,
  useBusinessAudit,
  useBusiness,
  useCapability,
  useBusinessStaff,
  useCreateStaffInvitation,
  useRevokeStaff,
  useRevokeStaffInvitation,
  useSetStaff,
  useStaffInvitations,
} from "../../api/hooks";
import { ApiError } from "../../api/client";
import {
  Badge,
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  NativeSelect,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { AuditAction } from "./biz";
import { MerchantSettings } from "./merchants";
import { ApplicationAccess } from "./application-access";

// ---------------------------------------------------------------------------
// /app/business#business/staff + #business/integrations + #business/audit —
// who can act (staff roles), the integration credentials that reach the API,
// and the immutable finance-audit + merchant-event log.
// ---------------------------------------------------------------------------

const STAFF_ROLES = ["admin", "finance", "support", "viewer"] as const;

export function BusinessStaffSection() {
  const { t } = useTranslation();
  const staff = useBusinessStaff();
  const setStaff = useSetStaff();
  const revoke = useRevokeStaff();
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState<(typeof STAFF_ROLES)[number]>("viewer");
  const [reason, setReason] = useState("");

  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.staff.title")}</h2>
      <Card title={t("biz.staff.appointTitle")}>
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            setStaff.mutate({ idempotencyKey: idem(), userId, role, reason }, {
              onSuccess: () => { setUserId(""); setReason(""); },
            });
          }}
        >
          <Field label={t("biz.staff.colUserId")} hint={t("biz.staff.userIdHint")}>
            <TextInput value={userId} onChange={(e) => setUserId(e.target.value)} required placeholder="user uuid" />
          </Field>
          <Field label={t("biz.staff.colRole")}>
            <NativeSelect value={role} onChange={(e) => setRole(e.target.value as typeof role)}>
              {STAFF_ROLES.map((r) => <option key={r} value={r}>{t(`biz.roles.${r}`)}</option>)}
            </NativeSelect>
          </Field>
          <Field label={t("biz.reason")}>
            <TextInput value={reason} onChange={(e) => setReason(e.target.value)} required />
          </Field>
          {setStaff.error instanceof ApiError ? <Banner tone="error">{setStaff.error.message}</Banner> : null}
          <Button type="submit" isLoading={setStaff.isPending}>{t("biz.staff.appoint")}</Button>
        </form>
      </Card>
      <StaffInvitations />
      <Card title={t("biz.staff.listTitle")}>
        {staff.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            head={[t("biz.staff.colEmail"), t("biz.staff.colRole"), ""]}
            loading={staff.isLoading ? 3 : 0}
          >
            {!staff.isLoading &&
              (staff.data ?? []).map((s) => (
                <tr key={s.userId}>
                  <td>{s.email}</td>
                  <td><Badge tone="default">{t(`biz.roles.${s.role}`)}</Badge></td>
                  <td className="text-right">
                    {s.role !== "owner" ? (
                      <AuditAction
                        trigger={<Button tone="danger">{t("biz.staff.revoke")}</Button>}
                        title={t("biz.staff.revokeTitle")}
                        description={t("biz.staff.revokeExplain", { email: s.email })}
                        confirmLabel={t("biz.staff.revoke")}
                        tone="danger"
                        reasonLabel={t("biz.reason")}
                        isPending={revoke.isPending}
                        error={revoke.error}
                        onConfirm={(reason, key) =>
                          revoke.mutateAsync({ idempotencyKey: key, userId: s.userId, reason })
                        }
                      />
                    ) : null}
                  </td>
                </tr>
              ))}
            {!staff.isLoading && (staff.data?.length ?? 0) === 0 ? (
              <EmptyRows colSpan={3} icon={<Users />}>{t("biz.staff.empty")}</EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// staff invitations — an email-bound grant a recipient accepts through their
// own session. The owner picks a role and address, never an internal UUID;
// the link is returned once and can be revoked while pending.
// ---------------------------------------------------------------------------

function StaffInvitations() {
  const { t } = useTranslation();
  const invitations = useStaffInvitations();
  const create = useCreateStaffInvitation();
  const revoke = useRevokeStaffInvitation();
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<(typeof STAFF_ROLES)[number]>("viewer");
  const [sendEmail, setSendEmail] = useState(true);
  const [reason, setReason] = useState("");
  const [issued, setIssued] = useState<{ token: string; mailState: string } | null>(null);

  return (
    <Card title={t("biz.staffInv.title")}>
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          setIssued(null);
          create.mutate(
            { idempotencyKey: idem(), email, role, sendEmail, reason },
            {
              onSuccess: (data) => {
                setEmail("");
                setReason("");
                setIssued({ token: data.token ?? "", mailState: data.mailState });
              },
            },
          );
        }}
      >
        <Field label={t("biz.staffInv.colEmail")} hint={t("biz.staffInv.emailHint")}>
          <TextInput type="email" value={email} onChange={(e) => setEmail(e.target.value)} required placeholder="staff@example.com" />
        </Field>
        <Field label={t("biz.staff.colRole")}>
          <NativeSelect value={role} onChange={(e) => setRole(e.target.value as typeof role)}>
            {STAFF_ROLES.map((r) => <option key={r} value={r}>{t(`biz.roles.${r}`)}</option>)}
          </NativeSelect>
        </Field>
        <Field label={t("biz.reason")}>
          <TextInput value={reason} onChange={(e) => setReason(e.target.value)} required />
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={sendEmail} onChange={(e) => setSendEmail(e.target.checked)} />
          {t("biz.staffInv.sendEmail")}
        </label>
        {create.error instanceof ApiError ? <Banner tone="error">{create.error.message}</Banner> : null}
        {issued ? (
          <Banner tone="success">
            {issued.mailState === "submitted" ? t("biz.staffInv.mailed") : t("biz.staffInv.copyLink")}
            {issued.token ? <code className="mono ml-2 break-all">{issued.token}</code> : null}
          </Banner>
        ) : null}
        <Button type="submit" isLoading={create.isPending}>{t("biz.staffInv.invite")}</Button>
      </form>
      {invitations.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          head={[t("biz.staffInv.colEmail"), t("biz.staff.colRole"), t("biz.staffInv.colState"), t("biz.staffInv.colMail"), ""]}
          loading={invitations.isLoading ? 3 : 0}
        >
          {!invitations.isLoading &&
            (invitations.data?.invitations ?? []).map((inv) => (
              <tr key={inv.id}>
                <td>{inv.email}</td>
                <td><Badge tone="default">{t(`biz.roles.${inv.role}`)}</Badge></td>
                <td><Badge tone={inv.state === "pending" ? "default" : inv.state === "accepted" ? "success" : "secondary"}>{t(`biz.staffInv.state.${inv.state}`)}</Badge></td>
                <td className="text-muted text-xs">{t(`biz.staffInv.mail.${inv.mailState}`)}</td>
                <td className="text-right">
                  {inv.state === "pending" ? (
                    <AuditAction
                      trigger={<Button tone="danger">{t("biz.staffInv.revoke")}</Button>}
                      title={t("biz.staffInv.revokeTitle")}
                      description={t("biz.staffInv.revokeExplain", { email: inv.email })}
                      confirmLabel={t("biz.staffInv.revoke")}
                      tone="danger"
                      reasonLabel={t("biz.reason")}
                      isPending={revoke.isPending}
                      error={revoke.error}
                      onConfirm={(reason, key) =>
                        revoke.mutateAsync({ invitationId: inv.id, idempotencyKey: key, reason })
                      }
                    />
                  ) : null}
                </td>
              </tr>
            ))}
          {!invitations.isLoading && (invitations.data?.invitations?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5} icon={<Users />}>{t("biz.staffInv.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

// ---------------------------------------------------------------------------
// integrations — merchant rails (T14) plus the integration credentials a
// partner reaches the API under (T04), with mint + revoke.
// ---------------------------------------------------------------------------

export function BusinessIntegrations() {
  const { t } = useTranslation();
  const business = useBusiness();
  const canManage = useCapability(business.data, "integrationsManage");
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.integrations.title")}</h2>
      <MerchantSettings />
      {canManage ? <ApplicationAccess /> : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// audit — the finance_audit log (the endpoint T24 added) filtered by action.
// ---------------------------------------------------------------------------

const AUDIT_ACTIONS = [
  "refund.approve",
  "refund.decline",
  "refund.release",
  "dispute.resolve",
  "journal.void",
  "statement.release",
  "reconciliation.resolve",
  "reconciliation.waive",
  "reconciliation.repair",
] as const;

export function BusinessAudit() {
  const { t } = useTranslation();
  const [action, setAction] = useState("");
  const [after, setAfter] = useState<string | undefined>();
  const audit = useBusinessAudit({ after, action: action || undefined });
  const page = audit.data;

  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.audit.title")}</h2>
      <Card title={t("biz.audit.listTitle")}>
        <div className="mb-3 max-w-xs">
          <Field label={t("biz.audit.colAction")}>
            <NativeSelect value={action} onChange={(e) => { setAction(e.target.value); setAfter(undefined); }}>
              <option value="">{t("common.all")}</option>
              {AUDIT_ACTIONS.map((a) => <option key={a} value={a}>{a}</option>)}
            </NativeSelect>
          </Field>
        </div>
        {audit.isError ? (
          <Banner tone="error">{t("biz.loadError")}</Banner>
        ) : (
          <TableShell
            head={[
              t("biz.audit.colWhen"),
              t("biz.audit.colActor"),
              t("biz.audit.colAction"),
              t("biz.audit.colTarget"),
              t("biz.audit.colDetail"),
            ]}
            loading={audit.isLoading ? 5 : 0}
          >
            {!audit.isLoading &&
              (page?.entries ?? []).map((e) => (
                <tr key={e.auditId}>
                  <Td mono>{e.createdAt.slice(0, 16).replace("T", " ")}</Td>
                  <Td mono truncate>{e.actorUserId.slice(0, 8)}</Td>
                  <td><Badge tone="secondary">{e.action}</Badge></td>
                  <Td mono truncate>{e.target}</Td>
                  <Td truncate>
                    <span className="mono text-xs">{JSON.stringify(e.detail)}</span>
                  </Td>
                </tr>
              ))}
            {!audit.isLoading && (page?.entries.length ?? 0) === 0 ? (
              <EmptyRows colSpan={5} icon={<ScrollText />}>{t("biz.audit.empty")}</EmptyRows>
            ) : null}
          </TableShell>
        )}
        {page?.nextCursor ? (
          <Button tone="ghost" className="mt-3" onClick={() => setAfter(page.nextCursor ?? undefined)}>
            {t("common.loadMore")}
          </Button>
        ) : null}
      </Card>
    </div>
  );
}
