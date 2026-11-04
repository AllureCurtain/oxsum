import { type FormEvent, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowDown, ArrowUp, Plus, Shield, Trash2, X } from "lucide-react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
  Banner,
  Button,
  Card,
  Checkbox,
  EmptyRows,
  Field,
  IconButton,
  NumberInput,
  Select,
  TableShell,
  Td,
  TextInput,
} from "../../components/ui";
import { ApiError } from "../../api/client";
import type { components } from "../../api/schema";
import { adminApi, useAdmin } from "./api";

type Policy = components["schemas"]["GuardrailPolicy"];
type Binding = components["schemas"]["GuardrailBinding"];

type Kind = "keyword" | "pii" | "moderation";
type Action = "flag" | "block";
type Scope = "channel" | "key";

interface Pattern {
  re: string;
  action: Action;
}

interface Channel {
  name: string;
}

const NAME_RE = /^[a-z0-9][a-z0-9._-]{0,63}$/;

function failure(err: unknown): string {
  return err instanceof ApiError ? err.message : "request failed";
}

function patternsOf(policy: Policy): Pattern[] {
  const config = policy.config as { patterns?: Pattern[] };
  return Array.isArray(config.patterns) ? config.patterns : [];
}

function endpointOf(policy: Policy): string {
  const config = policy.config as { endpoint?: string };
  return config.endpoint ?? "";
}

function timeoutOf(policy: Policy): string {
  const config = policy.config as { timeoutMs?: number };
  return typeof config.timeoutMs === "number" ? String(config.timeoutMs) : "";
}

/**
 * The operator console for the guardrail pipeline (issue #502, console in
 * issue #562): named policies, and the channel / key bindings that attach
 * them. The page only speaks the admin contract that already exists — it never
 * reads back a moderation bearer, and it does not invent a key list the server
 * does not expose.
 */
export function Guardrails({ token }: { token: string }) {
  const { t } = useTranslation();
  const policies = useAdmin<Policy[]>(token, "/guardrail-policies");
  const bindings = useAdmin<Binding[]>(token, "/guardrail-bindings");
  const channels = useAdmin<Channel[]>(token, "/channels");

  const policyRows = policies.data ?? [];
  const bindingRows = bindings.data ?? [];
  const channelRows = channels.data ?? [];

  const [editingName, setEditingName] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [kind, setKind] = useState<Kind>("keyword");
  const [patterns, setPatterns] = useState<Pattern[]>([{ re: "", action: "block" }]);
  const [endpoint, setEndpoint] = useState("");
  const [timeoutMs, setTimeoutMs] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [savingPolicy, setSavingPolicy] = useState(false);
  const [deletingPolicy, setDeletingPolicy] = useState<string | null>(null);
  const [policyNotice, setPolicyNotice] = useState<{ tone: "success" | "error"; text: string } | null>(null);

  const [scope, setScope] = useState<Scope>("channel");
  const [target, setTarget] = useState("");
  const [selectedPolicies, setSelectedPolicies] = useState<string[]>([]);
  const [savingBinding, setSavingBinding] = useState(false);
  const [deletingBinding, setDeletingBinding] = useState<string | null>(null);
  const [bindingNotice, setBindingNotice] = useState<{ tone: "success" | "error"; text: string } | null>(null);

  function resetPolicyForm() {
    setEditingName(null);
    setName("");
    setKind("keyword");
    setPatterns([{ re: "", action: "block" }]);
    setEndpoint("");
    setTimeoutMs("");
    setApiKey("");
  }

  function beginEdit(policy: Policy) {
    setEditingName(policy.name);
    setName(policy.name);
    setKind(policy.kind as Kind);
    setPatterns(patternsOf(policy).length ? patternsOf(policy).map((p) => ({ re: p.re, action: p.action })) : [{ re: "", action: "block" }]);
    setEndpoint(endpointOf(policy));
    setTimeoutMs(timeoutOf(policy));
    setApiKey("");
    setPolicyNotice(null);
  }

  async function savePolicy(e: FormEvent) {
    e.preventDefault();
    const policyName = name.trim();
    if (!policyName) {
      setPolicyNotice({ tone: "error", text: t("admin.guardrails.nameRequired") });
      return;
    }
    if (!NAME_RE.test(policyName)) {
      setPolicyNotice({ tone: "error", text: t("admin.guardrails.nameInvalid") });
      return;
    }

    let config: Record<string, unknown>;
    if (kind === "keyword") {
      const cleaned = patterns
        .map((p) => ({ re: p.re.trim(), action: p.action }))
        .filter((p) => p.re.length > 0);
      if (!cleaned.length) {
        setPolicyNotice({ tone: "error", text: t("admin.guardrails.patternRequired") });
        return;
      }
      config = { patterns: cleaned };
    } else if (kind === "pii") {
      config = {};
    } else {
      const url = endpoint.trim();
      if (!url) {
        setPolicyNotice({ tone: "error", text: t("admin.guardrails.endpointRequired") });
        return;
      }
      if (!/^https?:\/\/\S+$/i.test(url)) {
        setPolicyNotice({ tone: "error", text: t("admin.guardrails.endpointInvalid") });
        return;
      }
      config = { endpoint: url };
      if (timeoutMs.trim()) {
        const parsed = Number(timeoutMs);
        if (!Number.isFinite(parsed) || parsed <= 0 || !Number.isInteger(parsed)) {
          setPolicyNotice({ tone: "error", text: t("admin.guardrails.timeoutInvalid") });
          return;
        }
        config.timeoutMs = parsed;
      }
    }

    const body: { kind: Kind; config: Record<string, unknown>; apiKey?: string } = { kind, config };
    // The bearer is write-only: blank means "keep whatever is stored".
    if (kind === "moderation" && apiKey.trim()) body.apiKey = apiKey.trim();

    setSavingPolicy(true);
    try {
      await adminApi(token, `/guardrail-policies/${encodeURIComponent(policyName)}`, { method: "PUT", body });
      setPolicyNotice({ tone: "success", text: t("admin.guardrails.policySaved", { name: policyName }) });
      resetPolicyForm();
      policies.refresh();
      bindings.refresh();
    } catch (err) {
      setPolicyNotice({ tone: "error", text: failure(err) });
    } finally {
      setSavingPolicy(false);
    }
  }

  async function deletePolicy(policyName: string) {
    setDeletingPolicy(policyName);
    setPolicyNotice(null);
    try {
      await adminApi(token, `/guardrail-policies/${encodeURIComponent(policyName)}`, { method: "DELETE" });
      setPolicyNotice({ tone: "success", text: t("admin.guardrails.policyDeleted", { name: policyName }) });
      if (editingName === policyName) resetPolicyForm();
      policies.refresh();
      bindings.refresh();
    } catch (err) {
      setPolicyNotice({ tone: "error", text: failure(err) });
    } finally {
      setDeletingPolicy(null);
    }
  }

  function togglePolicy(policyName: string) {
    setSelectedPolicies((prev) =>
      prev.includes(policyName) ? prev.filter((n) => n !== policyName) : [...prev, policyName],
    );
  }

  function movePolicy(index: number, delta: -1 | 1) {
    setSelectedPolicies((prev) => {
      const next = [...prev];
      const at = index + delta;
      if (at < 0 || at >= next.length) return prev;
      [next[index], next[at]] = [next[at], next[index]];
      return next;
    });
  }

  function beginBinding(binding: Binding) {
    setScope(binding.scope as Scope);
    setTarget(binding.target);
    setSelectedPolicies(binding.policies);
    setBindingNotice(null);
  }

  async function saveBinding(e: FormEvent) {
    e.preventDefault();
    const trimmed = target.trim();
    if (!trimmed) {
      setBindingNotice({ tone: "error", text: t("admin.guardrails.targetRequired") });
      return;
    }
    setSavingBinding(true);
    try {
      await adminApi(token, `/guardrail-bindings/${scope}/${encodeURIComponent(trimmed)}`, {
        method: "PUT",
        body: { policies: selectedPolicies },
      });
      setBindingNotice({ tone: "success", text: t("admin.guardrails.bindingSaved") });
      bindings.refresh();
    } catch (err) {
      setBindingNotice({ tone: "error", text: failure(err) });
    } finally {
      setSavingBinding(false);
    }
  }

  async function deleteBinding(binding: Binding) {
    const key = `${binding.scope}/${binding.target}`;
    setDeletingBinding(key);
    setBindingNotice(null);
    try {
      await adminApi(token, `/guardrail-bindings/${binding.scope}/${encodeURIComponent(binding.target)}`, {
        method: "DELETE",
      });
      setBindingNotice({ tone: "success", text: t("admin.guardrails.bindingDeleted") });
      bindings.refresh();
    } catch (err) {
      setBindingNotice({ tone: "error", text: failure(err) });
    } finally {
      setDeletingBinding(null);
    }
  }

  const policyNames = policyRows.map((p) => p.name);
  const channelNames = channelRows.map((c) => c.name);
  const targetOptions = target && !channelNames.includes(target) ? [target, ...channelNames] : channelNames;

  /** A one-line config summary for the policy table — the kind's own shape. */
  function configText(policy: Policy): string {
    if (policy.kind === "keyword") {
      const list = patternsOf(policy);
      if (!list.length) return "—";
      return list
        .map((pattern) => `${pattern.re} · ${t(`admin.guardrails.action.${pattern.action}`)}`)
        .join(", ");
    }
    if (policy.kind === "pii") return t("admin.guardrails.piiSummary");
    return endpointOf(policy) || "—";
  }

  return (
    <div className="flex flex-col gap-4">
      <Card title={t("admin.guardrails.howTitle")}>
        <dl className="grid gap-3 text-sm sm:grid-cols-2">
          <div>
            <dt className="text-xs font-medium text-muted">{t("admin.guardrails.preCall")}</dt>
            <dd className="mt-1 text-ink">{t("admin.guardrails.preCallBody")}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("admin.guardrails.postCall")}</dt>
            <dd className="mt-1 text-ink">{t("admin.guardrails.postCallBody")}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("admin.guardrails.streamed")}</dt>
            <dd className="mt-1 text-ink">{t("admin.guardrails.streamedBody")}</dd>
          </div>
          <div>
            <dt className="text-xs font-medium text-muted">{t("admin.guardrails.recorded")}</dt>
            <dd className="mt-1 text-ink">{t("admin.guardrails.recordedBody")}</dd>
          </div>
        </dl>
        <p className="mt-3 text-xs text-muted">{t("admin.guardrails.failOpen")}</p>
      </Card>

      <Card
        title={t("admin.guardrails.policiesTitle")}
        actions={
          <span className="text-xs text-muted">
            {t("admin.guardrails.policiesCount", { count: policyRows.length })}
          </span>
        }
      >
        {policies.error ? <Banner tone="error" className="mb-3">{policies.error}</Banner> : null}
        <TableShell
          head={[
            t("admin.guardrails.colName"),
            t("admin.guardrails.colKind"),
            t("admin.guardrails.colConfig"),
            t("admin.guardrails.colCreated"),
            "",
          ]}
          density="compact"
          sticky={false}
          loading={policies.loading && !policies.data ? 3 : undefined}
        >
          {policyRows.length === 0 && !policies.loading ? (
            <EmptyRows colSpan={5} icon={<Shield className="size-5" />}>
              {t("admin.guardrails.policiesEmpty")}
            </EmptyRows>
          ) : (
            policyRows.map((policy) => {
              const editable = editingName === policy.name;
              return (
                <tr key={policy.name} className={editable ? "bg-canvas-inset" : undefined}>
                  <Td mono>{policy.name}</Td>
                  <Td>{t(`admin.guardrails.kind.${policy.kind}`)}</Td>
                  <Td className="max-w-80 truncate text-muted" title={configText(policy)}>
                    {configText(policy)}
                  </Td>
                  <Td align="right" className="whitespace-nowrap text-muted">
                    {String(policy.createdAt).slice(0, 10)}
                  </Td>
                  <Td align="right">
                    <div className="flex justify-end gap-1.5">
                      <Button type="button" tone="ghost" onClick={() => beginEdit(policy)}>
                        {t("admin.guardrails.edit")}
                      </Button>
                      <AlertDialog>
                        <AlertDialogTrigger asChild>
                          <Button type="button" tone="danger" aria-label={t("admin.guardrails.deletePolicy")}>
                            <Trash2 className="size-3.5" />
                          </Button>
                        </AlertDialogTrigger>
                        <AlertDialogContent>
                          <AlertDialogHeader>
                            <AlertDialogTitle>
                              {t("admin.guardrails.deletePolicyTitle", { name: policy.name })}
                            </AlertDialogTitle>
                            <AlertDialogDescription>{t("admin.guardrails.deletePolicyNote")}</AlertDialogDescription>
                          </AlertDialogHeader>
                          <AlertDialogFooter>
                            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
                            <AlertDialogAction asChild>
                              <Button
                                type="button"
                                tone="danger"
                                isLoading={deletingPolicy === policy.name}
                                loadingText={t("common.deleting")}
                                onClick={() => void deletePolicy(policy.name)}
                              >
                                {t("common.delete")}
                              </Button>
                            </AlertDialogAction>
                          </AlertDialogFooter>
                        </AlertDialogContent>
                      </AlertDialog>
                    </div>
                  </Td>
                </tr>
              );
            })
          )}
        </TableShell>
      </Card>

      <Card
        title={editingName ? t("admin.guardrails.editPolicy", { name: editingName }) : t("admin.guardrails.newPolicy")}
        actions={
          editingName ? (
            <Button type="button" tone="ghost" onClick={resetPolicyForm}>
              <X className="size-3.5" />
              {t("admin.guardrails.cancelEdit")}
            </Button>
          ) : null
        }
      >
        <form className="flex flex-col gap-4" onSubmit={savePolicy}>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            <Field label={t("admin.guardrails.name")} hint={editingName ? t("admin.guardrails.nameLocked") : t("admin.guardrails.nameHint")}>
              <TextInput
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="no-secrets"
                disabled={editingName !== null}
                required
              />
            </Field>
            <Field label={t("admin.guardrails.kindLabel")}>
              <Select value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
                <option value="keyword">{t("admin.guardrails.kind.keyword")}</option>
                <option value="pii">{t("admin.guardrails.kind.pii")}</option>
                <option value="moderation">{t("admin.guardrails.kind.moderation")}</option>
              </Select>
            </Field>
          </div>

          {kind === "keyword" ? (
            <div className="flex flex-col gap-2">
              <p className="text-xs text-muted">{t("admin.guardrails.keywordHint")}</p>
              {patterns.map((pattern, index) => (
                <div key={index} className="flex flex-col gap-2 sm:flex-row sm:items-end">
                  <Field className="flex-1" label={t("admin.guardrails.pattern")}>
                    <TextInput
                      value={pattern.re}
                      onChange={(e) =>
                        setPatterns((prev) => prev.map((p, i) => (i === index ? { ...p, re: e.target.value } : p)))
                      }
                      placeholder={t("admin.guardrails.patternPlaceholder")}
                    />
                  </Field>
                  <Field className="sm:w-40" label={t("admin.guardrails.actionLabel")}>
                    <Select
                      value={pattern.action}
                      onChange={(e) =>
                        setPatterns((prev) =>
                          prev.map((p, i) => (i === index ? { ...p, action: e.target.value as Action } : p)),
                        )
                      }
                    >
                      <option value="flag">{t("admin.guardrails.action.flag")}</option>
                      <option value="block">{t("admin.guardrails.action.block")}</option>
                    </Select>
                  </Field>
                  <IconButton
                    type="button"
                    label={t("admin.guardrails.removePattern")}
                    disabled={patterns.length === 1}
                    onClick={() => setPatterns((prev) => (prev.length === 1 ? prev : prev.filter((_, i) => i !== index)))}
                    className="mb-0.5"
                  >
                    <X className="size-3.5" />
                  </IconButton>
                </div>
              ))}
              <div>
                <Button
                  type="button"
                  tone="ghost"
                  onClick={() => setPatterns((prev) => [...prev, { re: "", action: "flag" }])}
                >
                  <Plus className="size-3.5" />
                  {t("admin.guardrails.addPattern")}
                </Button>
              </div>
            </div>
          ) : null}

          {kind === "pii" ? <p className="text-xs text-muted">{t("admin.guardrails.piiHint")}</p> : null}

          {kind === "moderation" ? (
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
              <Field className="sm:col-span-2" label={t("admin.guardrails.endpoint")} hint={t("admin.guardrails.endpointHint")}>
                <TextInput
                  value={endpoint}
                  onChange={(e) => setEndpoint(e.target.value)}
                  placeholder="https://moderation.example.com/screen"
                />
              </Field>
              <Field label={t("admin.guardrails.timeout")} hint={t("admin.guardrails.timeoutHint")}>
                <NumberInput min={1} step={1} value={timeoutMs} onChange={(e) => setTimeoutMs(e.target.value)} />
              </Field>
              <Field
                label={t("admin.guardrails.bearer")}
                hint={editingName ? t("admin.guardrails.bearerEditHint") : t("admin.guardrails.bearerHint")}
              >
                <TextInput
                  type="password"
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                  autoComplete="new-password"
                  placeholder={editingName ? "••••••" : ""}
                />
              </Field>
            </div>
          ) : null}

          {policyNotice ? (
            <Banner tone={policyNotice.tone === "error" ? "error" : "success"}>{policyNotice.text}</Banner>
          ) : null}

          <div className="flex items-center gap-2">
            <Button type="submit" isLoading={savingPolicy} loadingText={t("common.saving")}>
              {editingName ? t("admin.guardrails.replacePolicy") : t("admin.guardrails.createPolicy")}
            </Button>
            <Button type="button" tone="ghost" onClick={resetPolicyForm}>
              {t("admin.guardrails.reset")}
            </Button>
          </div>
        </form>
      </Card>

      <Card
        title={t("admin.guardrails.bindingsTitle")}
        actions={
          <span className="text-xs text-muted">
            {t("admin.guardrails.bindingsCount", { count: bindingRows.length })}
          </span>
        }
      >
        <p className="mb-3 text-xs text-muted">{t("admin.guardrails.bindingsHint")}</p>
        {bindings.error ? <Banner tone="error" className="mb-3">{bindings.error}</Banner> : null}
        <TableShell
          head={[
            t("admin.guardrails.colScope"),
            t("admin.guardrails.colTarget"),
            t("admin.guardrails.colPolicies"),
            "",
          ]}
          density="compact"
          sticky={false}
          loading={bindings.loading && !bindings.data ? 3 : undefined}
        >
          {bindingRows.length === 0 && !bindings.loading ? (
            <EmptyRows colSpan={4}>{t("admin.guardrails.bindingsEmpty")}</EmptyRows>
          ) : (
            bindingRows.map((binding) => {
              const key = `${binding.scope}/${binding.target}`;
              return (
                <tr key={key}>
                  <Td>{t(`admin.guardrails.scope.${binding.scope}`)}</Td>
                  <Td mono truncate>{binding.target}</Td>
                  <Td>
                    {binding.policies.length ? (
                      <span className="flex flex-wrap gap-1">
                        {binding.policies.map((policy) => (
                          <span key={policy} className="rounded-md bg-canvas px-1.5 py-0.5 font-mono text-[11px] text-ink">
                            {policy}
                          </span>
                        ))}
                      </span>
                    ) : (
                      <span className="text-muted">—</span>
                    )}
                  </Td>
                  <Td align="right">
                    <div className="flex justify-end gap-1.5">
                      <Button type="button" tone="ghost" onClick={() => beginBinding(binding)}>
                        {t("admin.guardrails.edit")}
                      </Button>
                      <AlertDialog>
                        <AlertDialogTrigger asChild>
                          <Button type="button" tone="danger" aria-label={t("admin.guardrails.deleteBinding")}>
                            <Trash2 className="size-3.5" />
                          </Button>
                        </AlertDialogTrigger>
                        <AlertDialogContent>
                          <AlertDialogHeader>
                            <AlertDialogTitle>
                              {t("admin.guardrails.deleteBindingTitle", { target: binding.target })}
                            </AlertDialogTitle>
                            <AlertDialogDescription>{t("admin.guardrails.deleteBindingNote")}</AlertDialogDescription>
                          </AlertDialogHeader>
                          <AlertDialogFooter>
                            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
                            <AlertDialogAction asChild>
                              <Button
                                type="button"
                                tone="danger"
                                isLoading={deletingBinding === key}
                                loadingText={t("common.deleting")}
                                onClick={() => void deleteBinding(binding)}
                              >
                                {t("common.delete")}
                              </Button>
                            </AlertDialogAction>
                          </AlertDialogFooter>
                        </AlertDialogContent>
                      </AlertDialog>
                    </div>
                  </Td>
                </tr>
              );
            })
          )}
        </TableShell>
      </Card>

      <Card title={t("admin.guardrails.newBinding")}>
        <form className="flex flex-col gap-4" onSubmit={saveBinding}>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            <Field label={t("admin.guardrails.scopeLabel")}>
              <Select
                value={scope}
                onChange={(e) => {
                  setScope(e.target.value as Scope);
                  setTarget("");
                }}
              >
                <option value="channel">{t("admin.guardrails.scope.channel")}</option>
                <option value="key">{t("admin.guardrails.scope.key")}</option>
              </Select>
            </Field>
            {scope === "channel" ? (
              <Field
                label={t("admin.guardrails.targetChannel")}
                hint={channelNames.length ? undefined : t("admin.guardrails.channelEmpty")}
              >
                <Select value={target} onChange={(e) => setTarget(e.target.value)} required>
                  <option value="">{t("admin.guardrails.chooseChannel")}</option>
                  {targetOptions.map((channel) => (
                    <option key={channel} value={channel}>
                      {channel}
                    </option>
                  ))}
                </Select>
              </Field>
            ) : (
              <Field label={t("admin.guardrails.targetKey")} hint={t("admin.guardrails.targetKeyHint")}>
                <TextInput
                  value={target}
                  onChange={(e) => setTarget(e.target.value)}
                  placeholder="00000000-0000-0000-0000-000000000000"
                  required
                />
              </Field>
            )}
          </div>

          <div className="flex flex-col gap-2">
            <p className="text-xs font-medium text-muted">{t("admin.guardrails.policiesLabel")}</p>
            {policyNames.length ? (
              <>
                <div className="flex flex-wrap gap-x-5 gap-y-2">
                  {policyRows.map((policy) => (
                    <label key={policy.name} className="flex items-center gap-2 text-sm text-ink">
                      <Checkbox
                        checked={selectedPolicies.includes(policy.name)}
                        onCheckedChange={() => togglePolicy(policy.name)}
                      />
                      <span className="font-mono text-xs">{policy.name}</span>
                      <span className="text-xs text-muted">{t(`admin.guardrails.kind.${policy.kind}`)}</span>
                    </label>
                  ))}
                </div>
                {selectedPolicies.length ? (
                  <div className="flex flex-col gap-1.5 rounded-lg border border-line bg-canvas-inset p-2">
                    <p className="text-xs text-muted">{t("admin.guardrails.selectedOrder")}</p>
                    {selectedPolicies.map((policy, index) => (
                      <div key={policy} className="flex items-center gap-2">
                        <span className="min-w-6 text-right font-mono text-[11px] text-muted">{index + 1}.</span>
                        <span className="flex-1 truncate font-mono text-xs text-ink">{policy}</span>
                        <IconButton
                          type="button"
                          label={t("admin.guardrails.moveUp")}
                          disabled={index === 0}
                          onClick={() => movePolicy(index, -1)}
                        >
                          <ArrowUp className="size-3.5" />
                        </IconButton>
                        <IconButton
                          type="button"
                          label={t("admin.guardrails.moveDown")}
                          disabled={index === selectedPolicies.length - 1}
                          onClick={() => movePolicy(index, 1)}
                        >
                          <ArrowDown className="size-3.5" />
                        </IconButton>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="text-xs text-muted">{t("admin.guardrails.noPoliciesSelected")}</p>
                )}
              </>
            ) : (
              <p className="text-xs text-muted">{t("admin.guardrails.noPoliciesYet")}</p>
            )}
          </div>

          {bindingNotice ? (
            <Banner tone={bindingNotice.tone === "error" ? "error" : "success"}>{bindingNotice.text}</Banner>
          ) : null}

          <div className="flex items-center gap-2">
            <Button type="submit" isLoading={savingBinding} loadingText={t("common.saving")}>
              {t("admin.guardrails.replaceBinding")}
            </Button>
            <Button
              type="button"
              tone="ghost"
              onClick={() => {
                setTarget("");
                setSelectedPolicies([]);
                setBindingNotice(null);
              }}
            >
              {t("admin.guardrails.reset")}
            </Button>
          </div>
        </form>
      </Card>
    </div>
  );
}
