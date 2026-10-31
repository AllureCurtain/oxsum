import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/utils";

// ---------------------------------------------------------------------------
// Badges & status — one semantic vocabulary: status dot, pill tag, banner.
// ---------------------------------------------------------------------------

/** A status word as the pages color it: settled green, refused red, the rest muted. */
const StatusBadge = React.forwardRef<
  HTMLSpanElement,
  React.ComponentProps<"span"> & { status?: string }
>(({ className, status, ...props }, ref) => {
  const s = status ?? "—";
  const cls =
    s === "settled"
      ? "text-success"
      : s === "refused" || s === "errored"
        ? "text-danger"
        : s === "credited" || s === "confirmed"
          ? "text-success"
          : "text-muted";
  return (
    <span
      ref={ref}
      data-slot="status-badge"
      className={cn("inline-flex items-center gap-1.5 text-xs font-medium", cls, className)}
      {...props}
    >
      <span className="inline-block h-1.5 w-1.5 rounded-full bg-current" />
      {s}
    </span>
  );
});
StatusBadge.displayName = "StatusBadge";

const tagVariants = cva("inline-block rounded-md px-1.5 py-0.5 text-[11px] font-medium", {
  variants: {
    tone: {
      default: "bg-secondary text-muted-foreground",
      accent: "bg-primary-soft text-primary",
    },
  },
  defaultVariants: { tone: "default" },
});

const Tag = React.forwardRef<
  HTMLSpanElement,
  React.ComponentProps<"span"> & VariantProps<typeof tagVariants> & { accent?: boolean }
>(({ className, tone, accent, ...props }, ref) => (
  <span
    ref={ref}
    data-slot="tag"
    className={cn(tagVariants({ tone: accent ? "accent" : tone }), className)}
    {...props}
  />
));
Tag.displayName = "Tag";

const badgeVariants = cva(
  "inline-flex h-5 items-center gap-1.5 rounded-full px-2 text-[11px] font-medium leading-none",
  {
    variants: {
      tone: {
        default: "bg-primary-soft text-primary",
        secondary: "bg-secondary text-muted-foreground",
        success: "bg-success/10 text-success",
        danger: "bg-danger/10 text-danger",
        outline: "border border-line text-ink",
      },
    },
    defaultVariants: { tone: "default" },
  },
);

const Badge = React.forwardRef<
  HTMLSpanElement,
  React.ComponentProps<"span"> &
    VariantProps<typeof badgeVariants> & {
      /** `variant` kept as an alias of `tone` for the badge's semantic colors. */
      variant?: "default" | "secondary" | "success" | "danger" | "destructive" | "outline";
      live?: boolean;
    }
>(({ className, tone, variant, live, children, ...props }, ref) => {
  const raw = variant ?? tone ?? "default";
  const resolved = raw === "destructive" ? "danger" : raw;
  return (
    <span
      ref={ref}
      data-slot="badge"
      className={cn(badgeVariants({ tone: resolved }), className)}
      {...props}
    >
      {live ? (
        <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-current motion-reduce:animate-none" />
      ) : null}
      {children}
    </span>
  );
});
Badge.displayName = "Badge";

// ---------------------------------------------------------------------------
// Banner — page/form level feedback.
// ---------------------------------------------------------------------------

const bannerVariants = cva("rounded-lg border px-3.5 py-2.5 text-sm", {
  variants: {
    tone: {
      info: "border-primary/40 bg-primary-soft text-primary",
      warn: "border-pending/40 bg-pending/10 text-pending",
      error: "border-danger/40 bg-danger/10 text-danger",
      success: "border-success/40 bg-success/10 text-success",
    },
  },
  defaultVariants: { tone: "info" },
});

const Banner = React.forwardRef<
  HTMLParagraphElement,
  React.ComponentProps<"p"> & VariantProps<typeof bannerVariants>
>(({ className, tone, ...props }, ref) => (
  <p
    ref={ref}
    data-slot="banner"
    className={cn(bannerVariants({ tone }), className)}
    {...props}
  />
));
Banner.displayName = "Banner";

export { StatusBadge, Tag, Badge, Banner };
