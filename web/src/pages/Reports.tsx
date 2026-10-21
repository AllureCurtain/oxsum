import { useState } from "react";
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
import { Banner, Button, Card, Cr, EmptyRows, Field, PageState, TableShell, TextInput } from "../components/ui";

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

function linkState(link: ReportLink): string {
  if (link.revokedAt) {
    return "revoked";
  }
  return new Date(link.expiresAt).getTime() < Date.now() ? "expired" : "active";
}

/// What the notice says about a minted link: whether the mail went out, and if
/// it did not, that the token below is still the way to pass it on.
function delivered(link: CreatedReportLink): string {
  if (link.delivery === "sent") {
    return "The link was created and emailed.";
  }
  if (link.delivery === "failed") {
    return "The link was created, but the mail did not go out — copy it below and pass it on.";
  }
  return "The link was created.";
}

function TokenOnce({ created }: { created: CreatedReportLink }) {
  const [copied, setCopied] = useState(false);
  const url = `${window.location.origin}/bill/${created.token}`;
  return (
    <div className="rounded-md border border-line bg-canvas p-4">
      <p className="text-sm font-semibold">The link, shown once</p>
      <p className="text-xs text-muted">
        The database keeps only its hash. Copy it now — if it is lost, revoke it and mint another.
      </p>
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
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
    </div>
  );
}

export function Reports() {
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
        <h1 className="text-2xl font-bold">Reports</h1>
        <p className="text-sm text-muted">
          A report is one downstream customer's signed bill over an ended span. Share it as a link
          the customer opens without an account; every row is provable against the organization's
          ledger.
        </p>
      </div>

      {notice ? <Banner tone="success">{notice}</Banner> : null}
      {error ? <Banner tone="error">{error.message}</Banner> : null}
      {created ? <TokenOnce created={created} /> : null}

      <Card title="Issue a report">
        <form
          className="grid gap-3 sm:grid-cols-4"
          onSubmit={(event) => {
            event.preventDefault();
            mint.mutate(
              { endUser: endUser.trim(), from, to },
              {
                onSuccess: (report) => {
                  setNotice(
                    `Report minted for ${report.endUser}: ${report.entryCount} entries, ${report.chargedTotalMinor} minor units.`,
                  );
                },
              },
            );
          }}
        >
          <Field label="Customer (endUser)">
            <TextInput
              value={endUser}
              onChange={(event) => setEndUser(event.target.value)}
              placeholder="customer-1"
              required
            />
          </Field>
          <Field label="From">
            <TextInput type="date" value={from} onChange={(event) => setFrom(event.target.value)} required />
          </Field>
          <Field label="To (ended day)">
            <TextInput type="date" value={to} onChange={(event) => setTo(event.target.value)} required />
          </Field>
          <div className="flex items-end">
            <Button tone="primary" disabled={mint.isPending}>
              {mint.isPending ? "Minting…" : "Mint report"}
            </Button>
          </div>
        </form>
        <p className="mt-2 text-xs text-muted">
          The span must be fully ended: a report over a day still open would commit a subset that can
          grow.
        </p>
      </Card>

      <Card title="Minted reports">
        {reports.isLoading ? (
          <PageState>Loading…</PageState>
        ) : reports.isError ? (
          <PageState>The reports could not be read.</PageState>
        ) : (
          <TableShell head={["Period", "Customer", "Entries", "Total", "Share"]}>
            {(reports.data?.rows ?? []).map((report) => (
              <tr key={report.reportId}>
                <td className="mono text-xs">
                  {report.from} → {report.to}
                </td>
                <td>{report.endUser}</td>
                <td>{report.entryCount}</td>
                <td className="mono">
                  <Cr minor={report.chargedTotalMinor} currency={display} />
                </td>
                <td>
                  <Button
                    tone="ghost"
                    onClick={() => {
                      setLinkFor(report.reportId);
                      setNotice(null);
                    }}
                  >
                    Create link
                  </Button>
                </td>
              </tr>
            ))}
            {(reports.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={5}>No reports yet — issue one above.</EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>

      {linkFor && selected ? (
        <Card title={`Share ${selected.endUser} — ${selected.from} to ${selected.to}`}>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Label (optional)">
              <TextInput
                value={label}
                onChange={(event) => setLabel(event.target.value)}
                placeholder="August bill"
              />
            </Field>
            <Field label="Email the link to (optional)">
              <TextInput
                type="email"
                value={reportEmail}
                onChange={(event) => setReportEmail(event.target.value)}
                placeholder="customer@example.com"
              />
            </Field>
            <Field label="Expires (optional, 30 days by default)">
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
                        setNotice(delivered(link));
                        setLinkFor(null);
                        setLabel("");
                        setReportEmail("");
                        setExpiresAt("");
                      },
                    },
                  )
                }
              >
                Create link
              </Button>
              <Button tone="ghost" onClick={() => setLinkFor(null)}>
                Cancel
              </Button>
            </div>
          </div>
        </Card>
      ) : null}

      <Card title="Links">
        {links.isLoading ? (
          <PageState>Loading…</PageState>
        ) : links.isError ? (
          <PageState>The links could not be read.</PageState>
        ) : (
          <TableShell head={["Scope", "Customer", "Report", "Expires", "Last used", "State", ""]}>
            {(links.data?.rows ?? []).map((link) => (
              <tr key={link.linkId}>
                <td>{link.scope === "report" ? "report" : "customer"}</td>
                <td>{link.endUser}</td>
                <td className="mono text-xs">{link.reportId ?? "—"}</td>
                <td className="text-xs">{when(link.expiresAt)}</td>
                <td className="text-xs">{when(link.lastUsedAt)}</td>
                <td>{linkState(link)}</td>
                <td>
                  {link.revokedAt ? null : (
                    <Button
                      tone="ghost"
                      disabled={revoke.isPending}
                      onClick={() =>
                        revoke.mutate(link.linkId, {
                          onSuccess: () => setNotice("The link was revoked."),
                        })
                      }
                    >
                      Revoke
                    </Button>
                  )}
                </td>
              </tr>
            ))}
            {(links.data?.rows.length ?? 0) === 0 ? (
              <EmptyRows colSpan={7}>No links yet — share a report above.</EmptyRows>
            ) : null}
          </TableShell>
        )}
      </Card>

      <Card title="Customer link">
        <p className="text-sm text-muted">
          One link that lists every report issued to a customer — for the customer who wants their
          history, not one bill.
        </p>
        <div className="mt-3 grid gap-3 sm:grid-cols-3">
          <Field label="Customer (endUser)">
            <TextInput
              value={customer}
              onChange={(event) => setCustomer(event.target.value)}
              placeholder="customer-1"
            />
          </Field>
          <Field label="Email the link to (optional)">
            <TextInput
              type="email"
              value={customerEmail}
              onChange={(event) => setCustomerEmail(event.target.value)}
              placeholder="customer@example.com"
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
                      setNotice(delivered(link));
                      setCustomer("");
                      setCustomerEmail("");
                    },
                  },
                )
              }
            >
              Create customer link
            </Button>
          </div>
        </div>
      </Card>
    </div>
  );
}
