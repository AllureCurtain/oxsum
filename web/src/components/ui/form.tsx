import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

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

/** An icon-only control. `label` is required — it becomes both `aria-label`
 *  and the tooltip, so the button is never a mystery glyph. The tap target is
 *  ≥44px on touch sizes (the mobile floor), the 32px look above `sm`. */
export function IconButton(props: React.ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  const { label, children, className, ...rest } = props;
  return (
    <button
      {...rest}
      aria-label={label}
      title={label}
      className={cn(
        "inline-flex h-11 w-11 items-center justify-center rounded-lg border border-line bg-surface text-muted transition-colors hover:bg-canvas hover:text-ink sm:h-8 sm:w-8",
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
