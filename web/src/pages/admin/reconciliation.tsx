import { Fragment, useState } from "react";
import { Banner, Button, PageState, TableShell, Td } from "../../components/ui";
import { useAdmin } from "./api";

export function Reconciliation({ token }: { token: string }) {
  const report = useAdmin<{
    checkedAt: string;
    organizations: number;
    clean: boolean;
    classes: { class: string; count: number; samples: { organization?: string; detail?: string }[] }[];
  }>(token, "/reconciliation");
  const [openClass, setOpenClass] = useState<string | null>(null);
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center gap-3">
        <Button tone="ghost" onClick={report.refresh}>
          Re-run scan
        </Button>
        {report.data ? (
          <span className="text-sm text-muted">
            {report.data.organizations} organizations · checked{" "}
            {String(report.data.checkedAt).slice(0, 19).replace("T", " ")}
          </span>
        ) : null}
      </div>
      {report.data ? (
        report.data.clean ? (
          <Banner tone="success">Every ledger reconciles.</Banner>
        ) : null
      ) : report.loading ? (
        <PageState>Scanning…</PageState>
      ) : null}
      {report.data ? (
        <TableShell
          head={["Class", { label: "Count", align: "right" }, "Sample"]}
          density="compact"
          sticky={false}
        >
          {report.data.classes.map((c) => {
            const open = openClass === c.class;
            const first = c.samples[0];
            return (
              <Fragment key={c.class}>
                <tr>
                  <td>
                    <button
                      type="button"
                      className="text-left text-primary"
                      onClick={() => setOpenClass(open ? null : c.class)}
                      aria-expanded={open}
                    >
                      {c.class}
                    </button>
                  </td>
                  <Td mono align="right" className={c.count > 0 ? "text-danger" : ""}>
                    {c.count}
                  </Td>
                  <td className="text-xs text-muted">
                    {c.count === 0
                      ? "none detected"
                      : first
                        ? `${first.organization ?? "platform"} ${first.detail ?? ""}`.trim()
                        : "—"}
                  </td>
                </tr>
                {open ? (
                  <tr>
                    <td colSpan={3} className="bg-canvas">
                      {c.samples.length === 0 ? (
                        <p className="text-xs text-muted">
                          Nothing in this class on the last scan.
                        </p>
                      ) : (
                        <ul className="flex flex-col gap-1">
                          {c.samples.map((sample, index) => (
                            <li key={index} className="mono text-xs">
                              <span className="text-muted">
                                {sample.organization ?? "platform"}
                              </span>{" "}
                              {sample.detail ?? "—"}
                            </li>
                          ))}
                        </ul>
                      )}
                      <p className="mt-1 text-xs text-muted">
                        {c.count > c.samples.length
                          ? `Showing ${c.samples.length} of ${c.count} offenders.`
                          : `${c.count} in this scan.`}
                      </p>
                    </td>
                  </tr>
                ) : null}
              </Fragment>
            );
          })}
        </TableShell>
      ) : null}
      <p className="text-xs text-muted">
        <code className="mono">upstream_price_drift</code> only appears for channels whose base URL
        serves the oxsum-compatible <code className="mono">GET /api/v1/pricing</code>. A real
        provider's cost is compared by the upstream bill sync instead.
      </p>
    </div>
  );
}
