import { Link } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Circle, CircleAlert, CircleCheck, Lock } from "lucide-react";
import { Card, Tag } from "../components/ui";
import { useGettingStarted, type GettingStartedStep } from "../lib/gettingStarted";

// The operator's path from an empty deployment to a customer bill the recipient
// can verify (issue #559). Every row reads server data on each load — the page
// is a map of the existing screens, not a second configuration surface.

interface StepCopy {
  key: GettingStartedStep["key"];
  title: string;
  role: string;
  done: string;
  todo: string;
  /** Extra state the step needs beyond "done or not". */
  blocked?: string;
  action?: { to: string; label: string };
  secondary?: { to: string; label: string; external?: boolean };
  note?: string;
}

export function GettingStarted() {
  const { t } = useTranslation();
  const checklist = useGettingStarted();

  const steps: StepCopy[] = [
    {
      key: "channel",
      title: t("gettingStarted.channel.title"),
      role: t("gettingStarted.role.platformAdmin"),
      done: t("gettingStarted.channel.done"),
      todo:
        (checklist.steps[0].detail?.channels ?? 0) === 0
          ? t("gettingStarted.channel.todoEmpty")
          : t("gettingStarted.channel.todoUnpriced", {
              channels: checklist.steps[0].detail?.channels ?? 0,
            }),
      blocked: t("gettingStarted.channel.blocked"),
      action: { to: "/admin#admin/channels", label: t("gettingStarted.channel.action") },
    },
    {
      key: "credit",
      title: t("gettingStarted.credit.title"),
      role: t("gettingStarted.role.owner"),
      done: t("gettingStarted.credit.done"),
      todo: t("gettingStarted.credit.todo"),
      action: { to: "/topup", label: t("gettingStarted.credit.action") },
    },
    {
      key: "key",
      title: t("gettingStarted.key.title"),
      role: t("gettingStarted.role.member"),
      done: t("gettingStarted.key.done"),
      todo: t("gettingStarted.key.todo"),
      action: { to: "/keys", label: t("gettingStarted.key.action") },
    },
    {
      key: "request",
      title: t("gettingStarted.request.title"),
      role: t("gettingStarted.role.member"),
      done: t("gettingStarted.request.done"),
      todo: t("gettingStarted.request.todoNone"),
      blocked: t("gettingStarted.receiptRestricted"),
      action: { to: "/chat", label: t("gettingStarted.request.action") },
      secondary: { to: "/requests", label: t("gettingStarted.request.secondary") },
      note: t("gettingStarted.request.note"),
    },
    {
      key: "bill",
      title: t("gettingStarted.bill.title"),
      role: t("gettingStarted.role.member"),
      done: t("gettingStarted.bill.done"),
      todo: t("gettingStarted.bill.todo"),
      blocked: t("gettingStarted.receiptRestricted"),
      action: { to: "/requests", label: t("gettingStarted.bill.action") },
      secondary: checklist.proofHref
        ? { to: checklist.proofHref, label: t("gettingStarted.bill.secondary"), external: true }
        : undefined,
      note: t("gettingStarted.bill.note"),
    },
  ];

  return (
    <div className="flex flex-col gap-5">
      <div>
        <h1 className="text-xl font-bold tracking-tight text-ink">{t("gettingStarted.title")}</h1>
        <p className="mt-1 max-w-3xl text-sm text-muted">{t("gettingStarted.intro")}</p>
      </div>

      <Card>
        <div className="flex flex-col gap-1.5">
          <div className="flex items-baseline justify-between gap-4">
            <p className="text-sm font-medium text-ink">
              {t("gettingStarted.progress", { done: checklist.done, total: checklist.total })}
            </p>
            {checklist.ready ? (
              <Tag tone="accent">{t("gettingStarted.ready")}</Tag>
            ) : null}
          </div>
          <div className="flex gap-1" aria-hidden="true">
            {checklist.steps.map((step) => (
              <span
                key={step.key}
                className={
                  "h-1.5 flex-1 rounded-full " +
                  (step.state === "done" ? "bg-success" : "bg-canvas-inset")
                }
              />
            ))}
          </div>
        </div>
      </Card>

      <ol className="flex flex-col gap-3">
        {steps.map((step, index) => {
          const live = checklist.steps[index];
          return (
            <li key={step.key}>
              <Card>
                <div className="flex items-start gap-3.5">
                  <StepIcon state={live.state} />
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <h2 className="text-[15px] font-semibold text-ink">
                        {index + 1}. {step.title}
                      </h2>
                      <Tag>{step.role}</Tag>
                    </div>
                    <p className="mt-1 text-sm text-muted">{statusLine(step, live.state, t)}</p>
                    {step.note && live.state !== "loading" ? (
                      <p className="mt-1 text-xs text-muted">{step.note}</p>
                    ) : null}
                    {live.state !== "done" && live.state !== "loading" && step.action ? (
                      <p className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-sm">
                        <StepLink to={step.action.to} label={step.action.label} />
                        {step.secondary ? (
                          <StepLink
                            to={step.secondary.to}
                            label={step.secondary.label}
                            external={step.secondary.external}
                          />
                        ) : null}
                      </p>
                    ) : null}
                    {live.state === "done" && step.secondary ? (
                      <p className="mt-2 text-sm">
                        <StepLink
                          to={step.secondary.to}
                          label={step.secondary.label}
                          external={step.secondary.external}
                        />
                      </p>
                    ) : null}
                  </div>
                </div>
              </Card>
            </li>
          );
        })}
      </ol>

      <Card title={t("gettingStarted.boundary.title")}>
        <p className="text-sm text-muted">{t("gettingStarted.boundary.body")}</p>
      </Card>
    </div>
  );
}

function StepIcon({ state }: { state: GettingStartedStep["state"] }) {
  if (state === "done") {
    return <CircleCheck className="mt-0.5 size-5 shrink-0 text-success" aria-hidden="true" />;
  }
  if (state === "error") {
    return <CircleAlert className="mt-0.5 size-5 shrink-0 text-danger" aria-hidden="true" />;
  }
  if (state === "blocked") {
    return <Lock className="mt-0.5 size-5 shrink-0 text-muted" aria-hidden="true" />;
  }
  return <Circle className="mt-0.5 size-5 shrink-0 text-muted" aria-hidden="true" />;
}

function statusLine(
  step: StepCopy,
  state: GettingStartedStep["state"],
  t: (key: string) => string,
): string {
  if (state === "done") return step.done;
  if (state === "loading") return t("gettingStarted.checking");
  if (state === "error") return t("gettingStarted.unreachable");
  if (state === "blocked") return step.blocked ?? step.todo;
  return step.todo;
}

function StepLink({ to, label, external }: { to: string; label: string; external?: boolean }) {
  const cls = "font-medium text-primary hover:underline";
  if (external) {
    return (
      <a className={cls} href={to}>
        {label} →
      </a>
    );
  }
  return (
    <Link className={cls} to={to}>
      {label} →
    </Link>
  );
}
