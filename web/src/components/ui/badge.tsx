import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

// ---------------------------------------------------------------------------
// Badges & status
// ---------------------------------------------------------------------------

/** A status word as the pages color it: settled green, refused red, the rest muted. */
export function StatusBadge(props: { status: string | undefined }) {
  const status = props.status ?? "—";
  const cls =
    status === "settled"
      ? "text-success"
      : status === "refused" || status === "errored"
        ? "text-danger"
        : status === "credited" || status === "confirmed"
          ? "text-success"
          : "text-muted";
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs font-medium ${cls}`}>
      <span className="inline-block h-1.5 w-1.5 rounded-full bg-current" />
      {status}
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
