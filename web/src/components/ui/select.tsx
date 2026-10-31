import * as React from "react";
import * as SelectPrimitive from "@radix-ui/react-select";
import { Check, ChevronDown } from "lucide-react";

import { cn } from "../../lib/utils";

/* Select — Radix primitives, product skin. The composed dropdown picker for
 * model/channel/etc. Issue #450: moved from @base-ui/react to Radix.
 * `SelectRoot` keeps its name (a plain `Select` would collide with the
 * native-select export in form.tsx); the inner pieces follow Radix's naming. */

const SelectRoot = SelectPrimitive.Root;

const SelectValue = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Value>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Value>
>(({ className, ...props }, ref) => (
  <SelectPrimitive.Value
    ref={ref}
    className={cn("flex flex-1 truncate text-left", className)}
    {...props}
  />
));
SelectValue.displayName = "SelectValue";

const SelectTrigger = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Trigger>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Trigger
    ref={ref}
    className={cn(
      "inline-flex min-h-9 items-center justify-between gap-2 rounded-lg border border-input bg-surface px-3 py-1.5 text-sm text-ink outline-none transition-[border-color,box-shadow] duration-150",
      "hover:border-line focus-visible:border-primary focus-visible:ring-2 focus-visible:ring-ring/30",
      "disabled:cursor-not-allowed disabled:opacity-50 data-placeholder:text-muted",
      className,
    )}
    {...props}
  >
    {children}
    <SelectPrimitive.Icon asChild>
      <ChevronDown className="size-4 text-muted" />
    </SelectPrimitive.Icon>
  </SelectPrimitive.Trigger>
));
SelectTrigger.displayName = "SelectTrigger";

const SelectContent = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Content>
>(({ className, children, position = "popper", ...props }, ref) => (
  <SelectPrimitive.Portal>
    <SelectPrimitive.Content
      ref={ref}
      position={position}
      className={cn(
        "isolate z-dropdown max-h-72 min-w-[var(--radix-select-trigger-width)] overflow-y-auto rounded-xl border border-line bg-surface-raised shadow-2",
        "data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95",
        "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
        "data-[side=bottom]:slide-in-from-top-2 data-[side=top]:slide-in-from-bottom-2",
        "duration-150 ease-out",
        position === "popper" && "data-[side=bottom]:translate-y-1 data-[side=top]:-translate-y-1",
        className,
      )}
      {...props}
    >
      <SelectPrimitive.Viewport className="p-1">{children}</SelectPrimitive.Viewport>
    </SelectPrimitive.Content>
  </SelectPrimitive.Portal>
));
SelectContent.displayName = "SelectContent";

const SelectItem = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Item>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Item>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Item
    ref={ref}
    className={cn(
      "relative flex min-h-9 w-full cursor-default select-none items-center gap-2 rounded-lg px-2.5 py-2 text-sm text-ink outline-none",
      "transition-colors data-[highlighted]:bg-primary-soft data-[highlighted]:text-primary",
      "data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
      className,
    )}
    {...props}
  >
    <SelectPrimitive.ItemText className="min-w-0 flex-1 truncate">{children}</SelectPrimitive.ItemText>
    <SelectPrimitive.ItemIndicator className="flex shrink-0 text-primary">
      <Check className="h-3.5 w-3.5" strokeWidth={2.5} />
    </SelectPrimitive.ItemIndicator>
  </SelectPrimitive.Item>
));
SelectItem.displayName = "SelectItem";

export { SelectRoot, SelectValue, SelectTrigger, SelectContent, SelectItem };
