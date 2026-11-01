import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { Trans, useTranslation } from "react-i18next";
import {
  ApiError,
  useAirwallexTopup,
  useBalance,
  useDisplay,
  useHyperswitchTopup,
  useRedeemCode,
  useStripeTopup,
  useTopup,
} from "../api/hooks";
import { Button } from "../components/ui";
import { TextInput } from "../components/ui";
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "../components/ui";
import { CodeBlock } from "../components/ui";
import { Banner, Card, Cr, Field, StatChip, creditsWithDisplay } from "../components/ui";

function amountToMinor(raw: string): number | null {
  const value = Number(raw);
  if (!Number.isFinite(value) || value <= 0) return null;
  return Math.round(value * 1_000_000);
}

/** The consequence line a consequential form owes its submit button — what
 *  pressing it actually does, stated plainly before the commit. */
function Consequence({ children }: { children: ReactNode }) {
  return <p className="text-xs text-muted">{children}</p>;
}

function ReceiptView({ receipt }: { receipt: { entryId: string; contentHash: string; isNew: boolean } }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-2">
      <Banner tone="success">
        {receipt.isNew ? t("topup.credited") : t("topup.receiptReplays")}
      </Banner>
      <CodeBlock
        code={JSON.stringify(receipt, null, 2)}
        language="json"
      />
      <p className="text-xs text-muted">
        <Trans
          i18nKey="topup.keepContentHash"
          components={{ code: <code className="mono" /> }}
        />
      </p>
    </div>
  );
}

/**
 * The shared amount→minor→submit rail every hosted/recorded payment rides.
 * Each rail supplies its mutation hook, its consequence copy, and — for the
 * hosted rails — the success handler that redirects to checkout. Hyperswitch
 * is the exception: its SDK-mount lifecycle is genuinely distinct, so it
 * keeps its own component below.
 */
function AmountRail<TData>(props: {
  intro: string;
  consequence: (amount: string) => ReactNode;
  submitLabel: string;
  loadingText: string;
  mutation: { mutate: (minor: number, opts?: { onSuccess?: (data: TData) => void }) => void; isPending: boolean; error: Error | null; data: TData | undefined };
  onSuccess?: (data: TData) => void;
  renderSuccess?: (data: TData) => ReactNode;
  renderError?: (error: ApiError) => ReactNode;
}) {
  const { mutation } = props;
  const { t } = useTranslation();
  const [amount, setAmount] = useState("");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const minor = amountToMinor(amount);
    if (minor !== null) {
      mutation.mutate(minor, props.onSuccess ? { onSuccess: props.onSuccess } : undefined);
    }
  };
  return (
    <form className="flex flex-col gap-3" onSubmit={submit}>
      <p className="text-sm text-muted">{props.intro}</p>
      <Field label={t("topup.amountCredits")}>
        <TextInput
          type="number"
          step="any"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          min="0.000001"
          trailingSlot={<span className="text-xs text-muted-foreground">cr</span>}
          required
        />
      </Field>
      <Consequence>{props.consequence(amount)}</Consequence>
      <Button type="submit" isLoading={mutation.isPending} loadingText={props.loadingText}>
        {props.submitLabel}
      </Button>
      {mutation.error instanceof ApiError
        ? (props.renderError?.(mutation.error) ?? <Banner tone="error">{mutation.error.message}</Banner>)
        : null}
      {mutation.data && props.renderSuccess ? props.renderSuccess(mutation.data) : null}
    </form>
  );
}

function ManualRail() {
  const { t } = useTranslation();
  const topup = useTopup();
  return (
    <AmountRail
      intro={t("topup.manualIntro")}
      consequence={(amount) => (
        <>
          {amountToMinor(amount) !== null
            ? t("topup.manualConsequence", { amount: `${amount} cr` })
            : t("topup.manualConsequence", { amount: t("topup.theAmount") })}
        </>
      )}
      submitLabel={t("topup.creditBalance")}
      loadingText={t("topup.booking")}
      mutation={topup}
      renderSuccess={(receipt) => <ReceiptView receipt={receipt} />}
    />
  );
}

function StripeRail() {
  const { t } = useTranslation();
  const stripe = useStripeTopup();
  return (
    <AmountRail
      intro={t("topup.stripeIntro")}
      consequence={() => <>{t("topup.stripeConsequence")}</>}
      submitLabel={t("topup.payStripe")}
      loadingText={t("topup.openingCheckout")}
      mutation={stripe}
      onSuccess={(res) => {
        window.location.href = res.checkoutUrl;
      }}
      renderError={(error) => (
        <Banner tone="error">
          {error.status === 503 ? t("topup.noStripeRail") : error.message}
        </Banner>
      )}
    />
  );
}

function AirwallexRail() {
  const { t } = useTranslation();
  const airwallex = useAirwallexTopup();
  return (
    <AmountRail
      intro={t("topup.airwallexIntro")}
      consequence={() => <>{t("topup.airwallexConsequence")}</>}
      submitLabel={t("topup.payAirwallex")}
      loadingText={t("topup.openingPayment")}
      mutation={airwallex}
      onSuccess={(res) => {
        window.location.href = res.checkoutUrl;
      }}
      renderError={(error) => (
        <Banner tone="error">
          {error.status === 503 ? t("topup.noAirwallexRail") : error.message}
        </Banner>
      )}
    />
  );
}

// The switch's browser SDK (issue #320): HyperLoader.js installs `window.Hyper`,
// the factory the Unified Checkout is mounted through. The script lives on the
// switch's own SDK origin — never on this dashboard's bundle — and only ever
// loads on this page; the bill page stays third-party-script-free.
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

function HyperswitchRail() {
  const { t } = useTranslation();
  const hyperswitch = useHyperswitchTopup();
  const [amount, setAmount] = useState("");
  const [stage, setStage] = useState<"idle" | "mounting" | "mounted" | "failed">("idle");
  const [mountError, setMountError] = useState<string | null>(null);
  const mountRef = useRef<HTMLDivElement>(null);
  const elementRef = useRef<HyperElement | null>(null);

  // Mount Unified Checkout as soon as the intent's handles land: the SDK
  // script loads from the origin the server named, then the payment element
  // binds to the div below. The element owns the redirect — confirm returns
  // to this page on `?hyperswitch=return`.
  useEffect(() => {
    const intent = hyperswitch.data;
    if (!intent || !mountRef.current) return;
    let cancelled = false;
    void (async () => {
      try {
        await loadHyper(intent.sdkUrl);
        if (cancelled || !mountRef.current || !window.Hyper) return;
        const hyper = window.Hyper(intent.publishableKey, {});
        const elements = hyper.elements({ clientSecret: intent.clientSecret });
        const element = elements.create("payment", {});
        element.mount("#hyperswitch-checkout");
        elementRef.current = element;
        if (!cancelled) setStage("mounted");
      } catch (error) {
        if (!cancelled) {
          setMountError(error instanceof Error ? error.message : t("topup.mountError"));
          setStage("failed");
        }
      }
    })();
    return () => {
      cancelled = true;
      // A fresh intent replaces the element: unmount the old iframe so the
      // div is empty for the new mount.
      elementRef.current?.unmount();
      elementRef.current = null;
    };
  }, [hyperswitch.data]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const minor = amountToMinor(amount);
    if (minor === null) return;
    setMountError(null);
    setStage("mounting");
    hyperswitch.mutate(minor, {
      onError: () => setStage("idle"),
    });
  };

  const reset = () => {
    elementRef.current?.unmount();
    elementRef.current = null;
    hyperswitch.reset();
    setStage("idle");
    setMountError(null);
  };

  return (
    <div className="flex flex-col gap-3">
      <form className="flex flex-col gap-3" onSubmit={submit}>
        <p className="text-sm text-muted">{t("topup.hyperswitchIntro")}</p>
        <Field label={t("topup.amountCredits")}>
          <TextInput
            type="number"
            step="any"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            min="0.000001"
            trailingSlot={<span className="text-xs text-muted-foreground">cr</span>}
            required
            disabled={stage === "mounting" || stage === "mounted"}
          />
        </Field>
        {stage !== "mounted" ? (
          <>
            <Consequence>{t("topup.hyperswitchConsequence")}</Consequence>
            <Button
              type="submit"
              isLoading={hyperswitch.isPending || stage === "mounting"}
              loadingText={t("topup.loadingCheckout")}
            >
              {t("topup.payHyperswitch")}
            </Button>
            {stage === "mounting" ? (
              <p className="text-xs text-muted" role="status">{t("topup.loadingSecure")}</p>
            ) : null}
            {stage === "failed" ? (
              <Banner tone="error">{t("topup.checkoutFailed")}</Banner>
            ) : null}
          </>
        ) : null}
        {hyperswitch.error instanceof ApiError ? (
          <Banner tone="error">
            {hyperswitch.error.status === 503
              ? t("topup.noHyperswitchRail")
              : hyperswitch.error.message}
          </Banner>
        ) : null}
        {mountError ? <Banner tone="error">{mountError}</Banner> : null}
      </form>
      {/* The checkout owns everything under this node once mounted. */}
      <div id="hyperswitch-checkout" ref={mountRef} />
      {stage === "mounted" ? (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-muted">{t("topup.finishPayment")}</p>
          <button
            type="button"
            className="self-start text-xs text-muted underline underline-offset-2"
            onClick={reset}
          >
            {t("topup.startOver")}
          </button>
        </div>
      ) : null}
    </div>
  );
}

function RedeemRail() {
  const { t } = useTranslation();
  const redeem = useRedeemCode();
  const display = useDisplay();
  const [code, setCode] = useState("");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (code.trim() !== "") redeem.mutate(code.trim());
  };
  return (
    <form className="flex flex-col gap-3" onSubmit={submit}>
      <p className="text-sm text-muted">
        <Trans
          i18nKey="topup.redeemIntro"
          components={{ code: <code className="mono" /> }}
        />
      </p>
      <Field label={t("topup.redemptionCode")}>
        <TextInput
          value={code}
          onChange={(e) => setCode(e.target.value)}
          placeholder="oxr-…"
          className="mono"
          required
        />
      </Field>
      <Consequence>{t("topup.redeemConsequence")}</Consequence>
      <Button type="submit" isLoading={redeem.isPending} loadingText={t("topup.claiming")}>
        {t("topup.redeemCode")}
      </Button>
      {redeem.error instanceof ApiError ? <Banner tone="error">{redeem.error.message}</Banner> : null}
      {redeem.data ? (
        <div className="flex flex-col gap-2">
          <Banner tone="success">
            {t("topup.creditedAmount", {
              amount: creditsWithDisplay(redeem.data.amountMinor, display),
            })}
          </Banner>
          <ReceiptView receipt={redeem.data} />
        </div>
      ) : null}
    </form>
  );
}

// Where a returning payer lands (`?hyperswitch=return`, set as the payment's
// return_url): the webhook may not have landed yet, so the balance is polled
// until the credit shows — success, cancel and failure all redirect here, so
// the poll is what tells them apart.
function HyperswitchReturn({ baseline }: { baseline: number | null }) {
  const { t } = useTranslation();
  const balance = useBalance({ refetchInterval: 2_000 });
  const [timedOut, setTimedOut] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setTimedOut(true), 60_000);
    return () => clearTimeout(timer);
  }, []);
  if (baseline !== null && balance.data && balance.data.availableMinor > baseline) {
    return (
      <Banner tone="success">
        {t("topup.paymentConfirmed", {
          amount: creditsWithDisplay(balance.data.availableMinor - baseline, null),
        })}
      </Banner>
    );
  }
  if (timedOut) {
    return <Banner tone="error">{t("topup.paymentTimeout")}</Banner>;
  }
  return <Banner tone="info">{t("topup.paymentWaiting")}</Banner>;
}

export function Topup() {
  const { t } = useTranslation();
  const balance = useBalance();
  const display = useDisplay();
  const [params, setParams] = useSearchParams();
  const returned = params.get("hyperswitch") === "return";
  // Snapshot the balance the first time a return is seen: the credit showing
  // above it is what "confirmed" means. A user who already had balance never
  // sees a false success — only a real increase counts.
  const baselineRef = useRef<number | null>(null);
  useEffect(() => {
    if (returned && balance.data && baselineRef.current === null) {
      baselineRef.current = balance.data.availableMinor;
    }
  }, [returned, balance.data]);
  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">{t("topup.title")}</h1>
      <Card title={t("topup.balance")}>
        <div className="flex flex-wrap items-center gap-x-12 gap-y-4">
          <div>
            <p className="text-[11px] font-medium uppercase tracking-wider text-muted">
              {t("topup.available")}
            </p>
            <p className="mt-1 text-3xl font-bold tabular-nums tracking-tight text-ink">
              {balance.data ? <Cr minor={balance.data.availableMinor} currency={display} /> : "…"}
            </p>
            {display && balance.data ? (
              <p className="mt-0.5 text-xs text-muted">
                ≈ {creditsWithDisplay(balance.data.availableMinor, display)}
              </p>
            ) : null}
          </div>
          <StatChip
            label={t("topup.inFlight")}
            value={balance.data ? <Cr minor={balance.data.reservedMinor} currency={display} /> : "…"}
            hint={t("topup.frozenHint")}
          />
          <StatChip
            label={t("topup.spentThisMonth")}
            value={balance.data ? <Cr minor={balance.data.spentThisMonthMinor} currency={display} /> : "…"}
          />
          <StatChip
            label={t("topup.creditLine")}
            value={
              balance.data && balance.data.creditLimitMinor > 0
                ? `${(balance.data.creditUsedMinor / 1_000_000).toFixed(0)} / ${(balance.data.creditLimitMinor / 1_000_000).toFixed(0)}`
                : "—"
            }
            hint={
              balance.data && balance.data.creditLimitMinor > 0
                ? t("topup.crDrawn")
                : t("topup.noLine")
            }
          />
        </div>
      </Card>
      {returned ? (
        <div className="flex flex-col gap-2">
          <HyperswitchReturn baseline={baselineRef.current} />
          <button
            type="button"
            className="self-start text-xs text-muted underline underline-offset-2"
            onClick={() => setParams({})}
          >
            {t("topup.dismiss")}
          </button>
        </div>
      ) : null}
      <Tabs defaultValue="manual">
        <TabsList variant="pill">
          <TabsTrigger value="manual">{t("topup.manual")}</TabsTrigger>
          <TabsTrigger value="stripe">{t("topup.stripe")}</TabsTrigger>
          <TabsTrigger value="airwallex">{t("topup.airwallex")}</TabsTrigger>
          <TabsTrigger value="hyperswitch">{t("topup.hyperswitch")}</TabsTrigger>
          <TabsTrigger value="redeem">{t("topup.redeemCode")}</TabsTrigger>
        </TabsList>
        <TabsContent value="manual" className="mt-4">
          <Card title={t("topup.manualTopup")}>
            <ManualRail />
          </Card>
        </TabsContent>
        <TabsContent value="stripe" className="mt-4">
          <Card title={t("topup.stripeCheckout")}>
            <StripeRail />
          </Card>
        </TabsContent>
        <TabsContent value="airwallex" className="mt-4">
          <Card title={t("topup.airwallexPage")}>
            <AirwallexRail />
          </Card>
        </TabsContent>
        <TabsContent value="hyperswitch" className="mt-4">
          <Card title={t("topup.hyperswitchCheckout")}>
            <HyperswitchRail />
          </Card>
        </TabsContent>
        <TabsContent value="redeem" className="mt-4">
          <Card title={t("topup.redeemACode")}>
            <RedeemRail />
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  );
}
