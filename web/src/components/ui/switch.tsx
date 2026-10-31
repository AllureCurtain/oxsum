import * as React from "react";
import * as SwitchPrimitive from "@radix-ui/react-switch";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "../../lib/utils";

/* Switch — Radix primitive, product skin (no underglow). Issue #450: moved
 * from @base-ui/react to Radix. The spring-animated thumb stays motion-driven. */

const switchSpring = { type: "spring" as const, bounce: 0.1, damping: 30, stiffness: 400 };

const Switch = React.forwardRef<
  React.ElementRef<typeof SwitchPrimitive.Root>,
  React.ComponentPropsWithoutRef<typeof SwitchPrimitive.Root> & {
    size?: "sm" | "default";
  }
>(({ className, size = "default", ...props }, ref) => {
  const reduceMotion = useReducedMotion() ?? false;
  const trackSize = size === "sm" ? "h-[18px] w-[32px]" : "h-[22px] w-[40px]";
  const thumbSize = size === "sm" ? "h-3 w-3" : "h-3.5 w-3.5";
  const travel = size === "sm" ? 14 : 18;
  return (
    <SwitchPrimitive.Root
      ref={ref}
      className={cn(
        "group/switch relative inline-flex shrink-0 cursor-pointer items-center rounded-full border border-line px-[3px] outline-none",
        "bg-canvas transition-[background-color,border-color,box-shadow] duration-150",
        "data-[state=checked]:border-primary/40 data-[state=checked]:bg-primary",
        "focus-visible:ring-2 focus-visible:ring-ring/30",
        "disabled:cursor-not-allowed disabled:opacity-50",
        "after:absolute after:-inset-x-2 after:-inset-y-2 after:content-['']", // ≥44px hit slop
        trackSize,
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb asChild>
        <motion.span
          animate={{ x: props.checked || props.defaultChecked ? travel : 0 }}
          className={cn(
            "pointer-events-none block rounded-full bg-surface shadow-[0_1px_2px_rgba(20,34,64,.18)] ring-1 ring-black/5",
            thumbSize,
            "data-[state=checked]:bg-white",
          )}
          initial={false}
          transition={reduceMotion ? { duration: 0 } : switchSpring}
        />
      </SwitchPrimitive.Thumb>
    </SwitchPrimitive.Root>
  );
});
Switch.displayName = "Switch";

export { Switch };
