import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "./tooltip";

// ---------------------------------------------------------------------------
// Form — one control vocabulary, aligned radius/height/states.
// ---------------------------------------------------------------------------

const Field = React.forwardRef<
  HTMLLabelElement,
  React.ComponentProps<"label"> & { label: string; hint?: string }
>(({ className, label, hint, children, ...props }, ref) => (
  <label
    ref={ref}
    data-slot="field"
    className={cn("flex flex-col gap-1.5 text-sm", className)}
    {...props}
  >
    <span className="text-xs font-medium text-muted">{label}</span>
    {children}
    {hint ? <span className="text-xs text-muted">{hint}</span> : null}
  </label>
));
Field.displayName = "Field";

const CONTROL =
  "rounded-lg border border-input bg-surface px-3 py-2 text-sm text-ink placeholder:text-muted " +
  "transition-[border-color,box-shadow] duration-150 ease-out " +
  "hover:border-line focus:border-primary focus:outline-none focus:ring-2 focus:ring-ring/30 " +
  "disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-danger";

const TextInput = React.forwardRef<
  HTMLInputElement,
  React.ComponentProps<"input"> & { trailingSlot?: React.ReactNode }
>(({ className, trailingSlot, ...props }, ref) => {
  if (trailingSlot == null) {
    return <input ref={ref} data-slot="text-input" className={cn(CONTROL, className)} {...props} />;
  }
  return (
    <div data-slot="text-input" className={cn(CONTROL, "flex items-center gap-2 px-3 py-0", className)}>
      <input
        ref={ref}
        className="min-w-0 flex-1 border-0 bg-transparent py-2 text-sm outline-none placeholder:text-muted"
        {...props}
      />
      <span className="shrink-0 text-muted">{trailingSlot}</span>
    </div>
  );
});
TextInput.displayName = "TextInput";

const NumberInput = React.forwardRef<
  HTMLInputElement,
  React.ComponentProps<"input">
>(({ className, ...props }, ref) => (
  <input ref={ref} type="number" step="any" data-slot="number-input" className={cn(CONTROL, className)} {...props} />
));
NumberInput.displayName = "NumberInput";

const Textarea = React.forwardRef<
  HTMLTextAreaElement,
  React.ComponentProps<"textarea">
>(({ className, ...props }, ref) => (
  <textarea
    ref={ref}
    data-slot="textarea"
    className={cn(CONTROL, "w-full resize-y leading-relaxed", className)}
    {...props}
  />
));
Textarea.displayName = "Textarea";

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 rounded-lg px-3.5 py-2 text-sm font-medium transition-[background-color,border-color,box-shadow,opacity] duration-150 disabled:cursor-not-allowed disabled:opacity-50",
  {
    variants: {
      tone: {
        primary: "bg-primary text-white shadow-[0_2px_8px_rgba(45,91,216,.28)] hover:bg-primary-hover",
        ghost: "border border-line bg-surface text-ink hover:bg-canvas",
        danger: "border border-danger bg-danger text-white hover:bg-danger-hover",
      },
    },
    defaultVariants: { tone: "primary" },
  },
);

const Button = React.forwardRef<
  HTMLButtonElement,
  React.ComponentProps<"button"> &
    VariantProps<typeof buttonVariants> & {
      isLoading?: boolean;
      loadingText?: string;
    }
>(({ className, tone, isLoading, loadingText, children, disabled, ...props }, ref) => (
  <button
    ref={ref}
    data-slot="button"
    aria-busy={isLoading || undefined}
    disabled={disabled || isLoading}
    className={cn(buttonVariants({ tone }), className)}
    {...props}
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
));
Button.displayName = "Button";

function Spinner({ className }: { className?: string }) {
  return (
    <svg aria-hidden className={cn("animate-spin", className)} viewBox="0 0 24 24" fill="none">
      <circle className="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="2.5" />
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="2.5" strokeDasharray="14 42" strokeLinecap="round" />
    </svg>
  );
}

/** An icon-only control. `label` is required — it becomes both `aria-label`
 *  and the tooltip content, so the button is never a mystery glyph. The tap
 *  target is ≥44px on touch sizes (the mobile floor), the 32px look above
 *  `sm`. The native `title` attribute is gone — the tooltip is the Radix one,
 *  positioned and animated, not the browser's delayed yellow box. */
const IconButton = React.forwardRef<
  HTMLButtonElement,
  React.ComponentProps<"button"> & { label: string }
>(({ className, label, children, ...props }, ref) => (
  <Tooltip>
    <TooltipTrigger asChild>
      <button
        ref={ref}
        data-slot="icon-button"
        aria-label={label}
        className={cn(
          "inline-flex h-11 w-11 items-center justify-center rounded-lg border border-line bg-surface text-muted transition-colors hover:bg-canvas hover:text-ink sm:h-8 sm:w-8",
          className,
        )}
        {...props}
      >
        {children}
      </button>
    </TooltipTrigger>
    <TooltipContent>{label}</TooltipContent>
  </Tooltip>
));
IconButton.displayName = "IconButton";

// ---------------------------------------------------------------------------
// Select — a plain native select for form fields; `Select` (below) is the
// custom dropdown for value-pickers that need a composed trigger/popup.
// ---------------------------------------------------------------------------

const NativeSelect = React.forwardRef<
  HTMLSelectElement,
  React.ComponentProps<"select">
>(({ className, ...props }, ref) => (
  <select ref={ref} data-slot="native-select" className={cn(CONTROL, className)} {...props} />
));
NativeSelect.displayName = "NativeSelect";

// Back-compat alias — pages still call it `Select`; it renders the native control.
export const Select = NativeSelect;

export { Field, TextInput, NumberInput, Textarea, Button, IconButton, NativeSelect };
