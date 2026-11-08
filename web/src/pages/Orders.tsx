import { decimalMinor } from "../lib/money";
import { useBundles } from "../api/hooks/bundles";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useTranslation } from "react-i18next";
import {
  ApiError,
  useCheckoutSession,
  useCreateOrder,
  useCollectionTerms,
  useDisplay,
  useOrder,
  useOrderFulfillment,
  useOrderRefunds,
  useOrders,
  useRequestRefund,
  type OrderCheckoutSession,
  type PaymentOrder,
} from "../api/hooks";
import {
  Badge,
  Banner,
  Button,
  Card,
  Cr,
  EmptyRows,
  Field,
  NativeSelect,
  PageState,
  TableShell,
  Td,
  TextInput,
  credits,
} from "../components/ui";
import { FileText } from "lucide-react";

// ---------------------------------------------------------------------------
// Shared rendering: the order's own fiat quote (`displayAmountMinor` of
// `displayCurrency`, ISO minor — 2 decimals like every rail we mount) beside
// the ledger credits it books.
// ---------------------------------------------------------------------------

function fiat(minor: number, currency: string): string {
  const sign = minor < 0 ? "-" : "";
  const abs = Math.abs(minor);
  const whole = Math.trunc(abs / 100);
  const frac = String(abs % 100).padStart(2, "0");
  return `${sign}${whole}.${frac} ${currency}`;
}

function orderStatusLabel(status: string): string {
  // `provider_pending`/`needs_attention` read awkward raw; the badge keeps the
  // wire vocabulary while the text softens the snake_case.
  return status.replace(/_/g, " ");
}

function OrderStatusBadge({ status }: { status: string }) {
  const cls =
    status === "fulfilled"
      ? "text-success"
      : status === "failed" || status === "needs_attention"
        ? "text-danger"
        : status === "paid"
          ? "text-pending"
          : "text-muted";
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs font-medium ${cls}`}>
      <span className="inline-block h-1.5 w-1.5 rounded-full bg-current" />
      {orderStatusLabel(status)}
    </span>
  );
}

function purposeLabel(t: (k: string) => string, purpose: string): string {
  switch (purpose) {
    case "walletTopup":
      return t("orders.purposeWalletTopup");
    case "bundle":
      return t("orders.purposeBundle");
    case "planPeriod":
      return t("orders.purposePlanPeriod");
    case "invoiceRepayment":
      return t("orders.purposeInvoiceRepayment");
    default:
      return purpose;
  }
}

/** The evidence column lands in a `jsonb` field the generated schema types as
 *  `Record<string, never>`; the fields the row actually carries are read
 *  through this narrow shape. The fulfillment's `purposePayload` is the same. */
type OrderEvidence = { reference?: string; reason?: string; recordedBy?: string };
type FulfillmentPayload = { creditedMinor?: number; promoMinor?: number };

// ---------------------------------------------------------------------------
// /app/orders — the payment history: provider orders and manual receipts in
// one newest-first list, with the purpose/status filters the API supports.
// A provider's return URL lands here as `?order={id}&…`; the list bounces the
// browser to that order's detail page, whose session poll is the truth.
// ---------------------------------------------------------------------------

export function Orders() {
  const { t } = useTranslation();
  const display = useDisplay();
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const purpose = params.get("purpose") ?? "";
  const status = params.get("status") ?? "";
  const returnedOrder = params.get("order");

  // The checkout's return lands on `/app/orders?order={id}&hyperswitch=return`
  // (or a rail's own marker). The return itself proves nothing — hand the
  // browser to the detail page, which polls the order/session for the truth.
  useEffect(() => {
    if (!returnedOrder) return;
    const rest = new URLSearchParams(params);
    rest.delete("order");
    rest.delete("purpose");
    rest.delete("status");
    const qs = rest.toString();
    navigate(`/orders/${returnedOrder}${qs ? `?${qs}` : ""}`, { replace: true });
  }, [returnedOrder, params, navigate]);

  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const page = useOrders({
    purpose: purpose === "" ? undefined : (purpose as PaymentOrder["purpose"]),
    status: status === "" ? undefined : status,
    after: cursorStack[cursorStack.length - 1],
  });
  const rows = page.data?.orders ?? [];
  const nextCursor = page.data?.nextCursor ?? null;

  const setFilter = (key: string, value: string) => {
    setCursorStack([]);
    setParams((prev) => {
      const next = new URLSearchParams(prev);
      if (value === "") next.delete(key);
      else next.set(key, value);
      return next;
    });
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t("orders.title")}</h1>
        <Link to="/orders/new">
          <Button>{t("orders.new")}</Button>
        </Link>
      </div>
      <div className="flex flex-wrap items-end gap-3">
        <Field label={t("orders.filterPurpose")}>
          <NativeSelect value={purpose} onChange={(e) => setFilter("purpose", e.target.value)}>
            <option value="">{t("orders.allPurposes")}</option>
            <option value="walletTopup">{t("orders.purposeWalletTopup")}</option>
            <option value="bundle">{t("orders.purposeBundle")}</option>
            <option value="planPeriod">{t("orders.purposePlanPeriod")}</option>
            <option value="invoiceRepayment">{t("orders.purposeInvoiceRepayment")}</option>
          </NativeSelect>
        </Field>
        <Field label={t("orders.filterStatus")}>
          <NativeSelect value={status} onChange={(e) => setFilter("status", e.target.value)}>
            <option value="">{t("orders.allStatuses")}</option>
            <option value="created">{t("orders.statusCreated")}</option>
            <option value="provider_pending">{t("orders.statusProviderPending")}</option>
            <option value="paid">{t("orders.statusPaid")}</option>
            <option value="fulfilled">{t("orders.statusFulfilled")}</option>
            <option value="failed">{t("orders.statusFailed")}</option>
            <option value="expired">{t("orders.statusExpired")}</option>
            <option value="needs_attention">{t("orders.statusNeedsAttention")}</option>
          </NativeSelect>
        </Field>
      </div>
      {page.isError ? (
        <PageState>{t("orders.listError")}</PageState>
      ) : (
        <TableShell
          head={[
            t("orders.colCreated"),
            t("orders.colPurpose"),
            t("common.status"),
            t("orders.colSource"),
            { label: t("orders.colCharged"), align: "right" },
            { label: t("orders.colCredits"), align: "right" },
            "",
          ]}
          loading={page.isLoading ? 4 : 0}
        >
          {rows.map((order) => (
            <tr key={order.orderId}>
              <td className="whitespace-nowrap">
                {order.createdAt.slice(0, 16).replace("T", " ")}
              </td>
              <td>{purposeLabel(t, order.purpose)}</td>
              <td>
                <OrderStatusBadge status={order.status} />
              </td>
              <td>
                {order.source === "manual" ? (
                  <Badge variant="secondary">{t("orders.sourceManual")}</Badge>
                ) : (
                  <span className="text-xs text-muted">{t("orders.sourceProvider")}</span>
                )}
              </td>
              <Td mono align="right">
                {fiat(order.displayAmountMinor, order.displayCurrency)}
              </Td>
              <Td mono align="right">
                <Cr minor={order.amountMinor} currency={display} />
              </Td>
              <td>
                <Link to={`/orders/${order.orderId}`} className="text-primary">
                  {t("orders.detailLink")}
                </Link>
              </td>
            </tr>
          ))}
          {!page.isLoading && rows.length === 0 ? (
            <EmptyRows colSpan={7} icon={<FileText />}>
              {t("orders.empty")}
            </EmptyRows>
          ) : null}
        </TableShell>
      )}
      <div className="flex items-center gap-2">
        <Button
          tone="ghost"
          disabled={cursorStack.length === 0}
          onClick={() => setCursorStack((s) => s.slice(0, -1))}
        >
          {t("orders.prev")}
        </Button>
        <Button
          tone="ghost"
          disabled={!nextCursor}
          onClick={() => setCursorStack((s) => [...s, rows[rows.length - 1].orderId])}
        >
          {t("orders.next")}
        </Button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// /app/orders/new — create a wallet-topup or bundle order, then land on the
// detail page where the checkout mounts against the session handles.
// ---------------------------------------------------------------------------

function fiatToMinor(raw: string): number | null { return decimalMinor(raw, 2); }


export function OrderNew() {
  const { t } = useTranslation();
  const terms = useCollectionTerms();
  const intent = useRef<{ payload: string; key: string } | null>(null);
  const navigate = useNavigate();
  const create = useCreateOrder();
  const [amount, setAmount] = useState("");
  const [purpose, setPurpose] = useState<"walletTopup" | "bundle">("walletTopup");
  const offers = useBundles();
  const [bundleId, setBundleId] = useState("");
  const offer = offers.data?.find((o) => o.bundleVersionId === bundleId);
  const currency = terms.data?.currency?.code ?? "USD";

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const minor = purpose === "bundle" ? offer?.terms.displayAmountMinor ?? null : fiatToMinor(amount);
    if (minor === null || (purpose === "bundle" && !offer) || !terms.data?.ready) return;
    const body = {
      rail: "hyperswitch" as const,
      purpose,
      displayAmountMinor: minor,
      displayCurrency: purpose === "bundle" ? offer!.terms.displayCurrency : currency,
      ...(purpose === "bundle" ? { purposeRef: bundleId } : {}),
    };
    const serialized = JSON.stringify(body);
    if (intent.current?.payload !== serialized) intent.current = { payload: serialized, key: crypto.randomUUID() };
    create.mutate(
      {
        expectedCollectionRevision: terms.data.revision,
        idempotencyKey: intent.current.key,
        ...body,
      },
      {
        onSuccess: (checkout) => {
          navigate(`/orders/${checkout.order.orderId}`, { replace: true });
        },
      },
    );
  };

  return (
    <div className="mx-auto flex max-w-lg flex-col gap-4">
      <h1 className="text-2xl font-bold">{t("orders.newTitle")}</h1>
      <Card title={t("orders.newCard")}>
        <form className="flex flex-col gap-3" onSubmit={submit}>
          {terms.isLoading ? <p>Loading collection terms…</p> : terms.isError ? <Banner tone="error">Collection terms could not be loaded.</Banner> : terms.data?.ready ? <p className="font-mono text-sm">1 credit = {terms.data.currency?.ratePerCredit} {currency}. Existing orders keep their agreed rate.</p> : <Banner tone="warn">The operator has not configured compatible collection terms.</Banner>}
          <Button type="button" tone="ghost" onClick={() => terms.refetch()} disabled={create.isPending}>Refresh collection terms</Button>
          <Field label={t("orders.filterPurpose")}>
            <NativeSelect
              value={purpose}
              onChange={(e) => setPurpose(e.target.value as "walletTopup" | "bundle")}
            >
              <option value="walletTopup">{t("orders.purposeWalletTopup")}</option>
              <option value="bundle">{t("orders.purposeBundle")}</option>
            </NativeSelect>
          </Field>
          {purpose === "walletTopup" ? <Field label={t("orders.amountLabel", { currency })} hint={t("orders.amountHint")}>
            <TextInput inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value)} required />
          </Field> : <>
            <Field label="Published bundle"><NativeSelect value={bundleId} onChange={(e) => setBundleId(e.target.value)} required disabled={offers.isLoading}>
              <option value="">Select a bundle</option>{offers.data?.map((o) => <option key={o.bundleVersionId} value={o.bundleVersionId}>{o.terms.name} · version {o.version} · {fiat(o.terms.displayAmountMinor, o.terms.displayCurrency)}</option>)}
            </NativeSelect></Field>
            {offers.isError ? <Banner tone="error">Bundle offers could not be loaded.</Banner> : !offers.isLoading && !offers.data?.length ? <Banner tone="info">No bundle offers are currently on sale.</Banner> : null}
            {offer ? <p>Pay {fiat(offer.terms.displayAmountMinor, offer.terms.displayCurrency)} for <Cr minor={offer.terms.purchasedMinor} /> purchased credit plus <Cr minor={offer.terms.promoMinor} /> promotional credit. Promotion {offer.terms.promoLifetimeDays == null ? "has no expiry" : `expires ${offer.terms.promoLifetimeDays} days after confirmed payment`}. A confirmed partial or full refund cancels remaining promotional credit without cash reimbursement.</p> : null}
          </>}
          <p className="text-xs text-muted">{t("orders.createConsequence")}</p>
          <Button type="submit" disabled={!terms.data?.ready || (purpose === "bundle" ? !offer : fiatToMinor(amount) === null)} isLoading={create.isPending} loadingText={t("orders.creating")}>
            {t("orders.createSubmit")}
          </Button>
          {create.error instanceof ApiError ? (
            <Banner tone="error">{create.error.message}</Banner>
          ) : null}
        </form>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// /app/orders/{id} — the order's truth: status poll plus the live checkout
// mount for the rail the order was created under.
// ---------------------------------------------------------------------------

// The switch's browser SDK (issue #320, same mount pattern as Topup.tsx):
// HyperLoader.js installs `window.Hyper`; it only ever loads on this page.
// The `Window.Hyper` declaration is global — the shapes below match Topup's
// exactly so the two modules' declarations merge cleanly.
interface HyperElement {
  mount(selector: string): void;
  unmount(): void;
  on?(event: string, handler: (payload: unknown) => void): void;
}
interface HyperElements {
  create(componentType: "payment", options?: Record<string, unknown>): HyperElement;
}
interface HyperInstance {
  elements(options: { clientSecret: string }): HyperElements;
}
declare global {
  interface Window {
    Hyper?: (publishableKey: string, options?: Record<string, unknown>) => HyperInstance;
  }
}

const hyperScriptByOrigin = new Map<string, Promise<void>>();

function loadHyper(sdkUrl: string): Promise<void> {
  const origin = sdkUrl.replace(/\/+$/, "");
  const cached = hyperScriptByOrigin.get(origin);
  if (cached) return cached;
  const promise = new Promise<void>((resolve, reject) => {
    const script = document.createElement("script");
    script.src = `${origin}/HyperLoader.js`;
    script.async = true;
    script.onload = () => {
      if (window.Hyper) resolve();
      else reject(new Error("HyperLoader.js did not expose window.Hyper"));
    };
    script.onerror = () => reject(new Error(`could not load the switch SDK from ${origin}`));
    document.head.appendChild(script);
  });
  hyperScriptByOrigin.set(origin, promise);
  return promise;
}

/** The mounted checkout for a live `provider_pending` session — rendered only
 *  while the session carries the `clientSecret`/`sdkUrl`/`publishableKey`
 *  triple the SDK needs. */
function SwitchCheckout({ session }: { session: OrderCheckoutSession }) {
  const { t } = useTranslation();
  const mountRef = useRef<HTMLDivElement>(null);
  const elementRef = useRef<HyperElement | null>(null);
  const [stage, setStage] = useState<"mounting" | "mounted" | "failed">("mounting");
  const [mountError, setMountError] = useState<string | null>(null);

  useEffect(() => {
    const { sdkUrl, clientSecret, publishableKey } = session;
    if (!sdkUrl || !clientSecret || !publishableKey) return;
    let cancelled = false;
    void (async () => {
      try {
        await loadHyper(sdkUrl);
        if (cancelled || !mountRef.current || !window.Hyper) return;
        const hyper = window.Hyper(publishableKey, {});
        const elements = hyper.elements({ clientSecret });
        const element = elements.create("payment", {});
        element.mount("#order-checkout-mount");
        elementRef.current = element;
        if (!cancelled) setStage("mounted");
      } catch (error) {
        if (!cancelled) {
          setMountError(error instanceof Error ? error.message : t("orders.mountError"));
          setStage("failed");
        }
      }
    })();
    return () => {
      cancelled = true;
      elementRef.current?.unmount();
      elementRef.current = null;
    };
  }, [session.sdkUrl, session.clientSecret, session.publishableKey, t]);

  return (
    <div className="flex flex-col gap-2">
      {stage === "mounting" ? (
        <p className="text-xs text-muted" role="status">
          {t("orders.loadingSecure")}
        </p>
      ) : null}
      {stage === "failed" ? (
        <Banner tone="error">{mountError ?? t("orders.checkoutFailed")}</Banner>
      ) : null}
      {/* The checkout owns everything under this node once mounted. */}
      <div id="order-checkout-mount" ref={mountRef} />
      {stage === "mounted" ? <p className="text-xs text-muted">{t("orders.finishPayment")}</p> : null}
    </div>
  );
}

/** The truth block every non-live state renders: `paid` polls until the
 *  fulfillment lands; `fulfilled` names what the grant produced; the rest
 *  state their terminal truth. */
function FulfillmentBlock({ order }: { order: PaymentOrder }) {
  const { t } = useTranslation();
  const live = order.status === "paid";
  const fulfillment = useOrderFulfillment(order.orderId, {
    refetchInterval: live ? 2_000 : false,
  });
  const record = fulfillment.data?.fulfillment;
  if (order.status === "fulfilled" && record) {
    const payload = record.purposePayload as FulfillmentPayload;
    const credited =
      typeof payload.creditedMinor === "number" ? payload.creditedMinor : order.amountMinor;
    return <Banner tone="success">{t("orders.fulfilled", { amount: credits(credited) })}</Banner>;
  }
  if (order.status === "paid") {
    return <Banner tone="info">{t("orders.paidWaiting")}</Banner>;
  }
  if (order.status === "failed") {
    return <Banner tone="error">{t("orders.failed")}</Banner>;
  }
  if (order.status === "expired") {
    return <Banner tone="error">{t("orders.expired")}</Banner>;
  }
  if (order.status === "needs_attention") {
    return <Banner tone="warn">{t("orders.needsAttention")}</Banner>;
  }
  return null;
}

// ---------------------------------------------------------------------------
// The refund card on a paid/fulfilled order (T23): the customer asks for
// review — never grants itself money. The ask reads the refundable remainder
// the server computes; a `requested` refund's state shows as it resolves.
// ---------------------------------------------------------------------------

function refundStateBadge(state: string) {
  switch (state) {
    case "succeeded":
      return <Badge tone="success">succeeded</Badge>;
    case "requested":
      return <Badge tone="secondary">requested</Badge>;
    case "declined":
      return <Badge tone="danger">declined</Badge>;
    case "failed":
      return <Badge tone="danger">failed</Badge>;
    case "provider_pending":
      return <Badge tone="secondary">provider pending</Badge>;
    case "released":
      return <Badge tone="secondary">released</Badge>;
    default:
      return <Badge tone="secondary">{state}</Badge>;
  }
}

function RefundCard({ order }: { order: PaymentOrder }) {
  const { t } = useTranslation();
  const display = useDisplay();
  const refunds = useOrderRefunds(order.orderId);
  const request = useRequestRefund(order.orderId);
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");

  const refundable = refunds.data?.refundableMinor ?? null;
  const list = refunds.data?.refunds ?? [];
  const eligible =
    (order.status === "paid" || order.status === "fulfilled") &&
    refundable !== null &&
    refundable > 0;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    request.mutate(
      { reason: reason.trim() },
      {
        onSuccess: () => {
          setOpen(false);
          setReason("");
        },
      },
    );
  };

  return (
    <Card
      title={t("orders.refunds")}
      actions={
        eligible ? (
          <Button tone="ghost" onClick={() => setOpen((v) => !v)}>
            {t("orders.requestRefund")}
          </Button>
        ) : null
      }
    >
      <div className="flex flex-col gap-4">
        {refundable !== null ? (
          <p className="text-sm text-muted">
            {t("orders.refundableRemainder")}{" "}
            <span className="tabular-nums">
              <Cr minor={refundable} currency={display} />
            </span>
          </p>
        ) : null}
        {list.length === 0 && !refunds.isLoading ? (
          <p className="text-sm text-muted">{t("orders.noRefunds")}</p>
        ) : null}
        {list.length > 0 ? (
          <ul className="flex flex-col gap-2">
            {list.map((r) => (
              <li
                key={r.refundId}
                className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-line px-3 py-2"
              >
                <div className="flex items-center gap-3">
                  {refundStateBadge(r.state)}
                  <span className="tabular-nums text-sm">
                    <Cr minor={r.amountMinor} currency={display} />
                  </span>
                  <span className="text-xs text-muted">{r.reason}</span>
                </div>
                <span className="text-xs text-muted">{r.createdAt.slice(0, 10)}</span>
              </li>
            ))}
          </ul>
        ) : null}
        {open && eligible ? (
          <form className="flex flex-col gap-2 border-t border-line pt-3" onSubmit={submit}>
            <Field label={t("orders.refundReason")} hint={t("orders.refundReasonHint")}>
              <TextInput
                value={reason}
                onChange={(e) => setReason(e.target.value)}
                required
                minLength={1}
                maxLength={500}
              />
            </Field>
            <div className="flex items-center gap-2">
              <Button type="submit" isLoading={request.isPending} loadingText={t("orders.requesting")}>
                {t("orders.refundSubmit")}
              </Button>
              <Button type="button" tone="ghost" onClick={() => setOpen(false)}>
                {t("common.cancel")}
              </Button>
            </div>
            {request.error instanceof ApiError ? (
              <Banner tone="error">{request.error.message}</Banner>
            ) : null}
          </form>
        ) : null}
      </div>
    </Card>
  );
}

export function OrderDetail() {
  const { t } = useTranslation();
  const { id } = useParams<{ id: string }>();
  const [params] = useSearchParams();
  const display = useDisplay();
  const order = useOrder(id);
  // Poll the session while the order can still move; a terminal status stops.
  const live = order.data
    ? order.data.status === "created" ||
      order.data.status === "provider_pending" ||
      order.data.status === "paid"
    : true;
  const session = useCheckoutSession(id, { refetchInterval: live ? 3_000 : false });
  const returned =
    params.get("hyperswitch") === "return" ||
    params.get("session") === "success" ||
    params.get("session") === "cancel";

  if (order.isError) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-bold">{t("orders.detailTitle")}</h1>
        <PageState>{t("orders.detailError")}</PageState>
      </div>
    );
  }
  if (!order.data) {
    return <PageState>{t("common.loading")}</PageState>;
  }
  const o = order.data;
  const s = session.data;
  const evidence = o.evidence as OrderEvidence;
  const canMountCheckout =
    s?.status === "provider_pending" &&
    Boolean(s.clientSecret) &&
    Boolean(s.sdkUrl) &&
    Boolean(s.publishableKey);
  const hostedCheckout =
    s?.status === "provider_pending" && Boolean(s.checkoutUrl) && !s.clientSecret;

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t("orders.detailTitle")}</h1>
        <OrderStatusBadge status={o.status} />
      </div>
      {returned && (o.status === "provider_pending" || o.status === "created") ? (
        <Banner tone="info">{t("orders.returnedWaiting")}</Banner>
      ) : null}
      <FulfillmentBlock order={o} />
      {o.bundleOffer ? <Card title={`${o.bundleOffer.terms.name} · version ${o.bundleOffer.version}`}>
        <p>This order purchases <Cr minor={o.bundleOffer.terms.purchasedMinor} /> and grants <Cr minor={o.bundleOffer.terms.promoMinor} /> promotional credit. Promotion {o.bundleOffer.terms.promoLifetimeDays == null ? "has no expiry" : `expires ${o.bundleOffer.terms.promoLifetimeDays} days after confirmed payment`}. A confirmed partial or full refund cancels remaining promotional credit without cash reimbursement.</p>
      </Card> : o.purpose === "bundle" && o.status !== "fulfilled" ? <Banner tone="warn">This legacy bundle has no published offer snapshot. Checkout and fulfillment require operator review.</Banner> : null}
      {o.planPayment ? <Card title="Purchased plan period"><p>Period {o.planPayment.periodIndex + 1} · exact sold fee <Cr minor={o.planPayment.feeMinor} />. {o.planPayment.periodStart ? `${o.planPayment.periodStart} – ${o.planPayment.periodEnd} (UTC)` : "First service period begins at paid activation."}</p><p className="text-sm text-muted">This payment grants plan entitlement and never adds wallet credit. Its sold version and collection quote remain fixed.</p></Card> : o.purpose === "planPeriod" && o.status !== "fulfilled" ? <Banner tone="warn">This legacy order has no verified period quote. Contact the operator for reconciliation; it cannot automatically grant new entitlement.</Banner> : null}
      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <div>
          <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
            {t("orders.colCharged")}
          </p>
          <p className="mt-1 text-lg font-semibold tabular-nums">
            {fiat(o.displayAmountMinor, o.displayCurrency)}
          </p>
        </div>
        <div>
          <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
            {t("orders.colCredits")}
          </p>
          <p className="mt-1 text-lg font-semibold tabular-nums">
            <Cr minor={o.amountMinor} currency={display} />
          </p>
        </div>
        <div>
          <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
            {t("orders.colPurpose")}
          </p>
          <p className="mt-1 text-lg font-semibold">{purposeLabel(t, o.purpose)}</p>
        </div>
        <div>
          <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
            {t("orders.colSource")}
          </p>
          <p className="mt-1 text-lg font-semibold">
            {o.source === "manual" ? t("orders.sourceManual") : t("orders.sourceProvider")}
          </p>
        </div>
      </div>
      {o.source === "manual" ? (
        <Card title={t("orders.evidence")}>
          <dl className="flex flex-col gap-1 text-sm">
            <div className="flex gap-2">
              <dt className="w-24 shrink-0 text-muted">{t("orders.evidenceReference")}</dt>
              <dd className="mono text-xs">{evidence.reference ?? "—"}</dd>
            </div>
            <div className="flex gap-2">
              <dt className="w-24 shrink-0 text-muted">{t("orders.evidenceReason")}</dt>
              <dd>{evidence.reason ?? "—"}</dd>
            </div>
          </dl>
        </Card>
      ) : null}
      {canMountCheckout ? (
        <Card title={t("orders.checkoutTitle")}>
          <SwitchCheckout session={s} />
        </Card>
      ) : null}
      {hostedCheckout ? (
        <Card title={t("orders.checkoutTitle")}>
          <p className="text-sm text-muted">{t("orders.hostedIntro")}</p>
          <a href={s?.checkoutUrl ?? "#"} className="text-primary">
            {t("orders.openHosted")}
          </a>
        </Card>
      ) : null}
      {session.isError ? <Banner tone="error">{t("orders.sessionError")}</Banner> : null}
      {o.status === "paid" || o.status === "fulfilled" ? <RefundCard order={o} /> : null}
      <div>
        <Link to="/orders" className="text-sm text-muted hover:text-ink">
          ← {t("orders.backToList")}
        </Link>
      </div>
    </div>
  );
}
