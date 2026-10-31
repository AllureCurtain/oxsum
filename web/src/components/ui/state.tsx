import * as React from "react";
import { cn } from "../../lib/utils";

// ---------------------------------------------------------------------------
// States — loading / empty / error placeholders (skeleton rows live in #414).
// ---------------------------------------------------------------------------

const PageState = React.forwardRef<
  HTMLDivElement,
  React.ComponentProps<"div"> & { icon?: React.ReactNode }
>(({ className, icon, children, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="page-state"
    className={cn("flex flex-col items-center gap-2 px-4 py-10 text-center", className)}
    {...props}
  >
    {icon ? <div className="text-muted">{icon}</div> : null}
    <p className="text-sm text-muted">{children}</p>
  </div>
));
PageState.displayName = "PageState";

const EmptyRows = React.forwardRef<
  HTMLTableRowElement,
  React.ComponentProps<"tr"> & { colSpan: number; icon?: React.ReactNode }
>(({ className, colSpan, icon, children, ...props }, ref) => (
  <tr ref={ref} data-slot="empty-rows" {...props}>
    <td colSpan={colSpan} className={cn("px-4 py-10 text-center text-sm text-muted", className)}>
      {icon ? <div className="mb-2 flex justify-center text-muted">{icon}</div> : null}
      {children}
    </td>
  </tr>
));
EmptyRows.displayName = "EmptyRows";

export { PageState, EmptyRows };
