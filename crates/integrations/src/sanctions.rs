//! The Solana rail's pre-credit sanctions screen (issue #1144): the
//! `SanctionsScreen` impl `credit_solana_deposit` runs on a located
//! transfer's payer before the ledger books the deposit.
//!
//! Two sources stand behind one verdict:
//!
//! - **The local OFAC SDN blocklist** — always checked, deterministic, and
//!   free. OFAC publishes crypto addresses on the Specially Designated
//!   Nationals list; [`EMBEDDED_SDN`] carries the Solana entries shipped with
//!   the binary, and `OXSUM_SOLANA_SDN_LIST_PATH` points at a refreshed or
//!   extended file (one base58 address per line, `#` comments) for a
//!   deployment that updates the list out-of-band.
//! - **The Chainalysis free sanctions API** —
//!   `GET https://public.chainalysis.com/api/v1/address/{address}` with the
//!   `X-API-Key` header Chainalysis issues from its public signup
//!   (`OXSUM_SOLANA_SANCTIONS_API_KEY`). A `200` whose `identifications`
//!   array is non-empty flags the address; an empty array clears it; any
//!   transport failure, non-200 or unreadable body is
//!   [`ScreenVerdict::TemporarilyUnavailable`] — never cached as clear.
//!
//! The combination is fail-asymmetric: a local-list hit flags without ever
//! asking the API; a miss still asks, so the API's coverage adds to the
//! list's. Answers sit in an in-memory TTL map keyed by address — flags cache
//! long (`FLAG_TTL`), clears short (`CLEAR_TTL`), and a
//! `TemporarilyUnavailable` is never cached, so the next deposit on the same
//! address retries the API rather than inheriting an outage.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use oxsum_core::{SanctionsScreen, ScreenVerdict};

/// The Chainalysis free sanctions API's address endpoint — `{address}` is the
/// only templated segment. The free tier is a public signup key, deliberately
/// not a paid oracle: the deployment's own operator registers it
/// (https://go.chainalysis.com/crypto-sanctions-screening.html).
const CHAINALYSIS_URL: &str = "https://public.chainalysis.com/api/v1/address";

/// How long a `Flagged` verdict may ride the cache. Flags change slowly —
/// an SDN listing or a Chainalysis identification does not un-list within a
/// deposit's lifetime — so a long TTL is safe and keeps a flagged payer from
/// hammering the API on every poll pass.
const FLAG_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a `Clear` verdict may ride the cache — short, because a listing
/// that lands between two deposits must not be masked by a stale clear. The
/// local list is always re-checked regardless; the TTL only covers the API.
const CLEAR_TTL: Duration = Duration::from_secs(5 * 60);

/// The shipped Solana SDN list: `data/ofac-solana-sdn.txt` embedded in the
/// binary, so a deployment screens against *something* even with no file and
/// no API key. The file documents its own provenance and refresh path
/// (`OXSUM_SOLANA_SDN_LIST_PATH`); `EMBEDDED_SDN_RAW` keeps its exact bytes.
const EMBEDDED_SDN_RAW: &str = include_str!("../data/ofac-solana-sdn.txt");

/// A parsed SDN list: the embedded entries plus, when a path is configured,
/// the file's lines. Matching is exact-string on the base58 address — Solana
/// addresses are case-sensitive, so no normalization is safe.
#[derive(Debug, Default)]
struct SdnList {
    addresses: std::collections::HashSet<String>,
}

impl SdnList {
    /// Parses one list document — the embedded bytes or a file's — into the
    /// set. `#` comments and blank lines parse out; entries match exactly.
    fn parse(raw: &str, addresses: &mut std::collections::HashSet<String>) {
        for line in raw.lines() {
            let entry = line.trim();
            if entry.is_empty() || entry.starts_with('#') {
                continue;
            }
            addresses.insert(entry.to_owned());
        }
    }

    /// The shipped entries plus the file at `path`, when one is configured.
    /// An unreadable file is a hard error — a deployment that named a list
    /// path expects it loaded; silently screening against only the embedded
    /// floor would mask the operator's intent.
    fn load(path: Option<&str>) -> Result<Self, String> {
        let mut addresses = std::collections::HashSet::new();
        Self::parse(EMBEDDED_SDN_RAW, &mut addresses);
        if let Some(path) = path {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| format!("OXSUM_SOLANA_SDN_LIST_PATH {path}: {e}"))?;
            Self::parse(&raw, &mut addresses);
        }
        Ok(Self { addresses })
    }

    fn contains(&self, address: &str) -> bool {
        self.addresses.contains(address)
    }
}

/// One cached verdict and the instant it stops being trusted. A `Flagged`
/// answer caches under [`FLAG_TTL`], a `Clear` under [`CLEAR_TTL`];
/// `TemporarilyUnavailable` is never stored at all.
struct Cached {
    verdict: ScreenVerdict,
    expires: Instant,
}

/// The composite screen [`SolanaRailVerifier`] holds: the local SDN list
/// always, the Chainalysis free API when a key is configured, and a TTL cache
/// in front of the API. Built by [`CompositeSanctions::new`]; the server's
/// `OXSUM_SOLANA_SANCTIONS_*` parse is the only caller.
pub struct CompositeSanctions {
    /// The local list — embedded plus the configured file.
    sdn: SdnList,
    /// The Chainalysis key; `None` runs the local list alone.
    api_key: Option<String>,
    /// The Chainalysis address endpoint — the hosted URL in production,
    /// overridable so a test stands a stub where `public.chainalysis.com`
    /// would be.
    chainalysis_url: String,
    /// The shared HTTP pool, the same `reqwest::Client` every upstream-facing
    /// client clones.
    http: reqwest::Client,
    /// Verdicts keyed by payer address, behind one mutex — the map is small
    /// (one entry per screened payer) and held for nanoseconds, so a `Mutex`
    /// outlives any `DashMap` dependency would.
    cache: Mutex<HashMap<String, Cached>>,
}

impl std::fmt::Debug for CompositeSanctions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeSanctions")
            .field("sdn_entries", &self.sdn.addresses.len())
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl CompositeSanctions {
    /// Builds the composite: the SDN list (embedded + the configured file),
    /// the optional Chainalysis key and the shared HTTP pool.
    ///
    /// # Errors
    ///
    /// `Err` when the configured list path cannot be read — a startup error,
    /// because a deployment that named a file expects it in force.
    pub fn try_new(
        http: reqwest::Client,
        api_key: Option<String>,
        sdn_list_path: Option<&str>,
    ) -> Result<Self, String> {
        Ok(Self {
            sdn: SdnList::load(sdn_list_path)?,
            api_key: api_key.filter(|k| !k.is_empty()),
            chainalysis_url: CHAINALYSIS_URL.to_owned(),
            http,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Overrides the Chainalysis endpoint — a test's seam the way every
    /// upstream URL in this crate is overridable.
    #[cfg(test)]
    fn with_chainalysis_url(mut self, url: String) -> Self {
        self.chainalysis_url = url;
        self
    }

    /// The cache check: `Some(verdict)` for a live entry, `None` for a miss
    /// or an expired one (an expired entry is dropped so the map does not
    /// grow stale rows). `Mutex` poisoning is answered with `expect` — our
    /// own rule: never read a poisoned interior.
    fn cached(&self, address: &str) -> Option<ScreenVerdict> {
        let mut cache = self.cache.lock().expect("sanctions cache mutex poisoned");
        match cache.get(address) {
            Some(entry) if entry.expires > Instant::now() => Some(entry.verdict.clone()),
            Some(_) => {
                cache.remove(address);
                None
            }
            None => None,
        }
    }

    fn store(&self, address: &str, verdict: ScreenVerdict) {
        // A TemporarilyUnavailable is never cached — the next screen retries
        // the source rather than inheriting the outage.
        let ttl = match &verdict {
            ScreenVerdict::Flagged { .. } => FLAG_TTL,
            ScreenVerdict::Clear => CLEAR_TTL,
            ScreenVerdict::TemporarilyUnavailable => return,
        };
        let mut cache = self.cache.lock().expect("sanctions cache mutex poisoned");
        cache.insert(
            address.to_owned(),
            Cached {
                verdict,
                expires: Instant::now() + ttl,
            },
        );
    }

    /// The Chainalysis leg: `200` + a non-empty `identifications` flags, a
    /// `200` with an empty one clears, anything else is
    /// `TemporarilyUnavailable` — a non-200 (rate limit, a bad key's 401, a
    /// 5xx) and a transport error are the same "cannot answer" at the seam.
    async fn chainalysis(&self, address: &str) -> ScreenVerdict {
        let Some(key) = &self.api_key else {
            // No key configured: the API leg simply has nothing to add — the
            // local list's answer stands on its own.
            return ScreenVerdict::Clear;
        };
        let url = format!("{}/{address}", self.chainalysis_url);
        let response = match self
            .http
            .get(&url)
            .header("X-API-Key", key)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "the chainalysis screen failed; answering unavailable");
                metrics::counter!("solana_sanctions_errors_total", "leg" => "chainalysis")
                    .increment(1);
                return ScreenVerdict::TemporarilyUnavailable;
            }
        };
        if !response.status().is_success() {
            tracing::warn!(
                status = %response.status(),
                "the chainalysis screen answered non-200; answering unavailable"
            );
            metrics::counter!("solana_sanctions_errors_total", "leg" => "chainalysis").increment(1);
            return ScreenVerdict::TemporarilyUnavailable;
        }
        let body: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "the chainalysis body was unreadable; answering unavailable");
                metrics::counter!("solana_sanctions_errors_total", "leg" => "chainalysis")
                    .increment(1);
                return ScreenVerdict::TemporarilyUnavailable;
            }
        };
        // { "identifications": [ {...}, ... ] } — non-empty flags; an absent
        // or empty array clears. A body without the field is treated as
        // clear-by-contract (the free API answers the key with an empty list
        // for an unlisted address).
        let identifications = body["identifications"].as_array();
        match identifications {
            Some(list) if !list.is_empty() => ScreenVerdict::Flagged {
                source: "chainalysis".to_owned(),
                reason: format!(
                    "chainalysis listed the address ({} identification{})",
                    list.len(),
                    if list.len() == 1 { "" } else { "s" }
                ),
            },
            _ => ScreenVerdict::Clear,
        }
    }
}

impl SanctionsScreen for CompositeSanctions {
    /// Screens one payer address: the SDN list first (a hit flags without a
    /// network call), then the cache, then Chainalysis. `TemporarilyUnavailable`
    /// surfaces only when the API was configured and could not answer — the
    /// local list's deterministic "not on it" is never downgraded by an API
    /// outage *into* a clear the deployment did not earn, so when the API is
    /// configured its non-answer is the verdict.
    fn screen<'a>(
        &'a self,
        address: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ScreenVerdict> + Send + 'a>> {
        Box::pin(async move {
            // The local list is always in force — a hit flags before any
            // network work, and the check costs nothing.
            if self.sdn.contains(address) {
                return ScreenVerdict::Flagged {
                    source: "ofac-sdn".to_owned(),
                    reason: "the payer is on the OFAC SDN list".to_owned(),
                };
            }
            // A cached verdict stands — flags for a day, clears for the
            // shorter window, unavailables never stored.
            if let Some(verdict) = self.cached(address) {
                return verdict;
            }
            let verdict = self.chainalysis(address).await;
            self.store(address, verdict.clone());
            verdict
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDN list's embedded entries match exactly — Solana addresses are
    /// case-sensitive base58, so no case-folding is safe.
    #[test]
    fn sdn_list_matches_exactly() {
        let list = SdnList {
            addresses: ["SanctionedAddr1111111111111111111111111111111".to_owned()]
                .into_iter()
                .collect(),
        };
        assert!(list.contains("SanctionedAddr1111111111111111111111111111111"));
        assert!(!list.contains("sanctionedaddr1111111111111111111111111111111"));
        assert!(!list.contains("OtherAddr"));
    }

    /// A configured file extends (not replaces) the embedded floor, and `#`
    /// comment / blank lines parse out.
    #[test]
    fn sdn_list_file_extends_the_embedded_floor() {
        let dir = std::env::temp_dir().join(format!("oxsum-sdn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sdn.txt");
        std::fs::write(
            &path,
            "# a comment\nFileListedAddr000000000000000000000000000000\n\n",
        )
        .unwrap();
        let list = SdnList::load(Some(path.to_str().unwrap())).unwrap();
        assert!(list.contains("FileListedAddr000000000000000000000000000000"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// An unreadable configured path is a hard error — never a silent
    /// fallback to the embedded floor.
    #[test]
    fn sdn_list_refuses_a_missing_configured_file() {
        let result = SdnList::load(Some("/nonexistent/ofac-solana-sdn.txt"));
        assert!(result.is_err(), "{result:?}");
    }

    /// The cache keeps a `Flagged` verdict under `FLAG_TTL` semantics — the
    /// stored entry answers for a same-address rescreen without the API.
    #[test]
    fn cache_stores_and_answers_verdicts() {
        let screen = CompositeSanctions::try_new(reqwest::Client::new(), None, None).unwrap();
        screen.store(
            "Addr1",
            ScreenVerdict::Flagged {
                source: "ofac-sdn".into(),
                reason: "test".into(),
            },
        );
        assert_eq!(
            screen.cached("Addr1"),
            Some(ScreenVerdict::Flagged {
                source: "ofac-sdn".into(),
                reason: "test".into()
            })
        );
        // An unavailable is never stored — a retry always re-runs the source.
        screen.store("Addr2", ScreenVerdict::TemporarilyUnavailable);
        assert_eq!(screen.cached("Addr2"), None);
    }

    /// An expired entry reads as a miss and is dropped — the next screen
    /// re-asks its source rather than inheriting a stale answer.
    #[test]
    fn cache_expires_entries() {
        let screen = CompositeSanctions::try_new(reqwest::Client::new(), None, None).unwrap();
        {
            let mut cache = screen.cache.lock().unwrap();
            cache.insert(
                "Addr".to_owned(),
                Cached {
                    verdict: ScreenVerdict::Clear,
                    expires: Instant::now() - Duration::from_secs(1),
                },
            );
        }
        assert_eq!(screen.cached("Addr"), None);
    }

    /// With no API key configured, an unlisted address clears on the local
    /// list alone — no network is touched.
    #[tokio::test]
    async fn screen_clears_an_unlisted_address_without_a_key() {
        let screen = CompositeSanctions::try_new(reqwest::Client::new(), None, None).unwrap();
        let verdict = SanctionsScreen::screen(&screen, "UnlistedPayerAddr").await;
        assert_eq!(verdict, ScreenVerdict::Clear);
    }

    /// An SDN-listed address flags from the local list even with no API key.
    #[tokio::test]
    async fn screen_flags_an_sdn_address_locally() {
        let dir = std::env::temp_dir().join(format!("oxsum-sdn2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sdn.txt");
        std::fs::write(&path, "ListedPayerAddr000000000000000000000000000000\n").unwrap();
        let screen =
            CompositeSanctions::try_new(reqwest::Client::new(), None, Some(path.to_str().unwrap()))
                .unwrap();
        let verdict =
            SanctionsScreen::screen(&screen, "ListedPayerAddr000000000000000000000000000000").await;
        assert!(matches!(verdict, ScreenVerdict::Flagged { .. }));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// A stub Chainalysis endpoint on `127.0.0.1` answering one canned body —
    /// the seam `with_chainalysis_url` opens. Returns the base URL and the
    /// spawned server task.
    async fn stub_api(
        body: serde_json::Value,
        status: axum::http::StatusCode,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new().fallback(axum::routing::get(move || {
            let body = body.clone();
            async move { (status, axum::Json(body)) }
        }));
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v1/address", socket.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            axum::serve(socket, app).await.unwrap();
        });
        (base, handle)
    }

    /// A `200` whose `identifications` is non-empty flags the address.
    #[tokio::test]
    async fn chainalysis_non_empty_identifications_flags() {
        let (base, handle) = stub_api(
            serde_json::json!({"identifications": [{"category": "sanctions", "name": "SDN"}]}),
            axum::http::StatusCode::OK,
        )
        .await;
        let screen =
            CompositeSanctions::try_new(reqwest::Client::new(), Some("test-key".to_owned()), None)
                .unwrap()
                .with_chainalysis_url(base);
        let verdict = SanctionsScreen::screen(&screen, "PayerAddrX").await;
        assert!(
            matches!(verdict, ScreenVerdict::Flagged { ref source, .. } if source == "chainalysis"),
            "{verdict:?}"
        );
        handle.abort();
    }

    /// A `200` with an empty `identifications` clears the address.
    #[tokio::test]
    async fn chainalysis_empty_identifications_clears() {
        let (base, handle) = stub_api(
            serde_json::json!({"identifications": []}),
            axum::http::StatusCode::OK,
        )
        .await;
        let screen =
            CompositeSanctions::try_new(reqwest::Client::new(), Some("test-key".to_owned()), None)
                .unwrap()
                .with_chainalysis_url(base);
        let verdict = SanctionsScreen::screen(&screen, "PayerAddrY").await;
        assert_eq!(verdict, ScreenVerdict::Clear);
        handle.abort();
    }

    /// A non-200 — a bad key's 401, a rate limit, a 5xx — is
    /// `TemporarilyUnavailable`, never a clear: the API was required and could
    /// not answer.
    #[tokio::test]
    async fn chainalysis_non_200_is_unavailable() {
        let (base, handle) = stub_api(
            serde_json::json!({"message": "invalid api key"}),
            axum::http::StatusCode::UNAUTHORIZED,
        )
        .await;
        let screen =
            CompositeSanctions::try_new(reqwest::Client::new(), Some("bad-key".to_owned()), None)
                .unwrap()
                .with_chainalysis_url(base);
        let verdict = SanctionsScreen::screen(&screen, "PayerAddrZ").await;
        assert_eq!(verdict, ScreenVerdict::TemporarilyUnavailable);
        handle.abort();
    }

    /// An unreachable endpoint is `TemporarilyUnavailable` — and, critically,
    /// is *not* cached as a clear: the same address rescreens after the
    /// outage rather than inheriting it.
    #[tokio::test]
    async fn chainalysis_outage_is_unavailable_and_uncached() {
        // Point at a port nothing listens on.
        let screen =
            CompositeSanctions::try_new(reqwest::Client::new(), Some("key".to_owned()), None)
                .unwrap()
                .with_chainalysis_url("http://127.0.0.1:1/api/v1/address".to_owned());
        let verdict = SanctionsScreen::screen(&screen, "PayerAddrW").await;
        assert_eq!(verdict, ScreenVerdict::TemporarilyUnavailable);
        assert_eq!(screen.cached("PayerAddrW"), None);
    }

    /// The deny-until-cleared half: a deployment with the API configured
    /// keeps a clear verdict for only `CLEAR_TTL`, while a flag rides
    /// `FLAG_TTL`. The stored entry's expiry proves which TTL applied.
    #[tokio::test]
    async fn cache_applies_the_verdicts_ttl() {
        let screen = CompositeSanctions::try_new(reqwest::Client::new(), None, None).unwrap();
        screen.store("ClearAddr", ScreenVerdict::Clear);
        screen.store(
            "FlagAddr",
            ScreenVerdict::Flagged {
                source: "test".into(),
                reason: "test".into(),
            },
        );
        let cache = screen.cache.lock().unwrap();
        let clear_ttl = cache["ClearAddr"].expires - Instant::now();
        let flag_ttl = cache["FlagAddr"].expires - Instant::now();
        // Flag outlives clear by orders of magnitude.
        assert!(flag_ttl > clear_ttl, "{flag_ttl:?} vs {clear_ttl:?}");
        assert!(clear_ttl <= CLEAR_TTL);
        assert!(flag_ttl <= FLAG_TTL);
    }
}
