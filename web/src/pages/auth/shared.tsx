import type { ReactNode } from "react";
import { ApiError } from "../../api/client";
import { SURFACE } from "../../components/ui";

/**
 * The centered card every session page shares, on a quiet cobalt substrate.
 * The animated dot grid that used to run behind it is gone — a continuous
 * canvas is brand-page grammar on a form (and a CPU/prefers-reduced-motion
 * liability the audit flagged P0). What's left is a static radial tint: the
 * brand cue without the motion or the reflex blue-600.
 */
export function AuthCard(props: { title: string; children: ReactNode }) {
  return (
    <main className="relative flex min-h-screen items-start justify-center overflow-hidden bg-canvas">
      {/* static substrate — one soft cobalt bloom, no canvas, no rAF loop */}
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0"
        style={{
          background:
            "radial-gradient(720px 480px at 50% -8%, rgba(45, 91, 216, .12), transparent 70%)," +
            "radial-gradient(520px 360px at 88% 108%, rgba(45, 91, 216, .07), transparent 70%)",
        }}
      />
      <div className="relative z-10 mt-24 w-full max-w-sm px-4">
        <div className={`${SURFACE} p-6`}>
          <h1 className="text-xl font-semibold">{props.title}</h1>
          <div className="mt-4">{props.children}</div>
        </div>
      </div>
    </main>
  );
}

export function ErrorText({ error }: { error: unknown }) {
  return error instanceof ApiError ? (
    <p className="text-sm text-danger">{error.message}</p>
  ) : null;
}
