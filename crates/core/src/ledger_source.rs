//! Canonical ledger reads on the caller's connection, without another pool checkout.
//!
//! Callers choose the transaction isolation level. Reconciliation uses a repeatable-read
//! snapshot; mutation paths see their own uncommitted entries. A verified entry preserves
//! its original key bytes and every hashed field, but does not prove log inclusion, current
//! entry-ID-to-key mapping, account-path labels, source ownership, or the correctness of
//! a business projection. Consumers must bind the verified raw key and registered account
//! IDs to their original business source; a hash alone does not establish that relationship.

#[cfg(test)]
#[path = "ledger_source/tests.rs"]
mod tests;

use std::collections::BTreeMap;
use std::fmt::Display;

use doubleentry::storage::postgres::PostgresError;
use doubleentry::{
    AccountId, Amount, Currency, Description, Dimensions, Direction, DocumentRef, Draft, Entry,
    EntryId, Hash, IdempotencyKey, Label, Layer, LogIndex, Posting, Provenance, StoredEntry,
};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use time::Date;
use uuid::Uuid;

use crate::{SCALE, wallet};

const ENTRY_LIMIT: usize = 20_000;
const POSTING_LIMIT: i64 = 640_000;
const DIMENSION_LIMIT: i64 = 640_000;

/// One selected row, including an explicit integrity or malformed-data failure.
#[derive(Debug)]
pub(crate) struct SourceEntry {
    pub id: Uuid,
    pub verification: Result<StoredEntry<SCALE>, PostgresError>,
}

/// Verify one entry without changing the caller's transaction or borrowing a pool slot.
/// Missing schemas/entries return `None`; malformed bytes and storage failures are errors.
/// An unsequenced entry remains readable, matching `LedgerStore::get` semantics.
pub(crate) async fn entry_in(
    connection: &mut PgConnection,
    tenant: &str,
    id: EntryId,
) -> Result<Option<StoredEntry<SCALE>>, PostgresError> {
    entries_in(connection, tenant, &[*id.as_uuid()])
        .await?
        .into_iter()
        .find(|source| source.id == *id.as_uuid())
        .map(|source| source.verification)
        .transpose()
}

/// Verify selected official entry bytes in bounded batches. Missing IDs are omitted;
/// corrupt entries retain their IDs and failures so reconciliation can report each one.
/// Database/query failures abort the entire read instead of becoming financial exceptions.
pub(crate) async fn entries_in(
    connection: &mut PgConnection,
    tenant: &str,
    ids: &[Uuid],
) -> Result<Vec<SourceEntry>, PostgresError> {
    wallet::validate_tenant_id(tenant).map_err(malformed)?;
    if ids.len() > ENTRY_LIMIT {
        return Err(malformed("ledger source entry limit exceeded"));
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let schema = format!("ledger_{tenant}");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("{schema}.entries"))
        .fetch_one(&mut *connection)
        .await?;
    if !exists {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(&format!(
        "SELECT * FROM {schema}.entries WHERE entry_id=ANY($1) ORDER BY entry_id"
    ))
    .bind(ids)
    .fetch_all(&mut *connection)
    .await?;
    let postings = sqlx::query(&format!(
        "SELECT * FROM {schema}.postings WHERE entry_id=ANY($1) \
         ORDER BY entry_id,posting_index LIMIT $2"
    ))
    .bind(ids)
    .bind(POSTING_LIMIT + 1)
    .fetch_all(&mut *connection)
    .await?;
    if postings.len() as i64 > POSTING_LIMIT {
        return Err(malformed("ledger source posting limit exceeded"));
    }
    let dimensions = sqlx::query(&format!(
        "SELECT * FROM {schema}.posting_dimensions WHERE entry_id=ANY($1) \
         ORDER BY entry_id,posting_index,axis LIMIT $2"
    ))
    .bind(ids)
    .bind(DIMENSION_LIMIT + 1)
    .fetch_all(&mut *connection)
    .await?;
    if dimensions.len() as i64 > DIMENSION_LIMIT {
        return Err(malformed("ledger source dimension limit exceeded"));
    }
    let mut posting_rows = group_by_entry(postings)?;
    let mut dimension_rows = group_by_entry(dimensions)?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("entry_id")?;
        let verification = reconstruct(
            &row,
            posting_rows.remove(&id).unwrap_or_default(),
            dimension_rows.remove(&id).unwrap_or_default(),
        );
        if let Err(PostgresError::Database(error)) = verification {
            return Err(error.into());
        }
        result.push(SourceEntry { id, verification });
    }
    Ok(result)
}

fn group_by_entry(rows: Vec<PgRow>) -> Result<BTreeMap<Uuid, Vec<PgRow>>, sqlx::Error> {
    let mut grouped = BTreeMap::<Uuid, Vec<PgRow>>::new();
    for row in rows {
        grouped
            .entry(row.try_get("entry_id")?)
            .or_default()
            .push(row);
    }
    Ok(grouped)
}

fn reconstruct(
    row: &PgRow,
    posting_rows: Vec<PgRow>,
    dimension_rows: Vec<PgRow>,
) -> Result<StoredEntry<SCALE>, PostgresError> {
    let mut dimensions = BTreeMap::<i16, Dimensions>::new();
    for dimension in dimension_rows {
        dimensions
            .entry(dimension.try_get("posting_index")?)
            .or_default()
            .set(
                Label::new(dimension.try_get::<String, _>("axis")?).map_err(malformed)?,
                Label::new(dimension.try_get::<String, _>("value")?).map_err(malformed)?,
            )
            .map_err(malformed)?;
    }
    let id: Uuid = row.try_get("entry_id")?;
    let key: Vec<u8> = row.try_get("idempotency_key")?;
    let hash = hash_from_bytes(row.try_get("content_hash")?)?;
    let mut draft = Entry::<Draft, SCALE>::new(
        EntryId::from_uuid(id),
        IdempotencyKey::new(key).map_err(malformed)?,
        row.try_get("booking_date")?,
    )
    .with_value_date(row.try_get("value_date")?)
    .with_description(
        Description::new(row.try_get::<String, _>("description")?).map_err(malformed)?,
    );
    let mut provenance = Provenance::none();
    if let Some(value) = row.try_get::<Option<String>, _>("provenance_actor")? {
        provenance = provenance.with_actor(&value).map_err(malformed)?;
    }
    if let Some(value) = row.try_get::<Option<String>, _>("provenance_source")? {
        provenance = provenance.with_source(&value).map_err(malformed)?;
    }
    if let Some(value) = row.try_get::<Option<String>, _>("provenance_correlation")? {
        provenance = provenance.with_correlation(&value).map_err(malformed)?;
    }
    draft = draft.with_provenance(provenance);
    if let Some(value) = row.try_get::<Option<String>, _>("kind")? {
        draft = draft.with_kind(Label::new(value).map_err(malformed)?);
    }
    let document_id: Option<String> = row.try_get("document_id")?;
    let document_hash: Option<Vec<u8>> = row.try_get("document_content_hash")?;
    if let Some(id) = document_id {
        let document = match document_hash {
            Some(hash) => DocumentRef::new(&id, hash_from_bytes(hash)?),
            None => DocumentRef::unverified(&id),
        };
        draft = draft.with_document(document.map_err(malformed)?);
    } else if document_hash.is_some() {
        return Err(malformed("document hash has no document identifier"));
    }
    match (
        row.try_get::<Option<Uuid>, _>("reverses")?,
        row.try_get::<Option<Date>, _>("original_booking_date")?,
    ) {
        (Some(id), Some(date)) => draft = draft.reversing(EntryId::from_uuid(id), date),
        (None, None) => {}
        _ => return Err(malformed("incomplete reversal metadata")),
    }
    for (index, posting) in posting_rows.into_iter().enumerate() {
        let stored_index: i16 = posting.try_get("posting_index")?;
        if usize::try_from(stored_index).ok() != Some(index) {
            return Err(malformed("non-contiguous posting indexes"));
        }
        let account: i32 = posting.try_get("account_index")?;
        let direction: String = posting.try_get("direction")?;
        let layer: String = posting.try_get("layer")?;
        let currency: String = posting.try_get("currency")?;
        let direction = match direction.as_str() {
            "D" => Direction::Debit,
            "C" => Direction::Credit,
            _ => return Err(malformed("unknown posting direction")),
        };
        let layer = match layer.as_str() {
            "settled" => Layer::Settled,
            "pending" => Layer::Pending,
            _ => return Err(malformed("unknown posting layer")),
        };
        draft = draft.post(
            Posting::new(
                AccountId::from_index(u32::try_from(account).map_err(malformed)?),
                direction,
                Amount::from_minor(posting.try_get("amount_minor")?),
                Currency::new(currency.trim()).map_err(malformed)?,
            )
            .in_layer(layer)
            .with_dimensions(dimensions.remove(&stored_index).unwrap_or_default()),
        );
    }
    if !dimensions.is_empty() {
        return Err(malformed("dimensions reference a missing posting"));
    }
    let index: Option<i64> = row.try_get("log_index")?;
    Ok(StoredEntry {
        index: index
            .map(|value| u64::try_from(value).map(LogIndex::new).map_err(malformed))
            .transpose()?,
        entry: draft.adopt_verified(hash)?,
        content_hash: hash,
    })
}

fn hash_from_bytes(bytes: Vec<u8>) -> Result<Hash, PostgresError> {
    Ok(Hash::from_bytes(
        bytes
            .try_into()
            .map_err(|_| malformed("hash is not 32 bytes"))?,
    ))
}

fn malformed(error: impl Display) -> PostgresError {
    PostgresError::Malformed(error.to_string())
}
