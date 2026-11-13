//! The Solana deposit rail (issue #1117): a top-up paid in native SOL or a
//! whitelisted SPL token to the deployment's deposit address, credited once
//! the transfer is found and validated on-chain. Issue #1140 widened the rail
//! from one configured asset to a mint whitelist: `sol` and `usdc` are always
//! available, the customer picks one at intent-open, and the intent's `meta`
//! locks that asset's mint, decimals and rate.
//!
//! The flow follows the Solana Pay recipe: `POST /api/v1/topups/solana` opens
//! a `pending` deposit whose `meta` carries a unique `reference` account — a
//! throwaway address that only marks this intent — and the amount of SOL (or
//! SPL token, in its own minor units) the payment must deliver. The caller
//! pays it from any wallet; the rail then either polls `pending` deposits on
//! a periodic pass or answers `POST /api/v1/topups/solana/confirm` with the
//! caller-supplied signature, and each path runs the same verify: locate the
//! transaction that references the intent (`getSignaturesForAddress`), then
//! prove it honors the terms (`getTransaction` — recipient, amount, mint and
//! the `reference` marker itself).
//!
//! The `payment_ref` anchors on the `reference` address, so one intent opens
//! one deposit ever; the located transaction's signature lands in
//! `facilitator_tx_hash` and keys the ledger credit `solana:<signature>`, so
//! a re-delivered poll or a raced confirm credits nothing twice. The
//! `pending → confirmed → credited` walk, the `deposit.credited` event and
//! the `balance.low` latch-clear are the ones every rail shares.
//!
//! Pricing at open (issue #1139): a `sol` intent is oracle-priced — the open
//! samples the deployment's `PriceOracle` (Pyth Hermes in production —
//! `crates/integrations::pyth`) for the `Crypto.SOL/USD` quote, refuses a
//! quote older than `max_quote_age_secs` or with `conf/price` over
//! `max_conf_ratio`, converts the USD figure to `rate_micro` (minor-credits
//! per whole SOL at 1 USD = 1 credit) and locks it into the intent's `meta`
//! with the quote's `price`/`conf`/`publish_time` for audit. `verify` only
//! ever reads the locked `meta`, so a live-rate drift or oracle change cannot
//! retro-price a pending deposit. A USD-pegged asset needs no oracle — its
//! rate is the fixed `USD_PEGGED_RATE_MICRO` conversion. `OXSUM_SOLANA_
//! RATE_MICRO` survives only for the legacy single-asset form and a `sol`
//! whitelist entry behind the explicit devnet opt-in (`SolanaPricing::
//! Static`, wired by `OXSUM_SOLANA_DEVNET_STATIC_RATE=1`) — never a silent
//! mainnet fallback.

use rand::Rng;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::Wallet;
use crate::webhooks::DEPOSIT_CREDITED;
use crate::x402::{RailSettlement, RailVerifier, SettleRefusal, VerifiedPayment};

/// The rail `POST /api/v1/topups/solana` writes under.
pub const RAIL_SOLANA: &str = "solana";

/// The fixed `rate_micro` a USD-pegged asset locks: one whole asset unit (a
/// dollar's worth of USDC) buys one whole credit — `1_000_000` minor units.
/// Every asset with `usd_pegged()` set prices at parity, never an oracle
/// call; volatile assets like SOL price through `SolanaPricing` at open
/// (issue #1139).
pub const USD_PEGGED_RATE_MICRO: i64 = 1_000_000;

/// One Solana deposit-intent `open` may quote an intent this many seconds
/// before the poll expires it (default when the config omits it): Solana
/// Pay's own unbounded-poll guard, so a never-paid intent does not poll
/// forever.
const DEFAULT_INTENT_TTL_SECS: i64 = 600;

/// The SPL Token-2022 program id: a mint whose owner is this program is
/// refused at boot (issue #1140). Token-2022 mints carry extensions —
/// transfer fees, hooks, memo enforcement — the `token_delta` validation does
/// not model, so the whitelist rejects them before they can misprice or
/// mis-credit a deposit.
pub const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// How a `sol` intent prices its `rate_micro` at open (issue #1139). A
/// USD-pegged asset never consults this policy — its rate is the fixed
/// `USD_PEGGED_RATE_MICRO` conversion either way.
#[derive(Clone, Debug)]
pub enum SolanaPricing {
    /// The devnet/test stand-in behind the explicit
    /// `OXSUM_SOLANA_DEVNET_STATIC_RATE=1` opt-in — and the shape the legacy
    /// single-asset form keeps: the intent locks the configured `rate_micro`
    /// verbatim. Never a mainnet default: a `sol` whitelist entry without
    /// this flag and without an oracle bound refuses to open rather than
    /// silently fall back.
    Static,
    /// Oracle-priced: the open samples `PriceOracle::latest` for the
    /// deployment's `feed_id`, refuses a stale or wide-confidence quote, and
    /// converts the USD price to `rate_micro` at 1 USD = 1 credit.
    Oracle {
        /// The price feed the oracle is asked for — `Crypto.SOL/USD` on Pyth
        /// Hermes by default (`OXSUM_SOLANA_PYTH_FEED_ID`).
        feed_id: String,
        /// The oldest a quote's `publish_time` may be (seconds before now)
        /// and still price an intent — `OXSUM_SOLANA_PYTH_MAX_AGE_SECS`,
        /// default 60. Staler refuses the open; the quote is too old to lock.
        max_quote_age_secs: i64,
        /// The largest `conf/price` a quote may carry — `OXSUM_SOLANA_PYTH_
        /// MAX_CONF_RATIO`, default 0.01 (1%). Wider refuses: the oracle's own
        /// uncertainty band is too loose to lock a rate from.
        max_conf_ratio: f64,
    },
}

/// A single oracle price quote, in the shape Pyth Hermes answers: `price` and
/// `conf` are integers scaled by `10^expo`, so `price * 10^expo` is the USD
/// figure and `conf/price` the relative uncertainty. Core keeps the raw
/// integer fields — the conversion and the freshness/confidence gates live
/// here so the integrations client stays a thin parser.
#[derive(Debug, Clone, Copy)]
pub struct OracleQuote {
    /// The quoted price, scaled by `10^expo` (e.g. 20493000000 at expo -8 =
    /// USD 204.93).
    pub price: i64,
    /// The oracle's confidence interval around `price`, same scaling —
    /// `conf/price` is the relative uncertainty the gate checks.
    pub conf: i64,
    /// The base-10 exponent `price`/`conf` are scaled by (typically negative).
    pub expo: i32,
    /// When the oracle published this quote, seconds since the Unix epoch —
    /// the freshness the staleness gate judges.
    pub publish_time: i64,
}

impl OracleQuote {
    /// The quote as minor-credits per whole asset at 1 USD = 1 credit
    /// (`rate_micro`): `price * 10^(expo+6)` computed in integers — a USD
    /// price of `price*10^expo` is `price*10^(expo+6)` minor-credits. `None`
    /// when the quote is non-positive or the scale overflows `i64`.
    #[must_use]
    pub fn to_rate_micro(&self) -> Option<i64> {
        if self.price <= 0 {
            return None;
        }
        let shift = i64::from(self.expo).checked_add(6)?;
        let scaled = if shift >= 0 {
            self.price
                .checked_mul(10_i64.checked_pow(u32::try_from(shift).ok()?)?)?
        } else {
            // Round toward zero — the seller's side: an inexact price floors
            // the rate, never rounds a customer's cost up.
            self.price
                .checked_div(10_i64.checked_pow(u32::try_from(-shift).ok()?)?)?
        };
        if scaled <= 0 {
            return None;
        }
        Some(scaled)
    }

    /// `conf/price` — the relative uncertainty the confidence gate checks.
    /// `f64` is exact enough here: the bound is a ratio comparison, not a
    /// money figure.
    #[must_use]
    pub fn conf_ratio(&self) -> f64 {
        if self.price <= 0 {
            return f64::INFINITY;
        }
        self.conf as f64 / self.price as f64
    }
}

/// The seam the open path asks for a fresh price, mirroring `ChainLookup`'s
/// object-safe shape so core stays HTTP-free and tests stub it in-process.
/// `crates/integrations::pyth::PythClient` is the production impl.
pub trait PriceOracle: std::fmt::Debug + Send + Sync {
    /// The freshest quote the oracle holds for `feed_id` — `None` when the
    /// oracle cannot answer (transport failure, unknown feed). A `None`
    /// answer refuses the open exactly like a stale quote does: a `sol`
    /// intent is priced by the oracle or not opened at all.
    fn latest<'a>(
        &'a self,
        feed_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<OracleQuote>> + Send + 'a>>;
}

/// What an open resolved for the intent's `rate_micro`: the figure plus the
/// provenance `meta` records for audit.
#[derive(Debug, Clone)]
pub enum LockedRate {
    /// A statically-priced rate — the configured devnet figure or a pegged
    /// asset's fixed parity: `meta.rateSource = "config"`.
    Static {
        /// Minor-credits per whole asset unit.
        rate_micro: i64,
    },
    /// An oracle-priced rate: `meta.rateSource = "pyth"` and the quote's own
    /// `price`/`conf`/`expo`/`publish_time` ride in `meta` so the locked
    /// figure can be re-derived and audited against what the oracle answered.
    Oracle {
        /// Minor-credits per whole asset, converted from the quote.
        rate_micro: i64,
        /// The quote the rate was priced from.
        quote: OracleQuote,
    },
}

impl LockedRate {
    /// Minor-credits per whole asset unit — the figure `solLamports` and the
    /// settle-side conversion price at.
    #[must_use]
    pub fn rate_micro(&self) -> i64 {
        match self {
            Self::Static { rate_micro } | Self::Oracle { rate_micro, .. } => *rate_micro,
        }
    }

    /// The `rateSource` `meta` records — `"config"` for a static figure,
    /// `"pyth"` for an oracle-priced one.
    #[must_use]
    pub fn source(&self) -> &'static str {
        match self {
            Self::Static { .. } => "config",
            Self::Oracle { .. } => "pyth",
        }
    }
}

/// One payable asset's terms inside the whitelist (issue #1140): the key the
/// open request names (`"sol"`, `"usdc"`, …), the SPL mint a top-up in it
/// must pay (`None` for native SOL), its base-unit scale, and the rate its
/// intents lock — minor-credits per whole asset, `10^asset_decimals` base
/// units. For a volatile asset (native SOL) `rate_micro` is the static
/// fallback the `SolanaPricing::Static` policy locks; under the oracle policy
/// the open re-prices it through `SolanaRailVerifier::resolve_rate` and locks
/// the quote-derived figure instead (issue #1139).
#[derive(Debug, Clone)]
pub struct AssetTerms {
    /// The asset's lowercase key — the `asset` an open request names and the
    /// `OXSUM_SOLANA_ASSETS` entry / `OXSUM_SOLANA_MINT_<KEY>` suffix.
    pub key: String,
    /// The SPL mint a top-up in this asset must pay, or `None` for native
    /// SOL — a whitelist entry named `sol` always has `None`.
    pub spl_token_mint: Option<String>,
    /// Base units per whole asset (`10^asset_decimals`, 9 for SOL / 6 for
    /// USDC). For an SPL asset the boot reads the figure from the mint's
    /// on-chain account rather than trusting an operator's declaration.
    pub asset_decimals: u32,
    /// Minor-credits per whole asset the intent locks. USD-pegged assets
    /// (every SPL mint on the whitelist that is not a volatile asset) hold
    /// [`USD_PEGGED_RATE_MICRO`]; a volatile asset holds the configured
    /// `OXSUM_SOLANA_RATE_MICRO` the `Static` policy locks — under the oracle
    /// policy the open re-prices it and locks the resolved figure instead
    /// (issue #1139).
    pub rate_micro: i64,
}

impl AssetTerms {
    /// Whether the asset prices at the fixed USD parity — every SPL mint on
    /// the whitelist today: native SOL is the only volatile asset, and
    /// `sol` is the only key with a `None` mint. A non-`sol` key without a
    /// mint is a config error, so this reads off the mint's presence alone.
    #[must_use]
    pub fn usd_pegged(&self) -> bool {
        self.spl_token_mint.is_some()
    }

    /// Whether an intent in this asset resolves its rate through the
    /// deployment's [`SolanaPricing`] policy at open — the inverse of
    /// `usd_pegged`: a volatile asset (native SOL) prices from the oracle or
    /// the devnet static rate, a pegged one never calls the oracle.
    #[must_use]
    pub fn needs_oracle(&self) -> bool {
        self.spl_token_mint.is_none()
    }
}

/// The rail's deployment: everything `verify` needs to decide whether a
/// transaction honors an intent, and everything `open_solana_deposit` needs
/// to write the intent's `meta`. Mirrors `SolanaConfig` in integrations so
/// core stays free of the HTTP type — the server hands the verifier one
/// already built from the environment.
///
/// `assets` is the whitelist `OXSUM_SOLANA_ASSETS` declares, in display
/// order; the flat `spl_token_mint`/`asset_decimals`/`rate_micro` fields
/// mirror the default asset's terms so an older caller (and an intent whose
/// `meta` predates the whitelist) keeps working — `terms_for_asset` is the
/// lookup a new caller takes.
#[derive(Clone)]
pub struct SolanaTerms {
    /// The deposit address every intent pays (`OXSUM_SOLANA_PAY_TO`) — one
    /// vault receives every asset; an SPL top-up lands in its ATA for the
    /// intent's mint.
    pub pay_to: String,
    /// The default asset's SPL mint — `None` when the default is native SOL.
    /// Kept for intents opened before the whitelist existed; new code reads
    /// [`SolanaTerms::terms_for_asset`].
    pub spl_token_mint: Option<String>,
    /// The default asset's base units per whole asset. `rate_micro` is per
    /// `10^asset_decimals` units — the conversion is mint-aware so a
    /// 6-decimal mint does not misprice by 1000× the way a fixed-1e9 SOL
    /// scale would.
    pub asset_decimals: u32,
    /// The default asset's locked rate: minor-credits per whole asset
    /// (`OXSUM_SOLANA_RATE_MICRO` for a volatile default,
    /// [`USD_PEGGED_RATE_MICRO`] for a pegged one).
    pub rate_micro: i64,
    /// The whitelist of payable assets (`OXSUM_SOLANA_ASSETS`, order =
    /// display order). `sol` and `usdc` are always present; `usdt` arrives
    /// with issue #1152. Never empty — `sol` alone is the floor.
    pub assets: Vec<AssetTerms>,
    /// The asset an open request that names no `asset` resolves —
    /// `OXSUM_SOLANA_DEFAULT_ASSET`, or the whitelist's first entry.
    /// `usdc` is the recommended mainnet default: its rate is fixed, so an
    /// intent that did not pick a side cannot be under-priced by SOL's drift.
    pub default_asset: String,
    /// Seconds a `pending` intent stays payable before the poll expires it.
    pub intent_ttl_secs: i64,
    /// The smallest `amount_minor` an intent may open for
    /// (`OXSUM_SOLANA_MIN_MINOR`, default 1).
    pub min_amount_minor: i64,
    /// The largest `amount_minor` an intent may open for
    /// (`OXSUM_SOLANA_MAX_MINOR`, default `i64::MAX` — unbounded).
    pub max_amount_minor: i64,
    /// The most intents one organization may open per minute
    /// (`OXSUM_SOLANA_OPENS_PER_MINUTE`, default 0 — unbounded). A bound
    /// against intent spam, applied per organization.
    pub opens_per_minute: i64,
}

impl SolanaTerms {
    /// The default TTL when a deployment does not state one.
    #[must_use]
    pub fn ttl(&self) -> i64 {
        if self.intent_ttl_secs > 0 {
            self.intent_ttl_secs
        } else {
            DEFAULT_INTENT_TTL_SECS
        }
    }

    /// The asset an open request named — `None` resolves the deployment's
    /// `default_asset` (never user input, so always resolvable); an unknown
    /// key resolves `None` so the route answers 400 rather than silently
    /// pricing a whitelist miss under the default.
    #[must_use]
    pub fn terms_for_asset(&self, key: Option<&str>) -> Option<AssetTerms> {
        let key = key.unwrap_or(&self.default_asset);
        self.assets.iter().find(|asset| asset.key == key).cloned()
    }

    /// Whether `mint` is a mint the whitelist accepts — the defensive half of
    /// verify: a locked intent `meta.splToken` naming a mint no configured
    /// asset carries is refused (issue #1140). `None` — native SOL — is
    /// always acceptable.
    #[must_use]
    pub fn mint_accepted(&self, mint: Option<&str>) -> bool {
        match mint {
            None => true,
            Some(mint) => self
                .assets
                .iter()
                .any(|asset| asset.spl_token_mint.as_deref() == Some(mint)),
        }
    }

    /// Builds the deployment terms around the whitelist — the constructor the
    /// config layer and tests share. `default_asset` falls back to the first
    /// entry when empty; the flat fields mirror the resolved default asset so
    /// pre-whitelist intents keep verifying under them.
    ///
    /// # Panics
    ///
    /// `assets` must not be empty — a rail with nothing payable is a config
    /// error `solana_of` refuses before this is ever built; tests build a
    /// one-asset `sol` whitelist for the legacy shape.
    #[must_use]
    pub fn new(
        pay_to: String,
        mut assets: Vec<AssetTerms>,
        mut default_asset: String,
        intent_ttl_secs: i64,
        min_amount_minor: i64,
        max_amount_minor: i64,
        opens_per_minute: i64,
    ) -> Self {
        assert!(!assets.is_empty(), "a solana rail needs at least one asset");
        if default_asset.is_empty() {
            default_asset = assets[0].key.clone();
        }
        let default = assets
            .iter()
            .find(|asset| asset.key == default_asset)
            .unwrap_or_else(|| {
                panic!("OXSUM_SOLANA_DEFAULT_ASSET {default_asset:?} is not in OXSUM_SOLANA_ASSETS")
            })
            .clone();
        Self {
            pay_to,
            spl_token_mint: default.spl_token_mint.clone(),
            asset_decimals: default.asset_decimals,
            rate_micro: default.rate_micro,
            assets: {
                assets.shrink_to_fit();
                assets
            },
            default_asset,
            intent_ttl_secs,
            min_amount_minor,
            max_amount_minor,
            opens_per_minute,
        }
    }
}

/// What a `pending` solana deposit looks up by its `payment_ref`/`reference`.
/// `(deposit_id, organization_id, amount_minor)` — everything `credit_solana_
/// deposit` needs to resume the walk.
type PendingIntent = (Uuid, Uuid, i64);

/// The rail's verifier: knows how to locate and validate a transfer on the
/// configured RPC endpoint against the deployment's terms. The actual RPC
/// client lives in `crates/integrations::solana` — this struct wraps whatever
/// `ChainLookup` the server built (the real client, or a test's stub), so
/// core never speaks JSON-RPC itself.
///
/// `SolanaTerms` is bundled in so `open`/`credit`/`poll` read their criteria
/// from one place rather than threading `pay_to`/`rate_micro` through every
/// call.
#[derive(Clone)]
pub struct SolanaRailVerifier {
    /// The chain lookup the verifier delegates `find_reference` /
    /// `validate_transfer` to.
    chain: std::sync::Arc<dyn ChainLookup>,
    /// The deployment's acceptance criteria.
    terms: SolanaTerms,
    /// How a volatile (`sol`) intent prices its `rate_micro` at open (issue
    /// #1139) — the static devnet stand-in or the oracle policy. A pegged
    /// intent never consults it.
    pricing: SolanaPricing,
    /// The price oracle the open path samples for a `sol` intent; `None`
    /// under `SolanaPricing::Static` and whenever no oracle was bound — a
    /// `sol` open then refuses rather than falling back (issue #1139).
    oracle: Option<std::sync::Arc<dyn PriceOracle>>,
    /// The sanctions screen the credit path runs on the located transfer's
    /// payer (issue #1144). `None` — the default — screens nothing: a
    /// deployment that never enabled `OXSUM_SOLANA_SANCTIONS_*` credits
    /// exactly as before.
    sanctions: Option<std::sync::Arc<dyn SanctionsScreen>>,
}

impl std::fmt::Debug for SolanaRailVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaRailVerifier")
            .field("pay_to", &self.terms.pay_to)
            .field("default_asset", &self.terms.default_asset)
            .field("pricing", &self.pricing)
            .field(
                "assets",
                &self
                    .terms
                    .assets
                    .iter()
                    .map(|asset| asset.key.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("sanctions", &self.sanctions.is_some())
            .finish_non_exhaustive()
    }
}

/// The seam the verifier talks to: the two calls the Solana Pay recipe needs,
/// expressed on the deployment's configured RPC. `crates/integrations::
/// solana::SolanaRpcClient` is the production impl; tests stub it in-process.
/// Object-safe boxed futures so `Config` can hold `Arc<dyn ChainLookup>`.
pub trait ChainLookup: std::fmt::Debug + Send + Sync {
    /// `getSignaturesForAddress(reference)`: the newest confirmed
    /// transactions touching the intent's marker, newest first — a Vec, not a
    /// single hit, because a later dishonoring transaction on the same marker
    /// must not mask an earlier honoring one (the verifier walks the list and
    /// tries each until one validates). Empty when none has landed yet.
    fn find_reference<'a>(
        &'a self,
        reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>>;

    /// `getTransaction(signature)`: prove the located transaction pays the
    /// `ExpectedTransfer`'s terms — recipient, delivered amount, mint, and
    /// the `reference` marker present. `None` means it does not.
    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>;

    /// A whitelisted SPL mint's on-chain decimals — `getAccountInfo(mint)`
    /// read once per mint at boot so a declared figure that disagrees with
    /// the real mint refuses startup (issue #1129); with the multi-asset
    /// whitelist the figure is authoritative, never operator-declared
    /// (issue #1140). `None` when the lookup cannot answer; never called for
    /// native SOL (there is no mint to read). It is never on the per-request
    /// path.
    fn mint_decimals<'a>(
        &'a self,
        _mint: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<u32>> + Send + 'a>> {
        Box::pin(async { None })
    }

    /// The program that owns the mint account — the Token or Token-2022
    /// program id `getAccountInfo` reports as `owner`. The boot reads it once
    /// per whitelisted SPL mint and refuses a Token-2022 mint: extensions the
    /// rail does not model (transfer fees, hooks, required memos) could
    /// misprice or mis-credit a deposit (issue #1140). `None` when the lookup
    /// cannot answer.
    fn mint_owner<'a>(
        &'a self,
        _mint: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async { None })
    }
}

/// The demand a `pending` intent places on the chain-side transfer, mirroring
/// `integrations::solana::ExpectedTransfer` without importing the HTTP type.
#[derive(Debug, Clone)]
pub struct ExpectedTransfer {
    /// The deposit address the transfer must pay.
    pub recipient: String,
    /// The amount it must deliver (lamports, or the SPL mint's base units).
    pub amount: i64,
    /// The SPL mint it must spend, or `None` for native SOL.
    pub spl_token_mint: Option<String>,
    /// The intent's `reference` marker the transaction must carry.
    pub reference: String,
}

/// A located-and-validated transfer, mirroring `integrations::solana::
/// ValidatedTransfer`.
#[derive(Debug, Clone)]
pub struct ValidatedTransfer {
    /// The transaction's signature.
    pub signature: String,
    /// The amount it delivered, in the asset's own minor units.
    pub delivered: i64,
    /// The on-chain `blockTime` — when the chain recorded the transfer.
    /// Expiry is judged against this, not the server's clock.
    pub block_time: Option<i64>,
    /// The payer's address — the account whose balance was debited to pay the
    /// recipient (issue #1144). `None` when the transaction's shape does not
    /// yield one; the screening seam treats `None` as unscreenable and the
    /// credit path then decides by the deployment's fail policy.
    pub payer: Option<String>,
}

/// The verdict a [`SanctionsScreen`] returns for one payer address (issue
/// #1144). `Clear` credits; `Flagged` parks the deposit for operator review;
/// `TemporarilyUnavailable` is *not* clear — a screening service that cannot
/// answer must not open the gate (deny-until-cleared), so the deposit stays
/// `pending` and the next pass retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenVerdict {
    /// No screen flagged the address.
    Clear,
    /// A screening source named the address: `source` says which (`ofac-sdn`
    /// for the local list, `chainalysis` for the external API), `reason` the
    /// human-readable hit detail a reviewer reads.
    Flagged {
        /// Which screening source produced the hit (`ofac-sdn`,
        /// `chainalysis`).
        source: String,
        /// The hit's detail — the SDN entry, the Chainalysis identifications
        /// count — so an operator reviewing the queue sees why it flagged.
        reason: String,
    },
    /// The external screen could not answer (transport error, non-200,
    /// unreadable body). Deliberately not `Clear`: a deployment that requires
    /// screening keeps the deposit `pending` until a screen answers.
    TemporarilyUnavailable,
}

/// The seam the rail screens a payer address through before crediting (issue
/// #1144): object-safe boxed futures like [`ChainLookup`], so `Config` holds
/// `Arc<dyn SanctionsScreen>` and tests stub it in-process.
/// `crates/integrations::sanctions::CompositeSanctions` is the production
/// impl — the local OFAC list plus the Chainalysis API behind one TTL cache.
pub trait SanctionsScreen: std::fmt::Debug + Send + Sync {
    /// Screens one payer address. Implementations combine their sources
    /// internally; the caller only ever sees one verdict.
    fn screen<'a>(
        &'a self,
        address: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ScreenVerdict> + Send + 'a>>;
}

impl SolanaRailVerifier {
    /// Builds the verifier around its chain lookup and terms — the static-rate
    /// shape, priced from each asset's `rate_micro`. Equivalent to
    /// [`Self::with_pricing`] under `SolanaPricing::Static` and no oracle: the
    /// devnet/test seam and the shape a verifier built before the rail's
    /// oracle cut keeps.
    #[must_use]
    pub fn new(chain: std::sync::Arc<dyn ChainLookup>, terms: SolanaTerms) -> Self {
        Self::with_pricing(chain, terms, SolanaPricing::Static, None)
    }

    /// Builds the verifier with its open-time pricing policy and oracle
    /// (issue #1139): `SolanaPricing::Oracle` plus the `PriceOracle` a `sol`
    /// intent samples, or `SolanaPricing::Static` (the `oracle` is then
    /// unused). Each `AssetTerms::rate_micro` still carries the static figure
    /// the `Static` arm locks and the pegged path always uses.
    #[must_use]
    pub fn with_pricing(
        chain: std::sync::Arc<dyn ChainLookup>,
        terms: SolanaTerms,
        pricing: SolanaPricing,
        oracle: Option<std::sync::Arc<dyn PriceOracle>>,
    ) -> Self {
        Self {
            chain,
            terms,
            pricing,
            oracle,
            sanctions: None,
        }
    }

    /// Attaches the sanctions screen the credit path runs on every located
    /// transfer's payer (issue #1144). `SolanaRailVerifier::new` leaves the
    /// screen off — a deployment that never configures `OXSUM_SOLANA_
    /// SANCTIONS_*` sees no change at all.
    #[must_use]
    pub fn with_sanctions(mut self, sanctions: std::sync::Arc<dyn SanctionsScreen>) -> Self {
        self.sanctions = Some(sanctions);
        self
    }

    /// The deployment's acceptance terms — the `meta` an intent writes.
    #[must_use]
    pub fn terms(&self) -> &SolanaTerms {
        &self.terms
    }

    /// A whitelisted mint's on-chain decimals via the chain lookup — `None`
    /// when the lookup cannot answer. The boot calls this once per
    /// whitelisted SPL mint so a mint's own account, not an operator's
    /// declaration, sets `asset_decimals` (issues #1129, #1140).
    pub async fn mint_decimals(&self, mint: &str) -> Option<u32> {
        self.chain.mint_decimals(mint).await
    }

    /// The program owning a whitelisted mint account — the boot refuses a
    /// Token-2022 mint (issue #1140). `None` when the lookup cannot answer.
    pub async fn mint_owner(&self, mint: &str) -> Option<String> {
        self.chain.mint_owner(mint).await
    }

    /// What an intent opened now locks as `rate_micro` for `asset` — the
    /// whitelist entry the open request named (issue #1139).
    ///
    /// A USD-pegged asset prices statically — the parity `rate_micro` is a
    /// constant of the deployment (a USDC rate does not float against an
    /// oracle), so `LockedRate::Static` answers without an oracle call.
    ///
    /// A volatile (`sol`) asset prices by `self.pricing`: `Static` (the
    /// legacy form or the devnet opt-in) locks its `AssetTerms::rate_micro`;
    /// `Oracle` samples the oracle and gates the quote — refusing `Err`
    /// rather than falling back — when the oracle cannot answer, the freshest
    /// quote is older than `max_quote_age_secs`, or `conf/price` exceeds
    /// `max_conf_ratio`. The quote itself rides in `LockedRate::Oracle` so
    /// the intent's `meta` records what priced it.
    ///
    /// # Errors
    ///
    /// `WalletError::Forbidden` when a `sol` intent has no usable oracle
    /// quote — the open must not mint a rate silently. `WalletError::
    /// Misconfigured` when `pricing` is `Oracle` but no oracle was bound (a
    /// server bug, not a market state).
    pub async fn resolve_rate(&self, asset: &AssetTerms) -> Result<LockedRate, WalletError> {
        // A USD-pegged asset prices at the fixed parity — no oracle is
        // consulted for a token whose rate is a constant of the deployment.
        if asset.usd_pegged() {
            return Ok(LockedRate::Static {
                rate_micro: asset.rate_micro,
            });
        }
        match &self.pricing {
            SolanaPricing::Static => Ok(LockedRate::Static {
                rate_micro: asset.rate_micro,
            }),
            SolanaPricing::Oracle {
                feed_id,
                max_quote_age_secs,
                max_conf_ratio,
            } => {
                let oracle = self.oracle.as_ref().ok_or_else(|| {
                    WalletError::Misconfigured(
                        "the solana rail is oracle-priced but no price oracle is bound".into(),
                    )
                })?;
                let quote = oracle.latest(feed_id).await.ok_or_else(|| {
                    WalletError::Forbidden(
                        "no usable SOL/USD price: the oracle did not answer".into(),
                    )
                })?;
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                let age = now - quote.publish_time;
                if age > *max_quote_age_secs {
                    return Err(WalletError::Forbidden(format!(
                        "the SOL/USD quote is stale: published {age}s ago, over the \
                         {max_quote_age_secs}s bound"
                    )));
                }
                // A quote from the future is a clock-skew symptom; treat it as
                // unusable rather than trusting a publish_time ahead of now.
                if age < -(*max_quote_age_secs) {
                    return Err(WalletError::Forbidden(
                        "the SOL/USD quote's publish_time is in the future".into(),
                    ));
                }
                if quote.conf_ratio() > *max_conf_ratio {
                    return Err(WalletError::Forbidden(format!(
                        "the SOL/USD quote's confidence is too wide: conf/price {:.4} exceeds \
                         the {:.4} bound",
                        quote.conf_ratio(),
                        max_conf_ratio
                    )));
                }
                let rate_micro = quote.to_rate_micro().ok_or_else(|| {
                    WalletError::Forbidden("the SOL/USD quote could not price a rate".into())
                })?;
                Ok(LockedRate::Oracle { rate_micro, quote })
            }
        }
    }

    /// The boot-time mint audit (issues #1129, #1140): every whitelisted SPL
    /// mint is read on-chain once — its `decimals` replaces the config's
    /// placeholder so the mint's own figure prices deposits, and its `owner`
    /// program must be the classic SPL Token program, never Token-2022
    /// (whose transfer-fee and hook extensions the rail does not model). A
    /// native-SOL-only whitelist reads nothing. `Err` names the mint and the
    /// fault — the boot refuses to start rather than misprice or accept an
    /// extension-laden mint. `Ok(false)` would mean a mint read failed and
    /// the caller should refuse too; the function returns `Err` in that case
    /// so there is only one refusal shape.
    pub async fn verify_mints(&mut self) -> Result<(), String> {
        for asset in &mut self.terms.assets {
            let Some(mint) = asset.spl_token_mint.clone() else {
                continue;
            };
            let key = asset.key.clone();
            let owner = self.chain.mint_owner(&mint).await.ok_or_else(|| {
                format!("could not read the {key:?} mint's owner program on-chain")
            })?;
            if owner == TOKEN_2022_PROGRAM_ID {
                return Err(format!(
                    "the {key:?} mint {mint} is owned by Token-2022 — the rail only \
                     accepts classic SPL Token mints (its transfer-fee and hook \
                     extensions are not modelled)"
                ));
            }
            let decimals =
                self.chain.mint_decimals(&mint).await.ok_or_else(|| {
                    format!("could not read the {key:?} mint's decimals on-chain")
                })?;
            // The mint's own figure is authoritative — under the whitelist
            // the spec's 0 placeholder just fills. The legacy single-mint
            // form carries the declared `OXSUM_SOLANA_DECIMALS` here instead,
            // so the same line cross-checks the operator's figure on-chain
            // the way the #1129 boot check did.
            if asset.asset_decimals != 0 && asset.asset_decimals != decimals {
                return Err(format!(
                    "the {key:?} asset declares {} decimals but its mint reports \
                     {decimals} on-chain — refusing to start on a 10^{} mispricing",
                    asset.asset_decimals,
                    (asset.asset_decimals as i64 - decimals as i64).abs()
                ));
            }
            asset.asset_decimals = decimals;
        }
        // The flat fields mirror the default asset — refresh them once the
        // audit filled the real decimals.
        let default = self
            .terms
            .assets
            .iter()
            .find(|asset| asset.key == self.terms.default_asset)
            .cloned()
            .unwrap_or_else(|| self.terms.assets[0].clone());
        self.terms.spl_token_mint = default.spl_token_mint;
        self.terms.asset_decimals = default.asset_decimals;
        self.terms.rate_micro = default.rate_micro;
        Ok(())
    }

    /// The asset figures an opened intent locked in its `meta` — what verify
    /// must honor. `asset` names the whitelist key the intent opened under;
    /// the figures fall back to that asset's *whitelist* terms for any field
    /// the meta did not record, so a deposit opened before a config change —
    /// or before the whitelist existed (no `asset` at all — the deployment's
    /// `default_asset` stands in, and the flat legacy fields keep their
    /// pre-whitelist meaning) — still verifies under what it promised.
    /// `meta` is the deposit's own column, not caller input, so a poisoned
    /// field simply does not override — it was written by
    /// `open_solana_deposit` itself.
    fn locked_asset_terms(&self, meta: &serde_json::Value) -> AssetTerms {
        let get = |key: &str| meta.get(key);
        // The figures the meta omits fall back to the whitelist entry the
        // locked `asset` names — or, for an intent that predates the
        // whitelist, the flat deployment terms (the default asset's figures,
        // which is what the legacy fields held).
        let fallback = meta
            .get("asset")
            .and_then(serde_json::Value::as_str)
            .and_then(|key| self.terms.terms_for_asset(Some(key)))
            .unwrap_or_else(|| AssetTerms {
                key: self.terms.default_asset.clone(),
                spl_token_mint: self.terms.spl_token_mint.clone(),
                asset_decimals: self.terms.asset_decimals,
                rate_micro: self.terms.rate_micro,
            });
        AssetTerms {
            key: get("asset")
                .and_then(serde_json::Value::as_str)
                .map_or(fallback.key, str::to_owned),
            spl_token_mint: match meta.get("splToken") {
                Some(serde_json::Value::Null) => None,
                Some(v) => v.as_str().map(str::to_owned).or(fallback.spl_token_mint),
                None => fallback.spl_token_mint,
            },
            asset_decimals: get("assetDecimals")
                .and_then(serde_json::Value::as_u64)
                .map_or(fallback.asset_decimals, |d| d as u32),
            rate_micro: get("rateMicro")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(fallback.rate_micro),
        }
    }

    /// Verifies a presented intent `payload` against the terms locked in
    /// `meta` — the intent's own promise, not the deployment's live config.
    /// This is what `credit_solana_deposit` calls once it holds the row's
    /// `meta`; a mid-flight `OXSUM_SOLANA_*` change cannot retro-price or
    /// re-target a pending deposit. A `meta.splToken` off the deployment's
    /// whitelist is refused outright — the stored mint was never payable, so
    /// the intent cannot settle (issue #1140, defensive).
    pub async fn verify_locked<'a>(
        &'a self,
        payload: &'a [u8],
        amount_minor: Option<i64>,
        meta: &'a serde_json::Value,
    ) -> Option<VerifiedPayment> {
        let locked = self.locked_asset_terms(meta);
        if !self.terms.mint_accepted(locked.spl_token_mint.as_deref()) {
            return None;
        }
        let pay_to = meta
            .get("payTo")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&self.terms.pay_to)
            .to_owned();
        self.verify_with(payload, amount_minor, &pay_to, &locked)
            .await
    }

    /// The shared verify body: `terms` is either the deployment's default
    /// asset's `AssetTerms` (the `verify` seam, used wherever no locked intent
    /// is in hand) or the intent's own locked asset figures (the credit
    /// path); `pay_to` is the recipient the terms demand.
    async fn verify_with(
        &self,
        payload: &[u8],
        amount_minor: Option<i64>,
        pay_to: &str,
        terms: &AssetTerms,
    ) -> Option<VerifiedPayment> {
        let reference = std::str::from_utf8(payload).ok()?.trim();
        if reference.is_empty() {
            return None;
        }
        let signatures = self.chain.find_reference(reference).await;
        // The intent's declared credit amount is what the deposit opened
        // for — `amount_minor` is the caller's claim, the opened deposit's
        // `amount_minor` is authoritative. Convert it to the base units the
        // payment had to deliver at the locked rate and decimals.
        let opened = amount_minor?;
        let expected = credits_to_asset_units(opened, terms.rate_micro, terms.asset_decimals);
        // Walk every signature the marker indexed, newest first, and take the
        // first that honors the intent — a later failed or dishonoring
        // transaction on the same marker must not stall the earlier paying
        // one.
        let mut validated = None;
        for signature in &signatures {
            validated = self
                .chain
                .validate_transfer(
                    signature,
                    &ExpectedTransfer {
                        recipient: pay_to.to_owned(),
                        amount: expected,
                        spl_token_mint: terms.spl_token_mint.clone(),
                        reference: reference.to_owned(),
                    },
                )
                .await;
            if validated.is_some() {
                break;
            }
        }
        let validated = validated?;
        Some(VerifiedPayment {
            // The credit the ledger books is the opened amount — the payment
            // met its terms. `receivedCredits` converts the delivered asset
            // units back at the locked rate so the deposit records what the
            // chain paid in the ledger's own units; an over/under lands there
            // for a human, never auto-patched.
            amount_minor: opened,
            payment_ref: reference.to_owned(),
            payer: validated.signature.clone(),
            context: json!({
                "signature": validated.signature,
                "delivered": validated.delivered,
                "blockTime": validated.block_time,
                "payerAddress": validated.payer,
                "receivedCredits": asset_units_to_credits(
                    validated.delivered,
                    terms.rate_micro,
                    terms.asset_decimals,
                ),
            }),
        })
    }

    /// Screens the payer address `verify` recorded in the payment's context
    /// (issue #1144). With no screen configured — a deployment that never
    /// enabled `OXSUM_SOLANA_SANCTIONS_*` — the verdict is `Clear` and the
    /// credit path is exactly as before. When screening *is* on but the
    /// transfer's shape yielded no payer, the verdict is
    /// `TemporarilyUnavailable`, never `Clear`: an address the rail cannot
    /// name is an address it cannot clear, and deny-until-cleared refuses to
    /// book unscreened money (the row stays `pending` for an operator, the
    /// same posture an unreachable screening API takes).
    pub async fn screen_payer(&self, payment: &VerifiedPayment) -> ScreenVerdict {
        let Some(sanctions) = &self.sanctions else {
            return ScreenVerdict::Clear;
        };
        let Some(payer) = payment.context["payerAddress"].as_str() else {
            tracing::warn!(
                signature = %payment.context["signature"].as_str().unwrap_or_default(),
                "the validated transfer carried no payer; screening cannot clear it"
            );
            return ScreenVerdict::TemporarilyUnavailable;
        };
        sanctions.screen(payer).await
    }
}

impl RailVerifier for SolanaRailVerifier {
    /// Verifies a presented intent `payload` — the deposit's `reference`
    /// address — by finding the transaction that references it and proving it
    /// pays the intent's terms. `None` when no matching transfer has landed
    /// yet, or when the located transaction does not honor the intent (wrong
    /// recipient, underpayment, wrong mint, missing marker): every refusal is
    /// indistinguishable at the seam, as the `RailVerifier` contract requires.
    fn verify<'a>(
        &'a self,
        payload: &'a [u8],
        amount_minor: Option<i64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<VerifiedPayment>> + Send + 'a>>
    {
        Box::pin(async move {
            // The generic seam verifies under the deployment's live default
            // asset — the credit path overrides with the intent's locked meta
            // via `verify_locked` so a pending deposit can never be
            // retro-priced or re-targeted to another mint.
            let default = self.terms.terms_for_asset(None)?;
            let pay_to = self.terms.pay_to.clone();
            self.verify_with(payload, amount_minor, &pay_to, &default)
                .await
        })
    }

    /// The chain already settled the transfer by the time `verify` finds it,
    /// so `settle` only confirms the signature `verify` recorded — there is
    /// nothing left to move. The `RailSettlement` carries it for the
    /// `facilitator_tx_hash` column, matching the x402 rail's shape.
    fn settle<'a>(
        &'a self,
        payment: &'a VerifiedPayment,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RailSettlement, SettleRefusal>> + Send + 'a>,
    > {
        Box::pin(async move {
            Ok(RailSettlement {
                tx_hash: payment.context["signature"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
    }
}

/// Base units per whole asset the rail prices in: `10^asset_decimals`, capped
/// at `i64::MAX`'s range — a mint's decimals are at most 9 on Solana today,
/// but the bound keeps the scale in `i64` whatever the config says.
fn asset_scale(decimals: u32) -> i64 {
    10_i64.saturating_pow(decimals.min(18))
}

/// Converts a credit amount in minor units to the asset units the payment
/// must deliver at the locked rate, rounding *up* (the customer's direction —
/// a fraction of a base unit charges the buyer the whole unit so the seller
/// is never underpaid). `rate_micro` is minor-credits per whole asset
/// (`10^asset_decimals` base units — 1e9 lamports for SOL, 1e6 for USDC), so
/// `units = ceil(credits_minor * 10^decimals / rate_micro)`. Multiplication
/// precedes division and the product saturates, so a huge intent clamps
/// rather than wrapping negative.
fn credits_to_asset_units(credits_minor: i64, rate_micro: i64, decimals: u32) -> i64 {
    if rate_micro <= 0 {
        return 0;
    }
    // ceil(n / d) for positive integers = (n + d - 1) / d. Scale the credit
    // figure by the mint's units-per-whole first so the ceil lands on base
    // units, not on whole units — a fraction of a unit still charges the next
    // base unit up.
    let numerator = credits_minor.saturating_mul(asset_scale(decimals));
    numerator.saturating_add(rate_micro - 1) / rate_micro
}

/// The inverse of [`credits_to_asset_units`]: the credit minor-units a
/// delivered amount of base units is worth at the locked rate —
/// `received_minor`'s figure. `credits = delivered * rate_micro /
/// 10^decimals`, the remainder dropping toward zero (the customer's
/// direction on the seller's side of the rail).
fn asset_units_to_credits(asset_units: i64, rate_micro: i64, decimals: u32) -> i64 {
    if rate_micro <= 0 {
        return 0;
    }
    asset_units
        .saturating_mul(rate_micro)
        .saturating_div(asset_scale(decimals))
}

/// Generates a fresh `reference` marker: 32 random bytes base58-encoded, the
/// same shape a Solana public key takes — it is only ever a read-only account
/// key on the paying transaction, never a signer, so no keypair is kept.
fn new_reference() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bs58::encode(bytes).into_string()
}

impl Db {
    /// Opens a Solana-rail deposit intent: `pending`, keyed by `payment_ref =
    /// <reference>` — the per-intent marker address the paying transaction
    /// must carry, minted here so the UNIQUE anchor is a value only this
    /// intent could name. `asset` is the whitelist key the customer picked at
    /// open (`None` resolves the deployment's `default_asset`); an unknown
    /// key is `InvalidInput`, never silently priced under another asset
    /// (issue #1140). Returns the `(reference, asset_units, expires_at)` the
    /// caller needs to pay, or `None` on a `payment_ref` collision — the same
    /// replay-safe insert every `open_*` rail makes.
    ///
    /// `meta` records what the intent declares: the `reference`, the `asset`
    /// key, the `payTo` address and `splToken` (null for SOL), the
    /// `solLamports` the payment must deliver at the locked rate, the
    /// `rateSource`/`rateLockedAt` of the lock — plus, for an oracle-priced
    /// intent, the quote's `pythPrice`/`pythConf`/`pythExpo`/`pythPublishTime`
    /// so the locked `rateMicro` can be re-derived and audited — and the
    /// `expiresAt` the poll honors.
    ///
    /// `rate` is the [`LockedRate`] the verifier's `resolve_rate` just priced
    /// for the resolved asset — the caller resolves it first because an
    /// oracle sample must not ride inside the open's insert path (issue
    /// #1139).
    pub async fn open_solana_deposit(
        &self,
        organization_id: Uuid,
        deposit_id: Uuid,
        amount_minor: i64,
        terms: &SolanaTerms,
        asset: Option<&str>,
        rate: &LockedRate,
    ) -> Result<Option<(String, i64, String)>, WalletError> {
        if amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a deposit's amount must be positive".into(),
            ));
        }
        // Per-intent amount bounds (issue #1130): a deployment caps the
        // credit a single intent may open, so a fat-fingered or abusive open
        // cannot demand an unbounded on-chain transfer.
        if amount_minor < terms.min_amount_minor || amount_minor > terms.max_amount_minor {
            return Err(WalletError::InvalidInput(format!(
                "a deposit's amount must be between {} and {} minor-credits",
                terms.min_amount_minor, terms.max_amount_minor
            )));
        }
        // Per-organization open rate (issue #1130): bound how many pending
        // intents one organization may mint inside a minute — intent spam is
        // a marker-address and poll-work exhaustion vector, not free.
        if terms.opens_per_minute > 0 {
            let recent: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM oxsum.deposits \
                 WHERE rail = $1 AND organization_id = $2 \
                   AND created_at > now() - interval '60 seconds'",
            )
            .bind(RAIL_SOLANA)
            .bind(organization_id)
            .fetch_one(self.pool())
            .await?;
            if recent >= terms.opens_per_minute {
                return Err(WalletError::Forbidden(format!(
                    "the organization opened too many solana intents; try again shortly \
                     (limit {}/min)",
                    terms.opens_per_minute
                )));
            }
        }
        // The asset the customer picked — `None` resolves the deployment's
        // default; a key off the whitelist is a 400, never a silent re-price
        // under another asset (issue #1140).
        let asset = terms.terms_for_asset(asset).ok_or_else(|| {
            WalletError::InvalidInput("the asset is not one the solana rail accepts".into())
        })?;
        let reference = new_reference();
        let lamports =
            credits_to_asset_units(amount_minor, rate.rate_micro(), asset.asset_decimals);
        let expires_at = time::OffsetDateTime::now_utc() + time::Duration::seconds(terms.ttl());
        // The quote's own figures ride in meta for an oracle-priced intent so
        // the locked `rateMicro` is auditable against what the oracle answered;
        // a static rate records only its `"config"` source (issue #1139).
        let oracle_fields = match rate {
            LockedRate::Oracle { quote, .. } => json!({
                "pythPrice": quote.price.to_string(),
                "pythConf": quote.conf.to_string(),
                "pythExpo": quote.expo,
                "pythPublishTime": quote.publish_time,
            }),
            LockedRate::Static { .. } => json!({}),
        };
        let mut meta = json!({
            "reference": reference,
            "asset": asset.key,
            "payTo": terms.pay_to,
            "splToken": asset.spl_token_mint,
            "solLamports": lamports,
            "rateMicro": rate.rate_micro(),
            "assetDecimals": asset.asset_decimals,
            "rateSource": rate.source(),
            "rateLockedAt": time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            "expiresAt": expires_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        });
        if let (Some(dst), Some(src)) = (meta.as_object_mut(), oracle_fields.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let inserted = sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status, meta) \
             VALUES ($1, $2, $3, $4, $5, 'pending', $6) \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(deposit_id)
        .bind(organization_id)
        .bind(RAIL_SOLANA)
        .bind(&reference)
        .bind(amount_minor)
        .bind(meta)
        .execute(self.pool())
        .await?
        .rows_affected();
        if inserted == 0 {
            return Ok(None);
        }
        // Answer the stored expiresAt, not a recomputed now+ttl — the figure
        // the poll honors is the one written into meta, so the caller sees
        // the same deadline the rail enforces (issue #1131).
        let stored_expiry = expires_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        Ok(Some((reference, lamports, stored_expiry)))
    }

    /// Finds the pending intent a `reference` names — the lookup the confirm
    /// route runs on a replayed submission, answering the same `(deposit_id,
    /// organization_id, amount_minor)` rather than opening a second intent.
    /// The expiry is deliberately *not* filtered here: it is judged on the
    /// paying transaction's `blockTime` inside `credit_solana_deposit`, so a
    /// payment that landed before `expiresAt` still finds its intent and
    /// credits even when confirmed after the TTL (issue #1128).
    pub async fn find_solana_deposit(
        &self,
        organization_id: Uuid,
        reference: &str,
    ) -> Result<Option<PendingIntent>, WalletError> {
        let row = sqlx::query(
            "SELECT deposit_id, organization_id, amount_minor \
             FROM oxsum.deposits \
             WHERE rail = $1 AND organization_id = $2 AND payment_ref = $3 \
               AND status = 'pending'",
        )
        .bind(RAIL_SOLANA)
        .bind(organization_id)
        .bind(reference)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => Ok(Some((
                row.try_get("deposit_id")?,
                row.try_get("organization_id")?,
                row.try_get("amount_minor")?,
            ))),
            None => Ok(None),
        }
    }

    /// Verifies and credits a `pending` solana deposit: the poll pass and the
    /// confirm route share this walk. The deposit row is taken `FOR UPDATE`,
    /// the `reference` in `meta` drives `verifier.verify`, the located
    /// transaction's signature is recorded in `facilitator_tx_hash`, and the
    /// ledger credit runs under `solana:<signature>` — a replayed poll or a
    /// raced confirm replays the standing receipt rather than crediting
    /// twice. `received_minor` records the asset units the chain delivered
    /// converted back at the locked rate; a mismatch with `amount_minor`
    /// stays for a human.
    ///
    /// Returns the receipt when a credit ran, `None` when the deposit was
    /// already `credited` or is no longer `pending`. A verification failure
    /// (no matching transfer yet, or a dishonoring one) is `Err(Forbidden)` —
    /// the poll surfaces it as "not paid yet" and the confirm route refuses
    /// the submission.
    pub async fn credit_solana_deposit(
        &self,
        wallet: &Wallet,
        deposit_id: Uuid,
        verifier: &SolanaRailVerifier,
        on: time::Date,
    ) -> Result<Option<crate::wallet::Receipt>, WalletError> {
        // Read the intent without a lock first — the chain round-trips the
        // verify needs (findReference + getTransaction) must not run inside a
        // `FOR UPDATE` window, where they would hold the deposit row's lock
        // across two network calls (issue #1128). The verify is pure read; the
        // state is re-checked under a short transaction before anything writes.
        let row = sqlx::query(
            "SELECT organization_id, amount_minor, status, payment_ref, meta \
             FROM oxsum.deposits \
             WHERE deposit_id = $1 AND rail = $2",
        )
        .bind(deposit_id)
        .bind(RAIL_SOLANA)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let organization_id: Uuid = row.try_get("organization_id")?;
        let amount_minor: i64 = row.try_get("amount_minor")?;
        let status: String = row.try_get("status")?;
        let payment_ref: String = row.try_get("payment_ref")?;
        let meta: serde_json::Value = row.try_get("meta")?;
        // Only a `pending` intent proceeds — `confirmed`/`credited` is done,
        // `expired` lapsed. The expiry is *not* gated on server `now()` here:
        // whether the intent lapsed is judged on the paying transaction's own
        // `blockTime` after verify, so a payment that landed on-chain before
        // `expiresAt` still credits even when it is confirmed after.
        if status != "pending" {
            return Ok(None);
        }
        // Verify under the terms this intent locked in `meta` — its own
        // promise, not the deployment's live config, so a mid-flight
        // OXSUM_SOLANA_* change cannot reprice or re-target it (issue #1127).
        let payment = verifier
            .verify_locked(payment_ref.as_bytes(), Some(amount_minor), &meta)
            .await
            .ok_or_else(|| WalletError::Forbidden("the payment is not verified".into()))?;
        // Expiry is the chain's clock, not the server's (issue #1128): a
        // payment is late only when its `blockTime` is at/after the intent's
        // `expiresAt`. A `blockTime` the chain did not return is treated as
        // unverifiable timing and refused — never silently accepted.
        let expires_at = meta["expiresAt"].as_str().and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        });
        let block_time = payment.context["blockTime"].as_i64();
        match (expires_at, block_time) {
            (Some(expiry), Some(bt))
                if time::OffsetDateTime::from_unix_timestamp(bt)
                    .map(|b| b >= expiry)
                    .unwrap_or(true) =>
            {
                return Err(WalletError::Forbidden(
                    "the paying transaction landed after the intent expired".into(),
                ));
            }
            (Some(_), None) => {
                return Err(WalletError::Forbidden(
                    "the chain did not report when the transaction landed".into(),
                ));
            }
            _ => {}
        }
        // Pre-credit sanctions screening on the payer (issue #1144): the
        // transfer's debited address is checked once the payment is otherwise
        // creditable — a late or mistimed transfer is refused above before the
        // screen ever runs, so the review queue only ever parks payments that
        // *would* have credited. `Flagged` parks the row at `flagged` — never
        // credited, never auto-refunded — with `meta.screening` recording the
        // verdict. `TemporarilyUnavailable` leaves the row `pending`: a
        // screening service that cannot answer is not a clear, and
        // deny-until-cleared refuses to book unscreened money, so the next
        // pass retries. The screen runs outside any row lock, the same
        // read-only footing the chain RPCs already take.
        match verifier.screen_payer(&payment).await {
            ScreenVerdict::Clear => {}
            ScreenVerdict::Flagged { source, reason } => {
                self.flag_solana_deposit(deposit_id, &meta, &payment, &source, &reason)
                    .await?;
                // The confirm route reads `Ok(None)` as "nothing credited" —
                // the same shape a deposit that already settled answers,
                // because the caller never learns the screening detail; the
                // operator's queue is where a flag belongs.
                return Ok(None);
            }
            ScreenVerdict::TemporarilyUnavailable => {
                return Err(WalletError::Forbidden(
                    "the payer could not be screened; the deposit stays pending".into(),
                ));
            }
        }
        // Record the located signature before the credit runs — the chain's
        // own idempotency anchor, durable whether the credit lands this pass.
        let settlement = verifier
            .settle(&payment)
            .await
            .map_err(|refusal| WalletError::Forbidden(refusal.reason.clone()))?;
        // The write half: a short transaction re-claims the row FOR UPDATE,
        // re-checks it is still `pending` (a raced poll or confirm may have
        // settled it while the RPC ran), and walks pending → confirmed →
        // credited.
        let mut tx = self.pool().begin().await?;
        let still_pending: bool = sqlx::query_scalar(
            "SELECT status = 'pending' FROM oxsum.deposits \
             WHERE deposit_id = $1 FOR UPDATE",
        )
        .bind(deposit_id)
        .fetch_one(&mut *tx)
        .await?;
        if !still_pending {
            tx.commit().await?;
            return Ok(None);
        }
        // One signature credits one deposit ever: a transaction carrying two
        // references pays once, so a second intent naming an already-used
        // signature must be refused, not credited in a second organization's
        // ledger (issue #1124). The pre-check gives a clean refusal; the
        // `deposits_solana_tx_hash` partial unique index is the backstop for
        // the race where two intents claim the same signature at once.
        // `SELECT EXISTS` answers bool — a bare `SELECT 1` decodes as INT4 and
        // the i64 scalar read it under would 500 the refusal.
        let already_used: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oxsum.deposits \
             WHERE rail = $1 AND facilitator_tx_hash = $2 AND deposit_id <> $3)",
        )
        .bind(RAIL_SOLANA)
        .bind(&settlement.tx_hash)
        .bind(deposit_id)
        .fetch_one(&mut *tx)
        .await?;
        if already_used {
            return Err(WalletError::Forbidden(
                "the paying transaction already credited another deposit".into(),
            ));
        }
        sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'confirmed', facilitator_tx_hash = $1, \
                 received_minor = $2, updated_at = now() \
             WHERE deposit_id = $3 AND status = 'pending'",
        )
        .bind(&settlement.tx_hash)
        .bind(payment.context["receivedCredits"].as_i64())
        .bind(deposit_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            // A unique violation on deposits_solana_tx_hash means a concurrent
            // intent claimed this signature first — a double-spend refusal,
            // not a storage fault.
            if let sqlx::Error::Database(ref db) = e
                && db.code().as_deref() == Some("23505")
            {
                return WalletError::Forbidden(
                    "the paying transaction already credited another deposit".into(),
                );
            }
            WalletError::from(e)
        })?;
        // The credit replays under the signature-derived key: a retried or
        // raced call answers the standing receipt (`is_new: false`), never a
        // second credit.
        let credit_key = format!("solana:{}", settlement.tx_hash);
        let receipt = wallet.top_up(&credit_key, payment.amount_minor, on).await?;
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, \
                 meta = meta || jsonb_build_object('signature', $2::text), \
                 updated_at = now() \
             WHERE deposit_id = $3 AND status <> 'credited'",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(&settlement.tx_hash)
        .bind(deposit_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flipped > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                DEPOSIT_CREDITED,
                json!({
                    "rail": RAIL_SOLANA,
                    "paymentRef": payment_ref,
                    "amountMinor": payment.amount_minor,
                    "signature": settlement.tx_hash,
                    "entryId": receipt.entry_id.as_uuid(),
                }),
            )
            .await?;
            if let Ok(available) = wallet.available().await {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(Some(receipt))
    }

    /// Parks a verified-but-flagged deposit at `flagged` (issue #1144): the
    /// transfer honored the intent's terms but the payer hit a sanctions
    /// screen, so the money is held for the operator queue — never credited,
    /// never auto-refunded. `meta.screening` records what the screen answered
    /// (`verdict`, `source`, `reason`, the screened `address` and `at`, plus
    /// the located `signature`) so a reviewer reads why the row is parked
    /// without re-deriving it; `facilitator_tx_hash` takes the signature too,
    /// so the flag and the double-spend index agree on which transaction is
    /// held. The write is status-guarded `pending`-only — a raced credit that
    /// already landed keeps its row, and a second flagging pass is a no-op.
    async fn flag_solana_deposit(
        &self,
        deposit_id: Uuid,
        meta: &serde_json::Value,
        payment: &crate::x402::VerifiedPayment,
        source: &str,
        reason: &str,
    ) -> Result<(), WalletError> {
        let signature = payment.context["signature"].as_str().unwrap_or_default();
        let payer = payment.context["payerAddress"].as_str().unwrap_or_default();
        let at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let mut flag_meta = meta.clone();
        flag_meta["screening"] = json!({
            "verdict": "flagged",
            "source": source,
            "reason": reason,
            "address": payer,
            "at": at,
        });
        flag_meta["signature"] = json!(signature);
        sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'flagged', facilitator_tx_hash = $1, meta = $2, \
                 updated_at = now() \
             WHERE deposit_id = $3 AND rail = $4 AND status = 'pending'",
        )
        .bind(signature)
        .bind(&flag_meta)
        .bind(deposit_id)
        .bind(RAIL_SOLANA)
        .execute(self.pool())
        .await
        .map_err(|e| {
            // The signature may already sit on another deposit — flagged or
            // credited: one on-chain transaction still pays at most one
            // deposit (issue #1124's rule holds under screening), and a
            // `deposits_solana_tx_hash` violation is the same Forbidden the
            // credit arm raises, never a storage 500.
            if let sqlx::Error::Database(ref db) = e
                && db.code().as_deref() == Some("23505")
            {
                return WalletError::Forbidden(
                    "the paying transaction already settled another deposit".into(),
                );
            }
            WalletError::from(e)
        })?;
        Ok(())
    }

    /// The operator's screening queue (issue #1144): every `flagged` solana
    /// deposit, newest first — the deposits a payer hit parked before credit.
    /// The row shape is the same [`AdminDeposit`] the money-in view lists;
    /// `meta.screening` carries the verdict detail a reviewer reads. #1142's
    /// broader exception queue lands on this same `flagged` status — this is
    /// the deposit-level half, shipped ahead of the queue's filters.
    pub async fn admin_flagged_solana_deposits(
        &self,
        limit: i64,
    ) -> Result<Vec<crate::AdminDeposit>, WalletError> {
        let rows = sqlx::query(
            "SELECT d.deposit_id, d.organization_id, o.name AS organization, d.rail, \
                    d.payment_ref, d.amount_minor, d.received_minor, d.status, \
                    d.entry_id, d.reversal_entry_id, d.reversed_at, d.created_at, \
                    d.meta->>'refundStatus' AS refund_status, \
                    (d.meta->>'disputedAt')::timestamptz AS disputed_at, \
                    d.meta->'screening' AS screening \
             FROM oxsum.deposits d \
             JOIN oxsum.organizations o ON o.organization_id = d.organization_id \
             WHERE d.rail = $1 AND d.status = 'flagged' \
             ORDER BY d.created_at DESC, d.deposit_id DESC \
             LIMIT $2",
        )
        .bind(RAIL_SOLANA)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(crate::AdminDeposit {
                    deposit_id: row.try_get("deposit_id")?,
                    organization_id: row.try_get("organization_id")?,
                    organization: row.try_get("organization")?,
                    rail: row.try_get("rail")?,
                    payment_ref: row.try_get("payment_ref")?,
                    amount_minor: row.try_get("amount_minor")?,
                    received_minor: row.try_get("received_minor")?,
                    status: row.try_get("status")?,
                    entry_id: row.try_get("entry_id")?,
                    reversal_entry_id: row.try_get("reversal_entry_id")?,
                    reversed_at: row.try_get("reversed_at")?,
                    refund_status: row.try_get("refund_status")?,
                    disputed_at: row.try_get("disputed_at")?,
                    screening: row.try_get("screening")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()

    /// The rail's operator-health snapshot (issue #1145): the per-status
    /// deposit counts the `solana_exception_queue_depth{status}` gauge emits,
    /// plus the paid-but-uncredited figure — deposits whose `confirmed` state
    /// means the chain validated a paying transaction yet the ledger credit
    /// never landed (a crash between the `confirmed` and `credited` writes).
    /// The poll emits this every pass so a scrape reads the same view the
    /// pass worked from, not a point-in-time query of its own.
    ///
    /// `(status_counts, paid_uncredited, paid_uncredited_oldest_secs)` —
    /// `paid_uncredited` is the number of `confirmed` rows (the only
    /// "verified on-chain, not credited" state the walk produces),
    /// `paid_uncredited_oldest_secs` the age in seconds since the oldest
    /// one's `updated_at` — the row's own clock, the closest durable figure
    /// to the chain's `blockTime` the table records — `None` when there is
    /// none.
    pub async fn solana_deposit_health(
        &self,
    ) -> Result<(Vec<(String, i64)>, i64, Option<i64>), WalletError> {
        let status_counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT status, count(*) FROM oxsum.deposits \
             WHERE rail = $1 GROUP BY status",
        )
        .bind(RAIL_SOLANA)
        .fetch_all(self.pool())
        .await?;
        let (paid_uncredited, oldest): (i64, Option<i64>) = sqlx::query_as(
            "SELECT count(*), \
                    max(EXTRACT(EPOCH FROM (now() - updated_at)))::bigint \
             FROM oxsum.deposits \
             WHERE rail = $1 AND status = 'confirmed'",
        )
        .bind(RAIL_SOLANA)
        .fetch_one(self.pool())
        .await?;
        Ok((status_counts, paid_uncredited, oldest))
    }

    /// Moves a Solana deposit to `expired` — the poll's outcome once
    /// `meta.expiresAt` has passed without a matching transfer, a no-op on a
    /// deposit that already ended its walk.
    pub async fn expire_solana_deposit(&self, deposit_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits SET status = 'expired', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status IN ('pending', 'confirmed')",
        )
        .bind(deposit_id)
        .bind(RAIL_SOLANA)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The periodic pass: walks every `pending` solana deposit whose
    /// `meta.expiresAt` has not passed, runs `credit_solana_deposit` under a
    /// `FOR UPDATE` claim, and expires the lapsed ones. Unleadered — each row
    /// serializes on its own lock and the `solana:<signature>` credit key
    /// makes a raced pass a replay, never a double credit.
    ///
    /// Returns `(checked, credited, expired)` counts for the job's log line.
    /// The `tenants` map resolves each deposit's organization to its wallet —
    /// the same pattern `recover_x402_credits` uses.
    pub async fn poll_solana_deposits(
        &self,
        verifier: &SolanaRailVerifier,
        tenants: &crate::Tenants,
        on: time::Date,
    ) -> Result<(u64, u64, u64), WalletError> {
        // Fetch the work list first: pending intents not yet expired, oldest
        // first so a backlog drains in order. Expired rows are swept after.
        // Every pending intent is a candidate, including ones past their
        // meta.expiresAt — the expiry verdict belongs to the paying
        // transaction's blockTime inside credit_solana_deposit, not to the
        // server clock here, so a payment that landed on-chain before the
        // TTL still credits on this pass.
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT d.deposit_id, d.organization_id \
             FROM oxsum.deposits d \
             WHERE d.rail = $1 AND d.status = 'pending' \
             ORDER BY d.created_at \
             LIMIT 256",
        )
        .bind(RAIL_SOLANA)
        .fetch_all(self.pool())
        .await?;
        // Expire lapsed intents in one statement — the same status-guarded
        // sweep every `expire_*` variant makes: `WHERE status='pending'` skips
        // a row mid-credit, which lands confirmed/credited under its own lock,
        // so the sweep races a credit safely without holding a lock itself.
        let expired_count = sqlx::query(
            "UPDATE oxsum.deposits SET status = 'expired', updated_at = now() \
             WHERE rail = $1 AND status = 'pending' \
               AND (meta->>'expiresAt')::timestamptz <= now()",
        )
        .bind(RAIL_SOLANA)
        .execute(self.pool())
        .await?
        .rows_affected();

        let mut credited: u64 = 0;
        for (deposit_id, organization_id) in &rows {
            // Resolve the organization to its wallet — same lookup the route
            // layer runs per request.
            let tenant_id: String = sqlx::query_scalar(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
            )
            .bind(organization_id)
            .fetch_one(self.pool())
            .await?;
            let wallet = tenants.get(&tenant_id).await?;
            match self
                .credit_solana_deposit(&wallet, *deposit_id, verifier, on)
                .await
            {
                Ok(Some(_)) => credited += 1,
                Ok(None) => {}
                // A verification refusal is "not paid yet" — the row stays
                // pending for the next pass.
                Err(WalletError::Forbidden(_)) => {}
                // Any other failure logs and moves to the next intent: one
                // bad row must not stall every intent behind it for another
                // whole pass.
                Err(error) => {
                    tracing::warn!(
                        deposit_id = %deposit_id,
                        error = %error,
                        "the solana deposit credit failed; the row stays pending"
                    );
                }
            }
        }
        Ok((rows.len() as u64, credited, expired_count))
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a
    // test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The whitelist every lookup test resolves: SOL (the volatile native
    /// asset, `sol`) plus a 6-decimal USDC mint — the two assets issue #1140
    /// ships.
    fn terms() -> SolanaTerms {
        SolanaTerms::new(
            "payToAddr".to_owned(),
            vec![
                AssetTerms {
                    key: "sol".to_owned(),
                    spl_token_mint: None,
                    asset_decimals: 9,
                    rate_micro: 200_000_000,
                },
                AssetTerms {
                    key: "usdc".to_owned(),
                    spl_token_mint: Some("usdcMint".to_owned()),
                    asset_decimals: 6,
                    rate_micro: USD_PEGGED_RATE_MICRO,
                },
            ],
            "usdc".to_owned(),
            600,
            1,
            i64::MAX,
            0,
        )
    }

    /// The conversion is mint-aware: `rate_micro` minor-credits buy one whole
    /// asset, and the mint's decimals — not a hardcoded 1e9 — set the base
    /// units a whole asset divides into. SOL (9) and USDC (6) price the same
    /// credit figure into very different base units.
    #[test]
    fn credits_to_asset_units_honors_the_mint_decimals() {
        // 1000 minor-credits at 200_000_000 minor/whole unit.
        // SOL (9 decimals): 1000 * 1e9 / 200e6 = 5_000 lamports.
        assert_eq!(credits_to_asset_units(1000, 200_000_000, 9), 5_000);
        // USDC (6 decimals): 1000 * 1e6 / 200e6 = 5 base units. A fixed-1e9
        // scale would have priced this 1000× too high — the bug this fixes.
        assert_eq!(credits_to_asset_units(1000, 200_000_000, 6), 5);
    }

    #[test]
    fn credits_to_asset_units_rounds_up_for_the_seller() {
        // A fraction of a base unit charges the next whole unit up.
        assert_eq!(credits_to_asset_units(1, 200_000_000, 9), 5);
        assert_eq!(credits_to_asset_units(1, 200_000_000, 6), 1);
    }

    #[test]
    fn asset_units_to_credits_is_the_inverse() {
        assert_eq!(asset_units_to_credits(5_000, 200_000_000, 9), 1000);
        assert_eq!(asset_units_to_credits(5, 200_000_000, 6), 1000);
        // The remainder drops toward zero (seller's side of the rail).
        assert_eq!(asset_units_to_credits(4_999, 200_000_000, 9), 999);
    }

    /// The USDC peg: one whole USDC buys exactly one credit, so its rate is
    /// the fixed `USD_PEGGED_RATE_MICRO` whatever `RATE_MICRO` says — the
    /// parity the whitelist locks for a stablecoin (issue #1140).
    #[test]
    fn a_usd_pegged_asset_prices_at_parity() {
        let usdc = terms().terms_for_asset(Some("usdc")).unwrap();
        assert!(usdc.usd_pegged());
        assert_eq!(usdc.rate_micro, USD_PEGGED_RATE_MICRO);
        // 1_000_000 minor-credits (1 credit) needs 1_000_000 base units
        // (1 USDC) — dollar for dollar.
        assert_eq!(
            credits_to_asset_units(1_000_000, usdc.rate_micro, usdc.asset_decimals),
            1_000_000
        );
        // SOL is the volatile side: not pegged, configured rate.
        let sol = terms().terms_for_asset(Some("sol")).unwrap();
        assert!(!sol.usd_pegged());
        assert_eq!(sol.rate_micro, 200_000_000);
    }

    /// The whitelist lookup: `None` resolves the deployment's declared
    /// default (here `usdc`), a known key resolves its entry, an unknown key
    /// is `None` so the route refuses it rather than repricing silently.
    #[test]
    fn terms_for_asset_resolves_named_default_and_refuses_unknown() {
        let terms = terms();
        assert_eq!(terms.terms_for_asset(None).unwrap().key, "usdc");
        assert_eq!(terms.terms_for_asset(Some("sol")).unwrap().key, "sol");
        assert!(
            terms.terms_for_asset(Some("doge")).is_none(),
            "a key off the whitelist is a refusal, not a default"
        );
        // The flat fields mirror the default asset — a pre-whitelist intent
        // verifies against them.
        assert_eq!(terms.spl_token_mint.as_deref(), Some("usdcMint"));
        assert_eq!(terms.asset_decimals, 6);
        assert_eq!(terms.rate_micro, USD_PEGGED_RATE_MICRO);
    }

    /// `mint_accepted`: native SOL always stands; a whitelisted mint stands;
    /// a mint no asset carries is refused — the defensive check that keeps a
    /// stored `meta.splToken` from settling a mint the deployment never
    /// accepted (issue #1140).
    #[test]
    fn mint_accepted_covers_only_the_whitelist() {
        let terms = terms();
        assert!(terms.mint_accepted(None));
        assert!(terms.mint_accepted(Some("usdcMint")));
        assert!(!terms.mint_accepted(Some("someRandomMint")));
    }

    /// An empty default resolves the whitelist's first entry; a default that
    /// is not on the whitelist is a build-time panic — the config refuses it
    /// before this ever runs.
    #[test]
    fn an_empty_default_resolves_the_first_asset() {
        let terms = SolanaTerms::new(
            "payTo".to_owned(),
            vec![AssetTerms {
                key: "sol".to_owned(),
                spl_token_mint: None,
                asset_decimals: 9,
                rate_micro: 200_000_000,
            }],
            String::new(),
            600,
            1,
            i64::MAX,
            0,
        );
        assert_eq!(terms.default_asset, "sol");
        assert_eq!(terms.terms_for_asset(None).unwrap().key, "sol");
    }

    #[test]
    #[should_panic(expected = "OXSUM_SOLANA_DEFAULT_ASSET")]
    fn a_default_off_the_whitelist_is_a_build_error() {
        let _ = SolanaTerms::new(
            "payTo".to_owned(),
            vec![AssetTerms {
                key: "sol".to_owned(),
                spl_token_mint: None,
                asset_decimals: 9,
                rate_micro: 1,
            }],
            "doge".to_owned(),
            600,
            1,
            i64::MAX,
            0,
        );
    }

    /// The locked-meta fallback: an intent carrying `asset` locks that
    /// asset's figures; one that predates the whitelist (no `asset`, only the
    /// flat `splToken`/`assetDecimals`/`rateMicro` fields) falls back to the
    /// deployment's default asset — exactly what those legacy fields held.
    #[test]
    fn locked_asset_terms_reads_the_intents_meta() {
        let terms = terms();
        let verifier = SolanaRailVerifier::new(std::sync::Arc::new(NoChain), terms.clone());
        // A whitelist-era intent: `asset` names the entry, and a meta that
        // omits the figures falls back to the whitelist's, not the caller's.
        let usdc = verifier.locked_asset_terms(&json!({"asset": "usdc"}));
        assert_eq!(usdc.key, "usdc");
        assert_eq!(usdc.spl_token_mint.as_deref(), Some("usdcMint"));
        assert_eq!(usdc.asset_decimals, 6);
        assert_eq!(usdc.rate_micro, USD_PEGGED_RATE_MICRO);
        // The same intent with explicit figures locks them — the promise the
        // open recorded stands over the live whitelist.
        let locked = verifier.locked_asset_terms(&json!({
            "asset": "usdc",
            "splToken": "usdcMint",
            "assetDecimals": 6,
            "rateMicro": 900_000
        }));
        assert_eq!(locked.rate_micro, 900_000);
        // A pre-whitelist intent: no `asset`, flat fields — the deployment's
        // own flat terms (the default asset) fill what the meta omits.
        let legacy = verifier.locked_asset_terms(&json!({
            "splToken": "legacyMint",
            "assetDecimals": 8,
            "rateMicro": 123
        }));
        assert_eq!(legacy.spl_token_mint.as_deref(), Some("legacyMint"));
        assert_eq!(legacy.asset_decimals, 8);
        assert_eq!(legacy.rate_micro, 123);
        // No fields at all: the default asset's terms, untouched.
        let bare = verifier.locked_asset_terms(&json!({}));
        assert_eq!(bare.key, terms.default_asset);
    }

    /// The boot's mint audit: a mint owned by Token-2022 is refused —
    /// extensions the rail does not model must never price a deposit — and a
    /// classic SPL mint's on-chain decimals replace the config placeholder
    /// (issue #1140).
    #[derive(Debug)]
    struct MintAudit {
        owner: Option<String>,
        decimals: Option<u32>,
    }

    impl ChainLookup for MintAudit {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
            Box::pin(async { Vec::new() })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>,
        > {
            Box::pin(async { None })
        }
        fn mint_decimals<'a>(
            &'a self,
            _mint: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<u32>> + Send + 'a>> {
            let decimals = self.decimals;
            Box::pin(async move { decimals })
        }
        fn mint_owner<'a>(
            &'a self,
            _mint: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + 'a>>
        {
            let owner = self.owner.clone();
            Box::pin(async move { owner })
        }
    }

    /// The Token-2022 program id is the audit's refusal key.
    #[test]
    fn token_2022_is_the_refused_owner() {
        assert_eq!(
            TOKEN_2022_PROGRAM_ID, "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
            "the pinned Token-2022 program id"
        );
    }

    #[tokio::test]
    async fn verify_mints_refuses_a_token_2022_mint() {
        let terms = terms();
        let mut verifier = SolanaRailVerifier::new(
            std::sync::Arc::new(MintAudit {
                owner: Some(TOKEN_2022_PROGRAM_ID.to_owned()),
                decimals: Some(6),
            }),
            terms,
        );
        let err = verifier.verify_mints().await.unwrap_err();
        assert!(err.contains("Token-2022"), "{err}");
        assert!(err.contains("usdc"), "{err}");
    }

    #[tokio::test]
    async fn verify_mints_fills_the_onchain_decimals() {
        // The config placeholder (0, or the legacy declared figure) is
        // replaced by the mint's own decimals; the flat fields follow the
        // default asset.
        let mut t = terms();
        t.assets[1].asset_decimals = 0; // the whitelist placeholder
        let mut verifier = SolanaRailVerifier::new(
            std::sync::Arc::new(MintAudit {
                owner: Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".to_owned()),
                decimals: Some(6),
            }),
            t,
        );
        verifier.verify_mints().await.unwrap();
        assert_eq!(verifier.terms().assets[1].asset_decimals, 6);
        assert_eq!(verifier.terms().asset_decimals, 6, "default asset mirrors");
    }

    #[tokio::test]
    async fn verify_mints_refuses_an_unreadable_mint() {
        let mut verifier = SolanaRailVerifier::new(
            std::sync::Arc::new(MintAudit {
                owner: None,
                decimals: None,
            }),
            terms(),
        );
        let err = verifier.verify_mints().await.unwrap_err();
        assert!(err.contains("usdc"), "{err}");
    }

    /// The oracle stub `resolve_rate` tests sample — one scripted quote, or
    /// `None` for "the oracle cannot answer".
    #[derive(Debug)]
    struct StubOracle(Option<OracleQuote>);

    impl PriceOracle for StubOracle {
        fn latest<'a>(
            &'a self,
            _feed_id: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<OracleQuote>> + Send + 'a>>
        {
            let quote = self.0;
            Box::pin(async move { quote })
        }
    }

    /// A fresh USD 204.93/SOL quote (expo -8): 204_930_000 minor-credits per
    /// whole SOL at 1 USD = 1 credit.
    fn fresh_quote() -> OracleQuote {
        OracleQuote {
            price: 20_493_000_000,
            conf: 15_300_000,
            expo: -8,
            publish_time: time::OffsetDateTime::now_utc().unix_timestamp(),
        }
    }

    /// The oracle pricing policy the `resolve_rate` tests carry.
    fn oracle_pricing() -> SolanaPricing {
        SolanaPricing::Oracle {
            feed_id: "feed.sol.usd".to_owned(),
            max_quote_age_secs: 60,
            max_conf_ratio: 0.01,
        }
    }

    /// Issue #1139: a USD-pegged asset answers its fixed parity without ever
    /// sampling the oracle — the policy could be `Oracle` and the call still
    /// never happens.
    #[tokio::test]
    async fn resolve_rate_prices_a_pegged_asset_statically() {
        let usdc = terms().terms_for_asset(Some("usdc")).unwrap();
        let verifier = SolanaRailVerifier::with_pricing(
            std::sync::Arc::new(NoChain),
            terms(),
            oracle_pricing(),
            Some(std::sync::Arc::new(StubOracle(Some(fresh_quote())))),
        );
        let locked = verifier.resolve_rate(&usdc).await.unwrap();
        assert_eq!(locked.rate_micro(), USD_PEGGED_RATE_MICRO);
        assert_eq!(locked.source(), "config");
        // `needs_oracle` is the per-asset gate the route reads.
        assert!(!usdc.needs_oracle());
        assert!(terms().terms_for_asset(Some("sol")).unwrap().needs_oracle());
    }

    /// Issue #1139: under `SolanaPricing::Static` a `sol` intent locks its
    /// `AssetTerms::rate_micro` verbatim — the devnet stand-in the oracle
    /// policy replaces.
    #[tokio::test]
    async fn resolve_rate_static_locks_the_configured_figure() {
        let sol = terms().terms_for_asset(Some("sol")).unwrap();
        let verifier = SolanaRailVerifier::new(std::sync::Arc::new(NoChain), terms());
        let locked = verifier.resolve_rate(&sol).await.unwrap();
        assert_eq!(locked.rate_micro(), 200_000_000);
        assert_eq!(locked.source(), "config");
    }

    /// Issue #1139: under `SolanaPricing::Oracle` a `sol` intent samples the
    /// oracle, converts the quote and carries it in `LockedRate::Oracle` for
    /// the meta to audit.
    #[tokio::test]
    async fn resolve_rate_oracle_converts_the_quote() {
        let sol = terms().terms_for_asset(Some("sol")).unwrap();
        let verifier = SolanaRailVerifier::with_pricing(
            std::sync::Arc::new(NoChain),
            terms(),
            oracle_pricing(),
            Some(std::sync::Arc::new(StubOracle(Some(fresh_quote())))),
        );
        let locked = verifier.resolve_rate(&sol).await.unwrap();
        assert_eq!(locked.rate_micro(), 204_930_000);
        assert_eq!(locked.source(), "pyth");
        match locked {
            LockedRate::Oracle { quote, .. } => assert_eq!(quote.price, 20_493_000_000),
            LockedRate::Static { .. } => panic!("an oracle-priced open carries the quote"),
        }
    }

    /// Issue #1139: the refusal is the point — a stale quote, a
    /// too-wide-confidence quote, an absent answer and an oracle the policy
    /// never bound all refuse `Err`, never fall back to a static figure.
    #[tokio::test]
    async fn resolve_rate_oracle_refuses_stale_wide_and_absent() {
        let sol = terms().terms_for_asset(Some("sol")).unwrap();
        let build = |quote| {
            SolanaRailVerifier::with_pricing(
                std::sync::Arc::new(NoChain),
                terms(),
                oracle_pricing(),
                Some(std::sync::Arc::new(StubOracle(quote))),
            )
        };
        // Stale: published 300 s ago, over the 60 s bound.
        let stale = OracleQuote {
            publish_time: fresh_quote().publish_time - 300,
            ..fresh_quote()
        };
        let err = build(Some(stale)).resolve_rate(&sol).await.unwrap_err();
        assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");
        // Wide: conf/price = 1.0, over the 0.01 bound.
        let wide = OracleQuote {
            conf: 20_493_000_000,
            ..fresh_quote()
        };
        let err = build(Some(wide)).resolve_rate(&sol).await.unwrap_err();
        assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");
        // The oracle answers nothing — refused, never static.
        let err = build(None).resolve_rate(&sol).await.unwrap_err();
        assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");
        // `Oracle` pricing with no oracle bound is a server bug, not a market
        // state: Misconfigured, still never a silent rate.
        let unbound = SolanaRailVerifier::with_pricing(
            std::sync::Arc::new(NoChain),
            terms(),
            oracle_pricing(),
            None,
        );
        let err = unbound.resolve_rate(&sol).await.unwrap_err();
        assert!(matches!(err, WalletError::Misconfigured(_)), "{err:?}");
    }

    /// The chain lookup the verifier tests need when no real lookup matters —
    /// `find_reference`/`validate_transfer` are never called in these cases;
    /// only the trait's presence is required to build a verifier.
    #[derive(Debug)]
    struct NoChain;

    impl ChainLookup for NoChain {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
            Box::pin(async { Vec::new() })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>,
        > {
            Box::pin(async { None })
        }
    }
}
