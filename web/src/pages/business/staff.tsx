import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Landmark, ScrollText, Users } from "lucide-react";
import {
  idem,
  useBusinessAudit,
  useBusinessStaff,
  useCreateIntegrationCredential,
  useIntegrationCredentials,
  useMerchantEvents,
  useMerchants,
  useRevokeIntegrationCredential,
  useRevokeStaff,
  useSetStaff,
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
  Skeleton,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { AuditAction, stateBadge } from "./biz";

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
                          revoke.mutate({ idempotencyKey: key, userId: s.userId, reason })
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
// integrations — merchant rails (T14) plus the integration credentials a
// partner reaches the API under (T04), with mint + revoke.
// ---------------------------------------------------------------------------

function MerchantCards() {
  const { t } = useTranslation();
  const merchants = useMerchants();
  const [picked, setPicked] = useState<string | null>(null);
  const events = useMerchantEvents(picked ?? undefined);

  return (
    <Card title={t("biz.integrations.merchantsTitle")}>
      {merchants.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          density="compact"
          head={[
            t("biz.integrations.colName"),
            t("biz.integrations.colRail"),
            t("biz.integrations.colMode"),
            t("biz.integrations.colAccount"),
            t("common.status"),
            "",
          ]}
          loading={merchants.isLoading ? 3 : 0}
          sticky={false}
        >
          {!merchants.isLoading &&
            (merchants.data?.merchants ?? []).map((m) => (
              <>
                <tr key={m.id} className="cursor-pointer" onClick={() => setPicked(picked === m.id ? null : m.id)}>
                  <td className="font-medium">{m.name}</td>
                  <td>{m.rail}</td>
                  <td>{m.mode}</td>
                  <Td mono truncate>{m.accountId}</Td>
                  <td>{m.enabled ? <Badge tone="success">{t("biz.integrations.enabled")}</Badge> : <Badge tone="secondary">{t("biz.integrations.disabled")}</Badge>}</td>
                  <Td mono>v{m.currentVersion.revision}</Td>
                </tr>
                {picked === m.id ? (
                  <tr key={`${m.id}-events`}>
                    <td colSpan={6} className="bg-canvas-inset">
                      <p className="mb-2 text-xs font-semibold text-muted">{t("biz.integrations.events")}</p>
                      {events.isLoading ? (
                        <Skeleton className="h-12 w-full" />
                      ) : (
                        <TableShell density="compact" sticky={false} head={[t("biz.integrations.colEvent"), t("biz.integrations.colKind"), t("common.status"), t("biz.integrations.colReason")]}>
                          {(events.data?.events ?? []).map((ev) => (
                            <tr key={ev.id}>
                              <Td mono truncate>{ev.eventId}</Td>
                              <td>{ev.kind}</td>
                              <td>{stateBadge(ev.state)}</td>
                              <Td truncate>{ev.reason ?? "—"}</Td>
                            </tr>
                          ))}
                          {(events.data?.events.length ?? 0) === 0 ? (
                            <EmptyRows colSpan={4}>{t("biz.integrations.noEvents")}</EmptyRows>
                          ) : null}
                        </TableShell>
                      )}
                    </td>
                  </tr>
                ) : null}
              </>
            ))}
          {!merchants.isLoading && (merchants.data?.merchants.length ?? 0) === 0 ? (
            <EmptyRows colSpan={6} icon={<Landmark />}>{t("biz.integrations.noMerchants")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

function Credentials() {
  const { t } = useTranslation();
  const creds = useIntegrationCredentials();
  const create = useCreateIntegrationCredential();
  const revoke = useRevokeIntegrationCredential();
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState("customersRead");

  return (
    <Card title={t("biz.integrations.credentialsTitle")}>
      <form
        className="mb-4 grid grid-cols-1 gap-2 sm:grid-cols-3"
        onSubmit={(e) => {
          e.preventDefault();
          create.mutate(
            { idempotencyKey: idem(), name: name || null, scopes: scopes.split(",").map((s) => s.trim()).filter(Boolean), reason: "console mint" },
            { onSuccess: () => setName("") },
          );
        }}
      >
        <Field label={t("biz.integrations.colName")}>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label={t("biz.integrations.colScopes")} hint={t("biz.integrations.scopesHint")}>
          <TextInput value={scopes} onChange={(e) => setScopes(e.target.value)} required />
        </Field>
        <div className="flex items-end">
          <Button type="submit" isLoading={create.isPending}>{t("biz.integrations.mint")}</Button>
        </div>
      </form>
      {create.error instanceof ApiError ? <Banner tone="error">{create.error.message}</Banner> : null}
      {create.data?.secret ? (
        <Banner tone="warn" className="mb-3">
          {t("biz.integrations.secretOnce")} <code className="mono">{create.data.secret}</code>
        </Banner>
      ) : null}
      {creds.isError ? (
        <Banner tone="error">{t("biz.loadError")}</Banner>
      ) : (
        <TableShell
          density="compact"
          head={[t("biz.integrations.colName"), t("biz.integrations.colPrefix"), t("biz.integrations.colScopes"), t("biz.integrations.colExpires"), t("common.status"), ""]}
          loading={creds.isLoading ? 3 : 0}
          sticky={false}
        >
          {!creds.isLoading &&
            (creds.data?.credentials ?? []).map((c) => (
              <tr key={c.credentialId}>
                <td>{c.name ?? "—"}</td>
                <Td mono>{c.prefix}</Td>
                <Td truncate>{c.scopes.join(", ")}</Td>
                <Td mono>{c.expiresAt.slice(0, 10)}</Td>
                <td>{c.revokedAt ? <Badge tone="secondary">{t("biz.integrations.revoked")}</Badge> : <Badge tone="success">{t("biz.integrations.live")}</Badge>}</td>
                <td className="text-right">
                  {!c.revokedAt ? (
                    <AuditAction
                      trigger={<Button tone="danger">{t("biz.integrations.revoke")}</Button>}
                      title={t("biz.integrations.revokeTitle")}
                      description={t("biz.integrations.revokeExplain")}
                      confirmLabel={t("biz.integrations.revoke")}
                      tone="danger"
                      reasonLabel={t("biz.reason")}
                      isPending={revoke.isPending}
                      error={revoke.error}
                      onConfirm={(reason, key) =>
                        revoke.mutate({ credentialId: c.credentialId, idempotencyKey: key, reason })
                      }
                    />
                  ) : null}
                </td>
              </tr>
            ))}
          {!creds.isLoading && (creds.data?.credentials.length ?? 0) === 0 ? (
            <EmptyRows colSpan={6}>{t("biz.integrations.noCredentials")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </Card>
  );
}

export function BusinessIntegrations() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-xl font-bold">{t("biz.integrations.title")}</h2>
      <MerchantCards />
      <Credentials />
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
