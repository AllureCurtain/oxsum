/**
 * `<verify-usage-report>` — the usage-report check as an embeddable element.
 *
 * The usage-report counterpart to `<verify-bill>` and `<verify-statement>`: it
 * runs `verifyUsageReport` inside the element's shadow root against a report
 * bundle — the frozen subset an organization hands one downstream customer:
 * the settlement entries attributed to them, each with an inclusion proof,
 * plus the marker entry that commits the subset digest.
 *
 * A bundle reaches the element two ways:
 *
 *   <!-- paste the bundle the customer already holds -->
 *   <verify-usage-report bundle='{"endUser":…}'></verify-usage-report>
 *   <!-- with no `bundle` it renders its own paste form -->
 *
 *   <!-- fetch it straight off the public bill page's embedded bundle -->
 *   <verify-usage-report server="https://oxsum.example.com"
 *                        token="the-bill-link-token"></verify-usage-report>
 *
 * The `server`+`token` mode resolves `/bill/{token}` — the credential-free
 * surface a share link opens — and lifts the `data-bundle` the page embeds.
 * `GET /api/v1/org/usage-reports/{id}` wants an organization credential, so it
 * is deliberately not a fetch mode here: a public element has no org key, and
 * the customer the report is *for* reaches the bundle through the bill link.
 *
 * Three inputs shape the verdict:
 *
 * - `bundle` — the report JSON (the `data` object). Its own `endUser`,
 *   `keyName`/`publicKey`/`keyHash`, `note` and `subsetHash` are read from it.
 * - `end-user` — who the report should be for. Left unset, the element uses
 *   the bundle's own `endUser` — but a customer who supplies the name they
 *   billed under turns a self-consistency check into a wrong-recipient check:
 *   a report committing a different `endUser` fails.
 * - `key` — the published operator key `{keyName, publicKey, keyHash}` the
 *   customer holds. Left unset, the element uses the key fields the bundle
 *   itself names (the same `keyOf` the bill page runs) — which still verifies
 *   the signature, just against the key the server asserted rather than one
 *   obtained independently.
 *
 * The unsigned deployment is honest: a bundle with no `note` proves the rows
 * are in the ledger and fold to the committed subset — membership, not a
 * signature — and the element says "this deployment signs no heads" rather
 * than claim a signature that was never checked.
 *
 * The wasm module instantiates lazily on first verify, so attaching the
 * element costs nothing until a check actually runs. On a browser without
 * WebAssembly the element renders a readable fallback rather than failing
 * blank.
 */

import { verifyUsageReport } from "./index.js";

const BILL_PATH = "/bill";

type View =
  | { kind: "form" }
  | { kind: "busy" }
  | {
      kind: "verdict";
      ok: boolean;
      signed: boolean;
      completeness: boolean;
      detail: string;
    }
  | { kind: "error"; message: string };

/** The element's attributes. Read once on connect and again on each verify. */
const ATTR = {
  server: "server",
  token: "token",
  bundle: "bundle",
  endUser: "end-user",
  key: "key",
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
  textarea { min-height: 8rem; font-family: ui-monospace, monospace; resize: vertical; }
  button {
    margin-top: 0.9rem; font: inherit; font-weight: 600; font-size: 0.9rem;
    padding: 0.55rem 1.1rem; border: 0; border-radius: 6px; cursor: pointer;
    background: #1d4ed8; color: #fff;
  }
  button[disabled] { opacity: 0.6; cursor: default; }
  a { color: #1d4ed8; }
`;

/** The report bundle's fields the element reads — the rest pass through. */
interface ReportBundle {
  endUser?: string;
  note?: string | null;
  keyName?: string | null;
  publicKey?: string | null;
  keyHash?: string | null;
  completeness?: unknown;
}

export class VerifyUsageReportElement extends HTMLElement {
  static get observedAttributes(): string[] {
    return Object.values(ATTR);
  }

  private root: ShadowRoot;

  constructor() {
    super();
    this.root = this.attachShadow({ mode: "open" });
  }

  connectedCallback(): void {
    if (this.serverAndToken || this.bundle) {
      void this.run();
    } else {
      this.render({ kind: "form" });
    }
  }

  attributeChangedCallback(): void {
    // A changed input invalidates a prior verdict only while the element is
    // showing its form — a completed check is not silently re-run.
    if (this.isConnected && !this.root.querySelector(".verdict")) {
      if (this.serverAndToken || this.bundle) {
        void this.run();
      } else {
        this.render({ kind: "form" });
      }
    }
  }

  private get serverAndToken(): { server: string; token: string } | null {
    const server = this.getAttribute(ATTR.server)?.replace(/\/$/, "");
    const token = this.getAttribute(ATTR.token);
    return server && token ? { server, token } : null;
  }

  private get bundle(): string | null {
    const raw = this.getAttribute(ATTR.bundle);
    return raw?.trim() ? raw.trim() : null;
  }

  private get endUserAttr(): string | null {
    const raw = this.getAttribute(ATTR.endUser);
    return raw?.trim() ? raw.trim() : null;
  }

  private get keyAttr(): string | null {
    const raw = this.getAttribute(ATTR.key);
    return raw?.trim() ? raw.trim() : null;
  }

  /**
   * Resolves the bundle to verify: the `bundle` attribute verbatim, else the
   * `data-bundle` the public `/bill/{token}` page embeds — the credential-free
   * surface a share link opens. The org endpoint wants a credential a public
   * element does not hold, so it is not a fetch mode.
   */
  private async resolveBundle(): Promise<string> {
    if (this.bundle) {
      return this.bundle;
    }
    const ref = this.serverAndToken;
    if (!ref) {
      throw new Error("no bundle and no server/token to fetch one");
    }
    const url = `${ref.server}${BILL_PATH}/${encodeURIComponent(ref.token)}`;
    const res = await fetch(url, { headers: { accept: "text/html" } });
    if (!res.ok) {
      throw new Error(`the bill page answered ${res.status}`);
    }
    const html = await res.text();
    // The page embeds each report's bundle on its verify button. Lift the
    // first — a scoped link shows one report; an end-user link several, and the
    // first is enough to verify (each carries its own committed subset).
    const doc = new DOMParser().parseFromString(html, "text/html");
    const button = doc.querySelector("button.verify-bill[data-bundle]");
    const embedded = button?.getAttribute("data-bundle");
    if (!embedded) {
      throw new Error("the bill page embeds no report bundle");
    }
    return embedded;
  }

  /**
   * The published-key argument `verifyUsageReport` wants: the `key` attribute
   * verbatim, else the key fields the bundle itself names — the same `keyOf`
   * the bill page runs, so the signature is still checked, just against the
   * key the server asserted. `null` on an unsigned bundle — membership only.
   */
  private keyOf(bundle: ReportBundle): string | null {
    if (this.keyAttr) {
      return this.keyAttr;
    }
    if (!bundle.keyName || !bundle.publicKey || !bundle.keyHash) {
      return null;
    }
    return JSON.stringify({
      keyName: bundle.keyName,
      publicKey: bundle.publicKey,
      keyHash: bundle.keyHash,
    });
  }

  private async run(): Promise<void> {
    this.render({ kind: "busy" });
    if (typeof WebAssembly !== "object") {
      this.render({
        kind: "error",
        message:
          "This browser cannot run WebAssembly, so the in-page check is unavailable. The same report verifies on the bill page's own checker.",
      });
      return;
    }
    try {
      const bundleJson = await this.resolveBundle();
      const bundle = JSON.parse(bundleJson) as ReportBundle;
      if (!bundle || typeof bundle !== "object") {
        throw new Error("the bundle is not a JSON object");
      }
      const endUser = this.endUserAttr ?? bundle.endUser ?? "";
      const key = this.keyOf(bundle);
      const result = await verifyUsageReport(bundleJson, endUser, key);
      this.render({
        kind: "verdict",
        ok: result.ok,
        signed: key !== null,
        completeness: bundle.completeness != null,
        detail: result.detail,
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
      view.kind === "verdict" ? "Verification result" : "Verify this report";
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
    const bundleLabel = document.createElement("label");
    bundleLabel.textContent = "Usage-report bundle";
    const bundleInput = document.createElement("textarea");
    bundleInput.name = "bundle";
    bundleInput.placeholder =
      "the report JSON — the data-bundle a /bill/{token} page embeds, or the data of GET /api/v1/org/usage-reports/{id}";
    bundleInput.spellcheck = false;
    card.appendChild(bundleLabel);
    card.appendChild(bundleInput);

    const userLabel = document.createElement("label");
    userLabel.textContent = "Billed end-user (optional)";
    const userInput = document.createElement("input");
    userInput.name = "endUser";
    userInput.placeholder =
      "the customer name the report should be for — supply it to check the recipient, not only the bundle's self-consistency";
    userInput.spellcheck = false;
    userInput.autocomplete = "off";
    if (this.getAttribute(ATTR.endUser)) {
      userInput.value = this.getAttribute(ATTR.endUser) ?? "";
    }
    card.appendChild(userLabel);
    card.appendChild(userInput);

    const note = document.createElement("p");
    note.className = "muted";
    note.textContent =
      "The check runs entirely in this browser; the bundle never leaves the page.";
    card.appendChild(note);

    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "Verify";
    button.addEventListener("click", () => {
      const b = bundleInput.value.trim();
      const u = userInput.value.trim();
      if (b) this.setAttribute(ATTR.bundle, b);
      if (u) this.setAttribute(ATTR.endUser, u);
      void this.run();
    });
    card.appendChild(button);
  }

  private renderVerdict(card: HTMLElement, view: View & { kind: "verdict" }): void {
    const verdict = document.createElement("p");
    const strong = document.createElement("strong");
    const note = document.createElement("p");
    note.className = "muted";

    if (view.ok) {
      verdict.className = "ok";
      strong.textContent = "The report verifies.";
      const signature = view.signed
        ? "and the head is signed by the operator key"
        : "and this deployment signs no heads, so no signature was checked";
      const completeness = view.completeness
        ? " The anchored day log proves no settlement in this span is missing."
        : " It does not prove completeness — the entry count is the organization's claim.";
      note.textContent =
        `Every shown row is in the ledger, the rows fold to the committed subset, ${signature}.` +
        completeness;
    } else {
      verdict.className = "error";
      strong.textContent = "The report does not verify.";
      note.textContent =
        view.detail ||
        "Either the bundle was altered, or the rows it shows are not the subset the marker committed.";
    }
    verdict.appendChild(strong);
    card.appendChild(verdict);
    card.appendChild(note);
  }
}

// Register under the spec name only once — a second import on the same page is
// a no-op, so bundlers that dedupe and pages that double-include both behave.
if (!customElements.get("verify-usage-report")) {
  customElements.define("verify-usage-report", VerifyUsageReportElement);
}

declare global {
  interface HTMLElementTagNameMap {
    "verify-usage-report": VerifyUsageReportElement;
  }
}
