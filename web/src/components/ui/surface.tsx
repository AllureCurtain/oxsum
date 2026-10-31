import * as React from "react";
import { credits, displayAmount, type DisplayCurrency } from "./money";
import { RollingNumber } from "./controls";
import { cn } from "../../lib/utils";

// ---------------------------------------------------------------------------
// Surfaces — the product substrate: tinted page, hairline border, whisper
// shadow, one locked radius scale (card xl / controls lg / pill full).
// ---------------------------------------------------------------------------

/** The card surface — the unified `rounded-xl` panel every sheet shares. */
export const SURFACE =
  "rounded-xl border border-line bg-surface shadow-[0_1px_3px_rgba(20,34,64,.05)]";

const Card = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"section"> & {
    title?: string;
    actions?: React.ReactNode;
    flush?: boolean;
  }
>(({ className, title, actions, children, flush, ...props }, ref) => (
  <section
    ref={ref}
    data-slot="card"
    className={cn(SURFACE, className)}
    {...props}
  >
    {title ? (
      <div className="flex items-center justify-between border-b border-line px-5 py-3.5">
        <h2 className="text-[15px] font-semibold text-ink">{title}</h2>
        {actions}
      </div>
    ) : null}
    <div className={flush ? "" : "p-5"}>{children}</div>
  </section>
));
Card.displayName = "Card";

const Kpi = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"div"> & {
    label: string;
    value: React.ReactNode;
    hint?: string;
    icon?: React.ReactNode;
  }
>(({ className, label, value, hint, icon, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="kpi"
    className={cn(`${SURFACE} p-4`, className)}
    {...props}
  >
    <div className="flex items-center justify-between gap-3">
      <p className="text-xs font-medium text-muted">{label}</p>
      {icon ? (
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary [&>svg]:size-4">
          {icon}
        </span>
      ) : null}
    </div>
    <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">{value}</p>
    {hint ? <p className="mt-1 text-xs text-muted">{hint}</p> : null}
  </div>
));
Kpi.displayName = "Kpi";

/**
 * A KPI whose value is a credit amount in minor units — the number rolls when it
 * changes (live billing updates land here through /ws/billing). With `currency`
 * set, the fiat equivalent renders under the credits figure.
 */
const AnimatedKpi = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"div"> & {
    label: string;
    minor: number;
    hint?: string;
    currency?: DisplayCurrency | null;
    icon?: React.ReactNode;
  }
>(({ className, label, minor, hint, currency, icon, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="animated-kpi"
    className={cn(`${SURFACE} p-4`, className)}
    {...props}
  >
    <div className="flex items-center justify-between gap-3">
      <p className="text-xs font-medium text-muted">{label}</p>
      {icon ? (
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary-soft text-primary [&>svg]:size-4">
          {icon}
        </span>
      ) : null}
    </div>
    <p className="mt-1.5 text-2xl font-bold tabular-nums tracking-tight text-ink">
      <RollingNumber
        value={minor / 1_000_000}
        precision={6}
        format={(v) => `${credits(Math.round(v * 1_000_000))}`}
      />{" "}
      <span className="text-sm font-normal text-muted">cr</span>
    </p>
    {currency ? (
      <p className="mt-0.5 text-xs text-muted">
        ≈ {displayAmount(minor, currency)} {currency.code}
      </p>
    ) : null}
    {hint ? <p className="mt-1 text-xs text-muted">{hint}</p> : null}
  </div>
));
AnimatedKpi.displayName = "AnimatedKpi";

export { Card, Kpi, AnimatedKpi };
