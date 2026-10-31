import * as React from "react";
import { useReducedMotion } from "motion/react";
import { cn } from "../../lib/utils";

/** A tiny sparkline — one series, no axes, no chrome. Meant to sit under a KPI
 *  or inside a stat chip where a full chart would be noise: it answers "which
 *  way is this number moving", not "what is the value on Tuesday". Values are
 *  scaled to the box; a zero-height series still draws the baseline so an
 *  empty week reads as flat, not broken. */
const Sparkline = React.forwardRef<
  SVGSVGElement,
  Omit<React.SVGProps<SVGSVGElement>, "values"> & {
    values: number[];
    /** px — the box the series is scaled into. Defaults to a wide thin strip. */
    width?: number;
    height?: number;
    /** "area" fills under the line; "line" is the stroke alone. */
    variant?: "line" | "area";
    /** stroke/fill color — any CSS color; defaults to the primary token. */
    color?: string;
  }
>(({ className, values, width = 120, height = 28, variant = "area", color, ...props }, ref) => {
  const reduceMotion = useReducedMotion();
  const n = values.length;
  const stroke = color ?? "var(--color-primary)";

  const padT = 2;
  const padB = 2;
  const innerH = height - padT - padB;
  const max = Math.max(1e-9, ...values);
  const x = (i: number) => (n <= 1 ? width / 2 : (i / (n - 1)) * width);
  const y = (v: number) => padT + innerH - (v / max) * innerH;

  const line = values
    .map((v, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(1)},${y(v).toFixed(1)}`)
    .join(" ");
  const area = `${line} L${x(n - 1).toFixed(1)},${(padT + innerH).toFixed(1)} L${x(0).toFixed(1)},${(padT + innerH).toFixed(1)} Z`;

  const gradientId = React.useId();

  return (
    <svg
      ref={ref}
      data-slot="sparkline"
      viewBox={`0 0 ${width} ${height}`}
      width={width}
      height={height}
      aria-hidden
      className={cn("block", className)}
      {...props}
    >
      <defs>
        <linearGradient id={gradientId} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0%" stopColor={stroke} stopOpacity="0.25" />
          <stop offset="100%" stopColor={stroke} stopOpacity="0.02" />
        </linearGradient>
      </defs>
      {variant === "area" ? (
        <path d={area} fill={`url(#${gradientId})`}>
          {!reduceMotion ? (
            <animate attributeName="opacity" from="0" to="1" dur="0.5s" fill="freeze" />
          ) : null}
        </path>
      ) : null}
      <path
        d={line}
        fill="none"
        stroke={stroke}
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        pathLength={reduceMotion ? undefined : 1}
        style={
          reduceMotion
            ? undefined
            : { strokeDasharray: 1, strokeDashoffset: 1, animation: "sparkline-draw 0.6s ease-out forwards" }
        }
      />
      {!reduceMotion ? (
        <style>{`@keyframes sparkline-draw { to { stroke-dashoffset: 0 } }`}</style>
      ) : null}
    </svg>
  );
});
Sparkline.displayName = "Sparkline";

/** One labeled stat for the aggregate bands — a small stacked number + label
 *  pair, optionally carrying a sparkline or delta hint on the side. */
const StatChip = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"div"> & {
    label: string;
    value: React.ReactNode;
    hint?: React.ReactNode;
  }
>(({ className, label, value, hint, children, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="stat-chip"
    className={cn("flex min-w-0 flex-col gap-0.5", className)}
    {...props}
  >
    <span className="text-[11px] font-medium uppercase tracking-wider text-muted">{label}</span>
    <span className="flex items-baseline gap-1.5">
      <span className="truncate text-lg font-semibold tabular-nums text-ink">{value}</span>
      {hint ? <span className="text-xs text-muted">{hint}</span> : null}
    </span>
    {children}
  </div>
));
StatChip.displayName = "StatChip";

export { Sparkline, StatChip };
