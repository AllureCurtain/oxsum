//! Credential-bearing provider requests use one checked DNS answer set, no
//! environment proxy, no redirects, and no caller-controlled absolute target.
use oxsum_core::ProviderDestinationPolicy;
use reqwest::{Client, Method, RequestBuilder};
use std::collections::VecDeque;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Provider destinations whose TLS 1.3 answers rustls cannot decrypt — the
/// client is capped at TLS 1.2 for them alone (issue #374). Every other
/// destination keeps the default, so a merchant-scoped Airwallex call to
/// `api-demo.airwallex.com` is capped while `api.airwallex.com` and every
/// non-Airwallex provider keep TLS 1.3.
const TLS12_DESTINATIONS: &[&str] = &["api-demo.airwallex.com"];

/// The TLS ceiling a destination's client is built with: 1.2 for the known
/// broken endpoints, the workspace default everywhere else. Matching on the
/// hostname is deliberate — the cap must survive the destination's IP
/// addresses changing under it (the cache holds a client per DNS answer set).
fn tls_cap(host: &str) -> Option<reqwest::tls::Version> {
    TLS12_DESTINATIONS
        .contains(&host)
        .then_some(reqwest::tls::Version::TLS_1_2)
}

/// Error categories contain no provider URL, credential, DNS answer or body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHttpError {
    Blocked,
    Unreachable,
}
impl std::fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Blocked => "provider destination is blocked by policy",
            Self::Unreachable => "provider destination could not be resolved",
        })
    }
}
impl std::error::Error for ProviderHttpError {}

pub type Resolution = Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, ProviderHttpError>> + Send>>;

/// Resolver seam used by deterministic DNS/rebinding tests. Every returned
/// address is validated by ProviderHttp before a credential can be attached.
pub trait ProviderResolver: Send + Sync {
    fn resolve(&self, host: &str, port: u16) -> Resolution;
}
struct SystemResolver;
struct NoUnvalidatedDns;
impl reqwest::dns::Resolve for NoUnvalidatedDns {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async {
            Err(
                Box::new(std::io::Error::other("unvalidated provider DNS refused"))
                    as Box<dyn std::error::Error + Send + Sync>,
            )
        })
    }
}
impl ProviderResolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> Resolution {
        let host = host.to_owned();
        Box::pin(async move {
            let answers = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::lookup_host((host.as_str(), port)),
            )
            .await
            .map_err(|_| ProviderHttpError::Unreachable)?
            .map_err(|_| ProviderHttpError::Unreachable)?;
            let mut ips: Vec<_> = answers.map(|address| address.ip()).collect();
            ips.sort();
            ips.dedup();
            Ok(ips)
        })
    }
}
#[derive(PartialEq, Eq)]
struct Destination {
    host: String,
    addresses: Vec<SocketAddr>,
}
type Clients = Arc<Mutex<VecDeque<(Destination, Client)>>>;

#[derive(Clone)]
pub struct ProviderHttp {
    resolver: Arc<dyn ProviderResolver>,
    clients: Clients,
}
impl std::fmt::Debug for ProviderHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderHttp([destination policy enforced])")
    }
}
impl Default for ProviderHttp {
    fn default() -> Self {
        Self::with_resolver(Arc::new(SystemResolver))
    }
}
impl ProviderHttp {
    pub fn with_resolver(resolver: Arc<dyn ProviderResolver>) -> Self {
        Self {
            resolver,
            clients: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Each segment is encoded as data; provider task IDs/model names cannot
    /// replace the configured origin/path or inject a query. Query parameters
    /// are added separately by the fixed protocol adapter.
    pub async fn request(
        &self,
        method: Method,
        base: &str,
        policy: &ProviderDestinationPolicy,
        segments: &[&str],
    ) -> Result<RequestBuilder, ProviderHttpError> {
        let mut url = policy
            .endpoint(base)
            .map_err(|_| ProviderHttpError::Blocked)?;
        if segments
            .iter()
            .any(|s| *s == "." || *s == ".." || s.len() > 2048)
        {
            return Err(ProviderHttpError::Blocked);
        }
        if !segments.is_empty() {
            url.path_segments_mut()
                .map_err(|_| ProviderHttpError::Blocked)?
                .pop_if_empty()
                .extend(segments.iter().copied());
        }
        let host = url.host().ok_or(ProviderHttpError::Blocked)?;
        let port = url
            .port_or_known_default()
            .ok_or(ProviderHttpError::Blocked)?;
        let ips = match host {
            url::Host::Ipv4(ip) => vec![IpAddr::V4(ip)],
            url::Host::Ipv6(ip) => vec![IpAddr::V6(ip)],
            url::Host::Domain(host) => self.resolver.resolve(host, port).await?,
        };
        policy
            .check_addresses(&ips)
            .map_err(|_| ProviderHttpError::Blocked)?;
        let mut addresses: Vec<_> = ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect();
        addresses.sort();
        addresses.dedup();
        let host = url.host_str().ok_or(ProviderHttpError::Blocked)?.to_owned();
        let destination = Destination { host, addresses };
        let client = self.client(destination)?;
        Ok(client.request(method, url))
    }

    fn client(&self, destination: Destination) -> Result<Client, ProviderHttpError> {
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| ProviderHttpError::Blocked)?;
        if let Some((_, client)) = clients.iter().find(|(d, _)| *d == destination) {
            return Ok(client.clone());
        }
        let client = Self::build(&destination)?;
        if clients.len() >= 128 {
            clients.pop_front();
        }
        clients.push_back((destination, client.clone()));
        Ok(client)
    }

    /// One client per destination: no proxy, no redirects, the validated DNS
    /// answer set pinned, and the destination's TLS ceiling applied. The cap
    /// is per-destination so a broken endpoint never downgrades the rest.
    fn build(destination: &Destination) -> Result<Client, ProviderHttpError> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .connect_timeout(Duration::from_secs(10))
            .dns_resolver(Arc::new(NoUnvalidatedDns))
            .resolve_to_addrs(&destination.host, &destination.addresses);
        if let Some(version) = tls_cap(&destination.host) {
            builder = builder.max_tls_version(version);
        }
        builder.build().map_err(|_| ProviderHttpError::Blocked)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::routing::any;
    use oxsum_core::ProviderNetwork;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Answers {
        calls: AtomicUsize,
        answers: Vec<Vec<IpAddr>>,
    }
    impl ProviderResolver for Answers {
        fn resolve(&self, _host: &str, _port: u16) -> Resolution {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let addresses = self.answers[n.min(self.answers.len() - 1)].clone();
            Box::pin(async move { Ok(addresses) })
        }
    }
    fn internal() -> ProviderDestinationPolicy {
        ProviderDestinationPolicy {
            network: ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }
    }
    type Seen = Arc<Mutex<Vec<(String, HeaderMap)>>>;
    async fn record(State(seen): State<Seen>, uri: Uri, headers: HeaderMap) -> StatusCode {
        seen.lock().unwrap().push((uri.to_string(), headers));
        StatusCode::OK
    }
    async fn server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", socket.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            axum::serve(socket, router).await.unwrap();
        });
        (base, handle)
    }
    #[tokio::test]
    async fn validated_dns_is_pinned_and_later_rebinding_is_refused() {
        let seen: Seen = Arc::default();
        let (base, handle) =
            server(Router::new().fallback(any(record)).with_state(seen.clone())).await;
        let base = base.replace("127.0.0.1", "provider.test");
        let resolver = Arc::new(Answers {
            calls: AtomicUsize::new(0),
            answers: vec![
                vec!["127.0.0.1".parse().unwrap()],
                vec!["169.254.169.254".parse().unwrap()],
            ],
        });
        let http = ProviderHttp::with_resolver(resolver.clone());
        let request = http
            .request(
                Method::GET,
                &format!("{base}/v1"),
                &internal(),
                &["tasks", "a/b?key=evil#fragment"],
            )
            .await
            .unwrap();
        let response = request
            .bearer_auth("private-provider-key")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            1,
            "connector must not resolve twice"
        );
        {
            let records = seen.lock().unwrap();
            assert_eq!(records.len(), 1);
            assert!(
                records[0]
                    .0
                    .starts_with("/v1/tasks/a%2Fb%3Fkey=evil%23fragment")
            );
            assert_eq!(records[0].1["authorization"], "Bearer private-provider-key");
        }
        assert!(matches!(
            http.request(Method::GET, &base, &internal(), &[]).await,
            Err(ProviderHttpError::Blocked)
        ));
        assert_eq!(seen.lock().unwrap().len(), 1);
        handle.abort();
    }
    #[tokio::test]
    async fn redirect_never_forwards_any_protocol_credential() {
        let seen: Seen = Arc::default();
        let (target, target_handle) =
            server(Router::new().fallback(any(record)).with_state(seen.clone())).await;
        let target_url = format!("{target}/stolen");
        let redirect = Router::new().route(
            "/",
            any(move || {
                let target = target_url.clone();
                async move { (StatusCode::TEMPORARY_REDIRECT, [("location", target)]) }
            }),
        );
        let (origin, origin_handle) = server(redirect).await;
        let http = ProviderHttp::default();
        for header in ["authorization", "x-api-key", "x-goog-api-key"] {
            let response = http
                .request(Method::POST, &origin, &internal(), &[])
                .await
                .unwrap()
                .header(header, "provider-secret")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        }
        assert!(seen.lock().unwrap().is_empty());
        origin_handle.abort();
        target_handle.abort();
    }
    #[tokio::test]
    async fn mixed_dns_and_public_to_private_are_blocked_before_send() {
        for addresses in [vec!["8.8.8.8", "127.0.0.1"], vec!["127.0.0.1"]] {
            let http = ProviderHttp::with_resolver(Arc::new(Answers {
                calls: AtomicUsize::new(0),
                answers: vec![addresses.iter().map(|v| v.parse().unwrap()).collect()],
            }));
            assert!(matches!(
                http.request(
                    Method::GET,
                    "https://provider.example/v1",
                    &ProviderDestinationPolicy::default(),
                    &[]
                )
                .await,
                Err(ProviderHttpError::Blocked)
            ));
        }
    }

    /// The TLS-1.2 ceiling is keyed on the destination hostname, so the broken
    /// Airwallex demo endpoint is capped while its production sibling and every
    /// other provider keep the default (TLS 1.3). A merchant-scoped Airwallex
    /// call resolves to the same `api-demo.airwallex.com` destination the env
    /// rail targets — the cap follows the host, not the rail that asked.
    #[tokio::test]
    async fn tls_12_cap_is_scoped_to_the_broken_destination() {
        // The demo endpoint is the only capped destination.
        assert_eq!(
            tls_cap("api-demo.airwallex.com"),
            Some(reqwest::tls::Version::TLS_1_2)
        );
        for host in [
            "api.airwallex.com",
            "api.stripe.com",
            "api.hyperswitch.io",
            "provider.test",
        ] {
            assert_eq!(tls_cap(host), None, "{host} must keep TLS 1.3");
        }
        // A merchant-scoped request to api-demo builds its client under the
        // same pinned-destination seam the env rail's tests exercise — the cap
        // is a transport property of the destination, proven at the one place
        // the client is constructed.
        let destination = Destination {
            host: "api-demo.airwallex.com".to_owned(),
            addresses: vec![std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                443,
            )],
        };
        assert!(
            ProviderHttp::build(&destination).is_ok(),
            "the capped destination still produces a client"
        );
    }
}
