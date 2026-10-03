//! The bills export: two GET routes that answer with a file, not a page.
//!
//! A page's table can only be read in a browser, and an archive has to be a file the
//! browser saves, so the export is delivered the browser's own way: a `GET` under the
//! page's own path with `Content-Disposition: attachment`, which needs no script on the
//! page and works for `curl` too. A `/_pages` server function would have handed the
//! browser a string to turn into a blob and click itself, and a new `/api/v1` endpoint
//! would have put a second, session-only surface inside the REST contract
//! (docs/decisions.md).
//!
//! The credential is the session cookie — the same one the page reads — so an export is
//! the organization's own records and nothing else, and the refusal for a missing or dead
//! session is `401`, not a redirect: this is a download, and the browser has nowhere to
//! send a reader next.

use axum::extract::State;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use oxsum_web::bills::{BILLS_LIMIT, BillView, csv, json};

use crate::AppState;
use crate::auth::session_cookie_value;

/// The CSV export: the page's rows, header first.
pub async fn export_csv(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match rows(&state, &headers).await {
        Ok(bills) => file(
            csv(&bills),
            "text/csv; charset=utf-8",
            "attachment; filename=\"oxsum-bills.csv\"",
        ),
        Err(refusal) => refusal.into_response(),
    }
}

/// The JSON export: the same rows, the same fields.
pub async fn export_json(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match rows(&state, &headers).await {
        Ok(bills) => match json(&bills) {
            Ok(body) => file(
                body,
                "application/json; charset=utf-8",
                "attachment; filename=\"oxsum-bills.json\"",
            ),
            Err(error) => {
                tracing::error!(%error, "the bills export did not serialize");
                Refusal::internal("the bills export could not be built").into_response()
            }
        },
        Err(refusal) => refusal.into_response(),
    }
}

/// The session's organization's settled entries, as the page's table shows them: the same
/// read and the same limit, so an export cannot list a different set of bills than the
/// page it was downloaded from. The `Err` is why it could not, and what to answer.
async fn rows(state: &AppState, headers: &HeaderMap) -> Result<Vec<BillView>, Refusal> {
    let Some(token) = session_cookie_value(headers) else {
        return Err(Refusal::unauthorized());
    };
    let principal = state
        .db
        .authenticate_session(&token)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the session could not be checked");
            Refusal::internal("the session could not be checked")
        })?
        .ok_or_else(Refusal::unauthorized)?;
    let wallet = state
        .tenants
        .get(&principal.organization.tenant_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the wallet could not be opened");
            Refusal::internal("the wallet could not be opened")
        })?;
    let settled = wallet.settled_entries(BILLS_LIMIT).await.map_err(|error| {
        tracing::error!(%error, "the bills could not be read");
        Refusal::internal("the bills could not be read")
    })?;
    Ok(settled.iter().map(BillView::from).collect())
}

/// Why a download could not be built: a status and one sentence. The `/api/v1` envelope is
/// that surface's, not this one's, and the sentence is all a browser shows of a failed
/// download.
struct Refusal {
    status: StatusCode,
    message: &'static str,
}

impl Refusal {
    /// No usable session. A download has nowhere to redirect a reader to, so it refuses.
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "log in to export bills",
        }
    }

    /// Our own side failed; the sentence names what, and the log keeps the detail.
    fn internal(message: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

/// A file the browser saves: the body plus the two headers a download needs. The filename
/// is ours, never the organization's own name — a caller-supplied name in a header is
/// header injection.
fn file(body: String, content_type: &'static str, disposition: &'static str) -> Response {
    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CONTENT_DISPOSITION, HeaderValue::from_static(disposition));
    response
}
