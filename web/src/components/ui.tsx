import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import { Check, ChevronDown, Copy } from "lucide-react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { Select as SelectPrimitive } from "@base-ui/react/select";
import { Switch as SwitchPrimitive } from "@base-ui/react/switch";
import { Tabs as TabsPrimitive } from "@base-ui/react/tabs";
import { Progress as ProgressPrimitive } from "@base-ui/react/progress";

import { cn } from "../lib/utils";

// ---------------------------------------------------------------------------
// Money helpers — integer minor units, 1 credit = 1_000_000, never float math.
// ---------------------------------------------------------------------------

export function credits(minor: number): string {
  const sign = minor < 0 ? "-" : "";
  const abs = Math.abs(minor);
  const whole = Math.trunc(abs / 1_000_000);
  const frac = String(abs % 1_000_000).padStart(6, "0").replace(/0+$/, "");
  return `${sign}${frac === "" ? String(whole) : `${whole}.${frac}`}`;
}

/** Minor units with the sign made explicit — the bills page's signed amounts. */
export function signedCredits(minor: number): string {
  const text = credits(minor);
  return minor > 0 ? `+${text}` : text;
}

/**
 * The org's display currency as the API answered it (issue #257). `null` —
 * or a missing org read — renders credits alone. The conversion is a pure
 * presentation: minor units are still the ledger's truth everywhere.
 */
export type DisplayCurrency = { code: string; ratePerCredit: string; decimals: number };

/**
 * What `minor` displays as in the currency, or `null` when the org renders
 * credits only. Decimal-string math (never a float): the rate is mantissa /
 * 10^scale, the render rounds half-up to `decimals` places.
 */
export function displayAmount(minor: number, currency: DisplayCurrency): string {
  const [whole, frac = ""] = currency.ratePerCredit.split(".");
  const mantissa = BigInt(whole + frac);
  const rateScale = frac.length;
  const renderScale = currency.decimals;
  const numerator = BigInt(minor) * mantissa * 10n ** BigInt(renderScale);
  const denominator = 10n ** BigInt(6 + rateScale); // 1_000_000 minor × rate scale
  const negative = numerator < 0n;
  const magnitude = negative ? -numerator : numerator;
  const rounded = (magnitude + denominator / 2n) / denominator;
  const unit = 10n ** BigInt(renderScale);
  const w = rounded / unit;
  const f = rounded % unit;
  const sign = negative && rounded > 0n ? "-" : "";
  return renderScale === 0
    ? `${sign}${w}`
    : `${sign}${w}.${String(f).padStart(renderScale, "0")}`;
}

/**
 * The same amount in both units: `"1.5 cr ≈ 0.01 USD"`, or `"1.5 cr"` when
 * the org renders credits only. The fiat half is presentational — a bill's
 * proof covers the credits, never the conversion.
 */
export function creditsWithDisplay(minor: number, currency: DisplayCurrency | null | undefined): string {
  const cr = credits(minor);
  return currency ? `${cr} cr ≈ ${displayAmount(minor, currency)} ${currency.code}` : `${cr} cr`;
}

/**
 * One money amount, rendered as the page shows it: the credits figure, with
 * the fiat conversion as a quiet second line when the org displays in one.
 * `minor` is the ledger's integer; the conversion never feeds a charge.
 */
export function Cr(props: { minor: number; currency?: DisplayCurrency | null }) {
  return (
    <span className="tabular-nums">
      {credits(props.minor)} cr
      {props.currency ? (
        <span className="text-muted">
          {" "}
          ≈ {displayAmount(props.minor, props.currency)} {props.currency.code}
        </span>
      ) : null}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Surfaces — the product substrate: tinted page, hairline border, whisper
// shadow, one locked radius scale (card xl / controls lg / pill full).
// ---------------------------------------------------------------------------

const SURFACE =
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

// ---------------------------------------------------------------------------
// States — loading / empty / error placeholders (skeleton rows live in #414).
// ---------------------------------------------------------------------------

export function PageState(props: { children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-4 py-10 text-center">
      <p className="text-sm text-muted">{props.children}</p>
    </div>
  );
}

export function EmptyRows(props: { colSpan: number; children: ReactNode }) {
  return (
    <tr>
      <td colSpan={props.colSpan} className="px-4 py-10 text-center text-sm text-muted">
        {props.children}
      </td>
    </tr>
  );
}

// ---------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------

export function TableShell(props: { head: ReactNode[]; children: ReactNode }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-[13px]">
        <thead>
          <tr className="border-b border-line text-left text-[11.5px] font-semibold uppercase tracking-wider text-muted">
            {props.head.map((h, i) => (
              <th key={i} className="px-4 py-2.5">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className="[&>tr]:border-b [&>tr]:border-line [&>tr:last-child]:border-0 [&>tr>td]:px-4 [&>tr>td]:py-2.5 [&>tr:hover]:bg-canvas/60 [&>tr:hover>td]:bg-canvas/60">
          {props.children}
        </tbody>
      </table>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Badges & status
// ---------------------------------------------------------------------------

/** A status word as the pages color it: settled green, refused red, the rest muted. */
export function StatusBadge(props: { status: string }) {
  const cls =
    props.status === "settled"
      ? "text-success"
      : props.status === "refused" || props.status === "errored"
        ? "text-danger"
        : props.status === "credited" || props.status === "confirmed"
          ? "text-success"
          : "text-muted";
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs font-medium ${cls}`}>
      <span className="inline-block h-1.5 w-1.5 rounded-full bg-current" />
      {props.status}
    </span>
  );
}

export function Tag(props: { children: ReactNode; accent?: boolean }) {
  return (
    <span
      className={`inline-block rounded-md px-1.5 py-0.5 text-[11px] font-medium ${
        props.accent
          ? "bg-primary-soft text-primary"
          : "bg-secondary text-muted-foreground"
      }`}
    >
      {props.children}
    </span>
  );
}

export function Badge(props: {
  children: ReactNode;
  /** `variant` kept as an alias of `tone` for the badge's semantic colors. */
  variant?: "default" | "secondary" | "success" | "danger" | "destructive" | "outline";
  tone?: "default" | "secondary" | "success" | "danger" | "outline";
  live?: boolean;
  className?: string;
}) {
  const raw = props.tone ?? props.variant ?? "default";
  const tone = raw === "destructive" ? "danger" : raw;
  const cls =
    tone === "secondary"
      ? "bg-secondary text-muted-foreground"
      : tone === "success"
        ? "bg-success/10 text-success"
        : tone === "danger"
          ? "bg-danger/10 text-danger"
          : tone === "outline"
            ? "border border-line text-ink"
            : "bg-primary-soft text-primary";
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center gap-1.5 rounded-full px-2 text-[11px] font-medium leading-none",
        cls,
        props.className,
      )}
    >
      {props.live ? (
        <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-current motion-reduce:animate-none" />
      ) : null}
      {props.children}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Banner — page/form level feedback.
// ---------------------------------------------------------------------------

export function Banner(props: { tone?: "info" | "warn" | "error" | "success"; children: ReactNode }) {
  const tone = props.tone ?? "info";
  const cls =
    tone === "warn"
      ? "border-pending/40 bg-pending/10 text-pending"
      : tone === "error"
        ? "border-danger/40 bg-danger/10 text-danger"
        : tone === "success"
          ? "border-success/40 bg-success/10 text-success"
          : "border-primary/40 bg-primary-soft text-primary";
  return <p className={`rounded-lg border px-3.5 py-2.5 text-sm ${cls}`}>{props.children}</p>;
}

// ---------------------------------------------------------------------------
// Form — one control vocabulary, aligned radius/height/states.
// ---------------------------------------------------------------------------

export function Field(props: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1.5 text-sm">
      <span className="text-xs font-medium text-muted">{props.label}</span>
      {props.children}
      {props.hint ? <span className="text-xs text-muted">{props.hint}</span> : null}
    </label>
  );
}

const CONTROL =
  "rounded-lg border border-input bg-surface px-3 py-2 text-sm text-ink placeholder:text-muted " +
  "transition-[border-color,box-shadow] duration-150 ease-out " +
  "hover:border-line focus:border-primary focus:outline-none focus:ring-2 focus:ring-ring/30 " +
  "disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-danger";

export function TextInput(props: React.InputHTMLAttributes<HTMLInputElement> & { trailingSlot?: ReactNode }) {
  const { trailingSlot, className, ...rest } = props;
  if (trailingSlot == null) {
    return <input {...rest} className={cn(CONTROL, className)} />;
  }
  return (
    <div className={cn(CONTROL, "flex items-center gap-2 px-3 py-0", className)}>
      <input
        {...rest}
        className="min-w-0 flex-1 border-0 bg-transparent py-2 text-sm outline-none placeholder:text-muted"
      />
      <span className="shrink-0 text-muted">{trailingSlot}</span>
    </div>
  );
}

export function NumberInput(props: React.InputHTMLAttributes<HTMLInputElement>) {
  return <input type="number" step="any" {...props} className={cn(CONTROL, props.className)} />;
}

export function Textarea(props: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      {...props}
      className={cn(CONTROL, "w-full resize-y leading-relaxed", props.className)}
    />
  );
}

export function Button(
  props: React.ButtonHTMLAttributes<HTMLButtonElement> & {
    tone?: "primary" | "ghost" | "danger";
    isLoading?: boolean;
    loadingText?: string;
  },
) {
  const { tone = "primary", isLoading, loadingText, children, disabled, ...rest } = props;
  const cls =
    tone === "ghost"
      ? "border border-line bg-surface text-ink hover:bg-canvas"
      : tone === "danger"
        ? "border border-danger bg-danger text-white hover:bg-danger-hover"
        : "bg-primary text-white shadow-[0_2px_8px_rgba(45,91,216,.28)] hover:bg-primary-hover";
  return (
    <button
      {...rest}
      aria-busy={isLoading || undefined}
      disabled={disabled || isLoading}
      className={cn(
        "inline-flex items-center justify-center gap-2 rounded-lg px-3.5 py-2 text-sm font-medium transition-[background-color,border-color,box-shadow,opacity] duration-150 disabled:cursor-not-allowed disabled:opacity-50",
        cls,
        props.className,
      )}
    >
      {isLoading ? (
        <>
          <Spinner className="size-3.5" />
          {loadingText ?? children}
        </>
      ) : (
        children
      )}
    </button>
  );
}

function Spinner({ className }: { className?: string }) {
  return (
    <svg aria-hidden className={cn("animate-spin", className)} viewBox="0 0 24 24" fill="none">
      <circle className="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="2.5" />
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="2.5" strokeDasharray="14 42" strokeLinecap="round" />
    </svg>
  );
}

export function IconButton(props: React.ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  const { label, children, className, ...rest } = props;
  return (
    <button
      {...rest}
      aria-label={label}
      title={label}
      className={cn(
        "inline-flex h-8 w-8 items-center justify-center rounded-lg border border-line bg-surface text-muted transition-colors hover:bg-canvas hover:text-ink",
        className,
      )}
    >
      {children}
    </button>
  );
}

// ---------------------------------------------------------------------------
// Select — a plain native select for form fields; `Select` (below) is the
// custom dropdown for value-pickers that need a composed trigger/popup.
// ---------------------------------------------------------------------------

export function NativeSelect(props: React.SelectHTMLAttributes<HTMLSelectElement>) {
  return <select {...props} className={cn(CONTROL, props.className)} />;
}

// Back-compat alias — pages still call it `Select`; it renders the native control.
export const Select = NativeSelect;

// ---------------------------------------------------------------------------
// Tabs — Base UI primitives, product skin. Underline + pill variants.
// ---------------------------------------------------------------------------

export function Tabs({ className, ...props }: TabsPrimitive.Root.Props) {
  return (
    <TabsPrimitive.Root
      {...props}
      className={cn("grid w-full min-w-0 gap-3", className)}
    />
  );
}

export function TabsList({
  className,
  variant = "underline",
  ...props
}: TabsPrimitive.List.Props & { variant?: "underline" | "pill" }) {
  return (
    <TabsPrimitive.List
      {...props}
      className={cn(
        "relative flex w-fit min-w-0 items-center gap-1",
        variant === "underline" && "w-full gap-5 border-b border-line pb-px",
        variant === "pill" && "rounded-full border border-line bg-canvas p-1",
        className,
      )}
      data-variant={variant}
    />
  );
}

export function TabsTrigger({ className, ...props }: TabsPrimitive.Tab.Props) {
  return (
    <TabsPrimitive.Tab
      {...props}
      className={cn(
        "relative inline-flex min-h-9 items-center justify-center whitespace-nowrap rounded-lg px-3 py-1.5 text-sm font-medium text-muted outline-none transition-colors duration-150",
        "hover:text-ink focus-visible:ring-2 focus-visible:ring-ring/30",
        "disabled:pointer-events-none disabled:opacity-50",
        "data-active:text-ink",
        "group-data-[variant=pill]:rounded-full group-data-[variant=pill]:px-4",
        className,
      )}
    />
  );
}

export function TabsIndicator({
  className,
  variant = "underline",
  ...props
}: TabsPrimitive.Indicator.Props & { variant?: "underline" | "pill" }) {
  return (
    <TabsPrimitive.Indicator
      {...props}
      className={cn(
        "pointer-events-none absolute z-0",
        variant === "underline" &&
          "-bottom-px left-(--active-tab-left) h-0.5 w-(--active-tab-width) rounded-full bg-primary transition-[left,width] duration-200 ease-out motion-reduce:transition-none",
        variant === "pill" &&
          "top-(--active-tab-top) left-(--active-tab-left) h-(--active-tab-height) w-(--active-tab-width) rounded-full bg-surface shadow-[0_1px_2px_rgba(20,34,64,.1)] ring-1 ring-line transition-[left,width,top,height] duration-200 ease-out motion-reduce:transition-none",
        className,
      )}
    />
  );
}

export function TabsContent({ className, ...props }: TabsPrimitive.Panel.Props) {
  return (
    <TabsPrimitive.Panel
      {...props}
      className={cn("w-full min-w-0 outline-none", className)}
    />
  );
}

// ---------------------------------------------------------------------------
// Switch — Base UI primitive, product skin (no underglow).
// ---------------------------------------------------------------------------

const switchSpring = { type: "spring" as const, bounce: 0.1, damping: 30, stiffness: 400 };

export function Switch({
  className,
  size = "default",
  ...props
}: SwitchPrimitive.Root.Props & { size?: "sm" | "default" }) {
  const reduceMotion = useReducedMotion() ?? false;
  const trackSize =
    size === "sm" ? "h-[18px] w-[32px]" : "h-[22px] w-[40px]";
  const thumbSize = size === "sm" ? "h-3 w-3" : "h-3.5 w-3.5";
  return (
    <SwitchPrimitive.Root
      {...props}
      className={cn(
        "group/switch relative inline-flex shrink-0 cursor-pointer items-center rounded-full border border-line px-[3px] outline-none",
        "bg-canvas transition-[background-color,border-color,box-shadow] duration-150",
        "data-checked:border-primary/40 data-checked:bg-primary",
        "focus-visible:ring-2 focus-visible:ring-ring/30",
        "disabled:cursor-not-allowed disabled:opacity-50",
        "after:absolute after:-inset-x-2 after:-inset-y-2 after:content-['']", // ≥44px hit slop
        trackSize,
        className,
      )}
    >
      <SwitchPrimitive.Thumb
        render={(thumbProps, thumbState) => {
          const {
            style: thumbStyle,
            onDrag: _d1, onDragStart: _d2, onDragEnd: _d3,
            onAnimationStart: _a1, onAnimationEnd: _a2,
            ...restThumb
          } = thumbProps;
          const { transform: _t, ...restStyle } = (thumbStyle ?? {}) as React.CSSProperties;
          return (
            <motion.span
              {...restThumb}
              animate={{ x: thumbState.checked ? (size === "sm" ? 14 : 18) : 0 }}
              className={cn(
                "pointer-events-none block rounded-full bg-surface shadow-[0_1px_2px_rgba(20,34,64,.18)] ring-1 ring-black/5",
                thumbSize,
                "data-checked:bg-white",
              )}
              initial={false}
              style={restStyle}
              transition={reduceMotion ? { duration: 0 } : switchSpring}
            />
          );
        }}
      />
    </SwitchPrimitive.Root>
  );
}

// ---------------------------------------------------------------------------
// Progress — Base UI primitive, product skin.
// ---------------------------------------------------------------------------

export function Progress({
  className,
  label,
  showValue,
  value,
  min = 0,
  max = 100,
  ...rootProps
}: Omit<ProgressPrimitive.Root.Props, "children"> & {
  label?: ReactNode;
  showValue?: boolean;
}) {
  const reduceMotion = useReducedMotion() ?? false;
  const isIndeterminate = value === null;
  const pct =
    value === null || !Number.isFinite(value)
      ? 0
      : Math.min(1, Math.max(0, (value - min) / Math.max(1e-9, max - min)));

  return (
    <ProgressPrimitive.Root
      {...rootProps}
      className={cn("flex w-full min-w-[8rem] flex-col gap-1.5", className)}
      max={max}
      min={min}
      value={value}
    >
      {(label != null || showValue) && (
        <div className="flex items-baseline justify-between gap-3">
          {label == null ? (
            <span className="sr-only">Progress</span>
          ) : (
            <ProgressPrimitive.Label className="text-xs font-medium text-ink">
              {label}
            </ProgressPrimitive.Label>
          )}
          {showValue ? (
            <ProgressPrimitive.Value className="text-xs text-muted tabular-nums" />
          ) : null}
        </div>
      )}
      <ProgressPrimitive.Track className="relative h-2 w-full overflow-hidden rounded-full border border-line bg-canvas">
        {isIndeterminate ? (
          <motion.div
            aria-hidden
            className="absolute inset-y-0 left-0 w-2/5 rounded-full bg-primary"
            animate={reduceMotion ? { x: "0%" } : { x: ["-45%", "145%"] }}
            transition={{
              duration: reduceMotion ? 0 : 1.15,
              ease: "easeInOut",
              repeat: reduceMotion ? 0 : Number.POSITIVE_INFINITY,
            }}
          />
        ) : (
          <motion.div
            aria-hidden
            className="absolute inset-y-0 left-0 w-full origin-left rounded-full bg-primary"
            animate={{ scaleX: pct }}
            initial={false}
            transition={reduceMotion ? { duration: 0 } : { duration: 0.3, ease: "easeOut" }}
          />
        )}
      </ProgressPrimitive.Track>
    </ProgressPrimitive.Root>
  );
}

// ---------------------------------------------------------------------------
// Composed select — Base UI dropdown for pickers (model, channel, etc.).
// ---------------------------------------------------------------------------

export function SelectRoot({
  children,
  className,
  ...props
}: SelectPrimitive.Root.Props<string, false> & { className?: string; placeholder?: string }) {
  return (
    <div className={cn("inline-block min-w-0", className)}>
      <SelectPrimitive.Root {...props}>{children}</SelectPrimitive.Root>
    </div>
  );
}

export function SelectTrigger({
  className,
  children,
  ...props
}: SelectPrimitive.Trigger.Props) {
  return (
    <SelectPrimitive.Trigger
      {...props}
      className={cn(
        "inline-flex min-h-9 items-center justify-between gap-2 rounded-lg border border-input bg-surface px-3 py-1.5 text-sm text-ink outline-none transition-[border-color,box-shadow] duration-150",
        "hover:border-line focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-ring/30",
        "disabled:cursor-not-allowed disabled:opacity-50 data-placeholder:text-muted",
        className,
      )}
    >
      {children}
      <SelectPrimitive.Icon render={<ChevronDown className="size-4 text-muted" />} />
    </SelectPrimitive.Trigger>
  );
}

export function SelectValue({ className, ...props }: SelectPrimitive.Value.Props) {
  return (
    <SelectPrimitive.Value
      {...props}
      className={cn("flex flex-1 truncate text-left", className)}
    />
  );
}

export function SelectContent({
  className,
  children,
  side = "bottom",
  sideOffset = 4,
  align = "start",
  ...props
}: SelectPrimitive.Popup.Props &
  Pick<SelectPrimitive.Positioner.Props, "align" | "side" | "sideOffset" | "alignItemWithTrigger">) {
  return (
    <SelectPrimitive.Portal>
      <SelectPrimitive.Positioner
        side={side}
        sideOffset={sideOffset}
        align={align}
        alignItemWithTrigger
        className="isolate z-50"
      >
        <SelectPrimitive.Popup
          {...props}
          className={cn(
            "isolate z-50 max-h-72 w-(--anchor-width) min-w-(--anchor-width) overflow-y-auto rounded-xl border border-line bg-surface shadow-[0_8px_24px_rgba(20,34,64,.12)]",
            "data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95 data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95",
            "data-[side=bottom]:slide-in-from-top-2 duration-150 ease-out",
            className,
          )}
        >
          <SelectPrimitive.List className="p-1">{children}</SelectPrimitive.List>
        </SelectPrimitive.Popup>
      </SelectPrimitive.Positioner>
    </SelectPrimitive.Portal>
  );
}

export function SelectItem({ className, children, ...props }: SelectPrimitive.Item.Props) {
  return (
    <SelectPrimitive.Item
      {...props}
      className={cn(
        "relative flex min-h-9 w-full cursor-default select-none items-center gap-2 rounded-lg px-2.5 py-2 text-sm text-ink outline-none",
        "transition-colors data-highlighted:bg-primary-soft data-highlighted:text-primary",
        "data-disabled:pointer-events-none data-disabled:opacity-50",
        className,
      )}
    >
      <SelectPrimitive.ItemText className="min-w-0 flex-1 truncate">{children}</SelectPrimitive.ItemText>
      <SelectPrimitive.ItemIndicator className="flex shrink-0 text-primary">
        <Check className="h-3.5 w-3.5" strokeWidth={2.5} />
      </SelectPrimitive.ItemIndicator>
    </SelectPrimitive.Item>
  );
}

// ---------------------------------------------------------------------------
// Copy button — ghost icon, clipboard + check feedback.
// ---------------------------------------------------------------------------

function iconMotion(reduceMotion: boolean) {
  if (reduceMotion) {
    return {
      initial: { opacity: 1, scale: 1, filter: "blur(0px)" },
      animate: { opacity: 1, scale: 1, filter: "blur(0px)" },
      exit: { opacity: 1, scale: 1, filter: "blur(0px)" },
      transition: { duration: 0 },
    };
  }
  return {
    initial: { opacity: 0, scale: 0.5, filter: "blur(3px)" },
    animate: { opacity: 1, scale: 1, filter: "blur(0px)" },
    exit: { opacity: 0, scale: 0.5, filter: "blur(3px)" },
    transition: { type: "spring" as const, duration: 0.25, bounce: 0 },
  };
}

export async function copyToClipboard(value: string): Promise<boolean> {
  if (typeof window === "undefined" || !value) return false;
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(value);
      return true;
    } catch {
      /* fall through */
    }
  }
  const ta = document.createElement("textarea");
  ta.value = value;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  const ok = document.execCommand("copy");
  document.body.removeChild(ta);
  return ok;
}

export function CopyButton({
  value,
  className,
  ...props
}: React.ComponentProps<"button"> & { value: string }) {
  const [copied, setCopied] = useState(false);
  const reduceMotion = useReducedMotion() ?? false;
  const m = iconMotion(reduceMotion);
  return (
    <button
      {...props}
      type="button"
      aria-label={copied ? "Copied" : "Copy"}
      data-copied={copied}
      className={cn(
        "relative inline-flex h-7 w-7 items-center justify-center rounded-md text-muted transition-colors hover:bg-canvas hover:text-ink",
        className,
      )}
      onClick={async () => {
        if (await copyToClipboard(value)) {
          setCopied(true);
          setTimeout(() => setCopied(false), 1800);
        }
      }}
    >
      <AnimatePresence initial={false} mode="sync">
        {copied ? (
          <motion.span key="check" className="absolute inset-0 flex items-center justify-center text-success" {...m}>
            <Check className="size-3.5" strokeWidth={2.5} />
          </motion.span>
        ) : (
          <motion.span key="copy" className="absolute inset-0 flex items-center justify-center" {...m}>
            <Copy className="size-3.5" strokeWidth={2} />
          </motion.span>
        )}
      </AnimatePresence>
    </button>
  );
}

// ---------------------------------------------------------------------------
// Code block — bordered <pre> + copy button, token-colored.
// ---------------------------------------------------------------------------

export function CodeBlock({
  code,
  language = "text",
  className,
}: {
  code: string;
  language?: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  return (
    <div className={cn("group relative overflow-hidden rounded-lg border border-line bg-canvas", className)}>
      <button
        type="button"
        aria-label="Copy code"
        onClick={async () => {
          if (await copyToClipboard(code)) {
            setCopied(true);
            setTimeout(() => setCopied(false), 1800);
          }
        }}
        className="absolute right-2 top-2 z-10 flex items-center gap-1 rounded-md border border-line bg-surface/90 px-2 py-1 text-xs text-muted opacity-0 backdrop-blur-sm transition-opacity hover:bg-canvas hover:text-ink focus-visible:opacity-100 group-hover:opacity-100"
      >
        {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
        {copied ? "Copied" : "Copy"}
      </button>
      <pre
        data-language={language}
        className="mono m-0 max-h-96 overflow-auto p-4 text-xs leading-relaxed text-ink"
      >
        <code>{code}</code>
      </pre>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Rolling number — KPI ticker, RM-gated.
// ---------------------------------------------------------------------------

import { useSpring, useTransform } from "motion/react";

export function RollingNumber({
  value,
  stiffness = 75,
  damping = 15,
  mass = 0.8,
  precision = 0,
  format = (n: number) => n.toLocaleString(),
}: {
  value: number;
  stiffness?: number;
  damping?: number;
  mass?: number;
  precision?: number;
  format?: (value: number) => string;
}) {
  const reduceMotion = useReducedMotion() ?? false;
  const spring = useSpring(value, { mass, stiffness, damping });
  const display = useTransform(spring, (c) => format(parseFloat(c.toFixed(precision))));
  // Drive the spring from an effect so we never call .set/.jump during render.
  useEffect(() => {
    if (reduceMotion) {
      spring.jump(value);
    } else {
      spring.set(value);
    }
  }, [spring, value, reduceMotion]);
  return <motion.span>{display}</motion.span>;
}

// ---------------------------------------------------------------------------
// Typewriter — empty-state text cycle, RM-gated.
// ---------------------------------------------------------------------------

import { animate, useMotionValue } from "motion/react";

export function Typewriter({ texts, className }: { texts: string[]; delay?: number; className?: string }) {
  const reduceMotion = useReducedMotion() ?? false;
  const [idx, setIdx] = useState(0);
  const count = useMotionValue(0);
  const rounded = useTransform(count, (v) => Math.round(v));
  const shown = useTransform(rounded, (v) => (texts[idx] ?? "").slice(0, v));

  useEffect(() => {
    if (reduceMotion) {
      setIdx(0);
      return;
    }
    const text = texts[idx] ?? "";
    const controls = animate(count, text.length, {
      type: "tween",
      duration: Math.max(0.6, text.length * 0.03),
      ease: "easeOut",
      onComplete: () => {
        window.setTimeout(() => {
          count.set(0);
          setIdx((i) => (i + 1) % texts.length);
        }, 1600);
      },
    });
    return () => controls.stop();
  }, [idx, texts, count, reduceMotion]);

  if (reduceMotion) {
    return <span className={className}>{texts[0]}</span>;
  }
  return (
    <span className={className}>
      <motion.span>{shown}</motion.span>
      <span className="ml-0.5 inline-block h-[1em] w-px animate-pulse bg-current align-[-0.15em] motion-reduce:hidden" />
    </span>
  );
}
