import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

// ---------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------

/**
 * A column header. Pass a plain string for a left-aligned text column, or an
 * object to declare alignment and sort. `mono`/`amount` columns right-align by
 * the product contract; `sort` is only set where the backend can actually
 * order — a header without a sorter never fakes one.
 */
export type Column =
  | string
  | {
      label: ReactNode;
      /** Alignment — `right` for amounts/dates/ids, `left` for text. */
      align?: "left" | "right" | "center";
      /** Sort state; `onSort` toggles. Only where the API can order. */
      sort?: { active: boolean; ascending: boolean; onSort: () => void };
      /** Columns with long mono content (ids, hashes) get a max-width + truncate. */
      truncate?: boolean;
    };

function colMeta(c: Column): { label: ReactNode; align: "left" | "right" | "center"; sort?: { active: boolean; ascending: boolean; onSort: () => void }; truncate: boolean } {
  if (typeof c === "string") return { label: c, align: "left", truncate: false };
  return { label: c.label, align: c.align ?? "left", sort: c.sort, truncate: c.truncate ?? false };
}

/**
 * A body cell that keeps a long value inside its column: `truncate` +
 * `title` (the full string on hover), `mono` for the monospace face,
 * `align` matching the column contract. Use it on id/hash/description cells
 * so one long value can never push the whole table into overflow.
 */
export function Td(props: {
  children: ReactNode;
  mono?: boolean;
  align?: "left" | "right" | "center";
  truncate?: boolean;
  colSpan?: number;
  className?: string;
}) {
  const text = typeof props.children === "string" ? props.children : undefined;
  const alignCls = props.align === "right" ? "text-right" : props.align === "center" ? "text-center" : "";
  return (
    <td
      colSpan={props.colSpan}
      title={props.truncate ? text : undefined}
      className={cn(
        props.mono ? "mono" : "",
        alignCls,
        props.truncate ? "max-w-40 truncate" : "",
        props.className,
      )}
    >
      {props.children}
    </td>
  );
}

/** Placeholder rows while a table loads — same height as real rows, not a
 *  centered spinner, so the page doesn't jump when data lands. */
export function SkeletonRows(props: { columns: number; rows?: number }) {
  return (
    <>
      {Array.from({ length: props.rows ?? 5 }).map((_, r) => (
        <tr key={r} aria-hidden>
          {Array.from({ length: props.columns }).map((__, c) => (
            <td key={c} className="px-4">
              <div
                className="h-3 animate-pulse rounded bg-line"
                style={{ width: `${[64, 82, 48, 70, 56][(r * 3 + c) % 5]}%` }}
              />
            </td>
          ))}
        </tr>
      ))}
    </>
  );
}

export function TableShell(props: {
  head: Column[];
  children: ReactNode;
  /** Row density — `compact` (32–36px) for dense admin sections, `normal` (40px) default. */
  density?: "normal" | "compact";
  /** Render this many skeleton rows instead of `children` while loading. */
  loading?: number;
  /** Sticky header on scroll — default on. Set false for short tables. */
  sticky?: boolean;
}) {
  const density = props.density ?? "normal";
  const sticky = props.sticky ?? true;
  const cols = props.head.map(colMeta);
  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-[13px]">
        <thead
          className={sticky ? "sticky z-10 bg-surface" : undefined}
          style={sticky ? { top: "var(--topbar-h)" } : undefined}
        >
          <tr className="border-b border-line text-[11.5px] font-semibold uppercase tracking-wider text-muted shadow-[inset_0_-1px_0_var(--color-line)]">
            {cols.map((h, i) => (
              <th
                key={i}
                scope="col"
                aria-sort={
                  h.sort ? (h.sort.ascending ? "ascending" : "descending") : undefined
                }
                className={cn(
                  "bg-surface px-4 py-2.5",
                  h.align === "right" ? "text-right" : h.align === "center" ? "text-center" : "text-left",
                )}
              >
                {h.sort ? (
                  <button
                    type="button"
                    onClick={h.sort.onSort}
                    className={cn(
                      "inline-flex items-center gap-1 uppercase tracking-wider hover:text-ink",
                      h.sort.active ? "text-ink" : "",
                    )}
                  >
                    {h.label}
                    <span aria-hidden>{h.sort.active ? (h.sort.ascending ? "↑" : "↓") : "↕"}</span>
                  </button>
                ) : (
                  h.label
                )}
              </th>
            ))}
          </tr>
        </thead>
        <tbody
          className={cn(
            "[&>tr]:border-b [&>tr]:border-line [&>tr:last-child]:border-0 [&>tr>td]:px-4 [&>tr:hover]:bg-canvas/60 [&>tr:hover>td]:bg-canvas/60",
            density === "compact" ? "[&>tr>td]:py-2" : "[&>tr>td]:py-2.5",
          )}
        >
          {props.loading ? <SkeletonRows columns={props.head.length} rows={props.loading} /> : props.children}
        </tbody>
      </table>
    </div>
  );
}
