import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
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
  TabsIndicator,
  TabsList,
  TabsTrigger,
} from "../components/ui";
import { CodeBlock } from "../components/ui";
import { Banner, Card, Cr, Field, creditsWithDisplay } from "../components/ui";

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
  return (
    <div className="flex flex-col gap-2">
      <Banner tone="success">
        {receipt.isNew ? "Credited." : "This request already landed — the receipt replays."}
      </Banner>
      <CodeBlock
        code={JSON.stringify(receipt, null, 2)}
        language="json"
      />
      <p className="text-xs text-muted">
        Keep <code className="mono">contentHash</code> — it names the ledger entry a bill verifies against.
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
      <Field label="Amount (credits)">
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
  const topup = useTopup();
  return (
    <AmountRail
      intro="Record a payment that arrived out of band; the credit lands immediately. For card payment use Stripe; for an operator-minted code use the redemption rail."
      consequence={(amount) => <>This books {amountToMinor(amount) !== null ? `${amount} cr` : "the amount"} onto your balance immediately.</>}
      submitLabel="Credit balance"
      loadingText="Booking…"
      mutation={topup}
      renderSuccess={(receipt) => <ReceiptView receipt={receipt} />}
    />
  );
}

function StripeRail() {
  const stripe = useStripeTopup();
  return (
    <AmountRail
      intro="Opens the hosted Stripe checkout; the deposit confirms through the Stripe webhook once the session completes. A deployment without Stripe configured answers 503."
      consequence={() => <>This opens Stripe's hosted checkout — you aren't charged until you confirm there.</>}
      submitLabel="Pay with Stripe"
      loadingText="Opening checkout…"
      mutation={stripe}
      onSuccess={(res) => {
        window.location.href = res.checkoutUrl;
      }}
      renderError={(error) => (
        <Banner tone="error">
          {error.status === 503 ? "This deployment has no Stripe rail configured." : error.message}
        </Banner>
      )}
    />
  );
}

function AirwallexRail() {
  const airwallex = useAirwallexTopup();
  return (
    <AmountRail
      intro="Opens the hosted Airwallex payment page; the deposit confirms through the Airwallex webhook once the intent succeeds. A deployment without Airwallex configured answers 503."
      consequence={() => <>This opens Airwallex's hosted payment page — you aren't charged until you confirm there.</>}
      submitLabel="Pay with Airwallex"
      loadingText="Opening payment page…"
      mutation={airwallex}
      onSuccess={(res) => {
        window.location.href = res.checkoutUrl;
      }}
      renderError={(error) => (
        <Banner tone="error">
          {error.status === 503 ? "This deployment has no Airwallex rail configured." : error.message}
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
          setMountError(error instanceof Error ? error.message : "the checkout could not mount");
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
        <p className="text-sm text-muted">
          Mounts the switch's Unified Checkout in place; the deposit confirms through the
          Hyperswitch webhook once the payment succeeds. A deployment without the rail configured
          answers 503.
        </p>
        <Field label="Amount (credits)">
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
            <Consequence>
              This creates a payment intent and mounts the switch's checkout below — nothing is
              charged until you complete it.
            </Consequence>
            <Button
              type="submit"
              isLoading={hyperswitch.isPending || stage === "mounting"}
              loadingText="Loading checkout…"
            >
              Pay with Hyperswitch
            </Button>
            {stage === "mounting" ? (
              <p className="text-xs text-muted" role="status">Loading the secure checkout…</p>
            ) : null}
            {stage === "failed" ? (
              <Banner tone="error">The checkout could not load — try again or pick another rail.</Banner>
            ) : null}
          </>
        ) : null}
        {hyperswitch.error instanceof ApiError ? (
          <Banner tone="error">
            {hyperswitch.error.status === 503
              ? "This deployment has no Hyperswitch rail configured."
              : hyperswitch.error.message}
          </Banner>
        ) : null}
        {mountError ? <Banner tone="error">{mountError}</Banner> : null}
      </form>
      {/* The checkout owns everything under this node once mounted. */}
      <div id="hyperswitch-checkout" ref={mountRef} />
      {stage === "mounted" ? (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-muted">
            Finish the payment in the form above — the wallet credits when the switch's webhook
            lands. Nothing is charged until you confirm.
          </p>
          <button
            type="button"
            className="self-start text-xs text-muted underline underline-offset-2"
            onClick={reset}
          >
            Start over
          </button>
        </div>
      ) : null}
    </div>
  );
}

function RedeemRail() {
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
        An <code className="mono">oxr-…</code> code the operator minted. Unknown, spent, or expired
        codes all answer 404 — which of the three it is stays the holder's business.
      </p>
      <Field label="Redemption code">
        <TextInput
          value={code}
          onChange={(e) => setCode(e.target.value)}
          placeholder="oxr-…"
          className="mono"
          required
        />
      </Field>
      <Consequence>This claims the code's value onto your balance immediately — a spent code can't be reused.</Consequence>
      <Button type="submit" isLoading={redeem.isPending} loadingText="Claiming…">
        Redeem code
      </Button>
      {redeem.error instanceof ApiError ? <Banner tone="error">{redeem.error.message}</Banner> : null}
      {redeem.data ? (
        <div className="flex flex-col gap-2">
          <Banner tone="success">Credited {creditsWithDisplay(redeem.data.amountMinor, display)}.</Banner>
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
  const balance = useBalance({ refetchInterval: 2_000 });
  const [timedOut, setTimedOut] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setTimedOut(true), 60_000);
    return () => clearTimeout(timer);
  }, []);
  if (baseline !== null && balance.data && balance.data.availableMinor > baseline) {
    return (
      <Banner tone="success">
        Payment confirmed — {creditsWithDisplay(balance.data.availableMinor - baseline, null)}{" "}
        landed on the balance.
      </Banner>
    );
  }
  if (timedOut) {
    return (
      <Banner tone="error">
        No credit landed within a minute. If you completed the payment the webhook is still on its
        way — the balance catches up on its own; if you cancelled, nothing was charged.
      </Banner>
    );
  }
  return (
    <Banner tone="info">
      Waiting for the payment to confirm… the switch's webhook credits the wallet; this updates as
      soon as it lands.
    </Banner>
  );
}

export function Topup() {
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
      <h1 className="text-2xl font-bold">Top up</h1>
      <Card title="Balance">
        <p className="text-2xl font-semibold tabular-nums">
          {balance.data ? <Cr minor={balance.data.availableMinor} currency={display} /> : "…"}
        </p>
      </Card>
      {returned ? (
        <div className="flex flex-col gap-2">
          <HyperswitchReturn baseline={baselineRef.current} />
          <button
            type="button"
            className="self-start text-xs text-muted underline underline-offset-2"
            onClick={() => setParams({})}
          >
            Dismiss
          </button>
        </div>
      ) : null}
      <Tabs defaultValue="manual">
        <TabsList variant="pill">
          <TabsTrigger value="manual">Manual</TabsTrigger>
          <TabsTrigger value="stripe">Stripe</TabsTrigger>
          <TabsTrigger value="airwallex">Airwallex</TabsTrigger>
          <TabsTrigger value="hyperswitch">Hyperswitch</TabsTrigger>
          <TabsTrigger value="redeem">Redeem code</TabsTrigger>
          <TabsIndicator variant="pill" />
        </TabsList>
        <TabsContent value="manual" className="mt-4">
          <Card title="Manual top-up">
            <ManualRail />
          </Card>
        </TabsContent>
        <TabsContent value="stripe" className="mt-4">
          <Card title="Stripe checkout">
            <StripeRail />
          </Card>
        </TabsContent>
        <TabsContent value="airwallex" className="mt-4">
          <Card title="Airwallex payment page">
            <AirwallexRail />
          </Card>
        </TabsContent>
        <TabsContent value="hyperswitch" className="mt-4">
          <Card title="Hyperswitch checkout">
            <HyperswitchRail />
          </Card>
        </TabsContent>
        <TabsContent value="redeem" className="mt-4">
          <Card title="Redeem a code">
            <RedeemRail />
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  );
}
