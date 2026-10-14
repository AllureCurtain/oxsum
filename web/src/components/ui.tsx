import type { ReactNode } from "react";

// Money is integer minor units, 1 credit = 1_000_000 — never float math.
export function credits(minor: number): string {
  const sign = minor < 0 ? "-" : "";
  const abs = Math.abs(minor);
  const whole = Math.trunc(abs / 1_000_000);
  const frac = String(abs % 1_000_000).padStart(6, "0").replace(/0+$/, "");
  return `${sign}${frac === "" ? String(whole) : `${whole}.${frac}`}`;
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

export function Card(props: { title?: string; children: ReactNode }) {
  return (
    <section className="rounded-md border border-line bg-surface">
      {props.title ? (
        <h2 className="border-b border-line px-4 py-3 text-base font-semibold">{props.title}</h2>
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
