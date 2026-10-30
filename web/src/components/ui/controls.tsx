import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import { Check, ChevronDown, Copy } from "lucide-react";
import {
  AnimatePresence,
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
  useSpring,
  useTransform,
} from "motion/react";
import { Select as SelectPrimitive } from "@base-ui/react/select";
import { Switch as SwitchPrimitive } from "@base-ui/react/switch";
import { Tabs as TabsPrimitive } from "@base-ui/react/tabs";
import { Progress as ProgressPrimitive } from "@base-ui/react/progress";

import { cn } from "../../lib/utils";

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
        /* `relative z-10`: the moving `TabsIndicator` pill is a same-layer
           sibling painted after the tab — without this lift it covers the
           active label with a bare white pill (the Topup dead-tab look). */
        "relative z-10 inline-flex min-h-9 items-center justify-center whitespace-nowrap rounded-lg px-3 py-1.5 text-sm font-medium text-muted outline-none transition-colors duration-150",
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
