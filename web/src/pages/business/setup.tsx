import { useTranslation } from "react-i18next";
import { CheckCircle2, Circle, LockKeyhole } from "lucide-react";
import { useBusinessReadiness } from "../../api/hooks/readiness";
import { Banner, Button, Card, Skeleton } from "../../components/ui";

/** Staff setup is distinct from a customer's key, funding and first bill. */
export function BusinessSetup() {
  const { t } = useTranslation();
  const query = useBusinessReadiness();
  const result = query.data;
  const done = result?.steps.filter((step) => step.state === "ready").length ?? 0;
  return <div className="flex flex-col gap-6">
    <div>
      <h2 className="text-xl font-bold">{t("businessReadiness.title")}</h2>
      <p className="mt-2 text-sm text-ink-secondary">{t("businessReadiness.intro")}</p>
    </div>
    <Card title={t("businessReadiness.checklist")} actions={
      <Button tone="ghost" disabled={query.isFetching} onClick={() => void query.refetch()}>{t("businessReadiness.refresh")}</Button>
    }>
      {query.isLoading ? <div className="flex flex-col gap-3">
        <Skeleton className="h-5 w-56" /><Skeleton className="h-5 w-72" /><Skeleton className="h-5 w-64" />
      </div> : query.isError || !result ? <Banner tone="error">{t("businessReadiness.error")}</Banner> : <>
        <p className="mb-4 text-sm text-ink-secondary" role="status">{t("businessReadiness.progress", { done, total: result.steps.length })} {result.ready ? t("businessReadiness.ready") : ""}</p>
        <ol className="flex flex-col gap-4">
          {result.steps.map((step, index) => {
            const Icon = step.state === "ready" ? CheckCircle2 : step.state === "restricted" ? LockKeyhole : Circle;
            return <li key={step.key} className="flex items-start gap-3">
              <Icon className={`mt-1 size-5 shrink-0 ${step.state === "ready" ? "text-success" : "text-muted"}`} aria-hidden />
              <div className="min-w-0">
                <h3 className="text-sm font-semibold">{index + 1}. {t(`businessReadiness.steps.${step.key}.title`)}</h3>
                <p className="mt-1 text-sm text-ink-secondary">{step.state === "restricted" ? t("businessReadiness.restricted") : t(`businessReadiness.steps.${step.key}.${step.state}`)}</p>
                {step.action ? <p className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-sm">
                  <a className="font-medium text-primary hover:underline" href={`#business/${step.action}`}>{t(`businessReadiness.steps.${step.key}.action`)} →</a>
                  {step.key === "collection" ? <a className="font-medium text-primary hover:underline" href="#business/collection">{t("businessReadiness.collectionTerms")} →</a> : null}
                </p> : step.state !== "ready" ? <p className="mt-1 text-xs text-muted">{t("businessReadiness.askOwner")}</p> : null}
              </div>
            </li>;
          })}
        </ol>
      </>}
    </Card>
    <Card title={t("businessReadiness.boundaryTitle")}>
      <p className="text-sm text-ink-secondary">{t("businessReadiness.boundary")}</p>
      <p className="mt-2 text-sm text-ink-secondary">{t("businessReadiness.optional")}</p>
    </Card>
  </div>;
}
