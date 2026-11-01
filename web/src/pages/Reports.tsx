import { useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import {
  useCreateCustomerLink,
  useCreateReportLink,
  useDisplay,
  useMintUsageReport,
  useReportLinks,
  useRevokeReportLink,
  useUsageReports,
  type CreatedReportLink,
  type ReportLink,
} from "../api/hooks";
import { FileText, Link2 } from "lucide-react";
import { Banner, Button, Card, Cr, EmptyRows, Field, PageState, TableShell, Td, TextInput } from "../components/ui";

// The reports page: the organization's side of an end-user bill (issues
// #261/#300). Mint a signed report for one customer over an ended span, then
// share it as a capability link — the customer opens /bill/{token} with no
// account. The token is shown once; the list below is the record of what was
// shared.

function dayString(offset: number): string {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() - offset);
  return d.toISOString().slice(0, 10);
}

function expiryToRfc3339(value: string): string | undefined {
  if (!value) {
    return undefined;
  }
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString();
}

function when(value: string | null | undefined): string {
  return value ? new Date(value).toLocaleString() : "—";
}

function linkState(link: ReportLink, t: TFunction): string {
  if (link.revokedAt) {
    return t("reports.stateRevoked");
  }
  return new Date(link.expiresAt).getTime() < Date.now()
    ? t("reports.stateExpired")
    : t("reports.stateActive");
}

/// What the notice says about a minted link: whether the mail went out, and if
/// it did not, that the token below is still the way to pass it on.
function delivered(link: CreatedReportLink, t: TFunction): string {
  if (link.delivery === "sent") {
    return t("reports.deliveredSent");
  }
  if (link.delivery === "failed") {
    return t("reports.deliveredFailed");
  }
  return t("reports.deliveredPlain");
}

function TokenOnce({ created }: { created: CreatedReportLink }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const url = `${window.location.origin}/bill/${created.token}`;
  return (
    <div className="rounded-xl border border-line bg-canvas p-4">
      <p className="text-sm font-semibold">{t("reports.linkShownOnce")}</p>
      <p className="text-xs text-muted">{t("reports.hashNote")}</p>
      <div className="mt-2 flex items-center gap-2">
        <code className="mono flex-1 break-all text-xs">{url}</code>
        <Button
          tone="ghost"
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(url);
              setCopied(true);
            } catch {
              setCopied(false);
            }
          }}
        >
          {copied ? t("common.copied") : t("common.copy")}
        </Button>
      </div>
    </div>
  );
}

export function Reports() {
  const { t } = useTranslation();
  const display = useDisplay();
  const reports = useUsageReports();
  const links = useReportLinks();
  const mint = useMintUsageReport();
  const createReportLink = useCreateReportLink();
  const createCustomerLink = useCreateCustomerLink();
  const revoke = useRevokeReportLink();

  const [endUser, setEndUser] = useState("");
  const [from, setFrom] = useState(dayString(31));
  const [to, setTo] = useState(dayString(1));
  const [linkFor, setLinkFor] = useState<string | null>(null);
  const [label, setLabel] = useState("");
  const [expiresAt, setExpiresAt] = useState("");
  const [reportEmail, setReportEmail] = useState("");
  const [customer, setCustomer] = useState("");
  const [customerEmail, setCustomerEmail] = useState("");
  const [created, setCreated] = useState<CreatedReportLink | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const error = (mint.error ??
    createReportLink.error ??
    createCustomerLink.error ??
    revoke.error) as Error | null;
  const selected = (reports.data?.rows ?? []).find((r) => r.reportId === linkFor);

  return (
    <div className="flex flex-col gap-6">
      <div>
        <h1 className="text-2xl font-bold">{t("reports.title")}</h1>
        <p className="text-sm text-muted">{t("reports.whatIsReport")}</p>
      </div>

      {notice ? <Banner tone="success">{notice}</Banner> : null}
      {error ? <Banner tone="error">{error.message}</Banner> : null}
      {created ? <TokenOnce created={created} /> : null}

      {/* The mint → share → record chain, as a rail: the cards below are the
          three steps of one workflow, not four sibling panels. */}
      <ol className="flex items-center gap-2 text-xs font-medium text-muted">
        <li className="flex items-center gap-1.5">
          <span className="flex h-5 w-5 items-center justify-center rounded-full bg-primary text-[11px] font-semibold text-white">1</span>
          {t("reports.stepMint")}
        </li>
        <li className="text-line">→</li>
        <li className="flex items-center gap-1.5">
          <span className="flex h-5 w-5 items-center justify-center rounded-full bg-primary-soft text-[11px] font-semibold text-primary">2</span>
          {t("reports.stepShare")}
        </li>
        <li className="text-line">→</li>
        <li className="flex items-center gap-1.5">
          <span className="flex h-5 w-5 items-center justify-center rounded-full bg-primary-soft text-[11px] font-semibold text-primary">3</span>
          {t("reports.stepTrack")}
        </li>
      </ol>

      <Card title={t("reports.issueTitle")}>
        <form
          className="grid gap-3 sm:grid-cols-4"
          onSubmit={(event) => {
            event.preventDefault();
            mint.mutate(
              { endUser: endUser.trim(), from, to },
              {
                onSuccess: (report) => {
                  setNotice(
                    t("reports.mintedNotice", {
                      endUser: report.endUser,
                      entries: report.entryCount,
                      minor: report.chargedTotalMinor,
                    }),
                  );
                },
              },
            );
          }}
        >
          <Field label={t("reports.customer")}>
            <TextInput
              value={endUser}
              onChange={(event) => setEndUser(event.target.value)}
              placeholder={t("reports.customerPlaceholder")}
              required
            />
          </Field>
          <Field label={t("reports.from")}>
            <TextInput type="date" value={from} onChange={(event) => setFrom(event.target.value)} required />
          </Field>
          <Field label={t("reports.toEnded")}>
            <TextInput type="date" value={to} onChange={(event) => setTo(event.target.value)} required />
          </Field>
          <div className="flex items-end">
            <Button tone="primary" disabled={mint.isPending}>
              {mint.isPending ? t("reports.mintingButton") : t("reports.mintReport")}
            </Button>
          </div>
          <p className="col-span-full text-xs text-muted" aria-live="polite">
            <Trans
              i18nKey="reports.minting"
              values={{
                customer: endUser.trim() || t("reports.theCustomer"),
                from,
                to,
              }}
              components={{ strong: <strong /> }}
            />
          </p>
        </form>
        <p className="mt-2 text-xs text-muted">{t("reports.spanEnded")}</p>
      </Card>

      <Card title={t("reports.minted")}>
        {reports.isError ? (
          <PageState>{t("reports.readError")}</PageState>
        ) : (
          <TableShell
            head={[
              t("reports.colPeriod"),
              t("reports.colCustomer"),
              { label: t("reports.colEntries"), align: "right" },
              { label: t("common.total"), align: "right" },
              t("reports.colShare"),
            ]}
            loading={reports.isLoading ? 4 : 0}
          >
            {!reports.isLoading &&
              (reports.data?.rows ?? []).map((report) => (
              <tr key={report.reportId}>
                <td className="mono text-xs">
                  {report.from} → {report.to}
                </td>
                <td>{report.endUser}</td>
                <Td align="right">{report.entryCount}</Td>
                <Td mono align="right">
                  <Cr minor={report.chargedTotalMinor} currency={display} />
                </Td>
                <td>
                  <Button
                    tone="ghost"
                    onClick={() => {
                      setLinkFor(report.reportId);
                      setNotice(null);
                    }}
                  >
                    {t("reports.createLink")}
                  </Button>
                </td>
              </tr>
            ))}
            {!reports.isLoading && (reports.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={5} icon={<FileText />}>
                {t("reports.noReports")}
              </EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>

      {linkFor && selected ? (
        <Card title={t("reports.shareTitle", { endUser: selected.endUser, from: selected.from, to: selected.to })}>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label={t("reports.labelOptional")}>
              <TextInput
                value={label}
                onChange={(event) => setLabel(event.target.value)}
                placeholder={t("reports.labelPlaceholder")}
              />
            </Field>
            <Field label={t("reports.emailLinkOptional")}>
              <TextInput
                type="email"
                value={reportEmail}
                onChange={(event) => setReportEmail(event.target.value)}
                placeholder={t("reports.emailPlaceholder")}
              />
            </Field>
            <Field label={t("reports.expiresOptional")}>
              <TextInput
                type="datetime-local"
                value={expiresAt}
                onChange={(event) => setExpiresAt(event.target.value)}
              />
            </Field>
            <div className="flex items-end gap-2">
              <Button
                tone="primary"
                disabled={createReportLink.isPending}
                onClick={() =>
                  createReportLink.mutate(
                    {
                      reportId: linkFor,
                      label: label.trim() || undefined,
                      email: reportEmail.trim() || undefined,
                      expiresAt: expiryToRfc3339(expiresAt),
                    },
                    {
                      onSuccess: (link) => {
                        setCreated(link);
                        setNotice(delivered(link, t));
                        setLinkFor(null);
                        setLabel("");
                        setReportEmail("");
                        setExpiresAt("");
                      },
                    },
                  )
                }
              >
                {t("reports.createLink")}
              </Button>
              <Button tone="ghost" onClick={() => setLinkFor(null)}>
                {t("common.cancel")}
              </Button>
            </div>
          </div>
        </Card>
      ) : null}

      <Card title={t("reports.linksTitle")}>
        {links.isError ? (
          <PageState>{t("reports.linksError")}</PageState>
        ) : (
          <TableShell
            head={[
              t("reports.colScope"),
              t("reports.colCustomer"),
              { label: t("reports.colReport"), truncate: true },
              t("reports.colExpires"),
              t("reports.colLastUsed"),
              t("reports.colState"),
              "",
            ]}
            loading={links.isLoading ? 4 : 0}
          >
            {!links.isLoading &&
              (links.data?.rows ?? []).map((link) => (
              <tr key={link.linkId}>
                <td>{link.scope === "report" ? t("reports.scopeReport") : t("reports.scopeCustomer")}</td>
                <td>{link.endUser}</td>
                <Td mono truncate className="text-xs">
                  {link.reportId ?? "—"}
                </Td>
                <td className="text-xs">{when(link.expiresAt)}</td>
                <td className="text-xs">{when(link.lastUsedAt)}</td>
                <td>{linkState(link, t)}</td>
                <td>
                  {link.revokedAt ? null : (
                    <Button
                      tone="ghost"
                      disabled={revoke.isPending}
                      onClick={() =>
                        revoke.mutate(link.linkId, {
                          onSuccess: () => setNotice(t("reports.revokedNotice")),
                        })
                      }
                    >
                      {t("reports.revoke")}
                    </Button>
                  )}
                </td>
              </tr>
            ))}
            {!links.isLoading && (links.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={7} icon={<Link2 />}>
                {t("reports.noLinks")}
              </EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>

      <Card title={t("reports.customerLink")}>
        <p className="text-sm text-muted">{t("reports.oneLink")}</p>
        <div className="mt-3 grid gap-3 sm:grid-cols-3">
          <Field label={t("reports.customer")}>
            <TextInput
              value={customer}
              onChange={(event) => setCustomer(event.target.value)}
              placeholder={t("reports.customerPlaceholder")}
            />
          </Field>
          <Field label={t("reports.emailLinkOptional")}>
            <TextInput
              type="email"
              value={customerEmail}
              onChange={(event) => setCustomerEmail(event.target.value)}
              placeholder={t("reports.emailPlaceholder")}
            />
          </Field>
          <div className="flex items-end">
            <Button
              tone="primary"
              disabled={createCustomerLink.isPending || customer.trim() === ""}
              onClick={() =>
                createCustomerLink.mutate(
                  {
                    endUser: customer.trim(),
                    email: customerEmail.trim() || undefined,
                  },
                  {
                    onSuccess: (link) => {
                      setCreated(link);
                      setNotice(delivered(link, t));
                      setCustomer("");
                      setCustomerEmail("");
                    },
                  },
                )
              }
            >
              {t("reports.createCustomerLink")}
            </Button>
          </div>
        </div>
      </Card>
    </div>
  );
}
