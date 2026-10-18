import { useState, type FormEvent } from "react";
import {
  ApiError,
  useAirwallexTopup,
  useBalance,
  useDisplay,
  useRedeemCode,
  useStripeTopup,
  useTopup,
} from "../api/hooks";
import { HaloButton } from "../components/cult/halo-button";
import { HaloInput } from "../components/cult/halo-input";
import {
  HaloTabs,
  HaloTabsContent,
  HaloTabsIndicator,
  HaloTabsList,
  HaloTabsTrigger,
} from "../components/cult/halo-tabs";
import { CodeBlock } from "../components/cult/code-block";
import { Banner, Card, Cr, Field, creditsWithDisplay } from "../components/ui";

function amountToMinor(raw: string): number | null {
  const value = Number(raw);
  if (!Number.isFinite(value) || value <= 0) return null;
  return Math.round(value * 1_000_000);
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

function ManualRail() {
  const topup = useTopup();
  const [amount, setAmount] = useState("");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const minor = amountToMinor(amount);
    if (minor !== null) topup.mutate(minor);
  };
  return (
    <form className="flex flex-col gap-3" onSubmit={submit}>
      <p className="text-sm text-muted">
        Record a payment that arrived out of band; the credit lands immediately. For card payment use
        Stripe; for an operator-minted code use the redemption rail.
      </p>
      <Field label="Amount (credits)">
        <HaloInput
          type="number"
          step="any"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          min="0.000001"
          trailingSlot={<span className="text-xs text-muted-foreground">cr</span>}
          required
        />
      </Field>
      <HaloButton type="submit" isLoading={topup.isPending} loadingText="Booking…">
        Credit balance
      </HaloButton>
      {topup.error instanceof ApiError ? <Banner tone="error">{topup.error.message}</Banner> : null}
      {topup.data ? <ReceiptView receipt={topup.data} /> : null}
    </form>
  );
}

function StripeRail() {
  const stripe = useStripeTopup();
  const [amount, setAmount] = useState("");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const minor = amountToMinor(amount);
    if (minor !== null) {
      stripe.mutate(minor, {
        onSuccess: (res) => {
          window.location.href = res.checkoutUrl;
        },
      });
    }
  };
  return (
    <form className="flex flex-col gap-3" onSubmit={submit}>
      <p className="text-sm text-muted">
        Opens the hosted Stripe checkout; the deposit confirms through the Stripe webhook once the
        session completes. A deployment without Stripe configured answers 503.
      </p>
      <Field label="Amount (credits)">
        <HaloInput
          type="number"
          step="any"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          min="0.000001"
          trailingSlot={<span className="text-xs text-muted-foreground">cr</span>}
          required
        />
      </Field>
      <HaloButton type="submit" isLoading={stripe.isPending} loadingText="Opening checkout…">
        Pay with Stripe
      </HaloButton>
      {stripe.error instanceof ApiError ? (
        <Banner tone="error">
          {stripe.error.status === 503
            ? "This deployment has no Stripe rail configured."
            : stripe.error.message}
        </Banner>
      ) : null}
    </form>
  );
}

function AirwallexRail() {
  const airwallex = useAirwallexTopup();
  const [amount, setAmount] = useState("");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const minor = amountToMinor(amount);
    if (minor !== null) {
      airwallex.mutate(minor, {
        onSuccess: (res) => {
          window.location.href = res.checkoutUrl;
        },
      });
    }
  };
  return (
    <form className="flex flex-col gap-3" onSubmit={submit}>
      <p className="text-sm text-muted">
        Opens the hosted Airwallex payment page; the deposit confirms through the Airwallex webhook
        once the intent succeeds. A deployment without Airwallex configured answers 503.
      </p>
      <Field label="Amount (credits)">
        <HaloInput
          type="number"
          step="any"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          min="0.000001"
          trailingSlot={<span className="text-xs text-muted-foreground">cr</span>}
          required
        />
      </Field>
      <HaloButton type="submit" isLoading={airwallex.isPending} loadingText="Opening payment page…">
        Pay with Airwallex
      </HaloButton>
      {airwallex.error instanceof ApiError ? (
        <Banner tone="error">
          {airwallex.error.status === 503
            ? "This deployment has no Airwallex rail configured."
            : airwallex.error.message}
        </Banner>
      ) : null}
    </form>
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
        <HaloInput
          value={code}
          onChange={(e) => setCode(e.target.value)}
          placeholder="oxr-…"
          className="mono"
          required
        />
      </Field>
      <HaloButton type="submit" isLoading={redeem.isPending} loadingText="Claiming…">
        Redeem code
      </HaloButton>
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

export function Topup() {
  const balance = useBalance();
  const display = useDisplay();
  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Top up</h1>
      <Card title="Balance">
        <p className="text-2xl font-semibold tabular-nums">
          {balance.data ? <Cr minor={balance.data.availableMinor} currency={display} /> : "…"}
        </p>
      </Card>
      <HaloTabs defaultValue="manual">
        <HaloTabsList variant="pill">
          <HaloTabsTrigger value="manual">Manual</HaloTabsTrigger>
          <HaloTabsTrigger value="stripe">Stripe</HaloTabsTrigger>
          <HaloTabsTrigger value="airwallex">Airwallex</HaloTabsTrigger>
          <HaloTabsTrigger value="redeem">Redeem code</HaloTabsTrigger>
          <HaloTabsIndicator variant="pill" />
        </HaloTabsList>
        <HaloTabsContent value="manual" className="mt-4">
          <Card title="Manual top-up">
            <ManualRail />
          </Card>
        </HaloTabsContent>
        <HaloTabsContent value="stripe" className="mt-4">
          <Card title="Stripe checkout">
            <StripeRail />
          </Card>
        </HaloTabsContent>
        <HaloTabsContent value="airwallex" className="mt-4">
          <Card title="Airwallex payment page">
            <AirwallexRail />
          </Card>
        </HaloTabsContent>
        <HaloTabsContent value="redeem" className="mt-4">
          <Card title="Redeem a code">
            <RedeemRail />
          </Card>
        </HaloTabsContent>
      </HaloTabs>
    </div>
  );
}
