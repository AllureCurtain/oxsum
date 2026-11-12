//! The read surface the dashboard lists: entries, transactions, requests, settlements.

use super::{
    BILLS_SCAN, ListPage, LogEntry, REQUESTS_SCAN, RequestEntry, SCALE, SettledEntry, SettledTurn,
    TransactionEntry, TransactionKind, Wallet, currency,
};
use crate::billing::SettlementRecord;
use crate::error::{WalletError, invalid};
use doubleentry::{
    AccountId, BalanceKey, BalanceQuery, Cursor, Direction, Layer, LedgerStore, LogIndex,
    StoredEntry,
};

impl Wallet {
    /// The newest entries of the log, newest first, at most `limit`.
    ///
    /// What the dashboard's transaction log lists: the position, the id, the description
    /// the writer recorded, and the content hash the Merkle log commits to. The proof
    /// page (a later item) is what verifies one; this only lists them.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_entries(&self, limit: usize) -> Result<Vec<LogEntry>, WalletError> {
        Ok(self.entries_page(None, limit).await?.rows)
    }
    /// The transaction log, one page at a time (issue #93): `before` bounds the page
    /// to entries with a smaller log index — the ledger's own cursor — and the
    /// answer's `next_cursor` resumes there, `None` at the log's start.
    ///
    /// Same rows as [`recent_entries`](Self::recent_entries), which reads the first
    /// page.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn entries_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<LogEntry>, WalletError> {
        self.scoped_entries_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }
    /// Raw entries filtered by their immutable provenance before they leave core.
    pub async fn scoped_entries_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<LogEntry>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), 100, &mut |stored| {
            if !scope.allows_entry(&stored.entry) {
                return None;
            }
            Some(LogEntry {
                index: stored
                    .require_index()
                    .map(LogIndex::get)
                    .unwrap_or(u64::MAX),
                id: stored.entry.id().to_string(),
                description: stored.entry.description().as_str().to_owned(),
                content_hash: stored.content_hash.to_string(),
            })
        })
        .await
    }
    /// The newest settled entries, newest first, at most `limit`.
    ///
    /// What the dashboard's bills page lists and its CSV and JSON exports carry: the id,
    /// the booking date, the charge in minor units and the content hash.
    ///
    /// A settled entry is one that *released a hold*, and the ledger says which those are:
    /// [`Wallet::settle`] always credits the wallet account in the pending layer — the
    /// mirror of the pending debit a hold takes — and no other write does. The entry's
    /// description cannot answer this: the wallet API's settlements record none
    /// (`POST /api/v1/settlements`, whose whole input is the hold key and the actual), so
    /// identifying settlements by their description would silently omit them.
    ///
    /// The charge is the entry's net debit on the wallet account in the *settled* layer,
    /// i.e. what the books hold; it is 0 for the settlement kinds that charge nothing (an
    /// upstream error, an unreachable upstream, a swept hold).
    ///
    /// The read walks back one page at a time and stops as soon as it has `limit` bills, so
    /// a busy log costs no more than the entries above the last one it returns.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settled_entries(&self, limit: usize) -> Result<Vec<SettledEntry>, WalletError> {
        Ok(self.settled_entries_page(None, limit).await?.rows)
    }
    /// The settled entries, one page at a time (issue #93): same rows as
    /// [`settled_entries`](Self::settled_entries), which reads the first page.
    /// `before` bounds the page to entries with a smaller log index and the answer's
    /// `next_cursor` resumes there.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settled_entries_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<SettledEntry>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), BILLS_SCAN, &mut |stored| {
            self.is_settlement(stored).then(|| SettledEntry {
                id: stored.entry.id().to_string(),
                booked_on: stored.entry.booking_date(),
                charged_minor: self.settled_charge(stored),
                content_hash: stored.content_hash.to_string(),
            })
        })
        .await
    }
    /// The organization's transactions as the bills page lists them, newest first,
    /// at most `limit`: top-ups, adjustments — the signup bonus among them — and
    /// settled requests (issue #90).
    ///
    /// Each entry carries its kind, its signed effect on the wallet's settled
    /// balance, the key that paid where the entry attributes one, and — for a
    /// settled gateway turn — the settlement record the entry's own description
    /// holds, so a row shows exactly what the bill's content hash covers. A hold
    /// bills nothing yet and is skipped; the overview's in-flight list is where
    /// it shows.
    ///
    /// The read walks back one page at a time and stops as soon as it has `limit`
    /// transactions, so a long log costs no more than the entries above the oldest
    /// one it returns.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_transactions(
        &self,
        limit: usize,
    ) -> Result<Vec<TransactionEntry>, WalletError> {
        Ok(self.transactions_page(None, limit).await?.rows)
    }
    /// The organization's transactions, one page at a time (issue #93): same rows as
    /// [`recent_transactions`](Self::recent_transactions), which reads the first page.
    /// `before` bounds the page to entries with a smaller log index and the answer's
    /// `next_cursor` resumes there — the bills page's older rows and the CSV and JSON
    /// exports, which now list *every* transaction, walk these pages.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn transactions_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<TransactionEntry>, WalletError> {
        self.scoped_transactions_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }
    /// Apply immutable record attribution before selecting a page of rows.
    pub async fn scoped_transactions_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<TransactionEntry>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), BILLS_SCAN, &mut |stored| {
            if !scope.allows_entry(&stored.entry) {
                return None;
            }
            let kind = self.classify(stored)?;
            Some(TransactionEntry {
                id: stored.entry.id().to_string(),
                index: stored
                    .require_index()
                    .map(LogIndex::get)
                    .unwrap_or(u64::MAX),
                booked_on: stored.entry.booking_date(),
                description: stored.entry.description().as_str().to_owned(),
                content_hash: stored.content_hash.to_string(),
                amount_minor: self.settled_effect(stored),
                kind,
                key_id: stored
                    .entry
                    .provenance()
                    .actor
                    .as_ref()
                    .map(|actor| actor.as_str().to_owned()),
                record: (kind == TransactionKind::Settlement)
                    .then(|| SettlementRecord::parse(stored.entry.description().as_str()))
                    .flatten(),
            })
        })
        .await
    }
    /// Which of the bills page's kinds an entry is, or `None` for an entry the page does
    /// not list: a hold, which bills nothing yet.
    ///
    /// The postings decide, never the description: a settlement is the pending-layer
    /// credit that releases the reserve, a top-up posts `Assets:Cash`, an adjustment
    /// posts `Equity:Adjustments` — each exactly once, because those are the only
    /// writers these accounts have.
    pub(super) fn classify(&self, stored: &StoredEntry<SCALE>) -> Option<TransactionKind> {
        let mut pending_credit = false;
        let mut pending_debit = false;
        let mut cash = false;
        let mut adjustments = false;
        let mut facility = false;
        for p in stored.entry.postings() {
            if self.is_pool(p.account) {
                match (p.layer, p.direction) {
                    (Layer::Pending, Direction::Credit) => pending_credit = true,
                    (Layer::Pending, Direction::Debit) => pending_debit = true,
                    _ => {}
                }
            } else if p.account == self.cash {
                cash = true;
            } else if p.account == self.adjustments {
                adjustments = true;
            } else if p.account == self.facility {
                facility = true;
            }
        }
        if pending_credit {
            return Some(TransactionKind::Settlement);
        }
        if pending_debit {
            return None;
        }
        if cash {
            return Some(TransactionKind::TopUp);
        }
        // A credit-limit change touches only the facility and the credit line:
        // an operator action that moves what the organization may draw, shown
        // like the other operator movements rather than hidden from the history.
        if adjustments || facility {
            return Some(TransactionKind::Adjustment);
        }
        None
    }
    /// The entry's effect on the pools' combined settled balance, signed: credits
    /// add, debits take. A top-up or a grant reads positive, a deduction or a
    /// settled charge negative, and a settlement that charged nothing reads zero —
    /// the pending-layer release it also writes is not a movement of settled money.
    /// The pool reclassification nets to zero across the two accounts.
    pub(super) fn settled_effect(&self, stored: &StoredEntry<SCALE>) -> i64 {
        stored
            .entry
            .postings()
            .iter()
            .filter(|p| self.is_pool(p.account) && p.layer == Layer::Settled)
            .map(|p| match p.direction {
                Direction::Credit => p.amount.to_minor(),
                Direction::Debit => -p.amount.to_minor(),
            })
            .sum()
    }
    /// True when the entry settled a hold: the pending-layer credit on a pool account
    /// that [`Wallet::settle`] writes for every settlement, whatever it charges.
    pub(super) fn is_settlement(&self, stored: &StoredEntry<SCALE>) -> bool {
        stored.entry.postings().iter().any(|p| {
            self.is_pool(p.account) && p.layer == Layer::Pending && p.direction == Direction::Credit
        })
    }
    /// What the entry charged: the pool accounts' net debit in the settled layer, in
    /// minor units. Nothing at all for a settlement that charged nothing.
    pub(super) fn settled_charge(&self, stored: &StoredEntry<SCALE>) -> i64 {
        stored
            .entry
            .postings()
            .iter()
            .filter(|p| self.is_pool(p.account) && p.layer == Layer::Settled)
            .map(|p| match p.direction {
                Direction::Debit => p.amount.to_minor(),
                Direction::Credit => -p.amount.to_minor(),
            })
            .sum()
    }
    /// The four pools' combined balance in one layer. All four are
    /// credit-normal — the wallet and the credit line are liabilities, the
    /// bonus and promo pools equity — so a credit balance is the positive side
    /// on each.
    pub(super) async fn pool_net(&self, layer: Layer) -> Result<i64, WalletError> {
        Ok(self.account_net(self.wallet, layer).await?
            + self.account_net(self.bonus, layer).await?
            + self.account_net(self.promo, layer).await?
            + self.account_net(self.credit_line, layer).await?)
    }
    /// Whether the account is one a hold reserves against and a settlement draws:
    /// the promo pool, the bonus pool, the purchased wallet, or the credit line.
    /// The facility and promo-expiry accounts are not pools — nothing ever
    /// spends out of them.
    pub(super) fn is_pool(&self, account: AccountId) -> bool {
        account == self.wallet
            || account == self.bonus
            || account == self.promo
            || account == self.credit_line
    }
    /// The bonus pool's settled balance: what a deduction may draw before it
    /// touches purchased credit.
    pub(super) async fn bonus_settled(&self) -> Result<i64, WalletError> {
        self.account_net(self.bonus, Layer::Settled).await
    }
    pub(super) async fn account_net(
        &self,
        account: AccountId,
        layer: Layer,
    ) -> Result<i64, WalletError> {
        let b = self
            .store
            .balance(
                BalanceKey {
                    account,
                    currency: currency(),
                    layer,
                },
                BalanceQuery::all(),
            )
            .await?;
        Ok(b.credits.to_minor() - b.debits.to_minor())
    }
    /// An account's settled balance on the caller's connection — credits minus
    /// debits — for reads that must agree with what a locked transaction sees.
    /// [`account_net`](Self::account_net) answers the same figure on its own
    /// connection; this one exists for the credit paths, whose read has to be
    /// covered by the advisory lock the caller already holds.
    pub(super) async fn settled_net_in(
        &self,
        conn: &mut sqlx::PgConnection,
        account: AccountId,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT COALESCE(SUM(CASE \
                 WHEN p.direction = 'C' THEN p.amount_minor \
                 ELSE -p.amount_minor END), 0)::bigint AS net \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = $1 AND p.layer = 'settled' AND e.log_index IS NOT NULL",
            schema = self.tenant_id,
        );
        let net: i64 = sqlx::query_scalar(&sql)
            .bind(account.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(net)
    }
    /// What a pool account can still fund: its settled balance minus the
    /// reservations pending holds still carry. Pending debits subtract what is
    /// held, pending credits add back what a settlement released — and
    /// `FundedReservations` keeps the pending layer's net on the hold side, so
    /// the figure can never exceed the settled balance.
    pub(super) async fn pool_available_in(
        &self,
        conn: &mut sqlx::PgConnection,
        account: AccountId,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT COALESCE(SUM(CASE \
                 WHEN p.layer = 'settled' AND p.direction = 'C' THEN p.amount_minor \
                 WHEN p.layer = 'settled' AND p.direction = 'D' THEN -p.amount_minor \
                 WHEN p.layer = 'pending' AND p.direction = 'D' THEN -p.amount_minor \
                 WHEN p.layer = 'pending' AND p.direction = 'C' THEN p.amount_minor \
                 ELSE 0 END), 0)::bigint AS available \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = $1 AND e.log_index IS NOT NULL",
            schema = self.tenant_id,
        );
        let available: i64 = sqlx::query_scalar(&sql)
            .bind(account.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(available)
    }
    /// The newest gateway requests, newest first, at most `limit`.
    ///
    /// What the dashboard's requests page lists (issue #55): each settled turn's request
    /// id, model, pricing kind, token counts, charge in minor units, booking date and the
    /// key that paid it. The facts come out of the settlement entry's own description —
    /// the record [`Settlement::description`] writes, covered by the entry's content hash
    /// — so the page cannot show a number that the bill does not prove, and the kind is
    /// read back as a [`SettlementKind`] rather than re-derived from the charge.
    ///
    /// A request appears once it has settled: a turn still in flight has no entry yet, and
    /// the overview is where its hold is shown live. A settlement the wallet API wrote by
    /// hand carries no record, and a hold or a top-up is not a request, so those entries
    /// are skipped.
    ///
    /// The read walks back one page at a time and stops as soon as it has `limit`
    /// requests, so a log full of top-ups costs no more than the entries above the last
    /// request it returns.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_requests(&self, limit: usize) -> Result<Vec<RequestEntry>, WalletError> {
        Ok(self.requests_page(None, limit).await?.rows)
    }
    /// The settled gateway requests, one page at a time (issue #93): same rows as
    /// [`recent_requests`](Self::recent_requests), which reads the first page.
    /// `before` bounds the page to entries with a smaller log index and the answer's
    /// `next_cursor` resumes there.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn requests_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<RequestEntry>, WalletError> {
        self.scoped_requests_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }
    /// Apply immutable record attribution before selecting a page of rows.
    pub async fn scoped_requests_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<RequestEntry>, WalletError> {
        Ok(self
            .scoped_settlements_page(before, limit, scope)
            .await?
            .map(|turn| RequestEntry {
                booked_on: turn.booked_on,
                request_id: turn.record.request,
                entry_id: turn.entry_id,
                model: turn.record.model,
                kind: turn.record.kind,
                input_tokens: turn.record.input_tokens,
                output_tokens: turn.record.output_tokens,
                charged_minor: turn.record.charged,
                discount_percent: turn.record.discount_percent,
                upstream_attempts: turn.record.upstream_attempts,
                key_id: turn.key_id,
                status: turn.record.kind.status(),
                failure_reason: turn.record.reason,
                failure_reason_commitment: turn.record.reason_commitment,
            }))
    }
    /// Each settled gateway turn's own [`SettlementRecord`], newest first, with its
    /// booking date and the key that paid it: the platform admin's anomalies list and
    /// the dashboard's requests page both read this (issue #57).
    ///
    /// Same contract as [`recent_requests`](Self::recent_requests): only entries whose
    /// description is a settlement record count, and the walk stops once `limit` turns
    /// are found.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_settlements(&self, limit: usize) -> Result<Vec<SettledTurn>, WalletError> {
        Ok(self.settlements_page(None, limit).await?.rows)
    }
    /// The settled gateway turns' own records, one page at a time (issue #93): same
    /// rows as [`recent_settlements`](Self::recent_settlements), which reads the first
    /// page.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settlements_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<SettledTurn>, WalletError> {
        self.scoped_settlements_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }
    /// Apply immutable record attribution before selecting a page of rows.
    pub async fn scoped_settlements_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<SettledTurn>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), REQUESTS_SCAN, &mut |stored| {
            if !scope.allows_entry(&stored.entry) {
                return None;
            }
            let record = SettlementRecord::parse(stored.entry.description().as_str())?;
            Some(SettledTurn {
                booked_on: stored.entry.booking_date(),
                entry_id: stored.entry.id(),
                record,
                key_id: stored
                    .entry
                    .provenance()
                    .actor
                    .as_ref()
                    .map(|actor| actor.as_str().to_owned()),
            })
        })
        .await
    }
    /// The walk every backward list shares: one store page at a time, the store's
    /// own log order reversed for the answer, at most `wanted` rows `keep`
    /// recognizes. `before` bounds the walk to indices below it — `None` reads the
    /// head; the resume point is the smallest index the walk left unvisited,
    /// `None` when index 0 was scanned (issue #93).
    pub(super) async fn scan_back<T>(
        &self,
        before: Option<u64>,
        wanted: usize,
        window: usize,
        keep: &mut impl FnMut(&StoredEntry<SCALE>) -> Option<T>,
    ) -> Result<ListPage<T>, WalletError> {
        let size = self.store.head().await?.size;
        let mut end = before.unwrap_or(size).min(size);
        let mut rows = Vec::new();
        while end > 0 && rows.len() < wanted {
            let start = end.saturating_sub(window as u64);
            let after = start.checked_sub(1).map(LogIndex::new);
            let page = self
                .store
                .page(Cursor {
                    after,
                    limit: (end - start) as usize,
                })
                .await?;
            let mut visited = start;
            // The store pages in log order; the page wants newest first.
            for stored in page.records.iter().rev() {
                if let Some(row) = keep(stored) {
                    rows.push(row);
                    if rows.len() == wanted {
                        // Unvisited indices are now [0, stored.index): resume below
                        // the row that filled the page, so no entry is skipped.
                        visited = stored.require_index().map_err(invalid)?.get();
                        break;
                    }
                }
            }
            end = visited;
        }
        Ok(ListPage {
            rows,
            next_cursor: (end > 0).then_some(end),
        })
    }
}
