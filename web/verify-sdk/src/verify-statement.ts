/**
 * `<verify-statement>` — the statement-anchor check as an embeddable element.
 *
 * The statement counterpart to `<verify-bill>`: it runs `verifyStatementAnchor`
 * inside the element's shadow root against the proof bundle
 * `GET /api/v1/statements/{id}/proof` answers — the self-contained shape built
 * precisely so a verifier needs nothing else. The window fold, the rollup
 * digest and the signed head that follows the anchor are all recomputed in the
 * page; no server round-trip stands between the answer and the verdict.
 *
 * Two ways to feed it a statement:
 *
 *   <!-- the element fetches the bundle itself -->
 *   <verify-statement server="https://oxsum.example.com"
 *                     statement="a8f3…uuid"></verify-statement>
 *
 *   <!-- or paste the bundle the customer already holds -->
 *   <verify-statement bundle='{"statement":…}'></verify-statement>
 *   <!-- with no `bundle` it renders its own paste form -->
 *
 * The bundle is a superset of what `verifyStatementAnchor` consumes: the
 * element splits it into the detail wire `{statement, lines, anchor}` (the
 * check's `json`) and the check `{windowHashes, expectedHead, expectedOrigin}`
 * (its `check`). A customer drops the endpoint's answer in and gets a verdict.
 *
 * The unsigned deployment is the honest edge: a bundle whose `anchor.note` and
 * `expectedHead` are `null` proves the window fold and the rollup — the two
 * digests — but no signature layer. The wasm binding's check wants a non-null
 * head, so the element feeds it a stand-in the unsigned path never reads and
 * renders "verified to the digests; this deployment signs no heads" rather
 * than claim a signature it never checked.
 *
 * The wasm module instantiates lazily on first verify, so attaching the
 * element costs nothing until a check actually runs. On a browser without
 * WebAssembly the element renders a readable fallback rather than failing
 * blank.
 */

import { verifyStatementAnchor } from "./index.js";

const PROOF_PATH = "/api/v1/statements";

/** The head an unsigned deployment cannot supply — the check never reads it. */
const DUMMY_HEAD = { size: 0, root: "0".repeat(64) };

type View =
  | { kind: "form" }
  | { kind: "busy" }
  | { kind: "verdict"; verdict: string; unsigned: boolean; detail: string }
  | { kind: "error"; message: string };

/** The element's attributes. Read once on connect and again on each verify. */
const ATTR = {
  server: "server",
  statement: "statement",
  bundle: "bundle",
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
  textarea {
    width: 100%; box-sizing: border-box; font: inherit; font-size: 0.85rem;
    border: 1px solid #c9ced6; border-radius: 6px; padding: 0.5rem 0.6rem;
    background: #fff; color: inherit;
  }
  textarea { min-height: 9rem; font-family: ui-monospace, monospace; resize: vertical; }
  button {
    margin-top: 0.9rem; font: inherit; font-weight: 600; font-size: 0.9rem;
    padding: 0.55rem 1.1rem; border: 0; border-radius: 6px; cursor: pointer;
    background: #1d4ed8; color: #fff;
  }
  button[disabled] { opacity: 0.6; cursor: default; }
  a { color: #1d4ed8; }
`;

/**
 * What the fetched or pasted bundle looks like to the splitter. Only the
 * fields the check consumes are typed — the rest pass through verbatim into
 * the detail wire.
 */
interface StatementProofBundle {
  statement?: unknown;
  lines?: unknown;
  anchor?: { note?: string | null } | null;
  windowHashes?: unknown;
  expectedHead?: { size: number; root: string } | null;
  expectedOrigin?: unknown;
}

export class VerifyStatementElement extends HTMLElement {
  static get observedAttributes(): string[] {
    return Object.values(ATTR);
  }

  private root: ShadowRoot;

  constructor() {
    super();
    this.root = this.attachShadow({ mode: "open" });
  }

  connectedCallback(): void {
    if (this.serverAndStatement || this.bundle) {
      void this.run();
    } else {
      this.render({ kind: "form" });
    }
  }

  attributeChangedCallback(): void {
    // A changed input invalidates a prior verdict only while the element is
    // showing its form — a completed check is not silently re-run.
    if (this.isConnected && !this.root.querySelector(".verdict")) {
      if (this.serverAndStatement || this.bundle) {
        void this.run();
      } else {
        this.render({ kind: "form" });
      }
    }
  }

  private get serverAndStatement(): { server: string; statement: string } | null {
    const server = this.getAttribute(ATTR.server)?.replace(/\/$/, "");
    const statement = this.getAttribute(ATTR.statement);
    return server && statement ? { server, statement } : null;
  }

  private get bundle(): string | null {
    const raw = this.getAttribute(ATTR.bundle);
    return raw?.trim() ? raw.trim() : null;
  }

  /**
   * Resolves the bundle to verify: the `bundle` attribute verbatim, else the
   * fetched `/api/v1/statements/{id}/proof` `data` object. The endpoint's
   * envelope is unwrapped here so the splitter always sees the inner bundle.
   */
  private async resolveBundle(): Promise<string> {
    if (this.bundle) {
      return this.bundle;
    }
    const ref = this.serverAndStatement;
    if (!ref) {
      throw new Error("no bundle and no server/statement to fetch one");
    }
    const url = `${ref.server}${PROOF_PATH}/${encodeURIComponent(ref.statement)}/proof`;
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

  /**
   * Splits the bundle into the two arguments `verifyStatementAnchor` wants:
   * the detail wire `{statement, lines, anchor}` and the check
   * `{windowHashes, expectedHead, expectedOrigin}`. The unsigned case —
   * `expectedHead: null` — is given the stand-in head the note path never
   * reads, and reported via the `unsigned` flag.
   */
  private split(bundleJson: string): {
    detailJson: string;
    checkJson: string;
    unsigned: boolean;
  } {
    const bundle = JSON.parse(bundleJson) as StatementProofBundle;
    if (!bundle || typeof bundle !== "object") {
      throw new Error("the bundle is not a JSON object");
    }
    const detail = {
      statement: bundle.statement,
      lines: bundle.lines,
      anchor: bundle.anchor,
    };
    const unsigned = bundle.expectedHead == null;
    const check = {
      windowHashes: bundle.windowHashes ?? [],
      // The unsigned path returns before the head is consulted — a stand-in
      // satisfies the binding's non-null shape without being read.
      expectedHead: bundle.expectedHead ?? DUMMY_HEAD,
      expectedOrigin: bundle.expectedOrigin ?? "",
    };
    return {
      detailJson: JSON.stringify(detail),
      checkJson: JSON.stringify(check),
      unsigned,
    };
  }

  private async run(): Promise<void> {
    this.render({ kind: "busy" });
    if (typeof WebAssembly !== "object") {
      this.render({
        kind: "error",
        message:
          "This browser cannot run WebAssembly, so the in-page check is unavailable. The same statement verifies against the ledger's published key and head endpoints.",
      });
      return;
    }
    try {
      const bundleJson = await this.resolveBundle();
      const { detailJson, checkJson, unsigned } = this.split(bundleJson);
      const result = await verifyStatementAnchor(detailJson, checkJson);
      this.render({
        kind: "verdict",
        verdict: result.ok ? "pass" : "fail",
        unsigned,
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
      view.kind === "verdict" ? "Verification result" : "Verify this statement";
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
    bundleLabel.textContent = "Statement proof bundle";
    const bundleInput = document.createElement("textarea");
    bundleInput.name = "bundle";
    bundleInput.placeholder =
      'the JSON from GET /api/v1/statements/{id}/proof — paste the inner "data"';
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
      const b = bundleInput.value.trim();
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

    if (view.verdict === "pass" && view.unsigned) {
      verdict.className = "ok";
      strong.textContent = "The statement's figures verify.";
      note.textContent =
        "The covered entries' hashes refold to the recorded window digest, and the statement's lines rebuild the committed rollup. This deployment signs no heads, so no signature attested the anchor — the two digests alone are what was checked.";
    } else if (view.verdict === "pass") {
      verdict.className = "ok";
      strong.textContent = "The statement verifies.";
      note.textContent =
        "The covered entries' hashes refold to the recorded window digest, the statement's lines rebuild the committed rollup, and the operator's signature attests the head that follows the anchor.";
    } else {
      verdict.className = "error";
      strong.textContent = "The statement does not verify.";
      note.textContent =
        view.detail ||
        "Either the bundle was altered, or the figures it shows are not the ones the anchor committed.";
    }
    verdict.appendChild(strong);
    card.appendChild(verdict);
    card.appendChild(note);
  }
}

// Register under the spec name only once — a second import on the same page is
// a no-op, so bundlers that dedupe and pages that double-include both behave.
if (!customElements.get("verify-statement")) {
  customElements.define("verify-statement", VerifyStatementElement);
}

declare global {
  interface HTMLElementTagNameMap {
    "verify-statement": VerifyStatementElement;
  }
}
