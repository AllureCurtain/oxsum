import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  CHUNK_PROBE_TIMEOUT_MS,
  CHUNK_RELOAD_DEADLINE_MS,
  CHUNK_RELOAD_KEY,
  CHUNK_RELOAD_WINDOW_MS,
  ChunkLoadError,
  claimChunkReload,
  createChunkRecovery,
  withChunkRecovery,
  type RecoveryWindow,
  type ReloadStore,
} from "../src/lib/chunk-reload.ts";

const t0 = 1_800_000_000_000;

function memoryStore(initial: string | null = null): ReloadStore & { value: string | null } {
  return {
    value: initial,
    getItem(key) {
      return key === CHUNK_RELOAD_KEY ? this.value : null;
    },
    setItem(key, value) {
      if (key === CHUNK_RELOAD_KEY) this.value = value;
    },
  };
}

test("a chunk failure reloads the tab once per window and records when", () => {
  const store = memoryStore();
  assert.equal(claimChunkReload(() => store, t0), true);
  assert.equal(store.value, String(t0));
  assert.equal(claimChunkReload(() => store, t0 + 1), false, "the failure right after the reload shows the prompt");
  assert.equal(claimChunkReload(() => store, t0 + CHUNK_RELOAD_WINDOW_MS - 1), false);
  assert.equal(store.value, String(t0), "a refused claim leaves the record alone");
  assert.equal(claimChunkReload(() => store, t0 + CHUNK_RELOAD_WINDOW_MS), true, "the guard re-arms after the window");
  assert.equal(store.value, String(t0 + CHUNK_RELOAD_WINDOW_MS));
});

test("without a usable record, the tab never reloads itself", () => {
  const blocked = () => {
    throw new Error("SecurityError: storage is blocked");
  };
  assert.equal(claimChunkReload(blocked, t0), false);
  const full: ReloadStore = {
    getItem: () => null,
    setItem: () => {
      throw new Error("QuotaExceededError");
    },
  };
  assert.equal(claimChunkReload(() => full, t0), false, "a reload that cannot be recorded could repeat forever");
});

test("an empty, malformed or future record does not hold back the first reload", () => {
  for (const record of ["", "soon", "0", String(t0 + 10_000)]) {
    const store = memoryStore(record);
    assert.equal(claimChunkReload(() => store, t0), true, JSON.stringify(record));
    assert.equal(store.value, String(t0));
  }
});

const within = (promise: Promise<unknown>, ms: number) =>
  Promise.race([
    promise.then(
      () => "resolved",
      () => "rejected",
    ),
    new Promise((resolve) => setTimeout(() => resolve("pending"), ms)),
  ]);

type Probe = { url: string; init: RequestInit };
type Answer = (init: RequestInit) => Promise<{ ok: boolean }>;

/** A check the server never answers. Like `fetch`, it rejects once its signal aborts. */
const unanswered: Answer = (init) =>
  new Promise((_, reject) => {
    init.signal?.addEventListener("abort", () => reject(init.signal?.reason));
  });

/**
 * A window whose server answers `probe`, with a clock the test moves and
 * timers the test fires.
 */
function fakeWindow(options: { store?: ReloadStore; probe?: Answer; href?: string }) {
  const timers: { id: number; ms: number; handler: () => void }[] = [];
  let lastTimer = 0;
  const probes: Probe[] = [];
  const state = { clock: t0, reloads: [] as string[] };
  const location = {
    href: options.href ?? "https://billing.example/app/keys",
    reload() {
      state.reloads.push(location.href);
    },
  };
  const win: RecoveryWindow = {
    sessionStorage: options.store ?? memoryStore(),
    location,
    async fetch(url, init) {
      probes.push({ url, init });
      return (options.probe ?? (async () => ({ ok: true })))(init);
    },
    setTimeout(handler, ms) {
      timers.push({ id: ++lastTimer, ms, handler });
      return lastTimer;
    },
    clearTimeout(id) {
      const index = timers.findIndex((timer) => timer.id === id);
      if (index >= 0) timers.splice(index, 1);
    },
  };
  return {
    win,
    state,
    probes,
    location,
    timers,
    recovery: createChunkRecovery(win, () => state.clock),
    fire() {
      for (const { handler } of timers.splice(0)) handler();
    },
  };
}

const missing = new TypeError("Failed to fetch dynamically imported module: /app/assets/Keys-old.js");

test("a page that loads passes through without consulting the recovery", async () => {
  let recoveries = 0;
  const load = withChunkRecovery(async () => ({ default: "Keys" }), {
    recover: async () => void recoveries++,
  });
  assert.deepEqual(await load(), { default: "Keys" });
  assert.equal(recoveries, 0);
});

test("a recovery that will not reload rejects for the boundary with the cause", async () => {
  const refused = withChunkRecovery(() => Promise.reject(missing), { recover: async () => {} });
  await assert.rejects(refused(), (error: unknown) => error instanceof ChunkLoadError && error.cause === missing);
});

test("a missing chunk checks the server, reloads once and waits for the reload", async () => {
  const store = memoryStore();
  const tab = fakeWindow({ store });
  const page = withChunkRecovery(() => Promise.reject(missing), tab.recovery)();
  assert.equal(await within(page, 20), "pending", "the page keeps its screen until the reload replaces it");
  assert.deepEqual(tab.state.reloads, ["https://billing.example/app/keys"]);
  assert.equal(store.value, String(t0), "the reload is recorded in sessionStorage");
  assert.equal(tab.probes.length, 1);
  assert.equal(tab.probes[0].url, "https://billing.example/app/keys", "the check asks for the page the reload fetches");
  assert.equal(tab.probes[0].init.method, "HEAD");
  assert.equal(tab.probes[0].init.cache, "no-store");
  assert.deepEqual(
    tab.timers.map(({ ms }) => ms),
    [CHUNK_RELOAD_DEADLINE_MS],
    "the check's timeout is cleared once the server answers",
  );

  tab.fire();
  assert.equal(await within(page, 20), "rejected", "a reload that never lands shows the prompt after the deadline");
  await assert.rejects(page, ChunkLoadError);
});

test("a check the server never answers gives up after its timeout", async () => {
  const store = memoryStore();
  const tab = fakeWindow({ store, probe: unanswered });
  const page = withChunkRecovery(() => Promise.reject(missing), tab.recovery)();
  assert.equal(await within(page, 20), "pending", "the check is still waiting for the server");
  assert.deepEqual(tab.timers.map(({ ms }) => ms), [CHUNK_PROBE_TIMEOUT_MS]);

  tab.fire();
  assert.equal(await within(page, 20), "rejected", "the prompt shows once the check gives up");
  await assert.rejects(page, ChunkLoadError);
  assert.deepEqual(tab.state.reloads, []);
  assert.equal(store.value, null, "nothing is recorded, so the guard stays armed");
});

test("a server that does not answer gets the prompt and no reload", async () => {
  const cases: [string, Answer][] = [
    ["refused", async () => Promise.reject(new TypeError("Failed to fetch"))],
    ["502", async () => ({ ok: false })],
  ];
  for (const [name, probe] of cases) {
    const store = memoryStore();
    const tab = fakeWindow({ store, probe });
    assert.equal(await within(tab.recovery.recover(), 20), "resolved", name);
    assert.deepEqual(tab.state.reloads, [], name);
    assert.equal(store.value, null, `${name}: nothing is recorded, so the guard stays armed`);
  }
});

test("the guard and unusable storage both stop the reload", async () => {
  const recent = fakeWindow({ store: memoryStore(String(t0 - 1_000)) });
  assert.equal(await within(recent.recovery.recover(), 20), "resolved");
  assert.deepEqual(recent.state.reloads, []);
  assert.equal(recent.probes.length, 0, "a reload the guard refuses does not wait for the check");

  const blocked = fakeWindow({});
  Object.defineProperty(blocked.win, "sessionStorage", {
    get() {
      throw new Error("SecurityError: storage is blocked");
    },
  });
  assert.equal(await within(blocked.recovery.recover(), 20), "resolved");
  assert.deepEqual(blocked.state.reloads, []);
  assert.equal(blocked.probes.length, 0, "storage that cannot be read does not wait for the check either");

  const full = fakeWindow({
    store: {
      getItem: () => null,
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
    },
  });
  assert.equal(await within(full.recovery.recover(), 20), "resolved");
  assert.deepEqual(full.state.reloads, [], "storage that cannot take the record refuses at the claim");
});

test("a failure while the reload is on its way joins it and reloads the newest page", async () => {
  const store = memoryStore();
  const tab = fakeWindow({ store });
  const first = tab.recovery.recover();
  assert.equal(await within(first, 20), "pending");

  tab.location.href = "https://billing.example/app/usage";
  tab.state.clock += 400;
  const second = tab.recovery.recover();
  assert.equal(await within(second, 20), "pending", "no prompt while the reload can still land");
  assert.deepEqual(tab.state.reloads, ["https://billing.example/app/keys", "https://billing.example/app/usage"]);
  assert.equal(tab.probes.length, 1, "the joining failure does not check again");
  assert.equal(store.value, String(t0), "the record keeps the first reload's time");

  tab.fire();
  assert.equal(await within(first, 20), "resolved");
  assert.equal(await within(second, 20), "resolved");

  tab.state.clock += CHUNK_RELOAD_DEADLINE_MS;
  assert.equal(await within(tab.recovery.recover(), 20), "resolved", "past the deadline the guard decides again");
  assert.equal(tab.state.reloads.length, 2, "and it refuses inside the window");
});

test("two failures checking the server at once reload once, and neither shows the prompt", async () => {
  const answers: ((value: { ok: boolean }) => void)[] = [];
  const tab = fakeWindow({ probe: () => new Promise((resolve) => answers.push(resolve)) });
  const first = tab.recovery.recover();
  const second = tab.recovery.recover();
  await within(Promise.resolve(), 20);
  assert.equal(answers.length, 2);

  answers[1]({ ok: true });
  assert.equal(await within(second, 20), "pending");
  answers[0]({ ok: true });
  assert.equal(await within(first, 20), "pending", "the refused claim joins the reload already on its way");
  assert.equal(tab.state.reloads.length, 1, "a failure on the same page does not restart the reload");
});

// The shared recovery is module state: after this test it stays bound to this
// fake window, so any test that comes after it passes its own recovery.
test("the dashboard's pages share one recovery per tab", async () => {
  const tab = fakeWindow({});
  const global = globalThis as { window?: RecoveryWindow };
  global.window = tab.win;
  try {
    const keys = withChunkRecovery(() => Promise.reject(missing))();
    assert.equal(await within(keys, 20), "pending");
    const usage = withChunkRecovery(() => Promise.reject(missing))();
    assert.equal(await within(usage, 20), "pending", "the second page joins the reload the first one started");
    assert.equal(tab.probes.length, 1);
    assert.equal(tab.state.reloads.length, 1);

    tab.fire();
    assert.equal(await within(keys, 20), "rejected");
    assert.equal(await within(usage, 20), "rejected");
  } finally {
    delete global.window;
  }
});

test("both locales carry the reload prompt", () => {
  for (const file of ["en.json", "zh-CN.json"]) {
    const { common } = JSON.parse(readFileSync(new URL(`../src/locales/${file}`, import.meta.url), "utf8"));
    for (const key of ["pageLoadFailed", "reload"]) {
      assert.ok(typeof common[key] === "string" && common[key].length > 0, `${file} common.${key}`);
    }
  }
});
