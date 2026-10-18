//! The last server-rendered page: `/verify` (public bill verification), plus
//! the stylesheet it links.
//!
//! Everything else the Topcoat dashboard served — the dashboard, auth and
//! device pages — retired to the React app under `/app` (issue #256), which the
//! server redirects the legacy paths to. `/verify` stays here on purpose: it is
//! the one surface deliberately kept out of the browser bundle.
//!
//! ## `/verify` trust model
//!
//! The page is a plain server-rendered form plus a progressive enhancement:
//! with no JavaScript it posts the bundle to the server and the server runs
//! `oxsum_verify` — the same check it always ran. When the browser supports
//! it, `verify.js` loads the `oxsum_verify` wasm build the crate exports
//! (`#[wasm_bindgen]` bindings in `crates/verify/src/wasm.rs`, issue #284) and
//! runs the proof check on the client's own machine — the verifier's bytes
//! travel to the user instead of the user trusting a verdict the server
//! rendered. The check is identical either way; only whose machine runs it
//! differs. A deployment built without the wasm toolchain serves the committed
//! placeholder module, which fails to import and quietly leaves the form on
//! the server path.

use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::{Css, Form, Js, Wasm};
use topcoat::router::error::bad_request;
use topcoat::router::{RouterBuilder, page, parse_query_params, route};
use topcoat::view::{View, view};

use crate::views::plain;

/// Registers the remaining server-rendered pages on the router builder.
///
/// Only `/verify` and its stylesheet are left: the dashboard, auth and device
/// pages all retired to the React app under `/app` (issue #256), which the
/// server redirects to. `/verify` stays server-rendered by design — see the
/// module doc above.
pub(crate) fn register(builder: RouterBuilder) -> RouterBuilder {
    builder
        .page(verify_page)
        .page(verify_submit)
        .route(style)
        .route(verify_js)
        .route(verify_wasm_js)
        .route(verify_wasm_wasm)
}

/// The public verification page: anyone holding a bill and its content hash
/// can verify it, no session needed.
#[page("/verify")]
async fn verify_page(cx: &Cx) -> Result<impl View> {
    let query = parse_query_params::<VerifyQuery>(cx).unwrap_or_default();
    let bundle = query.bundle.unwrap_or_default();
    let hash = query.hash.unwrap_or_default();
    let entry = query.entry.unwrap_or_default();
    Ok(view! {
        plain(
            title: "Verify a bill",
            <div class="card">
                <h1>"Verify a bill"</h1>
                <p class="muted">
                    "Paste a bill's proof bundle and the content hash you recorded with it. "
                    "The check runs in this browser when the verifier is served, on this server otherwise."
                </p>
                <form method="post" action="/verify">
                    <label>
                        "Proof bundle (JSON)"
                        <textarea name="bundle" rows="10" required="required">(bundle)</textarea>
                    </label>
                    <label>
                        "Content hash (hex)"
                        <input type="text" name="expected_hash" value=(hash) required="required" />
                    </label>
                    <input type="hidden" name="entry" value=(entry) />
                    <button type="submit">"Verify"</button>
                </form>
                <script type="module" src="/verify.js"></script>
            </div>
        )
    })
}

#[derive(Deserialize, Default)]
struct VerifyQuery {
    bundle: Option<String>,
    hash: Option<String>,
    entry: Option<String>,
}

#[derive(Deserialize)]
struct VerifyForm {
    bundle: String,
    expected_hash: String,
    #[allow(dead_code)]
    entry: Option<String>,
}

/// Runs the proof check on the server and renders the verdict.
///
/// Two bundle shapes share the one form: a bill's proof bundle (`{entry, head,
/// proof}`) checks against the pasted content hash, and an end-user usage
/// report (`{subsetHash, marker, entries, …}`, issue #261) checks itself —
/// the pasted `expected_hash` is ignored for the second, since the report's
/// own commitment is what is under test. The shape, not a flag, decides which.
#[page(POST "/verify")]
async fn verify_submit(cx: &Cx, Form(form): Form<VerifyForm>) -> Result<impl View> {
    // The proof API wraps the bundle in its `data` envelope; a pasted bundle may be
    // either the envelope or the bundle itself.
    let bundle = form.bundle.trim();
    let value = serde_json::from_str::<serde_json::Value>(bundle)
        .map_err(|_| bad_request("the bundle did not parse as JSON"))?;
    let inner = value
        .get("data")
        .cloned()
        .filter(|data| data.get("entry").is_some() || data.get("subsetHash").is_some())
        .map(|data| data.to_string())
        .unwrap_or_else(|| bundle.to_owned());

    // A usage report carries `subsetHash` where a bill carries `entry` — the
    // field, not a flag, picks the check. The report binds its own endUser and
    // head note, so the verifier needs only the published key and the origin
    // inside the note.
    //
    // For a bill: `verify_bill` composes inclusion, the charge recompute and
    // the price binding into one verdict — the same call the browser's wasm
    // export makes (issue #284), so the page reads the same whichever side ran
    // it. The binding reports under the pass, never a verdict of its own.
    enum Verdict {
        Pass { charge: String, price: String },
        ChargeMismatch,
        Fail,
    }
    // A usage report answers a different question than a bill, so it renders
    // its own verdict — but inside the one view, since `impl View` unifies no
    // two `view!` bodies.
    enum Page {
        Bill(Verdict),
        UsageReport { passed: bool, detail: String },
    }
    let page = if serde_json::from_str::<serde_json::Value>(&inner)
        .ok()
        .and_then(|v| v.get("subsetHash").cloned())
        .is_some()
    {
        let (passed, detail) = usage_report_outcome(cx, &inner).await?;
        Page::UsageReport { passed, detail }
    } else {
        let hash = oxsum_verify::Hash::parse_hex(form.expected_hash.trim())
            .map_err(|_| bad_request("the content hash is not 64 hex characters"))?;
        let bill = oxsum_verify::verify_bill(&inner, &hash)
            .map_err(|_| bad_request("the bundle did not parse as JSON"))?;
        let verdict = match bill.verdict {
            "pass" => Verdict::Pass {
                charge: bill.charge,
                price: bill.price,
            },
            "chargeMismatch" => Verdict::ChargeMismatch,
            _ => Verdict::Fail,
        };
        Page::Bill(verdict)
    };
    Ok(view! {
        plain(
            title: "Verification result",
            <div class="card">
                <h1>"Verification result"</h1>
                match page {
                    Page::UsageReport { passed, detail } => {
                        if passed {
                            <p class="ok"><strong>"The usage report verifies."</strong></p>
                            <p class="muted">
                                "Every shown entry is in the ledger the signed head attests, their recomputed hashes fold to the subset digest the marker committed, and the marker's DocumentRef binds that digest — the subset the report shows is the subset the organization committed once, for this endUser, over this span."
                            </p>
                        } else {
                            <p class="error"><strong>"The usage report does not verify."</strong></p>
                            <p class="muted">(detail)</p>
                        }
                    }
                    Page::Bill(verdict) => match verdict {
                    Verdict::Pass { charge, price } => {
                        <p class="ok"><strong>"The bill verifies."</strong></p>
                        <p class="muted">
                            "The content hash recomputes from the entry, and the inclusion proof links it to the tree head."
                            (charge)
                            (price)
                        </p>
                    }
                    Verdict::ChargeMismatch => {
                        <p class="error"><strong>"The charge does not add up."</strong></p>
                        <p class="muted">
                            "The entry is proven unaltered, but the charge it records does not recompute from the usage and rates it carries."
                        </p>
                    }
                    Verdict::Fail => {
                        <p class="error"><strong>"The bill does not verify."</strong></p>
                        <p class="muted">
                            "Either the bundle was altered, or the hash you recorded does not match this entry."
                        </p>
                    }
                    }
                }
                <p><a href="/verify">"Verify another"</a></p>
            </div>
        )
    })
}

/// Runs `oxsum_verify::verify_usage_report` on a pasted report and reduces the
/// outcome to the `(passed, detail)` the shared result view renders (issue
/// #261). The bundle is self-contained: the `endUser` it commits is the one
/// the check expects, and the `note`'s origin names the ledger — so the only
/// input the page adds is the deployment's published key, derived from
/// `head_seed` like the other signed-head pages do.
async fn usage_report_outcome(cx: &Cx, inner: &str) -> Result<(bool, String)> {
    // The report's own descriptor answers who the report is for and which log
    // its head names — both come off the bundle, never from the verifier, so a
    // report minted for somebody else cannot pass by borrowing this page's org.
    let value = serde_json::from_str::<serde_json::Value>(inner)
        .map_err(|_| bad_request("the report did not parse as JSON"))?;
    let end_user = value
        .get("endUser")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad_request("the report carries no endUser"))?
        .to_owned();
    // The published key: present on a signing deployment, absent otherwise —
    // `verify_usage_report` verifies membership to the two digests alone then.
    // `PublishedKey` borrows, so the name and both hex strings outlive it in
    // locals.
    let publication = match crate::web_config(cx)?.head_seed {
        Some(seed) => {
            let signing = oxsum_core::signing_key(seed).map_err(|_| {
                topcoat::router::error::internal_server_error(topcoat::Error::msg(
                    "the head signing key is misconfigured",
                ))
            })?;
            Some(oxsum_core::KeyPublication::of(&signing))
        }
        None => None,
    };
    let public_hex = publication
        .as_ref()
        .map(|k| {
            k.public_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        })
        .unwrap_or_default();
    let hash_hex = publication
        .as_ref()
        .map(|k| {
            k.key_hash
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        })
        .unwrap_or_default();
    let key = publication.as_ref().map(|k| oxsum_verify::PublishedKey {
        key_name: &k.name,
        public_key: &public_hex,
        key_hash: &hash_hex,
    });
    // The origin the head note attests — parsed from the bundle, not assumed,
    // so a report for another tenant's ledger is checked under its own origin.
    let origin = value
        .get("note")
        .and_then(|v| v.as_str())
        .and_then(|note| oxsum_core::SignedTreeHead::parse(note).ok())
        .map(|parsed| parsed.origin.as_str().to_owned())
        .unwrap_or_default();
    let outcome = oxsum_verify::verify_usage_report(inner, &origin, &end_user, key.as_ref());
    Ok(match outcome {
        Ok(()) => (true, String::new()),
        Err(error) => (false, format!("{error}.")),
    })
}

/// Serves the dashboard stylesheet: the source file, directly. The only page
/// left that uses it is the server-rendered /verify form (issue #256).
#[route(GET "/style/main.css")]
async fn style() -> Result<Css<&'static str>> {
    Ok(Css(STYLE))
}

const STYLE: &str = include_str!("../style/main.css");

/// The page's own script: intercepts the form's submit and runs the wasm
/// verifier when the browser has one. Checked in, not generated — this is the
/// page's behavior, not wasm-bindgen's output.
#[route(GET "/verify.js")]
async fn verify_js() -> Result<Js<&'static str>> {
    Ok(Js(VERIFY_JS))
}

const VERIFY_JS: &str = include_str!("verify.js");

/// The wasm-bindgen ES-module glue for `oxsum_verify` — generated by
/// `scripts/build-verify-wasm.sh` into `verify-wasm/` and embedded here. The
/// committed file is a placeholder whose `init` throws when no wasm build ran,
/// so the page's loader fails over to the server-side form.
#[route(GET "/verify-wasm/verify_wasm.js")]
async fn verify_wasm_js() -> Result<Js<&'static str>> {
    Ok(Js(VERIFY_WASM_JS))
}

const VERIFY_WASM_JS: &str = include_str!("../verify-wasm/verify_wasm.js");

/// The `oxsum_verify` wasm module itself, beside its glue so the generated
/// `new URL("verify_wasm_bg.wasm", import.meta.url)` resolves under the same
/// `/verify-wasm/` prefix. `application/wasm` is required for the browser's
/// streaming compilation entry points.
#[route(GET "/verify-wasm/verify_wasm_bg.wasm")]
async fn verify_wasm_wasm() -> Result<Wasm<&'static [u8]>> {
    Ok(Wasm(VERIFY_WASM))
}

const VERIFY_WASM: &[u8] = include_bytes!("../verify-wasm/verify_wasm_bg.wasm");
