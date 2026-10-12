//! The platform-admin pages: `/admin/channels`, `/admin/organizations`,
//! `/admin/in-flight`, `/admin/anomalies`, `/admin/reconciliation`,
//! `/admin/closing`.
//!
//! These are the operator token's surface (`OXSUM_ADMIN_TOKEN`), not a
//! session's: the token lives in the browser's `localStorage` and the pages
//! call `/api/v1/admin/*` directly from the browser — the same endpoints a
//! `curl` drives. The server renders the gate and the page shells; the data
//! arrives with the browser, exactly like the Leptos dashboard.
//!
//! The gate-and-fetch script is served as `/admin.js` (a plain `#[route]`),
//! parameterized by a `data-endpoint` attribute, so no JavaScript passes
//! through the `view!` macro.

use topcoat::Result;
use topcoat::router::content::Js;
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::{RouterBuilder, page, route};
use topcoat::view::{Child, View, component, view};

use crate::views::shell;

/// Registers the admin pages on the router builder.
pub(crate) fn register(builder: RouterBuilder) -> RouterBuilder {
    builder
        .route(admin_root)
        .route(admin_js)
        .page(admin_channels)
        .page(admin_organizations)
        .page(admin_in_flight)
        .page(admin_anomalies)
        .page(admin_reconciliation)
        .page(admin_closing)
}

/// `/admin` redirects to the channels page, as before.
#[route(GET "/admin")]
async fn admin_root() -> Result<SeeOther> {
    Ok(see_other("/admin/channels"))
}

/// The shared admin script: the token gate plus the generic table renderer.
#[route(GET "/admin.js")]
async fn admin_js() -> Result<Js<&'static str>> {
    Ok(Js(ADMIN_JS))
}

const ADMIN_JS: &str = r#"
(function () {
    const script = document.currentScript;
    const endpoint = script.dataset.endpoint;
    const gate = document.getElementById('gate');
    const page = document.getElementById('page');
    const tokenInput = document.getElementById('token');
    const tokenError = document.getElementById('token-error');
    const pageError = document.getElementById('page-error');
    const rows = document.getElementById('rows');
    let token = localStorage.getItem('oxsum.adminToken') || '';
    function esc(s) {
        return String(s).replace(/[&<>"']/g, function (m) {
            return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[m];
        });
    }
    async function api(path, options) {
        const response = await fetch(path, Object.assign({
            headers: { 'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json' },
        }, options || {}));
        if (response.status === 401) {
            localStorage.removeItem('oxsum.adminToken');
            token = '';
            gate.hidden = false;
            page.hidden = true;
            tokenError.hidden = false;
            tokenError.textContent = 'The token was rejected.';
            throw new Error('unauthorized');
        }
        if (!response.ok) throw new Error('request failed: ' + response.status);
        return response.json();
    }
    async function prove(candidate) {
        const response = await fetch('/api/v1/admin/channels', {
            headers: { 'Authorization': 'Bearer ' + candidate },
        });
        return response.ok;
    }
    function render(data) {
        if (Array.isArray(data) && data.length > 0 && typeof data[0] === 'object') {
            const keys = Object.keys(data[0]);
            rows.innerHTML = '<thead><tr>' + keys.map(function (k) {
                return '<th>' + esc(k) + '</th>';
            }).join('') + '</tr></thead><tbody>' + data.map(function (row) {
                return '<tr>' + keys.map(function (k) {
                    const v = row[k];
                    return '<td>' + esc(typeof v === 'object' ? JSON.stringify(v) : v) + '</td>';
                }).join('') + '</tr>';
            }).join('') + '</tbody>';
        } else {
            rows.innerHTML = '<pre>' + esc(JSON.stringify(data, null, 2)) + '</pre>';
        }
    }
    async function load() {
        pageError.hidden = true;
        try {
            render(await api(endpoint));
        } catch (error) {
            if (error.message !== 'unauthorized') {
                pageError.hidden = false;
                pageError.textContent = String(error);
            }
        }
    }
    document.getElementById('token-form').addEventListener('submit', async function (event) {
        event.preventDefault();
        const candidate = tokenInput.value.trim();
        tokenError.hidden = true;
        if (await prove(candidate)) {
            token = candidate;
            localStorage.setItem('oxsum.adminToken', token);
            gate.hidden = true;
            page.hidden = false;
            load();
        } else {
            tokenError.hidden = false;
            tokenError.textContent = 'The token was rejected.';
        }
    });
    if (token) {
        prove(token).then(function (ok) {
            if (ok) { gate.hidden = true; page.hidden = false; load(); }
        });
    }
})();
"#;

/// The shared admin frame: the token gate, the navigation, and the table the
/// script fills.
#[component]
async fn admin_frame(title: &str, child: Child<'_>) -> Result<impl View> {
    Ok(view! {
        shell(
            title: title,
            <header>
                <nav>
                    <a href="/admin/channels">"admin"</a>
                    <ul>
                        <li><a href="/admin/channels">"Channels"</a></li>
                        <li><a href="/admin/organizations">"Organizations"</a></li>
                        <li><a href="/admin/in-flight">"In-flight"</a></li>
                        <li><a href="/admin/anomalies">"Anomalies"</a></li>
                        <li><a href="/admin/reconciliation">"Reconciliation"</a></li>
                        <li><a href="/admin/closing">"Closing"</a></li>
                    </ul>
                </nav>
            </header>
            <main>
                <div id="gate">
                    <form id="token-form" class="card">
                        <h1>"Operator token"</h1>
                        <p class="muted">"This surface takes the deployment's operator token, not a login."</p>
                        <p id="token-error" class="error" hidden="hidden"></p>
                        <label>
                            "Token"
                            <input id="token" type="password" autocomplete="off" required="required" />
                        </label>
                        <button type="submit">"Continue"</button>
                    </form>
                </div>
                <div id="page" hidden="hidden">
                    <h1>(title)</h1>
                    <p id="page-error" class="error" hidden="hidden"></p>
                    <table id="rows"></table>
                </div>
            </main>
            (child)
        )
    })
}

#[page("/admin/channels")]
async fn admin_channels() -> Result<impl View> {
    Ok(view! {
        admin_frame(
            title: "Channels",
            <script src="/admin.js" data-endpoint="/api/v1/admin/channels"></script>
        )
    })
}

#[page("/admin/organizations")]
async fn admin_organizations() -> Result<impl View> {
    Ok(view! {
        admin_frame(
            title: "Organizations",
            <script src="/admin.js" data-endpoint="/api/v1/admin/organizations"></script>
        )
    })
}

#[page("/admin/in-flight")]
async fn admin_in_flight() -> Result<impl View> {
    Ok(view! {
        admin_frame(
            title: "In-flight",
            <script src="/admin.js" data-endpoint="/api/v1/admin/holds"></script>
        )
    })
}

#[page("/admin/anomalies")]
async fn admin_anomalies() -> Result<impl View> {
    Ok(view! {
        admin_frame(
            title: "Anomalies",
            <script src="/admin.js" data-endpoint="/api/v1/admin/anomalies"></script>
        )
    })
}

#[page("/admin/reconciliation")]
async fn admin_reconciliation() -> Result<impl View> {
    Ok(view! {
        admin_frame(
            title: "Reconciliation",
            <script src="/admin.js" data-endpoint="/api/v1/admin/reconciliation"></script>
        )
    })
}

#[page("/admin/closing")]
async fn admin_closing() -> Result<impl View> {
    Ok(view! {
        admin_frame(
            title: "Closing",
            <script src="/admin.js" data-endpoint="/api/v1/admin/closings"></script>
        )
    })
}
