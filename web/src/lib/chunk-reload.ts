// Recovery for a lazy page whose code cannot load (issue #1092).
//
// The dashboard's pages load as hashed chunks on demand (App.tsx). A tab
// opened before a redeploy still runs the old entry, so the first page it
// opens that it never loaded can ask for a chunk name the new build no longer
// has. The server answers 404 for the missing asset (crates/server/src/spa.rs).
// A reload fetches the current index, which is served no-cache, and with it
// the current chunk names. The guard allows one automatic reload per window
// per tab, so a build whose chunks are genuinely missing cannot reload the tab
// in a loop; past that, the page shows a reload prompt instead.

/**
 * The `sessionStorage` key holding when this tab last reloaded itself for a
 * chunk, in epoch milliseconds.
 */
export const CHUNK_RELOAD_KEY = "oxsum.chunk-reload-at";

/**
 * How long after an automatic reload another chunk failure shows the prompt
 * instead of reloading again.
 */
export const CHUNK_RELOAD_WINDOW_MS = 60_000;

/**
 * How long a failed import waits for the reload to replace the page. A reload
 * that has not landed by then, such as one the user stopped, shows the prompt
 * instead of leaving the page waiting.
 */
export const CHUNK_RELOAD_DEADLINE_MS = 10_000;

/** How long the check that the server answers may take. */
export const CHUNK_PROBE_TIMEOUT_MS = 5_000;

/** The part of `Storage` the guard reads and writes. */
export interface ReloadStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * A lazy page's code failed to load. The route boundary renders the reload
 * prompt for this error only.
 */
export class ChunkLoadError extends Error {
  constructor(cause: unknown) {
    super("The page's code could not be loaded.", { cause });
    this.name = "ChunkLoadError";
  }
}

/** Whether `storage` records a reload this tab made within the window. */
function reloadedRecently(storage: ReloadStore, now: number): boolean {
  const last = Number(storage.getItem(CHUNK_RELOAD_KEY));
  const age = now - last;
  return last > 0 && age >= 0 && age < CHUNK_RELOAD_WINDOW_MS;
}

/**
 * Whether the guard could allow a reload now, without recording one. It
 * answers false when the store cannot be read. A store that can be read but
 * not written passes here, and the claim refuses it.
 */
function guardAllows(store: () => ReloadStore, now: number): boolean {
  try {
    return !reloadedRecently(store(), now);
  } catch {
    return false;
  }
}

/**
 * Decides whether a chunk failure may reload the tab, and records the reload
 * when it may. It refuses when the tab reloaded itself within the window, and
 * when the store cannot be read or written: without a record, nothing would
 * stop the next failure from reloading again.
 *
 * `store` is a thunk because reading `window.sessionStorage` itself throws
 * where the browser blocks site storage.
 */
export function claimChunkReload(store: () => ReloadStore, now: number): boolean {
  try {
    const storage = store();
    if (reloadedRecently(storage, now)) return false;
    storage.setItem(CHUNK_RELOAD_KEY, String(now));
    return true;
  } catch {
    return false;
  }
}

/** The parts of `window` the recovery uses. A test passes a fake. */
export interface RecoveryWindow {
  readonly sessionStorage: ReloadStore;
  readonly location: { readonly href: string; reload(): void };
  fetch(url: string, init: RequestInit): Promise<{ readonly ok: boolean }>;
  setTimeout(handler: () => void, ms: number): number;
  clearTimeout(id: number): void;
}

/** What a failed page import does before it reaches the route boundary. */
export interface ChunkRecovery {
  /**
   * Resolves when the failure should reach the boundary: as soon as the tab
   * knows it will not reload, or `CHUNK_RELOAD_DEADLINE_MS` after it reloads,
   * in case that reload never replaces the page. Until then the screen keeps
   * what it shows.
   */
  recover(): Promise<void>;
}

/**
 * The recovery for one document.
 *
 * When no reload is on its way, a failure the guard would refuse goes to the
 * prompt at once. Any other failure first checks that the server answers the
 * URL a reload would fetch. `navigator.onLine` only sees the device's
 * network, so without the check a server that is restarting would reload the
 * tab into the browser's error page. The check fails offline as well, and
 * gives up after `CHUNK_PROBE_TIMEOUT_MS`. Then the failure takes the guard's
 * claim.
 *
 * A failure that arrives while a reload may still land joins it: the guard
 * would refuse it, because that reload's claim just wrote the record.
 * `location.reload()` reloads the URL current when it is called, so when the
 * user has opened another page since, the tab reloads again to land there.
 * Only a failure re-aims the reload; opening a page the tab already loaded
 * does not.
 */
export function createChunkRecovery(
  win: RecoveryWindow,
  now: () => number = Date.now,
): ChunkRecovery {
  const storage = () => win.sessionStorage;
  // The reload this document started last, and the URL it started from.
  let last: { at: number; href: string } | undefined;
  const reloading = () =>
    last !== undefined && now() - last.at < CHUNK_RELOAD_DEADLINE_MS;
  const reload = () => {
    last = { at: now(), href: win.location.href };
    win.location.reload();
  };
  // The timeout runs on the window's timer rather than `AbortSignal.timeout`,
  // so a test can fire it.
  const reachable = async () => {
    const controller = new AbortController();
    const timer = win.setTimeout(() => controller.abort(), CHUNK_PROBE_TIMEOUT_MS);
    try {
      const response = await win.fetch(win.location.href, {
        method: "HEAD",
        cache: "no-store",
        signal: controller.signal,
      });
      return response.ok;
    } catch {
      return false;
    } finally {
      win.clearTimeout(timer);
    }
  };
  return {
    async recover() {
      if (!reloading()) {
        const allowed =
          guardAllows(storage, now()) &&
          (await reachable()) &&
          claimChunkReload(storage, now());
        if (allowed) {
          reload();
        } else if (!reloading()) {
          return;
        }
        // Otherwise another failure started a reload during the check.
      }
      if (win.location.href !== last?.href) reload();
      await new Promise<void>((resolve) => {
        win.setTimeout(() => resolve(), CHUNK_RELOAD_DEADLINE_MS);
      });
    },
  };
}

/** The recovery the dashboard's pages share, created on the first failure. */
let browserRecovery: ChunkRecovery | undefined;

/**
 * Wraps a lazy page's import. A failure waits for the recovery, then rejects
 * with a `ChunkLoadError` for the route boundary. While a reload the recovery
 * started is on its way, the import stays pending, so the screen keeps what
 * it shows until the reload replaces it.
 */
export function withChunkRecovery<T>(
  load: () => Promise<T>,
  recovery?: ChunkRecovery,
): () => Promise<T> {
  return () =>
    load().catch(async (error: unknown) => {
      await (recovery ?? (browserRecovery ??= createChunkRecovery(window))).recover();
      throw new ChunkLoadError(error);
    });
}
