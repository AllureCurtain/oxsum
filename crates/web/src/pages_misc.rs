//! The remaining pages: `/verify` (public bill verification) and `/device`
//! (the device grant's approval page).
//!
//! ## `/verify` trust-model change
//!
//! The Leptos dashboard ran `oxsum_verify::verify_bundle` **in the browser**
//! (WASM): anyone holding a bill and its content hash could verify it without
//! trusting the server. Topcoat has no browser target, so the check now runs
//! **on the server**: the page posts the bundle JSON and the expected hash,
//! the server recomputes and answers. The proof math is unchanged, but the
//! user must trust the server's answer — the "don't trust the server" property
//! is gone.

use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::{Css, Form, Js};
use topcoat::router::error::bad_request;
use topcoat::router::{RouterBuilder, page, parse_query_params, route};
use topcoat::view::{View, view};

use crate::session::page_ctx;
use crate::views::{dashboard_frame, plain};

/// Registers the misc pages on the router builder.
pub(crate) fn register(builder: RouterBuilder) -> RouterBuilder {
    builder
        .page(verify_page)
        .page(verify_submit)
        .route(chat_js)
        .page(device_page)
        .page(device_submit)
        .route(style)
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
                    "The check runs on this server."
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
    // For a bill: once inclusion is proven, whether the charge inside the entry
    // is what its own usage and rates add up to, and whether the price the
    // description names is bound to a checkpoint the operator signed (issue
    // #204). The binding reports under the pass, never a verdict of its own.
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
        let ok = oxsum_verify::verify_bundle(&inner, &hash)
            .map_err(|_| bad_request("the bundle did not parse as JSON"))?;
        let verdict = if !ok {
            Verdict::Fail
        } else {
            let description = serde_json::from_str::<serde_json::Value>(&inner)
                .ok()
                .and_then(|bundle| {
                    bundle
                        .get("entry")?
                        .get("description")?
                        .as_str()
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            match oxsum_verify::verify_charge(&description) {
                oxsum_verify::ChargeCheck::Mismatch => Verdict::ChargeMismatch,
                check => {
                    let mandate = serde_json::from_str::<serde_json::Value>(&description)
                        .ok()
                        .and_then(|record| record.get("mandate").cloned())
                        .and_then(|mandate| {
                            let id = mandate.get("id")?.as_str()?.to_owned();
                            let hash = mandate.get("hash")?.as_str()?.to_owned();
                            Some(format!(
                                " The turn ran under spend mandate {id} (blob sha256 {}…).",
                                &hash[..16.min(hash.len())]
                            ))
                        });
                    let charge = match check {
                        oxsum_verify::ChargeCheck::Recomputed => {
                            " The recorded charge itself recomputes from the usage and rates inside the entry."
                                .to_owned()
                        }
                        oxsum_verify::ChargeCheck::NotASettlement => {
                            " The entry is not a usage settlement, so there is no charge to recompute."
                                .to_owned()
                        }
                        oxsum_verify::ChargeCheck::OlderSchema => {
                            " The record predates charge verification — the entry is proven, but its arithmetic is too old to recompute."
                                .to_owned()
                        }
                        oxsum_verify::ChargeCheck::NewerSchema { .. } => {
                            " The record is newer than this verifier — the entry is proven, but its arithmetic is skipped rather than guessed at."
                                .to_owned()
                        }
                        oxsum_verify::ChargeCheck::Mismatch => unreachable!("handled above"),
                    } + mandate.as_deref().unwrap_or("");
                    let price = match oxsum_verify::verify_price_head(&inner) {
                        Ok(()) => {
                            " The price it names is bound to a checkpoint the operator signed.".to_owned()
                        }
                        Err(oxsum_verify::PriceError::Absent) => {
                            " This bundle carries no signed price binding — the entry is proven, but its price is not independently attested.".to_owned()
                        }
                        Err(error) => format!(" Its price binding does not verify: {error}."),
                    };
                    Verdict::Pass { charge, price }
                }
            }
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
/// billing — the Leptos chat page's browser logic in vanilla JavaScript,
/// against the same endpoints.
#[route(GET "/chat.js")]
async fn chat_js() -> Result<Js<&'static str>> {
    Ok(Js(CHAT_JS))
}

const CHAT_JS: &str = r#"
(function () {
    const turnsEl = document.getElementById('turns');
    const form = document.getElementById('chat-form');
    const input = document.getElementById('input');
    const modelSelect = document.getElementById('model');
    const topupForm = document.getElementById('topup-form');
    const topupAmount = document.getElementById('topup-amount');
    const errorEl = document.getElementById('chat-error');
    const KEY_STORAGE = 'oxsum-chat-key';
    const HISTORY_STORAGE = 'oxsum-chat-history';
    let turns = [];
    try { turns = JSON.parse(localStorage.getItem(HISTORY_STORAGE) || '[]'); } catch (e) { turns = []; }
    function saveHistory() {
        try { localStorage.setItem(HISTORY_STORAGE, JSON.stringify(turns)); } catch (e) {}
    }
    function showError(message) {
        errorEl.hidden = false;
        errorEl.textContent = message;
    }
    function render() {
        turnsEl.innerHTML = turns.map(function (t) {
            return '<div class="turn"><p><strong>You:</strong> ' + esc(t.user_text) + '</p>' +
                '<p><strong>' + esc(t.model) + ':</strong> ' + esc(t.answer) + '</p>' +
                (t.failed ? '<p class="error">' + esc(t.failed) + '</p>' : '') + '</div>';
        }).join('');
    }
    function esc(s) {
        return String(s == null ? '' : s).replace(/[&<>"']/g, function (m) {
            return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[m];
        });
    }
    async function ensureKey() {
        let key = localStorage.getItem(KEY_STORAGE);
        if (key) return key;
        const response = await fetch('/dashboard/chat/key', { method: 'POST' });
        if (!response.ok) throw new Error('could not mint a chat key');
        const body = await response.json();
        localStorage.setItem(KEY_STORAGE, body.secret);
        return body.secret;
    }
    async function loadModels(key) {
        const response = await fetch('/v1/models', {
            headers: { 'Authorization': 'Bearer ' + key },
        });
        if (response.status === 401) {
            localStorage.removeItem(KEY_STORAGE);
            throw new Error('that key is not valid — the gateway refused it');
        }
        if (!response.ok) throw new Error('the model list failed (' + response.status + ')');
        const body = await response.json();
        const models = (body.data || []).map(function (m) { return m.id; });
        modelSelect.innerHTML = models.map(function (m) {
            return '<option>' + esc(m) + '</option>';
        }).join('') || '<option>No models</option>';
    }
    form.addEventListener('submit', async function (event) {
        event.preventDefault();
        const text = input.value.trim();
        if (!text) return;
        errorEl.hidden = true;
        const turn = { id: Date.now(), model: modelSelect.value, user_text: text, answer: '' };
        turns.push(turn);
        input.value = '';
        render();
        try {
            const key = await ensureKey();
            const requestId = 'chat-' + turn.id;
            const response = await fetch('/v1/chat/completions', {
                method: 'POST',
                headers: {
                    'Authorization': 'Bearer ' + key,
                    'Content-Type': 'application/json',
                    'x-oxsum-request-id': requestId,
                },
                body: JSON.stringify({
                    model: turn.model,
                    messages: [{ role: 'user', content: text }],
                    stream: true,
                }),
            });
            if (!response.ok) throw new Error('the chat request failed (' + response.status + ')');
            const reader = response.body.getReader();
            const decoder = new TextDecoder();
            let buffer = '';
            for (;;) {
                const { done, value } = await reader.read();
                if (done) break;
                buffer += decoder.decode(value, { stream: true });
                const lines = buffer.split('\n');
                buffer = lines.pop();
                for (const line of lines) {
                    const trimmed = line.trim();
                    if (!trimmed.startsWith('data:')) continue;
                    const data = trimmed.slice(5).trim();
                    if (data === '[DONE]') continue;
                    try {
                        const chunk = JSON.parse(data);
                        const delta = chunk.choices && chunk.choices[0] && chunk.choices[0].delta;
                        if (delta && delta.content) turn.answer += delta.content;
                    } catch (e) {}
                }
                render();
            }
        } catch (error) {
            turn.failed = String(error && error.message || error);
            showError(turn.failed);
        }
        saveHistory();
        render();
    });
    topupForm.addEventListener('submit', async function (event) {
        event.preventDefault();
        errorEl.hidden = true;
        try {
            const amount = topupAmount.value.trim();
            const response = await fetch('/api/v1/topups', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    idempotencyKey: 'topup-' + Date.now(),
                    amountMinor: Math.round(parseFloat(amount) * 1e6),
                }),
            });
            if (!response.ok) throw new Error('top-up failed (' + response.status + ')');
            topupAmount.value = '';
        } catch (error) {
            showError(String(error && error.message || error));
        }
    });
    // Live billing: the /ws/billing socket pushes turn events; the browser sends
    // the session cookie on its own.
    try {
        const ws = new WebSocket((location.protocol === 'https:' ? 'wss:' : 'ws:') + '//' + location.host + '/ws/billing');
        ws.onmessage = function () { /* the watch table stays the record; a reload shows it */ };
    } catch (e) {}
    render();
    ensureKey().then(loadModels).catch(showError);
})();
"#;

/// The device grant's approval page: a tool's `userCode` lands here — session-gated
/// inside the page, which sends visitors to `/login` first.
#[page("/device")]
async fn device_page(cx: &Cx) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let query = parse_query_params::<DeviceQuery>(cx).unwrap_or_default();
    let code = query.user_code.unwrap_or_default();
    Ok(view! {
        dashboard_frame(
            title: "Device",
            org_name: org_name,
            user_email: user_email,
            <h1>"Device approval"</h1>
            <form method="post" action="/device">
                <label>
                    "User code"
                    <input type="text" name="user_code" value=(code) placeholder="XXXX-XXXX" required="required" />
                </label>
                <button type="submit" name="approve" value="yes">"Approve"</button>
                <button type="submit" name="approve" value="no">"Deny"</button>
            </form>
        )
    })
}

#[derive(Deserialize, Default)]
struct DeviceQuery {
    user_code: Option<String>,
}

#[derive(Deserialize)]
struct DeviceForm {
    user_code: String,
    approve: String,
}

/// Approves or denies the device request.
#[page(POST "/device")]
async fn device_submit(cx: &Cx, Form(form): Form<DeviceForm>) -> Result<impl View> {
    let sctx = page_ctx(cx).await?;
    let org_name = sctx.principal.organization.name.clone();
    let user_email = sctx.principal.user.email.clone();
    let approve = form.approve == "yes";
    let outcome = sctx
        .db
        .decide_device_request(
            &form.user_code,
            approve,
            sctx.principal.user.id,
            sctx.principal.organization.id,
        )
        .await;
    let message: String = match outcome {
        Ok(_) if approve => "The device request is approved.".to_owned(),
        Ok(_) => "The device request is denied.".to_owned(),
        Err(error) => match error {
            oxsum_core::WalletError::NotFound(message) => message,
            _ => "the decision could not be recorded".to_owned(),
        },
    };
    Ok(view! {
        dashboard_frame(
            title: "Device",
            org_name: org_name,
            user_email: user_email,
            <h1>"Device approval"</h1>
            <p>(message)</p>
            <p><a href="/device">"Another code"</a></p>
        )
    })
}

/// Serves the dashboard stylesheet: the source file, directly. (The Leptos
/// build used to write a processed copy to the site dir; the spike skips the
/// asset pipeline.)
#[route(GET "/style/main.css")]
async fn style() -> Result<Css<&'static str>> {
    Ok(Css(STYLE))
}

const STYLE: &str = include_str!("../style/main.css");
