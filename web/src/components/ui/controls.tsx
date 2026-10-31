import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import {
  AnimatePresence,
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
  useSpring,
  useTransform,
} from "motion/react";

import { cn } from "../../lib/utils";

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
