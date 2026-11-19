import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { api } from "../api/client";
import type { components } from "../api/schema";
import { useBusiness, useOrg } from "../api/hooks";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogHeader, AlertDialogTitle, Banner, Button, Card, Field, NativeSelect, Skeleton, TextInput } from "./ui";

type Policy = components["schemas"]["BillingPolicyVersion"];
type Customer = components["schemas"]["CustomerBillingPolicy"];
type Acceptance = components["schemas"]["BillingPolicyAcceptance"];

function PolicyTerms({ policy }: { policy: Policy }) {
  return <div className="flex flex-col gap-2"><p>Version {policy.version} · {policy.implementation}</p><p>{policy.scope}</p><p>{policy.disclosure}</p><p className="break-all font-mono text-sm">Digest: {policy.digest}</p><p>{policy.runtimeReady ? "Available for explicit acceptance" : "Published for review; implementation is not enabled in this runtime"}</p></div>;
}

/** Publication creates an offer, never a customer acceptance. */
export function BusinessBillingPolicies() {
  const principal = useBusiness().data;
  const history = useQuery({ queryKey: ["business-billing-policies"], queryFn: () => api<Policy[]>("/business/billing-policies"), enabled: !!principal });
  const [implementation, setImplementation] = useState("provider-reported-text-v1");
  const [windowSecs, setWindowSecs] = useState("900");
  const [reason, setReason] = useState("");
  const [open, setOpen] = useState(false);
  const [request, setRequest] = useState<components["schemas"]["BillingPolicyPublish"]>();
  const publish = useMutation({ mutationFn: () => api<Policy>("/business/billing-policies", { method: "POST", body: request! }), onSuccess: () => { setOpen(false); setRequest(undefined); void history.refetch(); } });
  const valid = /^\d+$/.test(windowSecs) && Number(windowSecs) >= 1 && Number(windowSecs) <= 900 && reason.trim();
  return <Card title="Exception billing policies">
    <p className="mb-4">Existing customers retain their terms. Publishing makes an immutable offer; only a customer owner or admin can accept it for subsequent admissions.</p>
    {history.isLoading ? <Skeleton className="h-24" /> : history.error ? <Banner tone="error">{history.error.message}</Banner> : null}
    {principal?.capabilities.includes("catalogWrite") ? <div className="flex flex-col gap-4">
      <Field label="Implementation"><NativeSelect value={implementation} onChange={e => setImplementation(e.target.value)}><option value="provider-reported-text-v1">Provider-reported text v1</option><option value="legacy-estimation-v1">Explicit legacy estimation v1</option></NativeSelect></Field>
      <Field label="Maximum evidence window after terminal observation (seconds)"><TextInput inputMode="numeric" value={windowSecs} onChange={e => setWindowSecs(e.target.value)} /></Field>
      <Field label="Publication reason"><TextInput maxLength={500} value={reason} onChange={e => setReason(e.target.value)} /></Field>
      <Button disabled={!valid || !history.data} onClick={() => { setRequest({ idempotencyKey: crypto.randomUUID(), expectedVersion: history.data?.[0]?.version ?? 0, implementation: implementation as "provider-reported-text-v1" | "legacy-estimation-v1", evidenceWindowSecs: Number(windowSecs), reason }); setOpen(true); }}>Review publication</Button>
    </div> : null}
    <div className="mt-4 flex flex-col gap-4">{history.data?.length === 0 ? <p>No explicit exception policies. Legacy terms remain in effect.</p> : null}{history.data?.map(p => <details key={p.version}><summary className="cursor-pointer text-primary">Version {p.version} · {p.publishedAt}</summary><PolicyTerms policy={p} /></details>)}</div>
    <AlertDialog open={open} onOpenChange={v => { if (!publish.isPending) setOpen(v); }}><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>Publish immutable policy</AlertDialogTitle><AlertDialogDescription>Publish {request?.implementation} with a maximum {request?.evidenceWindowSecs}-second terminal evidence window. No customer selection or active request will change. The full fixed disclosure appears in the published history.</AlertDialogDescription></AlertDialogHeader>
      {publish.error ? <Banner tone="error">{publish.error.message}{publish.error instanceof Error && "status" in publish.error && publish.error.status === 409 ? <Button tone="ghost" onClick={() => { setOpen(false); setRequest(undefined); publish.reset(); void history.refetch(); }}>Reload and review again</Button> : null}</Banner> : null}<Button isLoading={publish.isPending} onClick={() => publish.mutate()}>Publish offer</Button><Button tone="ghost" disabled={publish.isPending} onClick={() => setOpen(false)}>Close</Button>
    </AlertDialogContent></AlertDialog>
  </Card>;
}

/** Customer consent is explicit and bound to the displayed immutable digest. */
export function CustomerBillingPolicies() {
  const org = useOrg().data;
  return org ? <CustomerPolicyForm key={org.id} organizationId={org.id} /> : <Skeleton className="h-24" />;
}
function CustomerPolicyForm({ organizationId }: { organizationId: string }) {
  const state = useQuery({ queryKey: ["customer-billing-policy", organizationId], queryFn: () => api<Customer>("/org/billing-policy"), retry: false });
  const [selected, setSelected] = useState<Policy>();
  const [request, setRequest] = useState<components["schemas"]["BillingPolicyAccept"]>();
  const accept = useMutation({ mutationFn: () => api<Acceptance>("/org/billing-policy/accept", { method: "POST", body: request! }), onSuccess: () => { setSelected(undefined); void state.refetch(); } });
  const current = state.data?.currentPolicy;
  return <Card title="Exception billing terms">
    {state.isLoading ? <Skeleton className="h-24" /> : state.error ? <Banner tone="error">{state.error.message}</Banner> : <div className="flex flex-col gap-4">
      {current ? <><p>Accepted version {current.version} · {state.data?.acceptance?.acceptedAt}</p><PolicyTerms policy={current} /></> : <p>{state.data?.legacyDisclosure}</p>}
      <p>Acceptance applies to future successful admissions in the disclosed scope. Existing requests keep the policy and prices bound when they were admitted. Publishing another version does not change this selection.</p>
      {state.data?.versions.map(p => <div className="border-t border-border pt-4" key={p.version}><PolicyTerms policy={p} /><Button className="mt-4" disabled={!p.runtimeReady || p.version === state.data?.acceptance?.version} onClick={() => { setSelected(p); setRequest({ idempotencyKey: crypto.randomUUID(), expectedOrganizationId: state.data!.organizationId, version: p.version, digest: p.digest, expectedAcceptanceId: state.data?.acceptance?.id ?? null }); }}>Review and accept version {p.version}</Button></div>)}
    </div>}
    <AlertDialog open={!!selected} onOpenChange={v => { if (!v && !accept.isPending) setSelected(undefined); }}><AlertDialogContent className="max-h-[85vh] overflow-y-auto"><AlertDialogHeader><AlertDialogTitle>Accept billing terms for future requests</AlertDialogTitle><AlertDialogDescription>Confirm the exact policy below for {state.data?.organizationName} ({request?.expectedOrganizationId}). Existing history and active requests remain on their admission terms.</AlertDialogDescription></AlertDialogHeader>
      {selected ? <PolicyTerms policy={selected} /> : null}{accept.error ? <Banner tone="error">{accept.error.message}{accept.error instanceof Error && "status" in accept.error && accept.error.status === 409 ? <Button tone="ghost" onClick={() => { setSelected(undefined); setRequest(undefined); accept.reset(); void state.refetch(); }}>Reload and review again</Button> : null}</Banner> : null}<Button isLoading={accept.isPending} onClick={() => accept.mutate()}>Accept this version and digest</Button><Button tone="ghost" disabled={accept.isPending} onClick={() => setSelected(undefined)}>Close</Button>
    </AlertDialogContent></AlertDialog>
  </Card>;
}
