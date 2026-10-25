import type { ReactNode } from "react";
import { RollingNumber } from "./cult/rolling-number";

// Money is integer minor units, 1 credit = 1_000_000 — never float math.
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

/* ---------- KPI tiles ---------- */

export function Kpi(props: { label: string; value: ReactNode; hint?: string }) {
  return (
    <div className="rounded-xl border border-line bg-surface p-4 shadow-[0_1px_3px_rgba(20,34,64,.05)]">
      <p className="text-xs font-medium text-muted">{props.label}</p>
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
    <div className="rounded-xl border border-line bg-surface p-4 shadow-[0_1px_3px_rgba(20,34,64,.05)]">
      <div className="flex items-center justify-between">
        <p className="text-xs font-medium text-muted">{props.label}</p>
        {props.icon ? (
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary-soft text-primary">
            {props.icon}
          </span>
        ) : null}
      </div>
      <p className="mt-1 text-2xl font-bold tabular-nums tracking-tight text-ink">
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

/* ---------- Card ---------- */

export function Card(props: { title?: string; actions?: ReactNode; children: ReactNode; flush?: boolean }) {
  return (
    <section className="rounded-xl border border-line bg-surface shadow-[0_1px_3px_rgba(20,34,64,.05)]">
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

/* ---------- States ---------- */

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

/* ---------- Table ---------- */

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
        <tbody className="[&>tr]:border-b [&>tr]:border-line [&>tr:last-child]:border-0 [&>tr>td]:px-4 [&>tr>td]:py-2.5">
          {props.children}
        </tbody>
      </table>
    </div>
  );
}

/* ---------- Badges ---------- */

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

/* ---------- Banner ---------- */

export function Banner(props: { tone?: "info" | "warn" | "error" | "success"; children: ReactNode }) {
  const tone = props.tone ?? "info";
  const cls =
    tone === "warn"
      ? "border-pending/40 bg-amber-50 text-pending"
      : tone === "error"
        ? "border-danger/40 bg-red-50 text-danger"
        : tone === "success"
          ? "border-success/40 bg-green-50 text-success"
          : "border-primary/40 bg-primary-soft text-primary";
  return <p className={`rounded-lg border px-3.5 py-2.5 text-sm ${cls}`}>{props.children}</p>;
}

/* ---------- Form ---------- */

export function Field(props: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1.5 text-sm">
      <span className="text-xs font-medium text-muted">{props.label}</span>
      {props.children}
      {props.hint ? <span className="text-xs text-muted">{props.hint}</span> : null}
    </label>
  );
}

const INPUT =
  "rounded-lg border border-input bg-surface px-3 py-2 text-sm text-ink placeholder:text-muted focus:border-primary focus:outline-none focus:ring-2 focus:ring-ring/30";

export function TextInput(props: React.InputHTMLAttributes<HTMLInputElement>) {
  return <input {...props} className={`${INPUT} ${props.className ?? ""}`} />;
}

export function NumberInput(props: React.InputHTMLAttributes<HTMLInputElement>) {
  return <input type="number" step="any" {...props} className={`${INPUT} ${props.className ?? ""}`} />;
}

export function Select(props: React.SelectHTMLAttributes<HTMLSelectElement>) {
  return <select {...props} className={`${INPUT} ${props.className ?? ""}`} />;
}

export function Button(props: React.ButtonHTMLAttributes<HTMLButtonElement> & { tone?: "primary" | "ghost" | "danger" }) {
  const tone = props.tone ?? "primary";
  const cls =
    tone === "ghost"
      ? "border border-line bg-surface text-ink hover:bg-canvas"
      : tone === "danger"
        ? "border border-danger bg-danger text-white hover:bg-danger-hover"
        : "bg-primary text-white shadow-[0_2px_8px_rgba(45,91,216,.3)] hover:bg-primary-hover";
  return (
    <button
      {...props}
      className={`rounded-lg px-3.5 py-2 text-sm font-medium transition-colors disabled:opacity-50 ${cls} ${props.className ?? ""}`}
    />
  );
}
