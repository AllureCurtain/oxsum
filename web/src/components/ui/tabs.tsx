import * as React from "react";
import * as TabsPrimitive from "@radix-ui/react-tabs";

import { cn } from "../../lib/utils";

/* Tabs — Radix primitives, product skin. Underline + pill variants.
 * Issue #450: the behavior layer moved from @base-ui/react to Radix so the
 * whole ui/ layer speaks one primitive library. The animated indicator that
 * @base-ui shipped as `TabsIndicator` is gone — Radix has no such primitive,
 * and shadcn's convention is that the trigger styles its own active state
 * rather than animating a floating pill behind it. */

const Tabs = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Root>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Root>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Root
    ref={ref}
    className={cn("grid w-full min-w-0 gap-3", className)}
    {...props}
  />
));
Tabs.displayName = "Tabs";

const TabsList = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.List>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.List> & {
    variant?: "underline" | "pill";
  }
>(({ className, variant = "underline", ...props }, ref) => (
  <TabsPrimitive.List
    ref={ref}
    className={cn(
      "relative flex w-fit min-w-0 items-center gap-1",
      variant === "underline" && "w-full gap-5 border-b border-line pb-px",
      variant === "pill" && "rounded-full border border-line bg-canvas p-1",
      className,
    )}
    data-variant={variant}
    {...props}
  />
));
TabsList.displayName = "TabsList";

const TabsTrigger = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Trigger
    ref={ref}
    className={cn(
      "relative z-10 inline-flex min-h-9 items-center justify-center whitespace-nowrap rounded-lg px-3 py-1.5 text-sm font-medium text-muted outline-none transition-colors duration-150",
      "hover:text-ink focus-visible:ring-2 focus-visible:ring-ring/30",
      "disabled:pointer-events-none disabled:opacity-50",
      /* underline variant: the active tab's text + a 2px bar it carries */
      "data-[state=active]:text-ink",
      "data-[state=active]:after:absolute data-[state=active]:after:-bottom-px data-[state=active]:after:left-0 data-[state=active]:after:right-0 data-[state=active]:after:h-0.5 data-[state=active]:after:rounded-full data-[state=active]:after:bg-primary",
      /* pill variant: the active tab's own white pill — no separate indicator */
      "group-data-[variant=pill]:rounded-full group-data-[variant=pill]:px-4",
      "group-data-[variant=pill]:data-[state=active]:bg-surface group-data-[variant=pill]:data-[state=active]:shadow-[0_1px_2px_rgba(20,34,64,.1)] group-data-[variant=pill]:data-[state=active]:ring-1 group-data-[variant=pill]:data-[state=active]:ring-line",
      "group-data-[variant=pill]:data-[state=active]:after:hidden",
      className,
    )}
    {...props}
  />
));
TabsTrigger.displayName = "TabsTrigger";

const TabsContent = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Content
    ref={ref}
    className={cn("w-full min-w-0 outline-none", className)}
    {...props}
  />
));
TabsContent.displayName = "TabsContent";

export { Tabs, TabsList, TabsTrigger, TabsContent };
