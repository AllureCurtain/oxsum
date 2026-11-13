import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Badge,
  Banner,
  Button,
  Card,
  EmptyRows,
  Field,
  Progress,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { ApiError } from "../../api/client";
import type { components } from "../../api/schema";
import { adminApi, useAdmin } from "./api";

type Closing = components["schemas"]["AdminClosing"];
type Accepted = components["schemas"]["CloseMonthAccepted"];
type CloseJob = components["schemas"]["CloseMonthJob"];

const jobPath = (jobId: string) => `/closings/jobs/${jobId}`;
const pollInterval = (job: CloseJob | undefined) =>
  job && (job.status === "pending" || job.status === "running") ? 2000 : false;

export function Closings({ token }: { token: string }) {
  const { t } = useTranslation();
  const closings = useAdmin<Closing[]>(token, "/closings");
  const [month, setMonth] = useState("");
  const [jobId, setJobId] = useState<string | null>(null);
  const [jobError, setJobError] = useState<string | null>(null);
  const [closing, setClosing] = useState(false);
  const job = useAdmin<CloseJob>(token, jobId === null ? "/closings" : jobPath(jobId), [], jobId !== null, pollInterval);

  const terminal = job.data?.status === "done" || job.data?.status === "dead";
  // The seals land when the pass does: refresh the list as the run reaches a
  // terminal state — and again if the job row disappears under the id.
  useEffect(() => {
    if (jobId !== null && (terminal || job.error !== null)) {
      closings.refresh();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jobId, terminal, job.error]);

  const jobBadge = (status: CloseJob["status"]) =>
    status === "pending" ? (
      <Badge tone="secondary" live>{t("admin.closings.jobPending")}</Badge>
    ) : status === "running" ? (
      <Badge live>{t("admin.closings.jobRunning")}</Badge>
    ) : status === "done" ? (
      <Badge tone="success">{t("admin.closings.jobDone")}</Badge>
    ) : (
      <Badge tone="danger">{t("admin.closings.jobDead")}</Badge>
    );

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setClosing(true);
    setJobError(null);
    try {
      const res = await adminApi<Accepted>(token, "/closings", { method: "POST", body: { month } });
      // The answer names the run — whether this call queued it or a retry found
      // it pending: watch it through the progress endpoint either way.
      setJobId(res.jobId);
    } catch (err) {
      setJobError(err instanceof ApiError ? err.message : t("admin.closings.submitFailed"));
    } finally {
      setClosing(false);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <Card title={t("admin.closings.sealTitle")}>
        <form className="flex items-end gap-2" onSubmit={submit}>
          <Field label={t("admin.closings.month")} hint={t("admin.closings.monthHint")}>
            <TextInput required pattern="\d{4}-\d{2}" value={month} onChange={(e) => setMonth(e.target.value)} placeholder="2026-04" />
          </Field>
          <Button type="submit" isLoading={closing}>{t("admin.closings.seal")}</Button>
        </form>
        {jobError ? (
          <Banner tone="error" className="mt-3">{jobError}</Banner>
        ) : null}
        {jobId !== null ? (
          <div className="mt-4 border-t border-line pt-4" aria-live="polite">
            {job.error ? (
              <Banner tone="error">{job.error}</Banner>
            ) : !job.data ? (
              <p className="text-xs text-muted">{t("admin.closings.jobLoading")}</p>
            ) : (
              <div className="flex flex-col gap-3">
                <div className="flex items-center gap-2">
                  {jobBadge(job.data.status)}
                  <span className="mono text-xs text-muted">
                    {t("admin.closings.jobTitle", { month: job.data.month })}
                  </span>
                </div>
                {job.data.organizations > 0 ? (
                  <Progress
                    value={job.data.processed}
                    max={job.data.organizations}
                    label={t("admin.closings.progress", { processed: job.data.processed, organizations: job.data.organizations })}
                    showValue
                  />
                ) : !terminal ? (
                  <Progress value={null} label={t("admin.closings.jobPending")} />
                ) : null}
                <p className="tnum text-xs text-muted">
                  {t("admin.closings.counts", { sealed: job.data.sealed, failed: job.data.failed })}
                </p>
                {job.data.failures.length > 0 ? (
                  <TableShell
                    head={[t("admin.closings.colOrganization"), t("admin.closings.colReason")]}
                    density="compact"
                  >
                    {job.data.failures.map((f) => (
                      <tr key={f.organizationId}>
                        <td>{f.name}</td>
                        <Td>{f.reason}</Td>
                      </tr>
                    ))}
                  </TableShell>
                ) : null}
                {job.data.failed > job.data.failures.length ? (
                  <p className="text-xs text-muted">
                    {t("admin.closings.failuresTruncated", { shown: job.data.failures.length, total: job.data.failed })}
                  </p>
                ) : null}
                {job.data.status === "dead" ? (
                  <p className="text-xs text-danger">{t("admin.closings.deadHint")}</p>
                ) : null}
              </div>
            )}
          </div>
        ) : null}
      </Card>
      {closings.error ? (
        <Banner tone="error">{closings.error}</Banner>
      ) : (
        <TableShell
          head={[
            t("admin.closings.colMonth"),
            t("admin.closings.colOrganization"),
            { label: t("admin.closings.colEntries"), align: "right" },
            { label: t("admin.closings.colTreeSize"), align: "right" },
            { label: t("admin.closings.colSeal"), truncate: true },
          ]}
          density="compact"
          loading={closings.loading ? 5 : 0}
        >
          {!closings.loading &&
            (closings.data ?? []).map((r) => (
            <tr key={`${r.organization}-${r.period}`}>
              <td className="mono">{r.period}</td>
              <td>{r.organization}</td>
              <Td align="right">{r.entryCount}</Td>
              <Td align="right">{r.treeSize}</Td>
              <Td mono truncate>{r.sealHash}</Td>
            </tr>
          ))}
          {!closings.loading && (closings.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={5}>{t("admin.closings.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
    </div>
  );
}
