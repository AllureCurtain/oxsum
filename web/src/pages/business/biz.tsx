import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
  Badge,
  Banner,
  Button,
  Field,
  Textarea,
} from "../../components/ui";
import { ApiError } from "../../api/client";

// ---------------------------------------------------------------------------
// Shared staff-console pieces (issue #615, T24): the status vocabulary, the
// key/value dl every detail page reuses, cursor pagination, and the
// confirm-with-reason dialog every audited write goes through.
// ---------------------------------------------------------------------------

/** The finance-state vocabulary → the badge tone the pages color it. One map
 * so a receivable, a refund and a journal never disagree on a state's color. */
export function stateBadge(state: string | undefined | null) {
  switch (state) {
    case "active":
    case "paid":
    case "fulfilled":
    case "succeeded":
    case "applied":
    case "issued":
    case "matched":
    case "resolved":
    case "won":
    case "reachable":
      return <Badge tone="success">{state ?? "—"}</Badge>;
    case "requested":
    case "created":
    case "provider_pending":
    case "pendingPayment":
    case "renewalPending":
    case "running":
    case "pending":
    case "open":
    case "posted":
    case "opened":
    case "evidence_due":
    case "draft":
      return <Badge tone="secondary" live={state === "requested" || state === "open"}>{state ?? "—"}</Badge>;
    case "pastDue":
    case "past_due":
    case "needs_attention":
    case "failed":
    case "exceptions":
    case "lost":
    case "blocked":
    case "authenticationFailed":
    case "httpError":
    case "unreachable":
      return <Badge tone="danger">{state ?? "—"}</Badge>;
    case "void":
    case "declined":
    case "waived":
    case "closed":
    case "expired":
    case "cancelled":
    case "archived":
    case "released":
    case "reversed":
    case "ignored":
    case "rejected":
      return <Badge tone="secondary">{state ?? "—"}</Badge>;
    default:
      return <Badge tone="secondary">{state ?? "—"}</Badge>;
  }
}

/** One labelled figure in a detail grid — `dt` muted caption, `dd` the value. */
export function Field2({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <dt className="text-xs font-medium text-muted">{label}</dt>
      <dd className="mt-0.5">{children}</dd>
    </div>
  );
}

/** A `dl` detail grid that stacks at mobile and spreads at ≥sm. */
export function DetailGrid({ children }: { children: ReactNode }) {
  return <dl className="grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">{children}</dl>;
}

/** The audited-write dialog: every destructive or financial staff action
 * confirms through this — it collects the mandatory `reason`, names what the
 * action commits, mints one idempotency key per open, and surfaces the API's
 * own field error. Replaces `window.confirm` per DESIGN.md. */
export function AuditAction({
  trigger,
  title,
  description,
  confirmLabel,
  tone = "primary",
  reasonLabel,
  reasonRequired = true,
  isPending,
  error,
  onConfirm,
}: {
  trigger: ReactNode;
  title: string;
  description: string;
  confirmLabel: string;
  tone?: "primary" | "danger";
  reasonLabel: string;
  reasonRequired?: boolean;
  isPending: boolean;
  error: unknown;
  /** Called with the collected reason + the idempotency key for this open. The
   * dialog closes only when the returned promise resolves — wire a mutation's
   * `mutateAsync` (or an equivalent promise) through so a failed write keeps
   * the typed reason and shows the API error instead of closing blind. */
  onConfirm: (reason: string, idempotencyKey: string) => Promise<unknown>;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  // Minted per dialog open — a retry of the same attempt replays it, while a
  // reopened dialog is a new intent and gets a fresh key.
  const [idemKey, setIdemKey] = useState(() => crypto.randomUUID());
  const close = (v: boolean) => {
    setOpen(v);
    if (!v) {
      setReason("");
      setIdemKey(crypto.randomUUID());
    }
  };
  const err = error instanceof ApiError ? error : null;
  return (
    <AlertDialog open={open} onOpenChange={close}>
      <AlertDialogTrigger asChild>{trigger}</AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        <Field label={reasonLabel} hint={reasonRequired ? undefined : t("biz.optional")}>
          <Textarea
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            rows={3}
            required={reasonRequired}
          />
        </Field>
        {err ? <Banner tone="error">{err.message}</Banner> : null}
        <AlertDialogFooter>
          <Button tone="ghost" onClick={() => close(false)}>
            {t("common.cancel")}
          </Button>
          <Button
            tone={tone}
            isLoading={isPending}
            disabled={reasonRequired && reason.trim() === ""}
            onClick={() => {
              void onConfirm(reason.trim(), idemKey).then(
                () => close(false),
                // A rejected write stays open: the reason is kept and the
                // mutation's error renders in the Banner above.
                () => {},
              );
            }}
          >
            {confirmLabel}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
