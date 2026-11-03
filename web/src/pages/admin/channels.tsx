import { type FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { ArrowDown, ArrowUp, Plus, RotateCcw, X } from "lucide-react";
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

type ModelFallbackChain = components["schemas"]["ModelFallbackChain"];
type ModelAlias = components["schemas"]["ModelAlias"];
type PriceRequest = components["schemas"]["PriceRequest"];
type RequestGroup = components["schemas"]["RequestGroup"];
type AdminOrganization = components["schemas"]["AdminOrganization"];

interface ChannelCredential {
  credentialId: string;
  apiKeyLast4: string;
  position: number;
  status: "active" | "disabled";
  cooldownUntil: string | null;
  lastError: string | null;
  lastUsedAt: string | null;
  createdAt: string;
}

interface Channel {
  id: string;
  name: string;
  baseUrl: string;
  protocol: string;
  credentials: ChannelCredential[];
  createdAt: string;
  models: { model: string; version: number; mode?: string }[];
}

export function Channels({ token }: { token: string }) {
  const { t } = useTranslation();
  const channels = useAdmin<Channel[]>(token, "/channels");
  const organizations = useAdmin<{ organizations: AdminOrganization[]; nextCursor?: string }>(token, "/organizations?limit=100");
  const [channelAliases, setChannelAliases] = useState<string[]>([]);
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [protocol, setProtocol] = useState("openai");
  const [priceChannel, setPriceChannel] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const orgRows = organizations.data?.organizations ?? [];
  const [groupOrgId, setGroupOrgId] = useState("");
  const [groupName, setGroupName] = useState("");
  const [selectedGroup, setSelectedGroup] = useState("");
  const [selectedChannels, setSelectedChannels] = useState<string[]>([]);
  const [groupMessage, setGroupMessage] = useState<{ tone: "success" | "error"; text: string } | null>(null);
  const [groupBusy, setGroupBusy] = useState(false);
  const groupsPath = `/groups?organizationId=${encodeURIComponent(groupOrgId)}`;
  const groups = useAdmin<RequestGroup[]>(token, groupsPath, [groupOrgId], Boolean(groupOrgId));
  const currentGroup = useMemo(() => groups.data?.find((g) => g.name === selectedGroup), [groups.data, selectedGroup]);
  useEffect(() => {
    if (!channels.data) return;
    let current = true;
    Promise.all(
      channels.data.map((channel) =>
        adminApi<ModelAlias[]>(token, `/channels/${encodeURIComponent(channel.name)}/aliases`)
          .then((aliases) => aliases.map(({ alias }) => alias))
          .catch(() => [] as string[]),
      ),
    ).then((aliases) => {
      if (current) setChannelAliases(aliases.flat());
    });
    return () => {
      current = false;
    };
  }, [channels.data, token]);
  useEffect(() => {
    if (!channels.data) return;
    const names = new Set(channels.data.flatMap((channel) => channel.models.map(({ model }) => model)));
    setChannelAliases((current) => current.filter((alias) => !names.has(alias)));
  }, [channels.data]);
  const modelNames = useMemo(
    () => [...new Set([
      ...(channels.data ?? []).flatMap((channel) => channel.models.map(({ model }) => model)),
      ...channelAliases,
    ])].sort(),
    [channelAliases, channels.data],
  );

  function chooseGroup(name: string) {
    setSelectedGroup(name);
    const next = groups.data?.find((g) => g.name === name);
    setGroupName(name);
    setSelectedChannels(next?.channelIds ?? []);
    setGroupMessage(null);
  }

  async function saveGroup(e: React.FormEvent) {
    e.preventDefault();
    if (!groupOrgId || !groupName.trim()) return;
    setGroupBusy(true);
    setGroupMessage(null);
    try {
      const saved = await adminApi<RequestGroup>(token, `/organizations/${groupOrgId}/groups/${encodeURIComponent(groupName.trim())}`, {
        method: "PUT",
        body: { channelIds: selectedChannels },
      });
      setSelectedGroup(saved.name);
      setGroupName(saved.name);
      setSelectedChannels(saved.channelIds);
      setGroupMessage({ tone: "success", text: t("admin.channels.groupSaved") });
      groups.refresh();
    } catch (err) {
      setGroupMessage({ tone: "error", text: err instanceof ApiError ? err.message : "failed" });
    } finally {
      setGroupBusy(false);
    }
  }

  async function deleteGroup() {
    if (!groupOrgId || !selectedGroup) return;
    setGroupBusy(true);
    setGroupMessage(null);
    try {
      await adminApi(token, `/organizations/${groupOrgId}/groups/${encodeURIComponent(selectedGroup)}`, { method: "DELETE" });
      setSelectedGroup("");
      setGroupName("");
      setSelectedChannels([]);
      setGroupMessage({ tone: "success", text: t("admin.channels.groupDeleted") });
      groups.refresh();
    } catch (err) {
      setGroupMessage({ tone: "error", text: err instanceof ApiError ? err.message : "failed" });
    } finally {
      setGroupBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-6">
      <Card title={t("admin.channels.upsertTitle")}>
        <form
          className="grid grid-cols-1 gap-3 sm:grid-cols-2"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setFormError(null);
            try {
              await adminApi(token, "/channels", {
                method: "POST",
                body: { name, baseUrl, apiKey, protocol },
              });
              setName("");
              setBaseUrl("");
              setApiKey("");
              channels.refresh();
            } catch (err) {
              setFormError(err instanceof ApiError ? err.message : "failed");
            } finally {
              setBusy(false);
            }
          }}
        >
          <Field label={t("admin.channels.name")}>
            <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label={t("admin.channels.baseUrl")}>
            <TextInput required type="url" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
          </Field>
          <Field label={t("admin.channels.apiKey")}>
            <TextInput required type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} />
          </Field>
          <Field label={t("admin.channels.protocol")}>
            <Select value={protocol} onChange={(e) => setProtocol(e.target.value)}>
              <option value="openai">openai</option>
              <option value="anthropic">anthropic</option>
              <option value="gemini">gemini</option>
            </Select>
          </Field>
          <div className="flex items-end gap-2">
            <Button type="submit" disabled={busy}>
              {t("admin.channels.saveChannel")}
            </Button>
            {formError ? <span className="text-xs text-danger">{formError}</span> : null}
          </div>
        </form>
      </Card>
      {channels.error ? (
        <Banner tone="error">{channels.error}</Banner>
      ) : (
        <TableShell
          head={[t("admin.channels.colName"), { label: t("admin.channels.colBaseUrl"), truncate: true }, t("admin.channels.colProtocol"), t("admin.channels.colKey"), { label: t("admin.channels.colModels"), align: "right" }, ""]}
          density="compact"
          loading={channels.loading ? 5 : 0}
        >
          {!channels.loading &&
            (channels.data ?? []).map((c) => (
            <tr key={c.id}>
              <td className="mono">{c.name}</td>
              <Td mono truncate>{c.baseUrl}</Td>
              <td>{c.protocol ?? "openai"}</td>
              <td className="mono">
                {c.credentials.length === 0 ? (
                  "—"
                ) : (
                  <>
                    …{c.credentials[0].apiKeyLast4}
                    {c.credentials.length > 1 ? (
                      <span className="text-muted"> +{c.credentials.length - 1}</span>
                    ) : null}
                  </>
                )}
              </td>
              <Td align="right">{c.models.length}</Td>
              <td>
                <Button tone="ghost" onClick={() => setPriceChannel(priceChannel === c.name ? null : c.name)}>
                  {t("admin.channels.prices")}
                </Button>
              </td>
            </tr>
          ))}
          {!channels.loading && (channels.data?.length ?? 0) === 0 ? (
            <EmptyRows colSpan={6}>{t("admin.channels.empty")}</EmptyRows>
          ) : null}
        </TableShell>
      )}
      <p className="text-xs text-muted">
        <Trans
          i18nKey="admin.channels.driftNote"
          components={{ code: <code className="mono" /> }}
        />
      </p>
      <ModelFallbacks token={token} models={modelNames} />
      <Card title={t("admin.channels.requestGroups")}>
        <div className="flex flex-col gap-4">
          <p className="text-xs text-muted">{t("admin.channels.groupsHint")}</p>
          {organizations.error ? <Banner tone="error">{organizations.error}</Banner> : null}
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <Field label={t("admin.channels.groupOrganization")}>
              <Select value={groupOrgId} onChange={(e) => {
                setGroupOrgId(e.target.value);
                setSelectedGroup("");
                setGroupName("");
                setSelectedChannels([]);
                setGroupMessage(null);
              }}>
                <option value="">{t("admin.channels.selectOrganization")}</option>
                {orgRows.map((org) => <option key={org.id} value={org.id}>{org.name}</option>)}
              </Select>
            </Field>
            <Field label={t("admin.channels.existingGroup")}>
              <Select value={selectedGroup} disabled={!groupOrgId || groups.loading} onChange={(e) => chooseGroup(e.target.value)}>
                <option value="">{t("admin.channels.newGroup")}</option>
                {(groups.data ?? []).map((group) => <option key={group.id} value={group.name}>{group.name}</option>)}
              </Select>
            </Field>
          </div>
          {groups.error ? <Banner tone="error">{groups.error}</Banner> : null}
          {groupOrgId ? (
            <form className="flex flex-col gap-4" onSubmit={saveGroup}>
              <Field label={t("admin.channels.groupName")} hint={t("admin.channels.groupNameHint")}>
              <TextInput value={groupName} onChange={(e) => setGroupName(e.target.value)} required maxLength={64} disabled={Boolean(currentGroup)} />
              </Field>
              <fieldset className="flex flex-col gap-2">
                <legend className="text-xs font-medium text-muted">{t("admin.channels.groupChannels")}</legend>
                {(channels.data ?? []).map((channel) => {
                  const checked = selectedChannels.includes(channel.id);
                  return (
                    <label key={channel.id} className="flex min-h-11 items-center gap-3 border-b border-line py-2 text-sm">
                      <Checkbox checked={checked} onCheckedChange={(value) => setSelectedChannels((ids) => value ? [...ids, channel.id] : ids.filter((id) => id !== channel.id))} />
                      <span>{channel.name}</span>
                      <span className="ml-auto text-xs text-muted">{channel.protocol} · {channel.models.length} {t("admin.channels.modelsShort")}</span>
                    </label>
                  );
                })}
                {!channels.loading && (channels.data?.length ?? 0) === 0 ? <p className="text-xs text-muted">{t("admin.channels.noChannels")}</p> : null}
              </fieldset>
              {selectedChannels.length === 0 ? <Banner tone="error">{t("admin.channels.emptyGroupWarning")}</Banner> : null}
              <div className="flex flex-wrap items-center gap-2">
                <Button type="submit" disabled={groupBusy || groups.loading || !groupName.trim()} isLoading={groupBusy} loadingText={t("common.saving")}>
                  {t("admin.channels.saveGroup")}
                </Button>
                {currentGroup ? (
                  <AlertDialog>
                    <AlertDialogTrigger asChild>
                      <Button type="button" tone="danger" disabled={groupBusy}>{t("admin.channels.deleteGroup")}</Button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                      <AlertDialogHeader>
                        <AlertDialogTitle>{t("admin.channels.deleteGroupTitle", { name: currentGroup.name })}</AlertDialogTitle>
                        <AlertDialogDescription>{t("admin.channels.deleteGroupNote")}</AlertDialogDescription>
                      </AlertDialogHeader>
                      <AlertDialogFooter>
                        <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
                        <AlertDialogAction asChild>
                          <Button type="button" tone="danger" isLoading={groupBusy} loadingText={t("common.deleting")} onClick={deleteGroup}>{t("admin.channels.deleteGroup")}</Button>
                        </AlertDialogAction>
                      </AlertDialogFooter>
                    </AlertDialogContent>
                  </AlertDialog>
                ) : null}
                {groupMessage ? <span className={`text-xs ${groupMessage.tone === "error" ? "text-danger" : "text-success"}`}>{groupMessage.text}</span> : null}
              </div>
            </form>
          ) : null}
        </div>
      </Card>
      {priceChannel ? <ChannelPrices token={token} channel={priceChannel} onSaved={channels.refresh} /> : null}
    </div>
  );
}

function ModelFallbacks({ token, models }: { token: string; models: string[] }) {
  const { t } = useTranslation();
  const [model, setModel] = useState("");
  const [loadedModel, setLoadedModel] = useState<string | null>(null);
  const [fallbackInput, setFallbackInput] = useState("");
  const [fallbacks, setFallbacks] = useState<string[]>([]);
  const [savedFallbacks, setSavedFallbacks] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const loadGeneration = useRef(0);
  const normalizedModel = model.trim();
  const isLoaded = loadedModel === normalizedModel;
  const hasChanges = JSON.stringify(fallbacks) !== JSON.stringify(savedFallbacks);
  const canSwitchModel = loadedModel === null || (isLoaded && !hasChanges && !saving);

  useEffect(() => {
    if (!model && models.length > 0) setModel(models[0]);
  }, [model, models]);

  async function loadChain() {
    if (!normalizedModel) {
      setError(t("admin.channels.fallbackModelRequired"));
      return;
    }
    if (!models.includes(normalizedModel)) {
      setError(t("admin.channels.fallbackModelUnknown", { model: normalizedModel }));
      return;
    }
    const generation = ++loadGeneration.current;
    setLoading(true);
    setError(null);
    setMessage(null);
    try {
      const chain = await adminApi<ModelFallbackChain>(
        token,
        `/model-fallbacks/${encodeURIComponent(normalizedModel)}`,
      );
      if (generation !== loadGeneration.current) return;
      setLoadedModel(normalizedModel);
      setFallbackInput("");
      setFallbacks(chain.fallbacks);
      setSavedFallbacks(chain.fallbacks);
    } catch (err) {
      if (generation !== loadGeneration.current) return;
      setError(err instanceof ApiError ? err.message : "failed");
    } finally {
      if (generation === loadGeneration.current) setLoading(false);
    }
  }

  function addFallback(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const candidate = fallbackInput.trim();
    setError(null);
    setMessage(null);
    if (!candidate) return;
    if (candidate === normalizedModel) {
      setError(t("admin.channels.fallbackSelf"));
      return;
    }
    if (fallbacks.includes(candidate)) {
      setError(t("admin.channels.fallbackDuplicate"));
      return;
    }
    setFallbacks((current) => [...current, candidate]);
    setFallbackInput("");
  }

  async function saveChain(nextFallbacks: string[]) {
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      const chain = await adminApi<ModelFallbackChain>(
        token,
        `/model-fallbacks/${encodeURIComponent(normalizedModel)}`,
        { method: "PUT", body: { fallbacks: nextFallbacks } satisfies ModelFallbackChain },
      );
      setFallbacks(chain.fallbacks);
      setSavedFallbacks(chain.fallbacks);
      setMessage(t("admin.channels.fallbackSaved"));
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "failed");
    } finally {
      setSaving(false);
    }
  }

  return (
    <Card title={t("admin.channels.fallbackTitle")}>
      <div className="flex flex-col gap-4">
        <p className="text-sm text-muted">{t("admin.channels.fallbackNote")}</p>
        <div className="grid grid-cols-1 items-end gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
          <Field label={t("admin.channels.fallbackModel")}>
            <TextInput
              list="channel-model-fallback-options"
              value={model}
              disabled={loading || loadedModel !== null || saving}
              onChange={(event) => setModel(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void loadChain();
                }
              }}
              placeholder={t("admin.channels.fallbackModelPlaceholder")}
            />
            <datalist id="channel-model-fallback-options">
              {models.map((name) => <option key={name} value={name} />)}
            </datalist>
          </Field>
          <Button type="button" tone="ghost" disabled={loading || saving || hasChanges || !normalizedModel} onClick={() => void loadChain()}>
            {loading ? t("admin.channels.fallbackLoading") : t("admin.channels.loadFallbacks")}
          </Button>
        </div>

        {isLoaded ? (
          <>
            <div className="flex items-center justify-between gap-3">
              <p className="text-xs text-muted">{t("admin.channels.editingModel", { model: loadedModel })}</p>
              <Button
                type="button"
                tone="ghost"
                disabled={!canSwitchModel}
                onClick={() => {
                  setLoadedModel(null);
                  setFallbackInput("");
                  setFallbacks([]);
                  setSavedFallbacks([]);
                  setError(null);
                  setMessage(null);
                }}
              >
                {t("admin.channels.changeFallbackModel")}
              </Button>
            </div>
            <div className="flex flex-col gap-2">
              <p className="text-xs font-medium text-muted">{t("admin.channels.fallbackOrder")}</p>
              {fallbacks.length === 0 ? (
                <p className="rounded-lg border border-dashed border-line px-3 py-4 text-sm text-muted">
                  {t("admin.channels.fallbackEmpty")}
                </p>
              ) : (
                <ol className="divide-y divide-line rounded-lg border border-line">
                  {fallbacks.map((fallback, index) => (
                    <li key={`${fallback}-${index}`} className="flex min-h-12 items-center gap-2 px-3 py-2">
                      <span className="w-6 shrink-0 text-right font-mono text-xs text-muted">{index + 1}</span>
                      <span className="min-w-0 flex-1 break-all font-mono text-sm text-ink">{fallback}</span>
                      <IconButton
                        type="button"
                        label={t("admin.channels.moveFallbackUp", { model: fallback })}
                        disabled={saving || index === 0}
                        onClick={() => setFallbacks((current) => {
                          const next = [...current];
                          [next[index - 1], next[index]] = [next[index], next[index - 1]];
                          return next;
                        })}
                      >
                        <ArrowUp className="size-4" />
                      </IconButton>
                      <IconButton
                        type="button"
                        label={t("admin.channels.moveFallbackDown", { model: fallback })}
                        disabled={saving || index === fallbacks.length - 1}
                        onClick={() => setFallbacks((current) => {
                          const next = [...current];
                          [next[index], next[index + 1]] = [next[index + 1], next[index]];
                          return next;
                        })}
                      >
                        <ArrowDown className="size-4" />
                      </IconButton>
                      <IconButton
                        type="button"
                        label={t("admin.channels.removeFallback", { model: fallback })}
                        disabled={saving}
                        onClick={() => setFallbacks((current) => current.filter((_, itemIndex) => itemIndex !== index))}
                      >
                        <X className="size-4" />
                      </IconButton>
                    </li>
                  ))}
                </ol>
              )}
            </div>

            <form className="grid grid-cols-1 items-end gap-3 sm:grid-cols-[minmax(0,1fr)_auto]" onSubmit={addFallback}>
              <Field label={t("admin.channels.fallbackAddLabel")}>
                <TextInput
                  list="channel-model-fallback-options"
                  value={fallbackInput}
                  disabled={saving}
                  onChange={(event) => setFallbackInput(event.target.value)}
                  placeholder={t("admin.channels.fallbackModelPlaceholder")}
                />
              </Field>
              <Button type="submit" tone="ghost" disabled={saving || !fallbackInput.trim()}>
                <Plus className="size-4" />
                {t("admin.channels.addFallback")}
              </Button>
            </form>

            <div className="flex flex-wrap items-center gap-3">
              <Button type="button" disabled={!hasChanges || saving} onClick={() => void saveChain(fallbacks)}>
                {saving ? t("admin.channels.fallbackSaving") : t("admin.channels.saveFallbacks")}
              </Button>
              <Button
                type="button"
                tone="ghost"
                disabled={!hasChanges || saving}
                onClick={() => {
                  setFallbacks(savedFallbacks);
                  setError(null);
                  setMessage(null);
                }}
              >
                <RotateCcw className="size-4" />
                {t("admin.channels.discardFallbackChanges")}
              </Button>
              {hasChanges ? <span className="text-xs text-muted">{t("admin.channels.fallbackUnsaved")}</span> : null}
              {message ? <span role="status" className="text-xs text-success">{message}</span> : null}
            </div>
          </>
        ) : (
          <p className="text-xs text-muted">
            {models.length === 0 ? t("admin.channels.fallbackNoModels") : t("admin.channels.fallbackLoadHint")}
          </p>
        )}

        {error ? <Banner tone="error">{error}</Banner> : null}
      </div>
    </Card>
  );
}

function ChannelPrices({ token, channel, onSaved }: { token: string; channel: string; onSaved: () => void }) {
  const { t } = useTranslation();
  // The channel's models with their current price versions are already on the
  // channel row; the price form appends a new version to one model.
  const [model, setModel] = useState("");
  const [mode, setMode] = useState<NonNullable<PriceRequest["mode"]>>("chat");
  const [inputPrice, setInputPrice] = useState("");
  const [outputPrice, setOutputPrice] = useState("");
  const [maxOutputTokens, setMaxOutputTokens] = useState("");
  const [costPerRequest, setCostPerRequest] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const hasOutput = mode === "chat" || mode === "event";

  return (
    <Card title={t("admin.channels.appendTitle", { channel })}>
      <form
        className="grid grid-cols-1 gap-3 sm:grid-cols-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setMsg(null);
          setError(null);
          try {
            const body: PriceRequest = {
              model,
              mode,
              weight: 100,
              inputPricePerMillion: Math.round(Number(inputPrice) * 1_000_000),
              ...(hasOutput && outputPrice !== "" ? {
                outputPricePerMillion: Math.round(Number(outputPrice) * 1_000_000),
              } : {}),
              ...(mode === "chat" ? { maxOutputTokens: Number(maxOutputTokens) } : {}),
              ...(costPerRequest !== "" ? {
                costPerRequest: Math.round(Number(costPerRequest) * 1_000_000),
              } : {}),
            };
            await adminApi(token, `/channels/${channel}/prices`, {
              method: "POST",
              body,
            });
            setMsg(t("admin.channels.priceStored"));
            onSaved();
          } catch (err) {
            setError(err instanceof ApiError ? err.message : "failed");
          } finally {
            setBusy(false);
          }
        }}
      >
        <Field label={t("admin.channels.model")}>
          <TextInput required value={model} onChange={(e) => setModel(e.target.value)} />
        </Field>
        <Field label={t("admin.channels.mode")}>
          <Select value={mode} onChange={(e) => setMode(e.target.value as NonNullable<PriceRequest["mode"]>)}>
            <option value="chat">chat</option>
            <option value="embeddings">embeddings</option>
            <option value="rerank">rerank</option>
            <option value="event">event</option>
          </Select>
        </Field>
        <Field label={t("admin.channels.inputPrice")}>
          <NumberInput required min={0} value={inputPrice} onChange={(e) => setInputPrice(e.target.value)} />
        </Field>
        {hasOutput ? (
          <Field label={t("admin.channels.outputPrice")}>
            <NumberInput min={0} value={outputPrice} onChange={(e) => setOutputPrice(e.target.value)} />
          </Field>
        ) : null}
        {mode === "chat" ? (
          <Field label={t("admin.channels.maxOutputTokens")}>
            <NumberInput required min={1} value={maxOutputTokens} onChange={(e) => setMaxOutputTokens(e.target.value)} />
          </Field>
        ) : null}
        <Field label={t("admin.channels.costPerRequest")}>
          <NumberInput min={0} value={costPerRequest} onChange={(e) => setCostPerRequest(e.target.value)} />
        </Field>
        <div className="flex items-end gap-2">
          <Button type="submit" disabled={busy}>{t("admin.channels.setPrice")}</Button>
          {msg ? <span className="text-xs text-success">{msg}</span> : null}
          {error ? <span className="text-xs text-danger">{error}</span> : null}
        </div>
      </form>
    </Card>
  );
}
