import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Card } from "./ui";
import { useGettingStarted } from "../lib/gettingStarted";

// The Overview's pointer to the setup checklist (issue #559). It disappears the
// moment every step is ready; the page stays reachable at /getting-started.
export function GettingStartedCard() {
  const { t } = useTranslation();
  const checklist = useGettingStarted();
  if (checklist.ready) {
    return null;
  }
  const next = checklist.steps.find((step) => step.state !== "done");
  return (
    <Card>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <p className="text-sm font-medium text-ink">{t("gettingStarted.card.title")}</p>
          <p className="mt-0.5 text-xs text-muted">
            {checklist.loading
              ? t("gettingStarted.card.checking")
              : t("gettingStarted.card.body", {
                  done: checklist.done,
                  total: checklist.total,
                })}
            {next && !checklist.loading
              ? " " +
                t("gettingStarted.card.next", {
                  step: t(`gettingStarted.${next.key}.title`),
                })
              : null}
          </p>
        </div>
        <Link
          to="/getting-started"
          className="shrink-0 rounded-lg border border-line px-3 py-1.5 text-sm font-medium text-ink transition-colors hover:bg-canvas-inset"
        >
          {t("gettingStarted.card.action")} →
        </Link>
      </div>
    </Card>
  );
}
