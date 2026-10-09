//! The requests page's row shape and its filters.
//!
//! The filters are the page's own URL query string (`?key=<prefix>&model=<name>`): the
//! form writes them, a row's key and model links write them, and the server function reads
//! them — one representation, so a filtered view is a link that survives a reload, and an
//! empty result is an empty table rather than an error.

use serde::{Deserialize, Serialize};

/// The requests page's path: the filter form's action and the base of every link here.
pub const REQUESTS_PATH: &str = "/dashboard/requests";

/// How many requests one page lists (issue #93). The pager walks the rest; the
/// ledger read behind it is bounded either way.
#[cfg(feature = "ssr")]
pub const REQUESTS_LIMIT: usize = 100;

/// One request as the requests page lists it: how the turn was priced, what it used, what
/// it cost, and which key paid it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestView {
    /// The request id from `x-oxsum-request-id`: what a caller quotes for one turn.
    pub request_id: String,
    /// The booking date, `YYYY-MM-DD`: the server's UTC date when the settlement was
    /// written. The ledger records no time of day.
    pub booked_on: String,
    /// The model the caller asked for.
    pub model: String,
    /// The display prefix of the API key that paid the turn — the identity the keys page
    /// shows and the filter matches. `None` for a turn held without key attribution.
    pub key_prefix: Option<String>,
    /// How the turn was priced, in the settlement record's own words (`usage`,
    /// `estimated`, `capped`, `swept`, …).
    pub status: String,
    /// Tokens billed as input, as the settlement recorded them.
    pub input_tokens: i64,
    /// Tokens billed as output, as the settlement recorded them.
    pub output_tokens: i64,
    /// What the turn charged, in minor units (1 credit = 1_000_000), as an integer. The page renders it with the dashboard
    pub cost_minor: i64,
}

#[cfg(feature = "ssr")]
impl RequestView {
    /// One row: a request read from the organization's ledger, plus the key that paid it
    /// when the session may see that key.
    ///
    /// The status is the settlement kind spelled the way the entry's description spells
    /// it, and the money stays an integer in minor units in the payload while the table renders it as credits.
    #[must_use]
    pub fn new(request: &oxsum_core::RequestEntry, key: Option<&oxsum_core::ApiKey>) -> Self {
        Self {
            request_id: request.request_id.clone(),
            booked_on: request.booked_on.to_string(),
            model: request.model.clone(),
            key_prefix: key.map(|key| key.prefix.clone()),
            status: status(request.kind),
            input_tokens: request.input_tokens,
            output_tokens: request.output_tokens,
            cost_minor: request.charged_minor,
        }
    }
}

/// The settlement kind in the record's own words: what `kind` carries in the entry's
/// description, which is also the table docs/user-guide.md documents.
///
/// The match is exhaustive on purpose: a new settlement kind is a compile error here
/// rather than a word the page invents.
#[cfg(feature = "ssr")]
pub(crate) fn status(kind: oxsum_core::SettlementKind) -> String {
    use oxsum_core::SettlementKind;
    match kind {
        SettlementKind::Usage => "usage",
        SettlementKind::Estimated => "estimated",
        SettlementKind::ClientCancelled => "client_cancelled",
        SettlementKind::UpstreamError => "upstream_error",
        SettlementKind::UpstreamUnreachable => "upstream_unreachable",
        SettlementKind::Capped => "capped",
        SettlementKind::Swept => "swept",
        SettlementKind::Unpriced => "unpriced",
        SettlementKind::Released => "released",
    }
    .to_owned()
}

/// The page's filters, as its query string carries them.
///
/// `key` names an API key by its display prefix — the identity the keys page shows — and
/// `model` names a model. An absent or empty value is no filter, so `?key=&model=` is the
/// unfiltered page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestFilters {
    pub key: Option<String>,
    pub model: Option<String>,
}

impl RequestFilters {
    /// The filters a query string carries: raw values, with the empty ones read as no
    /// filter at all.
    #[must_use]
    pub fn new(key: Option<&str>, model: Option<&str>) -> Self {
        Self {
            key: named(key),
            model: named(model),
        }
    }

    /// True when nothing is filtered: the page then lists every request.
    #[must_use]
    pub fn is_unfiltered(&self) -> bool {
        self.key.is_none() && self.model.is_none()
    }

    /// True when `row` passes every filter that is set. A key filter never matches a row
    /// that names no key. The filtering runs where the rows are read, on the server; the
    /// browser only carries the filters.
    #[cfg(feature = "ssr")]
    #[must_use]
    pub fn matches(&self, row: &RequestView) -> bool {
        let key_matches = match (&self.key, &row.key_prefix) {
            (None, _) => true,
            (Some(wanted), Some(prefix)) => wanted == prefix,
            (Some(_), None) => false,
        };
        let model_matches = self
            .model
            .as_ref()
            .is_none_or(|wanted| wanted == &row.model);
        key_matches && model_matches
    }

    /// These filters with the key filter set to `key`, the model filter kept: what a row's
    /// key link carries.
    #[must_use]
    pub fn with_key(&self, key: &str) -> Self {
        Self {
            key: Some(key.to_owned()),
            model: self.model.clone(),
        }
    }

    /// These filters with the model filter set to `model`, the key filter kept: what a
    /// row's model link carries.
    #[must_use]
    pub fn with_model(&self, model: &str) -> Self {
        Self {
            key: self.key.clone(),
            model: Some(model.to_owned()),
        }
    }

    /// These filters as a URL: the page's path plus the query string it reads. The form's
    /// action and every row link come through here, so what the page links is what the
    /// page reads.
    #[must_use]
    pub fn href(&self) -> String {
        let mut query: Vec<String> = Vec::new();
        if let Some(key) = &self.key {
            query.push(format!("key={}", encode(key)));
        }
        if let Some(model) = &self.model {
            query.push(format!("model={}", encode(model)));
        }
        match query.is_empty() {
            true => REQUESTS_PATH.to_owned(),
            false => format!("{REQUESTS_PATH}?{}", query.join("&")),
        }
    }

    /// These filters plus a `before` bound, as the pager's "older" link carries them
    /// (issue #93): the position moves with the filters kept. A filter link drops
    /// `before` instead — a different filtered list has no page to resume — and a
    /// resubmitted form writes `key`/`model` only, so filtering always restarts at
    /// the newest page.
    #[must_use]
    pub fn href_before(&self, before: u64) -> String {
        let href = self.href();
        let join = if href.contains('?') { "&" } else { "?" };
        format!("{href}{join}before={before}")
    }
}

/// One query-string value, or `None` when it is absent or empty.
fn named(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// One filter value, percent-encoded for a query string: a model name or a key prefix
/// that carries a reserved character must not break the link it sits in.
fn encode(value: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    // The query-component set, as the chat page's `/verify` links encode with.
    const QUERY: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}')
        .add(b'&')
        .add(b'=')
        .add(b'+')
        .add(b'%');
    utf8_percent_encode(value, QUERY).to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn row(request_id: &str, model: &str, key_prefix: Option<&str>) -> RequestView {
        RequestView {
            request_id: request_id.to_owned(),
            booked_on: "2026-10-03".to_owned(),
            model: model.to_owned(),
            key_prefix: key_prefix.map(str::to_owned),
            status: "usage".to_owned(),
            input_tokens: 116,
            output_tokens: 100,
            cost_minor: 316,
        }
    }

    /// An absent or empty query value is no filter, so the unfiltered page is what a form
    /// that submitted two empty fields asks for.
    #[test]
    fn an_empty_query_value_is_no_filter() {
        assert!(RequestFilters::new(None, None).is_unfiltered());
        assert!(RequestFilters::new(Some(""), Some("  ")).is_unfiltered());
        assert!(!RequestFilters::new(Some("oxs-1"), None).is_unfiltered());
        assert_eq!(RequestFilters::new(None, None), RequestFilters::default());
    }

    /// The filters are independent and both must pass; a key filter does not match a row
    /// that names no key.
    #[test]
    fn the_filters_match_a_key_and_a_model_together() {
        let billed = row("r1", "mock-a", Some("oxs-aaaa1111"));
        assert!(RequestFilters::new(None, None).matches(&billed));
        assert!(RequestFilters::new(Some("oxs-aaaa1111"), None).matches(&billed));
        assert!(!RequestFilters::new(Some("oxs-bbbb2222"), None).matches(&billed));
        assert!(RequestFilters::new(None, Some("mock-a")).matches(&billed));
        assert!(!RequestFilters::new(None, Some("mock-b")).matches(&billed));
        assert!(RequestFilters::new(Some("oxs-aaaa1111"), Some("mock-a")).matches(&billed));
        // A combination that matches nothing: the row is this key *and* that model.
        assert!(!RequestFilters::new(Some("oxs-aaaa1111"), Some("mock-b")).matches(&billed));
        // A key filter never matches an unattributed row.
        let attributed = row("r2", "mock-a", None);
        assert!(!RequestFilters::new(Some("oxs-aaaa1111"), None).matches(&attributed));
        assert!(RequestFilters::new(None, Some("mock-a")).matches(&attributed));
    }

    /// A row link keeps the other filter, so clicking a key while filtering by a model
    /// narrows rather than discards.
    #[test]
    fn a_row_link_keeps_the_other_filter() {
        let filtered = RequestFilters::new(Some("oxs-1"), None);
        assert_eq!(
            filtered.with_model("mock-a"),
            RequestFilters::new(Some("oxs-1"), Some("mock-a"))
        );
        assert_eq!(
            RequestFilters::new(None, Some("mock-a")).with_key("oxs-1"),
            RequestFilters::new(Some("oxs-1"), Some("mock-a"))
        );
    }

    /// The URL is the page's path plus the filters it reads, encoded: what the form
    /// submits, what a row link carries, and what the server function is handed.
    #[test]
    fn the_href_carries_the_filters_as_the_page_reads_them() {
        assert_eq!(RequestFilters::new(None, None).href(), REQUESTS_PATH);
        assert_eq!(
            RequestFilters::new(Some("oxs-1"), None).href(),
            "/dashboard/requests?key=oxs-1"
        );
        assert_eq!(
            RequestFilters::new(Some("oxs-1"), Some("mock-a")).href(),
            "/dashboard/requests?key=oxs-1&model=mock-a"
        );
        // A model name with a reserved character cannot break the query it sits in. A
        // slash is left readable: it is legal in a query and model names carry it.
        assert_eq!(
            RequestFilters::new(None, Some("a/b&c=d")).href(),
            "/dashboard/requests?model=a/b%26c%3Dd"
        );
        assert_eq!(
            RequestFilters::new(None, Some("100% free")).href(),
            "/dashboard/requests?model=100%25%20free"
        );
    }
}
