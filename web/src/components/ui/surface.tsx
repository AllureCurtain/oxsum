import type { ReactNode } from "react";
import { credits, displayAmount, type DisplayCurrency } from "./money";
import { RollingNumber } from "./controls";

// ---------------------------------------------------------------------------
// Surfaces — the product substrate: tinted page, hairline border, whisper
// shadow, one locked radius scale (card xl / controls lg / pill full).
// ---------------------------------------------------------------------------

/** The card surface — the unified `rounded-xl` panel every sheet shares. */
export const SURFACE =
  "rounded-xl border border-line bg-surface shadow-[0_1px_3px_rgba(20,34,64,.05)]";

export function Card(props: { title?: string; actions?: ReactNode; children: ReactNode; flush?: boolean }) {
  return (
    <section className={SURFACE}>
      {props.title ? (
        <div className="flex items-center justify-between border-b border-line px-5 py-3.5">
          <h2 className="text-[15px] font-semibold text-ink">{props.title}</h2>
          {props.actions}
        </div>
      ) : null}
      <div className={props.flush ? "" : "p-5"}>{props.children}</div>
    </section>
  );
}

export function Kpi(props: { label: string; value: ReactNode; hint?: string; icon?: ReactNode }) {
  return (
    <div className={`${SURFACE} p-4`}>
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs font-medium text-muted">{props.label}</p>
        {props.icon ? (
          <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary [&>svg]:size-4">
            {props.icon}
          </span>
        ) : null}
      </div>
      <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">{props.value}</p>
      {props.hint ? <p className="mt-1 text-xs text-muted">{props.hint}</p> : null}
    </div>
  );
}

/**
 * A KPI whose value is a credit amount in minor units — the number rolls when it
 * changes (live billing updates land here through /ws/billing). With `currency`
 * set, the fiat equivalent renders under the credits figure.
 */
export function AnimatedKpi(props: {
  label: string;
  minor: number;
  hint?: string;
  currency?: DisplayCurrency | null;
  icon?: ReactNode;
}) {
  return (
    <div className={`${SURFACE} p-4`}>
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs font-medium text-muted">{props.label}</p>
        {props.icon ? (
          <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary [&>svg]:size-4">
            {props.icon}
          </span>
        ) : null}
      </div>
      <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
        <RollingNumber
          value={props.minor / 1_000_000}
          precision={6}
          format={(v) => `${credits(Math.round(v * 1_000_000))}`}
        />{" "}
        <span className="text-sm font-normal text-muted">cr</span>
      </p>
      {props.currency ? (
        <p className="mt-0.5 text-xs text-muted">
          ≈ {displayAmount(props.minor, props.currency)} {props.currency.code}
        </p>
      ) : null}
      {props.hint ? <p className="mt-1 text-xs text-muted">{props.hint}</p> : null}
    </div>
  );
}
