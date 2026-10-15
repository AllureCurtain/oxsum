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

export function Kpi(props: { label: string; value: ReactNode; hint?: string }) {
  return (
    <div className="rounded-md border border-line bg-surface p-4">
      <p className="text-xs text-muted">{props.label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums">{props.value}</p>
      {props.hint ? <p className="mt-1 text-xs text-muted">{props.hint}</p> : null}
    </div>
  );
}

/**
 * A KPI whose value is a credit amount in minor units — the number rolls when it
 * changes (live billing updates land here through /ws/billing).
 */
export function AnimatedKpi(props: { label: string; minor: number; hint?: string }) {
  return (
    <div className="rounded-md border border-line bg-surface p-4">
      <p className="text-xs text-muted">{props.label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums">
        <RollingNumber
          value={props.minor / 1_000_000}
          precision={6}
          format={(v) => `${credits(Math.round(v * 1_000_000))}`}
        />{" "}
        <span className="text-sm font-normal text-muted">cr</span>
      </p>
      {props.hint ? <p className="mt-1 text-xs text-muted">{props.hint}</p> : null}
    </div>
  );
}

export function Card(props: { title?: string; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="rounded-md border border-line bg-surface">
      {props.title ? (
        <div className="flex items-center justify-between border-b border-line px-4 py-3">
          <h2 className="text-base font-semibold">{props.title}</h2>
          {props.actions}
        </div>
      ) : null}
      <div className="p-4">{props.children}</div>
    </section>
  );
}

export function PageState(props: { children: ReactNode }) {
  return <p className="px-2 py-8 text-center text-muted">{props.children}</p>;
}

export function EmptyRows(props: { colSpan: number; children: ReactNode }) {
  return (
    <tr>
      <td colSpan={props.colSpan} className="px-3 py-8 text-center text-muted">
        {props.children}
      </td>
    </tr>
  );
}

export function TableShell(props: { head: ReactNode[]; children: ReactNode }) {
  return (
    <div className="overflow-x-auto rounded-md border border-line bg-surface">
      <table className="w-full border-collapse text-sm">
        <thead>
          <tr className="border-b border-line text-left text-xs text-muted">
            {props.head.map((h, i) => (
              <th key={i} className="px-3 py-2 font-medium">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className="[&>tr]:border-b [&>tr]:border-line [&>tr:last-child]:border-0 [&>tr>td]:px-3 [&>tr>td]:py-2">
          {props.children}
        </tbody>
      </table>
    </div>
  );
}

/** A status word as the pages color it: settled green, refused red, the rest muted. */
export function StatusBadge(props: { status: string }) {
  const color =
    props.status === "settled"
      ? "text-success"
      : props.status === "refused" || props.status === "errored"
        ? "text-danger"
        : "text-muted";
  return <span className={`text-xs font-medium ${color}`}>{props.status}</span>;
}

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
  return <p className={`rounded-md border px-3 py-2 text-sm ${cls}`}>{props.children}</p>;
}

export function Field(props: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="text-xs font-medium text-muted">{props.label}</span>
      {props.children}
      {props.hint ? <span className="text-xs text-muted">{props.hint}</span> : null}
    </label>
  );
}

const INPUT =
  "rounded-md border border-line bg-surface px-2.5 py-1.5 text-sm text-ink placeholder:text-muted focus:border-primary";

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
        : "bg-primary text-white hover:bg-primary-hover";
  return (
    <button
      {...props}
      className={`rounded-md px-3 py-1.5 text-sm font-medium disabled:opacity-50 ${cls} ${props.className ?? ""}`}
    />
  );
}
