//! The Solana rail's price oracle (issue #1139): a narrow HTTP client for
//! Pyth Hermes that answers the freshest `Crypto.SOL/USD` quote a native-SOL
//! deposit intent prices against at open.
//!
//! `PythClient` speaks the Hermes REST endpoint
//! `GET {base}/v2/updates/price/latest?ids[]={feed_id}` over `reqwest` — one
//! call, a handful of fields (`price`, `conf`, `expo`, `publish_time`), the
//! same shape the Solana JSON-RPC client keeps: no `pyth-sdk`, because the
//! rail needs exactly one lookup. The response's `parsed` array carries the
//! feed the request named; the quote's integer fields ride out in
//! `oxsum_core::OracleQuote` untouched — freshness, confidence and the
//! USD→`rate_micro` conversion are all core's call, so this client stays a
//! thin parser.
//!
//! `Err` (transport failure, non-2xx, unparseable body) maps to `None` at the
//! `PriceOracle` seam — an unreachable oracle is indistinguishable from "no
//! quote", which the open path refuses rather than falling back to a stale or
//! static figure.

use serde_json::Value;

/// The `Crypto.SOL/USD` feed id a deployment defaults to
/// (`OXSUM_SOLANA_PYTH_FEED_ID` overrides it) — the price feed every native-SOL
/// intent is priced against unless the operator names another.
pub const DEFAULT_SOL_USD_FEED_ID: &str =
    "0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

/// A Hermes transport or parse failure — the call never produced a usable
/// quote. Mapped to `None` at the `PriceOracle` seam, indistinguishable from
/// "the oracle cannot answer".
pub struct PythError(pub String);

impl std::fmt::Debug for PythError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PythError").field("0", &self.0).finish()
    }
}

/// The narrow Hermes client the rail talks to, bound to the deployment's
/// Hermes endpoint (`OXSUM_SOLANA_PYTH_URL`, default
/// `https://hermes.pyth.network`) and the shared HTTP pool.
#[derive(Clone)]
pub struct PythClient {
    base_url: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for PythClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PythClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl PythClient {
    /// Builds the client around the Hermes endpoint and the shared HTTP pool.
    #[must_use]
    pub fn new(base_url: String, http: reqwest::Client) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http,
        }
    }

    /// `GET /v2/updates/price/latest?ids[]={feed_id}` — the freshest quote
    /// Hermes holds for the feed. `Ok(None)` means the response parsed but
    /// carried no `parsed` row for the feed (an unknown id); `Err` means the
    /// call itself failed. The `binary` half of the response is ignored — the
    /// rail wants the readable `parsed` quote, not an on-chain update payload.
    pub async fn latest_price(
        &self,
        feed_id: &str,
    ) -> Result<Option<oxsum_core::OracleQuote>, PythError> {
        let url = format!("{}/v2/updates/price/latest", self.base_url);
        let response = self
            .http
            .get(&url)
            .query(&[("ids[]", feed_id)])
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| PythError(format!("hermes latest price request failed: {e}")))?;
        if !response.status().is_success() {
            return Err(PythError(format!(
                "hermes latest price answered {}",
                response.status()
            )));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|e| PythError(format!("hermes latest price unreadable: {e}")))?;
        parse_latest(&value, feed_id)
    }
}

/// Pulls the `parsed` row for `feed_id` out of a `/v2/updates/price/latest`
/// body and reads its `price`/`conf`/`expo`/`publish_time`. The feed id in
/// the row is the hex string without a `0x` prefix — match either spelling so
/// a configured id with or without the prefix resolves.
fn parse_latest(
    value: &Value,
    feed_id: &str,
) -> Result<Option<oxsum_core::OracleQuote>, PythError> {
    let wanted = feed_id.strip_prefix("0x").unwrap_or(feed_id);
    let rows = value["parsed"].as_array().cloned().unwrap_or_default();
    for row in rows {
        let id = row["id"].as_str().unwrap_or_default();
        if id.strip_prefix("0x").unwrap_or(id) != wanted {
            continue;
        }
        let price = &row["price"];
        let quote = oxsum_core::OracleQuote {
            price: price_i64(&price["price"])
                .ok_or_else(|| PythError("parsed price row has no integer `price`".into()))?,
            conf: price_i64(&price["conf"])
                .ok_or_else(|| PythError("parsed price row has no integer `conf`".into()))?,
            expo: price["expo"]
                .as_i64()
                .and_then(|e| i32::try_from(e).ok())
                .ok_or_else(|| PythError("parsed price row has no `expo`".into()))?,
            publish_time: price_i64(&price["publish_time"])
                .ok_or_else(|| PythError("parsed price row has no `publish_time`".into()))?,
        };
        return Ok(Some(quote));
    }
    Ok(None)
}

/// Reads a Hermes integer that may arrive as a JSON number or a quoted string
/// — Hermes serializes `price`, `conf` and `publish_time` as strings (the
/// i64-safe form), so a bare `as_i64` would miss them. Never through `f64`.
fn price_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
}

impl oxsum_core::PriceOracle for PythClient {
    fn latest<'a>(
        &'a self,
        feed_id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<oxsum_core::OracleQuote>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.latest_price(feed_id)
                .await
                .inspect_err(|e| {
                    // A transport/parse failure is not "no price" — warn and
                    // count it so an unreachable oracle is a loud miss, never
                    // a silent static fallback.
                    tracing::warn!(
                        feed_id,
                        error = %e.0,
                        "pyth latest price failed; treating as no usable quote"
                    );
                    metrics::counter!("solana_pyth_errors_total", "call" => "latest_price")
                        .increment(1);
                })
                .ok()
                .flatten()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `/v2/updates/price/latest` body for the SOL/USD feed — the `parsed`
    /// array's `price` block Hermes answers, with `price`/`conf`/`publish_time`
    /// as strings and `expo` a number.
    fn body(price: &str, conf: &str, expo: i64, publish_time: i64) -> Value {
        serde_json::json!({
            "binary": {"encoding": "hex", "data": ["deadbeef"]},
            "parsed": [
                {
                    "id": "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
                    "price": {
                        "price": price,
                        "conf": conf,
                        "expo": expo,
                        "publish_time": publish_time
                    },
                    "ema_price": {
                        "price": price,
                        "conf": conf,
                        "expo": expo,
                        "publish_time": publish_time
                    }
                }
            ]
        })
    }

    const FEED: &str = DEFAULT_SOL_USD_FEED_ID;

    #[test]
    fn parse_latest_reads_the_price_block() {
        let v = body("20493000000", "15300000", -8, 1_763_000_000);
        let q = parse_latest(&v, FEED)
            .unwrap()
            .expect("the feed is present");
        assert_eq!(q.price, 20_493_000_000);
        assert_eq!(q.conf, 15_300_000);
        assert_eq!(q.expo, -8);
        assert_eq!(q.publish_time, 1_763_000_000);
    }

    #[test]
    fn parse_latest_matches_a_prefixed_feed_id_too() {
        // The row's `id` carries no `0x`; a configured `0x…` feed id must
        // still resolve to it.
        let v = body("1", "1", -8, 0);
        assert!(parse_latest(&v, FEED).unwrap().is_some());
        assert!(
            parse_latest(&v, FEED.strip_prefix("0x").unwrap())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn parse_latest_is_none_for_an_unknown_feed() {
        let v = body("1", "1", -8, 0);
        assert!(parse_latest(&v, "0xdeadbeef").unwrap().is_none());
    }

    #[test]
    fn parse_latest_is_none_without_parsed_rows() {
        let v = serde_json::json!({"binary": {"data": []}, "parsed": []});
        assert!(parse_latest(&v, FEED).unwrap().is_none());
    }

    #[test]
    fn parse_latest_errors_on_a_missing_price_field() {
        let mut v = body("1", "1", -8, 0);
        v["parsed"][0]["price"]["price"] = Value::Null;
        assert!(parse_latest(&v, FEED).is_err());
    }

    /// The quote→rate_micro conversion a SOL open prices at: USD 204.93 at
    /// expo -8 is 204_930_000 minor-credits per whole SOL (1 USD = 1 credit =
    /// 1_000_000 minor).
    #[test]
    fn quote_converts_to_rate_micro() {
        let v = body("20493000000", "0", -8, 0);
        let q = parse_latest(&v, FEED).unwrap().unwrap();
        assert_eq!(q.to_rate_micro(), Some(204_930_000));
    }

    #[test]
    fn quote_rate_micro_rejects_a_nonpositive_price() {
        for bad in ["0", "-1"] {
            let v = body(bad, "0", -8, 0);
            let q = parse_latest(&v, FEED).unwrap().unwrap();
            assert_eq!(q.to_rate_micro(), None, "price {bad}");
        }
    }

    /// conf/price: 15_300_000 / 20_493_000_000 ≈ 0.00075 — inside the 1%
    /// bound; a 1%-of-price conf sits exactly on it, wider refuses.
    #[test]
    fn quote_conf_ratio() {
        let tight = parse_latest(&body("20493000000", "15300000", -8, 0), FEED)
            .unwrap()
            .unwrap();
        assert!(tight.conf_ratio() < 0.01);
        let wide = parse_latest(&body("20493000000", "204930000", -8, 0), FEED)
            .unwrap()
            .unwrap();
        assert!(wide.conf_ratio() >= 0.01 - 1e-9);
        // A zero/negative price is unbounded uncertainty, never 0.
        let zero = parse_latest(&body("0", "1", -8, 0), FEED).unwrap().unwrap();
        assert!(zero.conf_ratio().is_infinite());
    }
}
