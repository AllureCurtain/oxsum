import { Fragment, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { Banner, Button, PageState, TableShell, Td } from "../../components/ui";
import { useAdmin } from "./api";

export function Reconciliation({ token }: { token: string }) {
  const { t } = useTranslation();
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
          {t("admin.reconciliation.rerun")}
        </Button>
        {report.data ? (
          <span className="text-sm text-muted">
            {t("admin.reconciliation.summary", {
              count: report.data.organizations,
              checkedAt: String(report.data.checkedAt).slice(0, 19).replace("T", " "),
            })}
          </span>
        ) : null}
      </div>
      {report.data ? (
        report.data.clean ? (
          <Banner tone="success">{t("admin.reconciliation.clean")}</Banner>
        ) : null
      ) : report.loading ? (
        <PageState>{t("admin.reconciliation.scanning")}</PageState>
      ) : null}
      {report.data ? (
        <TableShell
          head={[t("admin.reconciliation.colClass"), { label: t("admin.reconciliation.colCount"), align: "right" }, t("admin.reconciliation.colSample")]}
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
                      ? t("admin.reconciliation.noneDetected")
                      : first
                        ? `${first.organization ?? t("admin.reconciliation.platform")} ${first.detail ?? ""}`.trim()
                        : "—"}
                  </td>
                </tr>
                {open ? (
                  <tr>
                    <td colSpan={3} className="bg-canvas">
                      {c.samples.length === 0 ? (
                        <p className="text-xs text-muted">
                          {t("admin.reconciliation.nothingInClass")}
                        </p>
                      ) : (
                        <ul className="flex flex-col gap-1">
                          {c.samples.map((sample, index) => (
                            <li key={index} className="mono text-xs">
                              <span className="text-muted">
                                {sample.organization ?? t("admin.reconciliation.platform")}
                              </span>{" "}
                              {sample.detail ?? "—"}
                            </li>
                          ))}
                        </ul>
                      )}
                      <p className="mt-1 text-xs text-muted">
                        {c.count > c.samples.length
                          ? t("admin.reconciliation.showingOf", { shown: c.samples.length, count: c.count })
                          : t("admin.reconciliation.inThisScan", { count: c.count })}
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
        <Trans
          i18nKey="admin.reconciliation.driftNote"
          components={{ code: <code className="mono" /> }}
        />
      </p>
    </div>
  );
}
