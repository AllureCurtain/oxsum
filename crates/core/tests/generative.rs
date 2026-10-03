//! Randomised wallet operation sequences, checked against an in-memory model.
//!
//! Unit tests check the cases somebody thought of. This drives arbitrary sequences of top-ups,
//! holds, settlements, replays and key collisions through the real ledger on PostgreSQL, and
//! after *every* step asserts what the wallet promises: the available balance is exactly the
//! model's, it never goes negative, a call either happens or changes nothing, a replay is a
//! replay, and every entry the case wrote still proves.
//!
//! Each sequence comes from a proptest seed, so a failure reproduces exactly and shrinks to the
//! shortest sequence that still fails. `PROPTEST_CASES` sets how many sequences run (1000 by
//! default, which is what TODO.md asks CI for).
//!
//! Four things are pinned here on purpose, because they are decisions rather than accidents:
//!
//! - **A settlement releases at most what is held in total.** The wallet's limit refuses a
//!   pending credit the outstanding holds cannot cover, so a settlement naming an amount that was
//!   never held — or naming a hold it has already released, with nothing else covering it — is
//!   `INSUFFICIENT_FUNDS`. The model predicts that from the one number the rule is stated in, the
//!   reserved total, and the generator is free to settle any earlier op's amount, hold or not: the
//!   restriction that it only settle holds the ledger actually took is gone with the fix (#6).
//!   The pairing is aggregate (docs/decisions.md), so a settlement *may* release more than the
//!   hold it names while other holds cover the total; tracking the total rather than per-hold
//!   bookkeeping is what makes the model exact there rather than merely safe.
//! - **A settlement within the reserved total cannot fail for lack of funds.** It charges
//!   `actual` out of settled money and releases `held` of reservation, and `held >= actual`, so
//!   the available balance grows or stays put. The model therefore expects exactly two refusals:
//!   an `actual` outside `0..=held`, and a release beyond the reserved total.
//! - **A call's own argument is bounded before the ledger is consulted.** An `actual` outside
//!   `0..=held` is invalid input even when the key already holds an entry, so replaying a refused
//!   settlement stays invalid input instead of becoming a conflict. The model reads it in that
//!   order because the wallet does; a version that read the key first mispredicted exactly that
//!   replay, once a collision had written under the key.
//! - **A settlement releases the hold the ledger holds**, read when the call arrives rather than
//!   carried in the request. A hold refused for insufficient funds leaves its key free, so a
//!   collision can write a hold there later; a settlement that was refused before that hold
//!   existed, replayed afterwards, then releases *that* hold in full. The module used to subtract
//!   the placeholder it had recorded in the request instead — a counterexample that took CI down
//!   (issue #41, pinned by `a_settlement_releases_the_hold_the_ledger_holds`). For the same
//!   reason the placeholder is not part of the request's identity: the wallet compares the hold,
//!   the actual and the description, so the model compares those three and nothing else.
//! - **A key reused with different content is refused**, and the model only asserts that: that it
//!   changes nothing and reports an error. The domain layer maps the engine's refusal to
//!   `CONFLICT`, so the refusal arrives as a conflict, but the model reads all three classes the
//!   same way and does not depend on that mapping beyond it not being a storage failure.
//! - **The cases share eight ledgers rather than creating one each.** Creating a ledger is a DDL
//!   migration (an extension plus eleven tables); a thousand of them would spend the whole run on
//!   schema creation and leave a thousand schemas behind. Each case reads its tenant's settled and
//!   reserved totals and its log size and works from there, so what a case asserts is absolute
//!   rather than a delta, and the leftover state of a shared ledger cancels out of both sides.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::OnceLock;

use oxsum_core::{
    EntryId, Hash, Receipt, Tenants, Wallet, WalletError, settlement_key_for, verify_bundle,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use sqlx::postgres::PgPoolOptions;
use time::Date;
use time::macros::date;

/// The single posting date the generator uses. Periods, seals and their boundaries have their own
/// tests; what is under test here is the sequence of calls.
const DAY: Date = date!(2026 - 10 - 01);

/// One credit, in minor units.
const ONE: i64 = 1_000_000;

/// How many ledgers the cases share, round-robin.
const LEDGERS: usize = 8;

// ── what proptest generates ──────────────────────────────────────────────────

/// Raw material for one op.
///
/// It is resolved positionally into an [`Op`], which is what makes a back-reference safe: a
/// back-reference can only ever name an index that already exists.
#[derive(Debug, Clone)]
enum Raw {
    TopUp(i64),
    Hold(i64),
    Settle { back: u8, mode: Mode },
    Replay { back: u8 },
    Collide { back: u8, up: bool },
}

/// How much of a hold a generated settlement charges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The whole hold: the call cost exactly what was reserved for it.
    Full,
    /// Nothing: the call failed, or was cancelled before it cost anything.
    Zero,
    /// Half the hold: a partial charge, the difference returns to the user.
    Half,
    /// One minor unit more than was held: the wallet must refuse it.
    Over,
    /// An arbitrary amount, which may also be above the hold.
    Exact(i64),
}

impl Mode {
    fn actual(self, held: i64) -> i64 {
        match self {
            Mode::Full => held,
            Mode::Zero => 0,
            Mode::Half => held / 2,
            Mode::Over => held + 1,
            Mode::Exact(amount) => amount,
        }
    }
}

/// One generated op, with its back-reference already resolved to an index.
#[derive(Debug, Clone)]
enum Op {
    /// A top-up under a key of its own.
    TopUp { key: String, amount: i64 },
    /// A hold under a key of its own.
    Hold { key: String, amount: i64 },
    /// Settle the hold taken by op `which`. The settlement names the hold; its idempotency
    /// key is derived from the hold's key, not a key of its own.
    Settle { which: usize, mode: Mode },
    /// Re-issue op `which` byte for byte, under its key.
    Replay { which: usize },
    /// Re-use op `which`'s key with a different amount.
    Collide { which: usize, up: bool },
}

/// One concrete wallet call. Two requests are the same request when their content matches, which
/// is what idempotency is about.
///
/// A settlement names the hold it releases: the hold's key selects the reservation, and the
/// wallet reads the held amount from the hold entry, at the moment of the call. That amount is
/// therefore *not* part of the request — the wallet compares the hold, the actual and the
/// description, so a second settlement naming the same hold with the same actual amount *is* the
/// same request however much that hold has become since. The model reads the release the same way
/// ([`Case::held_now`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    TopUp {
        amount: i64,
    },
    Hold {
        amount: i64,
    },
    Settle {
        hold_key: String,
        /// The charge, computed when the request was built, from the amount the named key held
        /// *then*. A settlement built before that key held anything is a placeholder's worth of
        /// actual; the model refuses such a call before it reaches the wallet, and a replay of it
        /// later carries the same actual, which is what the caller would send.
        actual: i64,
    },
}

impl Request {
    /// The same call with a different amount: close to the original, always valid, and never
    /// equal to it, so a collision is always a collision of content rather than an argument error.
    ///
    /// `held` bounds the perturbed actual of a settlement and is the amount the named key holds
    /// *now* — the request's own actual may have been computed before that hold existed, and a
    /// perturbed actual above the real hold would be invalid input rather than the conflict this
    /// op is about.
    fn perturbed(&self, up: bool, held: i64) -> Self {
        match self {
            Request::TopUp { amount } => Request::TopUp {
                amount: bumped(*amount, up),
            },
            Request::Hold { amount } => Request::Hold {
                amount: bumped(*amount, up),
            },
            Request::Settle { hold_key, actual } => Request::Settle {
                hold_key: hold_key.clone(),
                actual: bumped_actual(*actual, held, up),
            },
        }
    }
}

/// A different amount, as close to the original as possible, always positive.
fn bumped(amount: i64, up: bool) -> i64 {
    if up || amount == 1 {
        amount + 1
    } else {
        amount - 1
    }
}

/// A different actual amount, always within `0..=held`.
fn bumped_actual(actual: i64, held: i64, up: bool) -> i64 {
    if up && actual < held {
        actual + 1
    } else if actual > 0 {
        actual - 1
    } else {
        held
    }
}

/// Turns raw ops into ops, resolving each back-reference to an earlier index.
///
/// The first op has no earlier op to point at, so a back-reference there becomes the write it
/// would otherwise have referred to.
///
/// Keys carry the case's own number: the ledgers are shared between cases, a ledger's idempotency
/// space is the ledger's, and a case reusing `op-0` would otherwise collide with the case before
/// it instead of being the fresh sequence it is meant to be.
fn resolve(raw: &[Raw], case: u64) -> Vec<Op> {
    raw.iter()
        .enumerate()
        .map(|(index, raw)| {
            let key = format!("c{case}-op-{index}");
            let back = |b: u8| -> Option<usize> {
                if index == 0 {
                    return None;
                }
                Some(index - 1 - (usize::from(b) % index))
            };
            match raw {
                Raw::TopUp(amount) => Op::TopUp {
                    key,
                    amount: *amount,
                },
                Raw::Hold(amount) => Op::Hold {
                    key,
                    amount: *amount,
                },
                Raw::Settle { back: b, mode } => match back(*b) {
                    Some(which) => Op::Settle { which, mode: *mode },
                    None => Op::TopUp { key, amount: ONE },
                },
                Raw::Replay { back: b } => match back(*b) {
                    Some(which) => Op::Replay { which },
                    None => Op::TopUp { key, amount: ONE },
                },
                Raw::Collide { back: b, up } => match back(*b) {
                    Some(which) => Op::Collide { which, up: *up },
                    None => Op::TopUp { key, amount: ONE },
                },
            }
        })
        .collect()
}

/// A top-up or hold amount: mostly small, so holds run into the balance and the insufficient
/// funds path is exercised often, with occasional larger ones.
fn amount() -> impl Strategy<Value = i64> {
    prop_oneof![
        4 => 1i64..=4,
        3 => 1i64..=2 * ONE,
        2 => 1i64..=10 * ONE,
        1 => Just(ONE),
    ]
}

fn mode() -> impl Strategy<Value = Mode> {
    prop_oneof![
        2 => Just(Mode::Full),
        1 => Just(Mode::Zero),
        2 => Just(Mode::Half),
        1 => Just(Mode::Over),
        2 => amount().prop_map(Mode::Exact),
    ]
}

fn raw_op() -> impl Strategy<Value = Raw> {
    prop_oneof![
        4 => amount().prop_map(Raw::TopUp),
        4 => amount().prop_map(Raw::Hold),
        3 => (any::<u8>(), mode()).prop_map(|(back, mode)| Raw::Settle { back, mode }),
        2 => any::<u8>().prop_map(|back| Raw::Replay { back }),
        2 => (any::<u8>(), any::<bool>()).prop_map(|(back, up)| Raw::Collide { back, up }),
    ]
}

fn sequence() -> impl Strategy<Value = Vec<Raw>> {
    vec(raw_op(), 1..=10)
}

// ── the model ────────────────────────────────────────────────────────────────

/// What the model expects a call to do, decided before the call is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Predicted {
    /// The wallet writes a new entry.
    Writes,
    /// The key already holds this exact request: the same entry comes back.
    Replays,
    /// The key already holds a different request.
    Conflict,
    /// A hold the balance cannot cover.
    InsufficientFunds,
    /// A settlement whose actual amount is not within `0..=held`.
    InvalidInput,
    /// A settlement naming a key that is not an outstanding hold.
    HoldNotFound,
}

/// The five ways a call can be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Conflict,
    InsufficientFunds,
    InvalidInput,
    HoldNotFound,
}

impl Predicted {
    fn refusal(self) -> Option<Refusal> {
        match self {
            Predicted::Conflict => Some(Refusal::Conflict),
            Predicted::InsufficientFunds => Some(Refusal::InsufficientFunds),
            Predicted::InvalidInput => Some(Refusal::InvalidInput),
            Predicted::HoldNotFound => Some(Refusal::HoldNotFound),
            Predicted::Writes | Predicted::Replays => None,
        }
    }
}

fn refusal_of(error: &WalletError) -> Option<Refusal> {
    match error {
        WalletError::Conflict(_) => Some(Refusal::Conflict),
        WalletError::InsufficientFunds => Some(Refusal::InsufficientFunds),
        WalletError::InvalidInput(_) => Some(Refusal::InvalidInput),
        WalletError::HoldNotFound(_) => Some(Refusal::HoldNotFound),
        // No arm for the engine's `IdempotencyConflict` wrapped as a storage failure: the
        // domain layer maps it to `Conflict`, so a caller seeing it as storage would be a
        // regression, and not one this model reads as a refusal.
        _ => None,
    }
}

/// What one op asked for.
#[derive(Debug, Clone)]
struct Step {
    /// The idempotency key the op used. A replay or a collision borrows the key of the op it
    /// names, so several steps can share one.
    key: String,
    /// `None` when the generator skipped the op.
    request: Option<Request>,
}

/// One case: the wallet, the model of its balance, and the record of what was asked for.
struct Case<'a> {
    wallet: &'a Wallet,
    /// The wallet account's settled layer, in the wallet's own units.
    settled: i64,
    /// What holds have reserved of it and settlements have not released yet. The wallet's limit
    /// keeps this on the reserved side, so it is also the ceiling on what a settlement may release.
    reserved: i64,
    /// The entry log's size at the start of the case.
    log_size: u64,
    /// One record per generated op, in order.
    steps: Vec<Step>,
    /// The request that wrote an entry under a key, and the entry it wrote.
    written: HashMap<String, (Request, EntryId)>,
    /// Entries this case wrote, with the hash their proof has to verify against.
    entries: Vec<(EntryId, Hash)>,
}

impl Case<'_> {
    fn available(&self) -> i64 {
        self.settled - self.reserved
    }

    /// What the model expects of this call, before making it.
    fn predict(&self, key: &str, request: &Request) -> Predicted {
        // The wallet bounds a call's own argument before it consults the ledger, so an impossible
        // actual is invalid input even when the hold does not exist. The model has to read that
        // first, in the same order.
        if let Request::Settle { actual, .. } = request
            && *actual < 0
        {
            return Predicted::InvalidInput;
        }
        // A settlement's hold and bound checks run before its append, so its collision check waits
        // for them below. Top-ups and holds collide on their key straight away.
        if !matches!(request, Request::Settle { .. })
            && let Some((prior, _)) = self.written.get(key)
        {
            return if prior == request {
                Predicted::Replays
            } else {
                Predicted::Conflict
            };
        }
        match request {
            Request::TopUp { .. } => Predicted::Writes,
            Request::Hold { amount } => {
                if *amount > self.available() {
                    Predicted::InsufficientFunds
                } else {
                    Predicted::Writes
                }
            }
            Request::Settle {
                hold_key, actual, ..
            } => {
                // The wallet reads the hold from the ledger: no entry under the key, or an entry
                // that is not a hold, means there is nothing to release. A written settlement
                // implies a written hold — the wallet only writes one after confirming the
                // other — so the derived key cannot have collided when the hold is missing.
                let Some(outstanding) = self.held_now(hold_key) else {
                    return Predicted::HoldNotFound;
                };
                // The bound is checked after the hold is read, and before the append: a second
                // settlement that also overshoots is invalid input, not a conflict.
                if *actual > outstanding {
                    return Predicted::InvalidInput;
                }
                // The settlement's idempotency key is derived from the hold's; a second settlement
                // collides there, inside the append.
                if let Some((prior, _)) = self.written.get(key) {
                    return if prior == request {
                        Predicted::Replays
                    } else {
                        Predicted::Conflict
                    };
                }
                // The settlement releases exactly what its hold reserved, which is still
                // outstanding, so the wallet's limit cannot refuse it: the pairing is exact.
                Predicted::Writes
            }
        }
    }

    /// What this op asks for, and under which key. A settlement's key is derived from the
    /// hold's key; the step records the derived key so replays and collisions find it.
    fn resolve(&self, op: &Op) -> (String, Option<Request>) {
        match op {
            Op::TopUp { key, amount } => (key.clone(), Some(Request::TopUp { amount: *amount })),
            Op::Hold { key, amount } => (key.clone(), Some(Request::Hold { amount: *amount })),
            Op::Settle { which, mode } => {
                // The key of the op that took the hold — whether or not it was a hold, whether
                // or not the wallet took it. Settling a top-up, a refused hold, or a settlement
                // is exactly the naming of something that is not an outstanding hold.
                let hold_key = self.steps[*which].key.clone();
                // What the hold is worth now, the way the wallet reads it from the ledger. A
                // placeholder when the named op is not an outstanding hold: the model refuses the
                // call before it is used, and a later replay of it re-reads the release.
                let held = self.held_now(&hold_key).unwrap_or(ONE);
                let derived = settlement_key_for(&hold_key);
                (
                    derived,
                    Some(Request::Settle {
                        hold_key,
                        actual: mode.actual(held),
                    }),
                )
            }
            // A replay reproduces the request as it was issued, even if that op was skipped or
            // refused: a refused attempt leaves no trace, so trying it again is a fresh attempt.
            Op::Replay { which } => (
                self.steps[*which].key.clone(),
                self.steps[*which].request.clone(),
            ),
            Op::Collide { which, up } => {
                let previous = &self.steps[*which];
                // A settlement's perturbed actual is bounded by what the named key holds now, not
                // by what it held when the request was built.
                let held = match &previous.request {
                    Some(Request::Settle { hold_key, .. }) => {
                        self.held_now(hold_key).unwrap_or(ONE)
                    }
                    _ => ONE,
                };
                (
                    previous.key.clone(),
                    previous.request.clone().map(|r| r.perturbed(*up, held)),
                )
            }
        }
    }

    /// What the wallet holds under `key`, as the model knows it: the amount of the entry written
    /// under that key, which is exactly what the wallet reads out of the ledger. `None` when the
    /// key names no outstanding hold.
    fn held_now(&self, key: &str) -> Option<i64> {
        match self.written.get(key) {
            Some((Request::Hold { amount }, _)) => Some(*amount),
            _ => None,
        }
    }

    /// Checks the call against the prediction, and updates the model when it wrote.
    fn check(
        &mut self,
        index: usize,
        op: &Op,
        key: &str,
        request: &Request,
        predicted: Predicted,
        receipt: &Result<Receipt, WalletError>,
    ) -> Result<(), String> {
        let context = |detail: &str| {
            format!(
                "{detail}\n  op {index}: {op:?}\n  key {key:?}, asking {request:?}\n  the model expected {predicted:?}"
            )
        };
        match (predicted, receipt) {
            (Predicted::Writes, Ok(receipt)) => {
                if !receipt.is_new {
                    return Err(context(
                        "the model expected a new entry, the wallet replayed one",
                    ));
                }
                match request {
                    Request::TopUp { amount } => self.settled += amount,
                    Request::Hold { amount } => self.reserved += amount,
                    Request::Settle { hold_key, actual } => {
                        // The settlement releases exactly what its hold reserved, and charges the
                        // actual: the pairing the wallet enforces. The release is the entry the
                        // hold's key holds *now* — `predict` allowed the call on that same amount,
                        // and it is not the amount the request was built with (issue #41).
                        let Some(held) = self.held_now(hold_key) else {
                            return Err(context(
                                "a settlement wrote while the model held nothing under its key",
                            ));
                        };
                        self.reserved -= held;
                        self.settled -= actual;
                    }
                }
                self.log_size += 1;
                self.written
                    .insert(key.to_owned(), (request.clone(), receipt.entry_id));
                self.entries.push((receipt.entry_id, receipt.content_hash));
                Ok(())
            }
            (Predicted::Writes, Err(error)) => Err(context(&format!(
                "the model expected a new entry, got {error:?}"
            ))),
            (Predicted::Replays, Ok(receipt)) => {
                let expected = self.written.get(key).map(|(_, entry)| *entry);
                if receipt.is_new {
                    return Err(context(
                        "the model expected a replay, the wallet wrote an entry",
                    ));
                }
                if Some(receipt.entry_id) != expected {
                    return Err(context(&format!(
                        "the model expected entry {expected:?}, the wallet returned {}",
                        receipt.entry_id
                    )));
                }
                Ok(())
            }
            (Predicted::Replays, Err(error)) => Err(context(&format!(
                "the model expected a replay, got {error:?}"
            ))),
            (predicted, Err(error)) => {
                let expected = predicted.refusal();
                match refusal_of(error) {
                    refusal if refusal == expected => Ok(()),
                    refusal => Err(context(&format!(
                        "the model expected {expected:?}, the wallet answered {refusal:?} ({error:?})"
                    ))),
                }
            }
            (predicted, Ok(receipt)) => Err(context(&format!(
                "the model expected {predicted:?}, the wallet accepted the call with {receipt:?}"
            ))),
        }
    }

    /// The balance and the log agree with the model, and the balance is not negative.
    async fn check_book(&self, index: usize, op: &Op) -> Result<(), String> {
        let context = format!("after op {index}: {op:?}");
        let available = self
            .wallet
            .available()
            .await
            .map_err(|error| format!("{context}: reading the balance: {error}"))?;
        if available != self.available() {
            return Err(format!(
                "{context}: the wallet says {available} available, the model says {} (settled {}, reserved {})",
                self.available(),
                self.settled,
                self.reserved
            ));
        }
        if available < 0 {
            return Err(format!("{context}: available went negative: {available}"));
        }
        if self.reserved < 0 {
            return Err(format!(
                "{context}: the model released more than it had reserved: {}",
                self.reserved
            ));
        }
        let log_size = self
            .wallet
            .log_size()
            .await
            .map_err(|error| format!("{context}: reading the log size: {error}"))?;
        if log_size != self.log_size {
            return Err(format!(
                "{context}: the log holds {log_size} entries, the model counts {}",
                self.log_size
            ));
        }
        Ok(())
    }

    /// Plays one generated op.
    async fn run(&mut self, index: usize, op: &Op) -> Result<(), String> {
        let (key, request) = self.resolve(op);
        let Some(request) = request else {
            self.steps.push(Step { key, request: None });
            return Ok(());
        };
        let predicted = self.predict(&key, &request);
        // A settlement or hold records why it happened, and the record is part of the entry: the key
        // is a deterministic stand-in for it here, which is what a real caller's record must be too,
        // or a retry would be refused as a different request.
        let because = key.clone();
        let receipt = match &request {
            Request::TopUp { amount } => self.wallet.top_up(&key, *amount, DAY).await,
            Request::Hold { amount } => self.wallet.hold(&key, &because, *amount, DAY).await,
            Request::Settle {
                hold_key, actual, ..
            } => {
                // The settlement names the hold; the key is derived from it. The description is
                // the derived key, so a replay is byte-identical and a collision is one of
                // content.
                self.wallet.settle(hold_key, &because, *actual, DAY).await
            }
        };
        let outcome = self.check(index, op, &key, &request, predicted, &receipt);
        self.steps.push(Step {
            key,
            request: Some(request),
        });
        outcome?;
        self.check_book(index, op).await
    }

    /// Every entry the case wrote is provable, and its proof does not survive being tampered with.
    async fn check_proofs(&self) -> Result<(), String> {
        for (entry_id, hash) in &self.entries {
            let Some(bundle) = self
                .wallet
                .receipt_proof(*entry_id)
                .await
                .map_err(|error| format!("the proof for {entry_id}: {error}"))?
            else {
                return Err(format!("{entry_id} is in the log but has no proof"));
            };
            let json = serde_json::to_string(&bundle)
                .map_err(|error| format!("serialising the bundle for {entry_id}: {error}"))?;
            match verify_bundle(&json, hash) {
                Ok(true) => {}
                other => {
                    return Err(format!(
                        "the bundle for {entry_id} did not verify: {other:?}"
                    ));
                }
            }
            // Tampering means changing what the entry records. A flipped byte can legitimately
            // survive: the verifier ignores fields it does not know, deliberately, so that a newer
            // server can add one without breaking browsers that already shipped. What it must never
            // survive is a different amount.
            let tampered = tamper_with_an_amount(&json)?;
            if matches!(verify_bundle(&tampered, hash), Ok(true)) {
                return Err(format!(
                    "the bundle for {entry_id} verified after its amount was changed: {tampered}"
                ));
            }
        }
        Ok(())
    }
}

/// A copy of a bundle with one posting's amount moved by one minor unit.
fn tamper_with_an_amount(json: &str) -> Result<String, String> {
    let mut value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("re-parsing a bundle that was just serialised: {error}"))?;
    let posting = value
        .get_mut("entry")
        .and_then(|entry| entry.get_mut("postings"))
        .and_then(|postings| postings.as_array_mut())
        .and_then(|postings| postings.first_mut())
        .and_then(|posting| posting.as_object_mut())
        .ok_or_else(|| format!("a bundle with no entry.postings[0] to tamper with: {json}"))?;
    let amount = posting
        .get("amount")
        .and_then(|amount| amount.as_str())
        .ok_or_else(|| format!("a posting whose amount is not a string: {json}"))?
        .to_owned();
    let mut digits: Vec<char> = amount.chars().collect();
    let last = digits
        .last_mut()
        .ok_or_else(|| format!("a posting with an empty amount: {json}"))?;
    *last = if *last == '0' { '1' } else { '0' };
    posting.insert(
        "amount".to_owned(),
        serde_json::Value::String(digits.into_iter().collect()),
    );
    serde_json::to_string(&value).map_err(|error| format!("re-serialising a bundle: {error}"))
}

/// Plays one whole case and reports the first disagreement.
async fn check_case(
    tenants: &Tenants,
    tenant_id: &str,
    number: u64,
    raw: &[Raw],
) -> Result<(), String> {
    let wallet = tenants
        .get(tenant_id)
        .await
        .map_err(|error| format!("opening {tenant_id}: {error}"))?;
    // The ledger is shared with the cases before it, so the model starts from what is actually
    // there: both layers in the wallet's own units, and the log size. Nothing is a delta.
    let settled = wallet
        .settled()
        .await
        .map_err(|error| format!("reading {tenant_id}'s settled balance: {error}"))?;
    let reserved = wallet
        .reserved()
        .await
        .map_err(|error| format!("reading {tenant_id}'s reservations: {error}"))?;
    let mut case = Case {
        settled,
        reserved,
        log_size: wallet
            .log_size()
            .await
            .map_err(|error| format!("reading {tenant_id}'s log size: {error}"))?,
        wallet: &wallet,
        steps: Vec::new(),
        written: HashMap::new(),
        entries: Vec::new(),
    };
    for (index, op) in resolve(raw, number).iter().enumerate() {
        case.run(index, op).await?;
    }
    case.check_proofs().await
}

// ── the harness ──────────────────────────────────────────────────────────────

/// How many sequences to run. 1000 is what TODO.md asks CI for; `PROPTEST_CASES` narrows it for
/// a quick local run (`PROPTEST_CASES=50 cargo test -p oxsum-core --test generative`).
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1000)
}

/// proptest's runner is synchronous, so the cases drive the async wallet themselves.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let _ = dotenvy::dotenv();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a tokio runtime")
    })
}

/// The ledger the case at `index` uses.
fn tenant_id(ledger: usize) -> String {
    format!("generative_{ledger}")
}

/// A number no other case in this run has: the per-case key prefix.
fn next_case() -> u64 {
    static CASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    CASE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// `DATABASE_URL` for the suite: the process environment wins, then `.env`
/// (searched from the current directory upward, see docs/development.md).
/// `None` means no database is configured and the suite skips, also per
/// docs/development.md.
fn database_url() -> Option<String> {
    // Load `.env` first: the documented setup is `cp .env.example .env` with
    // nothing exported, and reading the variable before this line is what let
    // the suite pass vacuously (issue #18). This mirrors the `url()` helper
    // in the other suites.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

/// The ledger of the deterministic regression case. Not one of the eight the generated cases
/// share: that case needs a starting balance of its own, and the shared ones carry the residue of
/// whichever cases ran before.
const REGRESSION_LEDGER: &str = "generative_regression";

/// The shared pool and tenant registry, or `None` when no database is configured.
///
/// The eight ledgers are dropped and recreated once, before the first case: every run then starts
/// from an empty ledger, and a run leaves eight schemas behind rather than a thousand. The
/// regression ledger is dropped here too — the case needs a balance it can predict, and it is
/// created again on first use ([`Tenants::get`]).
fn tenants() -> Option<&'static Tenants> {
    static TENANTS: OnceLock<Option<Tenants>> = OnceLock::new();
    TENANTS
        .get_or_init(|| {
            let url = database_url()?;
            let pool = runtime()
                .block_on(async {
                    PgPoolOptions::new()
                        .max_connections(LEDGERS as u32 + 2)
                        .connect(&url)
                        .await
                })
                .expect("connecting to the database");
            runtime().block_on(async {
                for ledger in (0..LEDGERS)
                    .map(tenant_id)
                    .chain([REGRESSION_LEDGER.to_owned()])
                {
                    sqlx::query(&format!("DROP SCHEMA IF EXISTS ledger_{ledger} CASCADE"))
                        .execute(&pool)
                        .await
                        .expect("dropping a stale ledger");
                }
            });
            let tenants = Tenants::new(pool);
            runtime().block_on(async {
                for ledger in 0..LEDGERS {
                    tenants
                        .get(&tenant_id(ledger))
                        .await
                        .expect("opening a ledger");
                }
            });
            Some(tenants)
        })
        .as_ref()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// One case: a ledger out of the pool, and a random sequence of calls against it.
    #[test]
    fn generated_sequences_match_the_model(ledger in 0..LEDGERS, raw in sequence()) {
        let Some(tenants) = tenants() else {
            // Warn once, not once per case: a silent skip is what made the
            // suite pass vacuously (issue #18).
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!(
                    "DATABASE_URL not set (neither in the environment nor in .env), skipping all {} generative cases",
                    cases()
                );
            });
            return Ok(());
        };
        if let Err(failure) =
            runtime().block_on(check_case(tenants, &tenant_id(ledger), next_case(), &raw))
        {
            return Err(TestCaseError::fail(failure));
        }
    }
}

/// A settlement releases the hold the ledger holds, not the one its request was built with
/// (issue #41).
///
/// The counterexample that took CI down: a hold refused for insufficient funds leaves its key
/// free, so a collision can later write a hold there, and a replay of a settlement that was
/// refused *before* that hold existed then succeeds — releasing the entry the ledger holds while
/// the model subtracted the placeholder it had recorded in the request.
///
/// It is pinned here rather than by a proptest seed because it needs a ledger whose balance sits
/// inside a band: enough for the collided hold, not enough for the first one. The seed CI saved
/// carries the sequence but not the balance the shared ledger happened to have, so replaying it
/// against a fresh database passes.
#[test]
fn a_settlement_releases_the_hold_the_ledger_holds() {
    let Some(tenants) = tenants() else {
        // Same warn-once honesty as the generated cases (issue #18).
        eprintln!(
            "DATABASE_URL not set (neither in the environment nor in .env), skipping the regression case"
        );
        return;
    };
    // The balance band, in the order the case reaches it: the first hold is refused, the top-up
    // funds the account past the collided hold, and the collided hold writes because the first
    // one never did.
    let raw = [
        Raw::Hold(1_911_017),
        Raw::TopUp(1_148_977),
        Raw::Settle {
            back: 142,
            mode: Mode::Full,
        },
        Raw::Settle {
            back: 167,
            mode: Mode::Zero,
        },
        Raw::Collide {
            back: 251,
            up: true,
        },
        Raw::Replay { back: 86 },
    ];
    let failure = runtime().block_on(async {
        let wallet = tenants
            .get(REGRESSION_LEDGER)
            .await
            .expect("opening the regression ledger");
        wallet
            .top_up("regression-fund", ONE, DAY)
            .await
            .expect("funding the regression ledger");
        check_case(tenants, REGRESSION_LEDGER, 0, &raw).await
    });
    if let Err(failure) = failure {
        panic!("the model and the wallet disagreed: {failure}");
    }
}

/// The `.env`-first lookup, pinned (issue #18).
///
/// Runs one proptest case in a child process whose environment has no
/// `DATABASE_URL` and whose working directory holds a `.env` pointing at a
/// database that cannot exist. The suite must *try* the database — the child
/// must fail on the value — rather than skip vacuously and pass. Hermetic: it
/// needs no real database and no repo `.env`.
#[test]
fn dot_env_is_consulted_before_the_database_check() {
    // The probe child runs only the proptest (see the `--exact` filter
    // below), but guard against recursion anyway.
    if std::env::var("OXSUM_GENERATIVE_DOTENV_PROBE").is_ok() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "oxsum-generative-dotenv-probe-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("probe dir");
    // Not a URL at all: the parse fails before any I/O, so the probe stays
    // fast. A hostname that hangs (rather than refusing) would make every
    // `cargo test` pay the pool timeout here.
    std::fs::write(dir.join(".env"), "DATABASE_URL=not-a-database-url\n").expect("probe .env");
    let exe = std::env::current_exe().expect("test executable");
    // Capture the child's output: its failure is the assertion, and letting
    // it through would print a FAILED block into this run's log.
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("generated_sequences_match_the_model")
        .env("PROPTEST_CASES", "1")
        .env("OXSUM_GENERATIVE_DOTENV_PROBE", "1")
        .env_remove("DATABASE_URL")
        .current_dir(&dir)
        .output()
        .expect("probe child");
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        !output.status.success(),
        "the suite skipped vacuously: with DATABASE_URL only in .env, it must try the database; child stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
