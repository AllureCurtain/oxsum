/**
 * `<verify-bill>` — the verifiable-bill check as an embeddable element.
 *
 * Drop it on any page and it runs the same proof the `/verify` page and the
 * `verifyBill` export run: recompute the entry's content hash from its
 * canonical bytes, link it to the ledger's Merkle head, recompute the charge,
 * and check the signed price binding — all inside the element's shadow root,
 * with no server round-trip for the verdict itself.
 *
 * Two ways to feed it a bill:
 *
 *   <!-- the element fetches the bundle itself -->
 *   <verify-bill server="https://oxsum.example.com"
 *                entry="a8f3…uuid"
 *                expected-hash="e2b9…64hex"></verify-bill>
 *
 *   <!-- or paste the bundle the user already holds -->
 *   <verify-bill expected-hash="e2b9…64hex"></verify-bill>
 *   <!-- with no `bundle` it renders its own paste form -->
 *
 * `expected-hash` is the half the check cannot supply: the 64-hex content hash
 * the *customer* recorded with the bill. The element never trusts a hash the
 * server echoes — feeding it the bundle's own `contentHash` would prove the
 * bundle agrees with itself, nothing more.
 *
 * The wasm module instantiates lazily on first verify, so attaching the
 * element costs nothing until a check actually runs. On a browser without
 * WebAssembly the element renders a readable fallback that points at the
 * server-side `/verify` page rather than failing blank.
 */

import { verifyBill } from "./index.js";

const PROOF_PATH = "/api/v1/entries";

type View =
  | { kind: "form" }
  | { kind: "busy" }
  | { kind: "verdict"; verdict: string; charge?: string; price?: string; detail?: string }
  | { kind: "error"; message: string };

/** The element's attributes. Read once on connect and again on each verify. */
const ATTR = {
  server: "server",
  entry: "entry",
  bundle: "bundle",
  expectedHash: "expected-hash",
} as const;

const STYLES = /* css */ `
  :host {
    display: block;
    font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    color: #111;
    line-height: 1.5;
  }
  .card {
    border: 1px solid #d9dce1;
    border-radius: 10px;
    padding: 1.25rem 1.5rem;
    background: #fff;
    max-width: 42rem;
  }
  h1 { font-size: 1.15rem; margin: 0 0 0.75rem; font-weight: 650; }
  p { margin: 0.5rem 0; }
  .ok { color: #0a7a3d; }
  .error { color: #b3261e; }
  .muted { color: #5a6472; font-size: 0.92rem; }
  label { display: block; margin: 0.75rem 0 0.25rem; font-size: 0.88rem; font-weight: 550; }
  textarea, input {
    width: 100%; box-sizing: border-box; font: inherit; font-size: 0.85rem;
    border: 1px solid #c9ced6; border-radius: 6px; padding: 0.5rem 0.6rem;
    background: #fff; color: inherit;
  }
  textarea { min-height: 7rem; font-family: ui-monospace, monospace; resize: vertical; }
  button {
    margin-top: 0.9rem; font: inherit; font-weight: 600; font-size: 0.9rem;
    padding: 0.55rem 1.1rem; border: 0; border-radius: 6px; cursor: pointer;
    background: #1d4ed8; color: #fff;
  }
  button[disabled] { opacity: 0.6; cursor: default; }
  a { color: #1d4ed8; }
`;

export class VerifyBillElement extends HTMLElement {
  static get observedAttributes(): string[] {
    return Object.values(ATTR);
  }

  private root: ShadowRoot;

  constructor() {
    super();
    this.root = this.attachShadow({ mode: "open" });
  }

  connectedCallback(): void {
    // Rerender on connect; attribute changes after first connect rerender only
    // when the element is back in its input state (a verdict is left alone).
    if (this.expectedHash && (this.serverAndEntry || this.bundle)) {
      void this.run();
    } else {
      this.render({ kind: "form" });
    }
  }

  attributeChangedCallback(): void {
    // A changed input invalidates a prior verdict only while the element is
    // showing its form — a completed check is not silently re-run.
    if (this.isConnected && !this.root.querySelector(".verdict")) {
      if (this.expectedHash && (this.serverAndEntry || this.bundle)) {
        void this.run();
      } else {
        this.render({ kind: "form" });
      }
    }
  }

  private get serverAndEntry(): { server: string; entry: string } | null {
    const server = this.getAttribute(ATTR.server)?.replace(/\/$/, "");
    const entry = this.getAttribute(ATTR.entry);
    return server && entry ? { server, entry } : null;
  }

  private get bundle(): string | null {
    const raw = this.getAttribute(ATTR.bundle);
    return raw?.trim() ? raw.trim() : null;
  }

  private get expectedHash(): string | null {
    const raw = this.getAttribute(ATTR.expectedHash);
    return raw && /^[0-9a-f]{64}$/i.test(raw.trim()) ? raw.trim() : null;
  }

  /**
   * Resolves the bundle to verify: the `bundle` attribute verbatim, else the
   * fetched `/api/v1/entries/{entryId}/proof` `data` object. The endpoint's
   * envelope is unwrapped here so `verifyBill` always sees the inner bundle.
   */
  private async resolveBundle(): Promise<string> {
    if (this.bundle) {
      return this.bundle;
    }
    const ref = this.serverAndEntry;
    if (!ref) {
      throw new Error("no bundle and no server/entry to fetch one");
    }
    const url = `${ref.server}${PROOF_PATH}/${encodeURIComponent(ref.entry)}/proof`;
    const res = await fetch(url, { headers: { accept: "application/json" } });
    if (!res.ok) {
      throw new Error(`the proof endpoint answered ${res.status}`);
    }
    const body = (await res.json()) as { data?: unknown };
    if (body && typeof body === "object" && "data" in body && body.data != null) {
      return JSON.stringify(body.data);
    }
    return JSON.stringify(body);
  }

  private async run(): Promise<void> {
    this.render({ kind: "busy" });
    if (typeof WebAssembly !== "object") {
      this.render({
        kind: "error",
        message:
          "This browser cannot run WebAssembly, so the in-page check is unavailable. The same bill verifies on the server's /verify page.",
      });
      return;
    }
    const hash = this.expectedHash;
    if (!hash) {
      this.render({
        kind: "error",
        message:
          "Set the 64-character content hash you recorded with the bill — it is the half the check cannot supply itself.",
      });
      return;
    }
    try {
      const bundleJson = await this.resolveBundle();
      const result = await verifyBill(bundleJson, hash);
      this.render({
        kind: "verdict",
        verdict: result.verdict,
        charge: result.charge,
        price: result.price,
      });
    } catch (error) {
      this.render({
        kind: "error",
        message:
          error instanceof Error ? error.message : "the verification could not run",
      });
    }
  }

  private render(view: View): void {
    this.root.innerHTML = "";
    const style = document.createElement("style");
    style.textContent = STYLES;
    this.root.appendChild(style);

    const card = document.createElement("div");
    card.className = "card";
    this.root.appendChild(card);

    const h1 = document.createElement("h1");
    h1.textContent =
      view.kind === "verdict" ? "Verification result" : "Verify this bill";
    card.appendChild(h1);

    if (view.kind === "form") {
      this.renderForm(card);
    } else if (view.kind === "busy") {
      const p = document.createElement("p");
      p.className = "muted";
      p.textContent = "Verifying in this browser…";
      card.appendChild(p);
    } else if (view.kind === "verdict") {
      card.classList.add("verdict");
      this.renderVerdict(card, view);
    } else {
      const p = document.createElement("p");
      p.className = "error";
      const strong = document.createElement("strong");
      strong.textContent = "The check could not run.";
      p.appendChild(strong);
      const note = document.createElement("p");
      note.className = "muted";
      note.textContent = view.message;
      card.appendChild(p);
      card.appendChild(note);
    }
  }

  /** The paste-a-bundle form, shown when no input attributes are set. */
  private renderForm(card: HTMLElement): void {
    const hashLabel = document.createElement("label");
    hashLabel.textContent = "Content hash recorded with the bill";
    const hashInput = document.createElement("input");
    hashInput.name = "hash";
    hashInput.placeholder = "64 lowercase hex characters";
    hashInput.spellcheck = false;
    hashInput.autocomplete = "off";
    if (this.getAttribute(ATTR.expectedHash)) {
      hashInput.value = this.getAttribute(ATTR.expectedHash) ?? "";
    }
    card.appendChild(hashLabel);
    card.appendChild(hashInput);

    const bundleLabel = document.createElement("label");
    bundleLabel.textContent = "Proof bundle";
    const bundleInput = document.createElement("textarea");
    bundleInput.name = "bundle";
    bundleInput.placeholder =
      'the JSON from GET /api/v1/entries/{id}/proof — paste the inner "data"';
    bundleInput.spellcheck = false;
    card.appendChild(bundleLabel);
    card.appendChild(bundleInput);

    const note = document.createElement("p");
    note.className = "muted";
    note.textContent =
      "The check runs entirely in this browser; the bundle never leaves the page.";
    card.appendChild(note);

    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "Verify";
    button.addEventListener("click", () => {
      const h = hashInput.value.trim();
      const b = bundleInput.value.trim();
      if (h) this.setAttribute(ATTR.expectedHash, h);
      if (b) this.setAttribute(ATTR.bundle, b);
      void this.run();
    });
    card.appendChild(button);
  }

  private renderVerdict(card: HTMLElement, view: View & { kind: "verdict" }): void {
    const verdict = document.createElement("p");
    const strong = document.createElement("strong");
    const note = document.createElement("p");
    note.className = "muted";

    if (view.verdict === "pass") {
      verdict.className = "ok";
      strong.textContent = "The bill verifies.";
      note.textContent =
        "The content hash recomputes from the entry, and the inclusion proof links it to the tree head." +
        (view.charge ?? "") +
        (view.price ?? "");
    } else if (view.verdict === "chargeMismatch") {
      verdict.className = "error";
      strong.textContent = "The charge does not add up.";
      note.textContent =
        "The entry is proven unaltered, but the charge it records does not recompute from the usage and rates it carries.";
    } else {
      verdict.className = "error";
      strong.textContent = "The bill does not verify.";
      note.textContent =
        view.detail ??
        "Either the bundle was altered, or the hash you recorded does not match this entry.";
    }
    verdict.appendChild(strong);
    card.appendChild(verdict);
    card.appendChild(note);
  }
}

// Register under the spec name only once — a second import on the same page is
// a no-op, so bundlers that dedupe and pages that double-include both behave.
if (!customElements.get("verify-bill")) {
  customElements.define("verify-bill", VerifyBillElement);
}

declare global {
  interface HTMLElementTagNameMap {
    "verify-bill": VerifyBillElement;
  }
}
