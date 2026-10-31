import {
  CircleCheckIcon,
  InfoIcon,
  Loader2Icon,
  OctagonXIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { Toaster as Sonner, type ToasterProps } from "sonner";

/* Toaster — sonner on the product palette (issue #452). `next-themes` is a
 * Next.js dependency this repo doesn't carry; the dashboard is light-only
 * for now, so the theme is fixed rather than detected. When a dark mode
 * lands this is the one place that flips. */

const Toaster = ({ ...props }: ToasterProps) => (
  <Sonner
    theme="light"
    className="toaster group"
    icons={{
      success: <CircleCheckIcon className="size-4" />,
      info: <InfoIcon className="size-4" />,
      warning: <TriangleAlertIcon className="size-4" />,
      error: <OctagonXIcon className="size-4" />,
      loading: <Loader2Icon className="size-4 animate-spin" />,
    }}
    style={
      {
        "--normal-bg": "var(--color-surface-raised)",
        "--normal-text": "var(--color-ink)",
        "--normal-border": "var(--color-line)",
        "--border-radius": "var(--radius-md)",
      } as React.CSSProperties
    }
    {...props}
  />
);

export { Toaster };
