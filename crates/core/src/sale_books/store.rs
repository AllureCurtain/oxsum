//! Sale-book persistence: drafts, published versions and signed snapshots.

use crate::{
    BusinessCapability, Db, SaleBook, SaleBookHistory, SaleBookPage, SaleChange, SaleChangeRequest,
    SaleDraft, SaleModelMap, SaleMutationResult, SalePreviewRequest, SalePreviewSource, SalePrice,
    SalePricePreview, SalePriceSnapshot, SaleSchedule, SaleTerms, SaleVersion, WalletError,
    business_staff as authority,
};
use serde_json::{Value, json};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

const SCHEDULES: &str = "SELECT s.*, COALESCE(e.effective_until,s.scheduled_until) AS effective_until FROM oxsum.sale_schedules s LEFT JOIN oxsum.sale_schedule_ends e USING (schedule_id)";
fn book(row: &PgRow) -> Result<SaleBook, WalletError> {
    Ok(SaleBook {
        id: row.try_get("book_id")?,
        name: row.try_get("name")?,
        created_at: row.try_get("created_at")?,
    })
}
fn schedule(row: &PgRow) -> Result<SaleSchedule, WalletError> {
    Ok(SaleSchedule {
        id: row.try_get("schedule_id")?,
        kind: row.try_get("kind")?,
        organization_id: row.try_get("organization_id")?,
        model: row.try_get("model")?,
        book_id: row.try_get("book_id")?,
        version: row.try_get("version")?,
        terms: row
            .try_get::<Option<Value>, _>("terms")?
            .map(super::stored)
            .transpose()?,
        effective_from: row.try_get("effective_from")?,
        scheduled_until: row.try_get("scheduled_until")?,
        effective_until: row.try_get("effective_until")?,
        created_at: row.try_get("created_at")?,
    })
}
async fn owned_book(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
) -> Result<SaleBook, WalletError> {
    let row = sqlx::query("SELECT * FROM oxsum.sale_books WHERE business_id=$1 AND book_id=$2")
        .bind(business)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(super::missing)?;
    book(&row)
}
async fn customer(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    org: Uuid,
) -> Result<(), WalletError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.customer_profiles WHERE business_id=$1 AND organization_id=$2)").bind(business).bind(org).fetch_one(&mut **tx).await?;
    if !exists {
        return Err(super::missing());
    }
    Ok(())
}
async fn owned_schedule(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
) -> Result<SaleSchedule, WalletError> {
    let row = sqlx::query(&format!(
        "{SCHEDULES} WHERE s.business_id=$1 AND s.schedule_id=$2"
    ))
    .bind(business)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(super::missing)?;
    schedule(&row)
}
async fn version(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
    number: i64,
) -> Result<SaleVersion, WalletError> {
    let row=sqlx::query("SELECT v.version,v.draft_revision,v.published_at,d.models FROM oxsum.sale_versions v JOIN oxsum.sale_drafts d ON d.book_id=v.book_id AND d.revision=v.draft_revision WHERE v.business_id=$1 AND v.book_id=$2 AND v.version=$3").bind(business).bind(id).bind(number).fetch_optional(&mut **tx).await?.ok_or_else(super::missing)?;
    Ok(SaleVersion {
        version: row.try_get("version")?,
        draft_revision: row.try_get("draft_revision")?,
        models: super::stored(row.try_get("models")?)?,
        published_at: row.try_get("published_at")?,
    })
}
async fn end(
    tx: &mut Transaction<'_, Postgres>,
    current: &SaleSchedule,
    until: OffsetDateTime,
    now: OffsetDateTime,
) -> Result<(), WalletError> {
    if until < now
        || until < current.effective_from
        || current.scheduled_until.is_some_and(|old| until > old)
    {
        return Err(super::invalid(
            "a schedule end cannot be backdated, precede its start or extend its original end",
        ));
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM oxsum.sale_schedule_ends WHERE schedule_id=$1)",
    )
    .bind(current.id)
    .fetch_one(&mut **tx)
    .await?;
    if exists {
        return Err(WalletError::Conflict(
            "schedule already has an immutable end event".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO oxsum.sale_schedule_ends (schedule_id,effective_until) VALUES ($1,$2)",
    )
    .bind(current.id)
    .bind(until)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
async fn add_schedule(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    next: SaleSchedule,
    replaces: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<Uuid, WalletError> {
    if next.effective_from < now
        || next
            .effective_until
            .is_some_and(|until| until <= next.effective_from)
    {
        return Err(super::invalid(
            "sale windows must start now or later and end strictly after their start",
        ));
    }
    if let Some(org) = next.organization_id {
        customer(tx, business, org).await?;
    }
    if let (Some(id), Some(number)) = (next.book_id, next.version) {
        version(tx, business, id, number).await?;
    }
    if let Some(terms) = &next.terms {
        terms.validate()?;
    }
    if let Some(model) = &next.model {
        super::model_name(model)?;
    }
    if let Some(id) = replaces {
        let current = owned_schedule(tx, business, id).await?;
        if current.kind != next.kind
            || current.organization_id != next.organization_id
            || current.model != next.model
        {
            return Err(WalletError::Conflict(
                "replacement must have the same sale scope".into(),
            ));
        }
        end(tx, &current, next.effective_from, now).await?;
    }
    // The business row serializes all schedule writes and current role changes.
    // Half-open windows and cancelled empty windows are compared consistently.
    let overlap: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.sale_schedules s LEFT JOIN oxsum.sale_schedule_ends e USING (schedule_id) WHERE s.business_id=$1 AND s.kind=$2 AND s.organization_id IS NOT DISTINCT FROM $3 AND s.model IS NOT DISTINCT FROM $4 AND (COALESCE(e.effective_until,s.scheduled_until) IS NULL OR COALESCE(e.effective_until,s.scheduled_until)>s.effective_from) AND (COALESCE(e.effective_until,s.scheduled_until) IS NULL OR COALESCE(e.effective_until,s.scheduled_until)>$5) AND ($6::timestamptz IS NULL OR s.effective_from<$6))").bind(business).bind(&next.kind).bind(next.organization_id).bind(&next.model).bind(next.effective_from).bind(next.effective_until).fetch_one(&mut **tx).await?;
    if overlap {
        return Err(WalletError::Conflict(
            "overlapping sale windows in the same scope".into(),
        ));
    }
    sqlx::query("INSERT INTO oxsum.sale_schedules (business_id,schedule_id,kind,organization_id,model,book_id,version,terms,effective_from,scheduled_until) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(business).bind(next.id).bind(next.kind).bind(next.organization_id).bind(next.model).bind(next.book_id).bind(next.version).bind(next.terms.map(|t|json!(t))).bind(next.effective_from).bind(next.scheduled_until).execute(&mut **tx).await?;
    Ok(next.id)
}
fn new_schedule(
    kind: &str,
    start: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Result<SaleSchedule, WalletError> {
    let until = until.map(super::instant).transpose()?;
    Ok(SaleSchedule {
        id: Uuid::new_v4(),
        kind: kind.into(),
        organization_id: None,
        model: None,
        book_id: None,
        version: None,
        terms: None,
        effective_from: super::instant(start.unwrap_or(now))?,
        scheduled_until: until,
        effective_until: until,
        created_at: now,
    })
}

impl Db {
    /// Applies one authorized change to the caller's sale terms — a book,
    /// draft, version or schedule — and records the mutation result.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] when the authority token lacks the capability
    /// the change requires, plus input and storage failures.
    pub async fn change_sale_terms(
        &self,
        token: &str,
        request: &SaleChangeRequest,
    ) -> Result<SaleMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = super::read_authority(&mut tx, business, token).await?;
        let capability = if let SaleChange::EndSchedule { schedule_id, .. } = &request.change {
            if owned_schedule(&mut tx, business, *schedule_id).await?.kind == "default" {
                BusinessCapability::CatalogWrite
            } else {
                BusinessCapability::CommercialWrite
            }
        } else {
            request.change.capability()
        };
        if !actor.role.allows(capability) {
            return Err(WalletError::Forbidden(
                "business capability is not granted".into(),
            ));
        }
        let input = json!({"reason":request.reason,"change":request.change});
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            request.change.action(),
            &input,
        )
        .await?
        {
            let row = sqlx::query(
                "SELECT resource_id,revision FROM oxsum.sale_mutation_results WHERE mutation_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            let result = SaleMutationResult {
                id,
                resource_id: row.try_get("resource_id")?,
                revision: row.try_get("revision")?,
            };
            tx.commit().await?;
            return Ok(result);
        }
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let (resource_id, revision) = match &request.change {
            SaleChange::CreateBook { name } => {
                if name.trim() != name || name.is_empty() || name.chars().count() > 200 {
                    return Err(super::invalid(
                        "book name must contain 1 to 200 characters without surrounding whitespace",
                    ));
                }
                let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.sale_books WHERE business_id=$1 AND name=$2)").bind(business).bind(name).fetch_one(&mut *tx).await?;
                if exists {
                    return Err(WalletError::Conflict(
                        "sale book name is already used".into(),
                    ));
                }
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO oxsum.sale_books (business_id,book_id,name) VALUES ($1,$2,$3)",
                )
                .bind(business)
                .bind(id)
                .bind(name)
                .execute(&mut *tx)
                .await?;
                (id, None)
            }
            SaleChange::SaveDraft {
                book_id,
                expected_revision,
                models,
            } => {
                owned_book(&mut tx, business, *book_id).await?;
                super::types::validate_models(models)?;
                let current: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(max(revision),0) FROM oxsum.sale_drafts WHERE book_id=$1",
                )
                .bind(book_id)
                .fetch_one(&mut *tx)
                .await?;
                if current != *expected_revision {
                    return Err(WalletError::Conflict("draft revision changed".into()));
                }
                let next = current
                    .checked_add(1)
                    .ok_or_else(|| super::invalid("sale revision exhausted"))?;
                sqlx::query(
                    "INSERT INTO oxsum.sale_drafts (book_id,revision,models) VALUES ($1,$2,$3)",
                )
                .bind(book_id)
                .bind(next)
                .bind(json!(models))
                .execute(&mut *tx)
                .await?;
                (*book_id, Some(next))
            }
            SaleChange::PublishDraft {
                book_id,
                draft_revision,
            } => {
                owned_book(&mut tx, business, *book_id).await?;
                let models: Value = sqlx::query_scalar(
                    "SELECT models FROM oxsum.sale_drafts WHERE book_id=$1 AND revision=$2",
                )
                .bind(book_id)
                .bind(draft_revision)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(super::missing)?;
                super::types::validate_models(&super::stored(models)?)?;
                let published:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.sale_versions WHERE book_id=$1 AND draft_revision=$2)").bind(book_id).bind(draft_revision).fetch_one(&mut *tx).await?;
                if published {
                    return Err(WalletError::Conflict(
                        "draft revision is already published".into(),
                    ));
                }
                let current: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(max(version),0) FROM oxsum.sale_versions WHERE book_id=$1",
                )
                .bind(book_id)
                .fetch_one(&mut *tx)
                .await?;
                let next = current
                    .checked_add(1)
                    .ok_or_else(|| super::invalid("sale version exhausted"))?;
                sqlx::query("INSERT INTO oxsum.sale_versions (business_id,book_id,version,draft_revision) VALUES ($1,$2,$3,$4)").bind(business).bind(book_id).bind(next).bind(draft_revision).execute(&mut *tx).await?;
                (*book_id, Some(next))
            }
            SaleChange::ScheduleDefault {
                book_id,
                version,
                effective_from,
                effective_until,
                replaces,
            } => {
                let mut next = new_schedule("default", *effective_from, *effective_until, now)?;
                next.book_id = Some(*book_id);
                next.version = Some(*version);
                (
                    add_schedule(&mut tx, business, next, *replaces, now).await?,
                    None,
                )
            }
            SaleChange::AssignBook {
                organization_id,
                book_id,
                version,
                effective_from,
                effective_until,
                replaces,
            } => {
                let mut next = new_schedule("assigned", *effective_from, *effective_until, now)?;
                next.organization_id = Some(*organization_id);
                next.book_id = Some(*book_id);
                next.version = Some(*version);
                (
                    add_schedule(&mut tx, business, next, *replaces, now).await?,
                    None,
                )
            }
            SaleChange::OverrideModel {
                organization_id,
                model,
                terms,
                effective_from,
                effective_until,
                replaces,
            } => {
                let mut next = new_schedule("override", *effective_from, *effective_until, now)?;
                next.organization_id = Some(*organization_id);
                next.model = Some(model.clone());
                next.terms = Some((**terms).clone());
                (
                    add_schedule(&mut tx, business, next, *replaces, now).await?,
                    None,
                )
            }
            SaleChange::EndSchedule {
                schedule_id,
                effective_until,
            } => {
                let current = owned_schedule(&mut tx, business, *schedule_id).await?;
                end(&mut tx, &current, super::instant(*effective_until)?, now).await?;
                (*schedule_id, None)
            }
        };
        let id = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            request.change.action(),
            &input,
        )
        .await?
        .id;
        sqlx::query("INSERT INTO oxsum.sale_mutation_results (mutation_id,resource_id,revision) VALUES ($1,$2,$3)").bind(id).bind(resource_id).bind(revision).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(SaleMutationResult {
            id,
            resource_id,
            revision,
        })
    }

    /// Lists the caller's sale books after the `after` cursor, newest key
    /// order, with a `next_after` when more remain. `limit` is 1..=100.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for an out-of-range limit, plus authority
    /// and storage failures.
    pub async fn sale_books(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<SaleBookPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(super::invalid("limit must be 1..=100"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        super::read_authority(&mut tx, business, token).await?;
        let rows=sqlx::query("SELECT * FROM oxsum.sale_books WHERE business_id=$1 AND ($2::uuid IS NULL OR book_id>$2) ORDER BY book_id LIMIT $3").bind(business).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut books = rows.iter().map(book).collect::<Result<Vec<_>, _>>()?;
        let more = books.len() > limit as usize;
        books.truncate(limit as usize);
        let next_after = if more {
            books.last().map(|b| b.id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(SaleBookPage { books, next_after })
    }
    /// Returns one sale book with its draft revisions, published versions and
    /// schedules, in creation order.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] when the book is not the caller's, plus
    /// authority and storage failures.
    pub async fn sale_book_history(
        &self,
        token: &str,
        id: Uuid,
    ) -> Result<SaleBookHistory, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        super::read_authority(&mut tx, business, token).await?;
        let book = owned_book(&mut tx, business, id).await?;
        let rows=sqlx::query("SELECT revision,models,created_at FROM oxsum.sale_drafts WHERE book_id=$1 ORDER BY revision").bind(id).fetch_all(&mut *tx).await?;
        let drafts = rows
            .iter()
            .map(|r| {
                Ok(SaleDraft {
                    revision: r.try_get("revision")?,
                    models: super::stored(r.try_get("models")?)?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, WalletError>>()?;
        let rows=sqlx::query("SELECT v.version,v.draft_revision,v.published_at,d.models FROM oxsum.sale_versions v JOIN oxsum.sale_drafts d ON d.book_id=v.book_id AND d.revision=v.draft_revision WHERE v.business_id=$1 AND v.book_id=$2 ORDER BY v.version").bind(business).bind(id).fetch_all(&mut *tx).await?;
        let versions = rows
            .iter()
            .map(|r| {
                Ok(SaleVersion {
                    version: r.try_get("version")?,
                    draft_revision: r.try_get("draft_revision")?,
                    models: super::stored(r.try_get("models")?)?,
                    published_at: r.try_get("published_at")?,
                })
            })
            .collect::<Result<Vec<_>, WalletError>>()?;
        let rows=sqlx::query(&format!("{SCHEDULES} WHERE s.business_id=$1 AND s.book_id=$2 ORDER BY s.effective_from,s.schedule_id")).bind(business).bind(id).fetch_all(&mut *tx).await?;
        let schedules = rows.iter().map(schedule).collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(SaleBookHistory {
            book,
            drafts,
            versions,
            schedules,
        })
    }
    /// Returns the sale schedules that apply to one customer organization,
    /// ordered by effective time.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] when `org` is not the caller's customer, plus
    /// authority and storage failures.
    pub async fn customer_sale_terms(
        &self,
        token: &str,
        org: Uuid,
    ) -> Result<Vec<SaleSchedule>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        super::read_authority(&mut tx, business, token).await?;
        customer(&mut tx, business, org).await?;
        let rows=sqlx::query(&format!("{SCHEDULES} WHERE s.business_id=$1 AND s.organization_id=$2 ORDER BY s.effective_from,s.schedule_id")).bind(business).bind(org).fetch_all(&mut *tx).await?;
        let schedules = rows.iter().map(schedule).collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(schedules)
    }
    /// Computes what one model would cost for a customer under the sale terms
    /// effective at the request's `at` time, without writing anything.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] when the customer is not the caller's, plus
    /// input and storage failures.
    pub async fn preview_sale_price(
        &self,
        token: &str,
        request: &SalePreviewRequest,
    ) -> Result<SalePricePreview, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        super::read_authority(&mut tx, business, token).await?;
        customer(&mut tx, business, request.organization_id).await?;
        super::model_name(&request.model)?;
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let at = super::instant(request.at.unwrap_or(now))?;
        let mut snapshot = match &request.source {
            SalePreviewSource::Effective => {
                resolve(
                    &mut tx,
                    business,
                    request.organization_id,
                    &request.model,
                    at,
                )
                .await?
            }
            SalePreviewSource::Draft { book_id, revision } => {
                owned_book(&mut tx, business, *book_id).await?;
                let row=sqlx::query("SELECT models,created_at FROM oxsum.sale_drafts WHERE book_id=$1 AND revision=$2").bind(book_id).bind(revision).fetch_optional(&mut *tx).await?.ok_or_else(super::missing)?;
                let mut models: SaleModelMap = super::stored(row.try_get("models")?)?;
                let terms = models.remove(&request.model).ok_or_else(super::missing)?;
                SalePriceSnapshot {
                    business_id: business,
                    organization_id: request.organization_id,
                    model: request.model.clone(),
                    at,
                    source: "draft".into(),
                    schedule_id: None,
                    book_id: Some(*book_id),
                    version: None,
                    draft_revision: Some(*revision),
                    legacy_channel: None,
                    effective_from: row.try_get("created_at")?,
                    effective_until: None,
                    terms,
                }
            }
            SalePreviewSource::Legacy { channel, version } => {
                if *version < 1 || *version > i64::from(i32::MAX) {
                    return Err(super::invalid("legacy version must be 1..=2147483647"));
                }
                let row=sqlx::query("SELECT p.* FROM oxsum.channel_prices p JOIN oxsum.channels c USING (channel_id) WHERE c.business_id=$1 AND c.name=$2 AND p.model=$3 AND p.version=$4").bind(business).bind(channel).bind(&request.model).bind(*version as i32).fetch_optional(&mut *tx).await?.ok_or_else(super::missing)?;
                let terms = SaleTerms {
                    price: SalePrice::legacy(crate::channels::price_from_row(&row)?)?,
                    discount_percent: 0,
                };
                SalePriceSnapshot {
                    business_id: business,
                    organization_id: request.organization_id,
                    model: request.model.clone(),
                    at,
                    source: "legacy".into(),
                    schedule_id: None,
                    book_id: None,
                    version: Some(*version),
                    draft_revision: None,
                    legacy_channel: Some(channel.clone()),
                    effective_from: row.try_get("created_at")?,
                    effective_until: None,
                    terms,
                }
            }
        };
        snapshot.at = at;
        let result = snapshot.preview(request.usage.usage())?;
        tx.commit().await?;
        Ok(result)
    }
    /// Internal consumer boundary: business/customer IDs must come from admission.
    /// Does not grant serving permission or turn a draft/legacy snapshot into live terms.
    pub async fn resolve_sale_price(
        &self,
        business: Uuid,
        org: Uuid,
        model: &str,
        at: OffsetDateTime,
    ) -> Result<SalePriceSnapshot, WalletError> {
        let mut tx = self.pool().begin().await?;
        if authority::lock_business(&mut tx).await? != business {
            return Err(super::missing());
        }
        customer(&mut tx, business, org).await?;
        super::model_name(model)?;
        let result = resolve(&mut tx, business, org, model, super::instant(at)?).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Reads the canonical snapshot bytes a committed digest names, plus the
    /// business whose catalog signed it (issue #600).
    ///
    /// The proof path and the `/api/v1/log/sale-head` endpoint both serve the
    /// stored row verbatim — the digest was taken over those exact bytes, so
    /// re-serving them keeps the checkpoint's root honest. `None` when no
    /// resolution recorded the digest.
    pub async fn sale_snapshot_by_digest(
        &self,
        digest: &str,
    ) -> Result<Option<(uuid::Uuid, String)>, WalletError> {
        let row = sqlx::query(
            "SELECT business_id, canonical_row FROM oxsum.sale_snapshot_digests WHERE digest=$1",
        )
        .bind(digest)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| {
            (
                r.try_get("business_id").unwrap_or_default(),
                r.try_get("canonical_row").unwrap_or_default(),
            )
        }))
    }

    /// Signs a resolved sale snapshot as an operator checkpoint: the
    /// `/api/v1/log/sale-head` response (issue #600).
    ///
    /// `digest` selects a stored snapshot directly — the value a settlement
    /// committed. Otherwise `organization_id`, `model` and `at` re-resolve the
    /// terms in force; the resolution records its digest so the signed row is
    /// recoverable forever. Either way the served `row` is the canonical bytes
    /// the checkpoint's root commits to.
    pub async fn sign_sale_snapshot(
        &self,
        key: &crate::HeadSigningKey,
        digest: Option<&str>,
        organization_id: Option<Uuid>,
        model: Option<&str>,
        at: Option<OffsetDateTime>,
    ) -> Result<Option<(crate::SignedSaleHead, String)>, WalletError> {
        // By digest: the stored canonical row re-signs deterministically — the
        // checkpoint's root is the digest itself, so a bill that committed it
        // stays verifiable however the catalog has since moved.
        if let Some(digest) = digest {
            let Some((business, canonical)) = self.sale_snapshot_by_digest(digest).await? else {
                return Ok(None);
            };
            // The signed head needs the snapshot's identity fields; the
            // canonical row carries them all (it is what they were hashed from),
            // so read them back out of it rather than re-resolving.
            let snapshot: serde_json::Value = serde_json::from_str(&canonical)
                .map_err(|e| WalletError::Misconfigured(format!("stored sale row: {e}")))?;
            let version = snapshot
                .get("version")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let organization = snapshot
                .get("organizationId")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse().ok())
                .unwrap_or_default();
            let model = snapshot
                .get("model")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let signed = crate::sign_sale_checkpoint(
                key,
                business,
                organization,
                &model,
                version,
                &canonical,
            )
            .map_err(crate::error::invalid)?;
            return Ok(Some((signed, canonical)));
        }
        // By terms: re-resolve what was in force and sign the fresh snapshot.
        let (Some(org), Some(model)) = (organization_id, model) else {
            return Err(super::invalid(
                "sale-head needs a digest or organizationId and model",
            ));
        };
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        customer(&mut tx, business, org).await?;
        super::model_name(model)?;
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let snapshot = resolve(
            &mut tx,
            business,
            org,
            model,
            super::instant(at.unwrap_or(now))?,
        )
        .await?;
        tx.commit().await?;
        let canonical = crate::canonical_sale_snapshot(&snapshot)?;
        let signed =
            crate::sign_sale_head(key, &snapshot, &canonical).map_err(crate::error::invalid)?;
        Ok(Some((signed, canonical)))
    }
}
async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    org: Uuid,
    model: &str,
    at: OffsetDateTime,
) -> Result<SalePriceSnapshot, WalletError> {
    let rows=sqlx::query(&format!("{SCHEDULES} WHERE s.business_id=$1 AND (s.kind='default' OR (s.organization_id=$2 AND (s.kind='assigned' OR s.model=$3))) AND s.effective_from<=$4 AND (COALESCE(e.effective_until,s.scheduled_until) IS NULL OR $4<COALESCE(e.effective_until,s.scheduled_until)) ORDER BY CASE s.kind WHEN 'override' THEN 0 WHEN 'assigned' THEN 1 ELSE 2 END")).bind(business).bind(org).bind(model).bind(at).fetch_all(&mut **tx).await?;
    let schedules = rows.iter().map(schedule).collect::<Result<Vec<_>, _>>()?;
    // Refuse corrupted ambiguity instead of relying on row order to choose a bill.
    for kind in ["override", "assigned", "default"] {
        if schedules.iter().filter(|s| s.kind == kind).count() > 1 {
            return Err(WalletError::Misconfigured(
                "ambiguous active sale terms".into(),
            ));
        }
    }
    for current in schedules {
        let (terms, draft_revision) = if let Some(terms) = current.terms {
            (Some(terms), None)
        } else if let (Some(id), Some(number)) = (current.book_id, current.version) {
            let mut published = version(tx, business, id, number).await?;
            (
                published.models.remove(model),
                Some(published.draft_revision),
            )
        } else {
            return Err(WalletError::Misconfigured("invalid sale schedule".into()));
        };
        if let Some(terms) = terms {
            let snapshot = SalePriceSnapshot {
                business_id: business,
                organization_id: org,
                model: model.into(),
                at,
                source: current.kind,
                schedule_id: Some(current.id),
                book_id: current.book_id,
                version: current.version,
                draft_revision,
                legacy_channel: None,
                effective_from: current.effective_from,
                effective_until: current.effective_until,
                terms,
            };
            record_digest(tx, &snapshot).await?;
            return Ok(snapshot);
        }
    }
    Err(WalletError::NotFound(
        "no active sale price for this model and customer".into(),
    ))
}

/// Persists the snapshot's digest → canonical-row mapping so a later proof can
/// re-serve the exact bytes the settlement committed (issue #600).
///
/// The digest is content-derived — `sale_snapshot_hash` over the canonical
/// `SaleSnapshotV1` bytes — so the insert is idempotent: two resolutions of the
/// same terms write the same row and `ON CONFLICT` does nothing. The canonical
/// row is stored, never re-rendered, because the checkpoint's root commits to
/// those exact bytes.
async fn record_digest(
    tx: &mut Transaction<'_, Postgres>,
    snapshot: &SalePriceSnapshot,
) -> Result<crate::Hash, WalletError> {
    let canonical = crate::canonical_sale_snapshot(snapshot)?;
    let digest = crate::sale_snapshot_hash(&canonical);
    sqlx::query(
        "INSERT INTO oxsum.sale_snapshot_digests
             (digest, business_id, organization_id, model, canonical_row, version)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (digest) DO NOTHING",
    )
    .bind(digest.to_hex())
    .bind(snapshot.business_id)
    .bind(snapshot.organization_id)
    .bind(&snapshot.model)
    .bind(&canonical)
    .bind(snapshot.version.unwrap_or(0))
    .execute(&mut **tx)
    .await?;
    Ok(digest)
}
