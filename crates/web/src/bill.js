// The bill page's in-browser checker (issue #305).
//
// The page renders the frozen report and embeds its bundle as a `data-bundle`
// attribute. This module progressively enhances that skeleton: when the browser
// has a wasm engine and the deployment shipped the verifier, pressing "Verify
// this bill" runs the same `oxsum_verify` checks the server runs — on the
// customer's own machine, over the exact bytes the page embedded.
//
// Every failure mode (no wasm, the placeholder bundle, a fetch error) leaves the
// rendered bill alone and says the check could not run; it never turns a broken
// verifier into a passing verdict.

let verifier;

async function loadVerifier() {
  if (verifier) {
    return verifier;
  }
  const response = await fetch("/verify-wasm/verify_wasm_bg.wasm");
  if (!response.ok) {
    throw new Error("the verifier artifact is not served");
  }
  const bytes = await response.arrayBuffer();
  const module = await import("/verify-wasm/verify_wasm.js");
  const verify = await module.default(bytes);

  // The artifact's hash, so the customer can say which verifier ran. A
  // non-secure context has no SubtleCrypto; the verdict still stands.
  let hash = "";
  if (globalThis.crypto && globalThis.crypto.subtle) {
    try {
      const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
      hash = Array.from(new Uint8Array(digest), (byte) =>
        byte.toString(16).padStart(2, "0"),
      ).join("");
    } catch {
      hash = "";
    }
  }
  verifier = { verify, hash };
  return verifier;
}

// The bundle names its own operator key; pass it so the head's signature is
// checked. A deployment that signs no heads carries no key, and the verifier
// checks membership to the committed digests alone — the page already says the
// key is not published.
function keyOf(bundle) {
  if (!bundle.keyName || !bundle.publicKey || !bundle.keyHash) {
    return null;
  }
  return JSON.stringify({
    keyName: bundle.keyName,
    publicKey: bundle.publicKey,
    keyHash: bundle.keyHash,
  });
}

function render(status, result, hash, signed) {
  const tail = hash ? ` (verifier ${hash.slice(0, 16)}…)` : "";
  if (result.ok) {
    status.className = "verdict success";
    const signature = signed
      ? "and the head is signed by the operator key"
      : "and this deployment signs no heads, so no signature was checked";
    status.textContent =
      `This bill verifies: every shown row is in the ledger, the rows fold to the ` +
      `committed subset, ${signature}. It does not prove completeness — the entry ` +
      `count is the organization's claim.` +
      tail;
  } else {
    status.className = "verdict error";
    status.textContent = `This bill does not verify: ${result.detail}${tail}`;
  }
}

export async function attach() {
  const buttons = document.querySelectorAll("button.verify-bill");
  if (!buttons.length || typeof WebAssembly !== "object") {
    return;
  }
  for (const button of buttons) {
    button.addEventListener("click", async () => {
      const status = button.parentElement.querySelector(".verdict");
      status.className = "verdict";
      status.textContent = "Verifying in this browser…";
      try {
        const loaded = await loadVerifier();
        const bundle = JSON.parse(button.dataset.bundle);
        const key = keyOf(bundle);
        const outcome = loaded.verify.verifyUsageReport(
          button.dataset.bundle,
          button.dataset.endUser,
          key,
        );
        render(status, JSON.parse(outcome), loaded.hash, key !== null);
      } catch (error) {
        status.className = "verdict error";
        status.textContent =
          "The check could not run here: " +
          String((error && error.message) || error) +
          ". The bill above is still the frozen record the organization committed.";
      }
    });
  }
}

attach();
