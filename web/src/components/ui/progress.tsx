import * as React from "react";
import * as ProgressPrimitive from "@radix-ui/react-progress";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "../../lib/utils";

/* Progress — Radix primitive, product skin. `value === null` renders the
 * indeterminate sweep. Issue #450: moved from @base-ui/react to Radix. */

const Progress = React.forwardRef<
  React.ElementRef<typeof ProgressPrimitive.Root>,
  Omit<React.ComponentPropsWithoutRef<typeof ProgressPrimitive.Root>, "children" | "min"> & {
    label?: React.ReactNode;
    showValue?: boolean;
    min?: number;
  }
>(({ className, label, showValue, value, min = 0, max = 100, ...rootProps }, ref) => {
  const reduceMotion = useReducedMotion() ?? false;
  const isIndeterminate = value == null;
  const pct =
    value == null || !Number.isFinite(value)
      ? 0
      : Math.min(1, Math.max(0, (value - min) / Math.max(1e-9, max - min)));

  return (
    <div className={cn("flex w-full min-w-[8rem] flex-col gap-1.5", className)}>
      {(label != null || showValue) && (
        <div className="flex items-baseline justify-between gap-3">
          {label == null ? (
            <span className="sr-only">Progress</span>
          ) : (
            <span className="text-xs font-medium text-ink">{label}</span>
          )}
          {showValue ? (
            <span className="text-xs text-muted tabular-nums">
              {value == null ? "—" : `${Math.round(pct * 100)}%`}
            </span>
          ) : null}
        </div>
      )}
      <ProgressPrimitive.Root
        ref={ref}
        className="relative h-2 w-full overflow-hidden rounded-full border border-line bg-canvas"
        max={max}
        value={value ?? undefined}
        {...rootProps}
      >
        <ProgressPrimitive.Indicator asChild>
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
        </ProgressPrimitive.Indicator>
      </ProgressPrimitive.Root>
    </div>
  );
});
Progress.displayName = "Progress";

export { Progress };
