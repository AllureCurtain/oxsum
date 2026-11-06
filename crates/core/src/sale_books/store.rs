use super::*;
use crate::Db;
use serde_json::json;
use sqlx::{Row, postgres::PgRow};

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
            .map(stored)
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
        .ok_or_else(missing)?;
    book(&row)
}
async fn customer(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    org: Uuid,
) -> Result<(), WalletError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.customer_profiles WHERE business_id=$1 AND organization_id=$2)").bind(business).bind(org).fetch_one(&mut **tx).await?;
    if !exists {
        return Err(missing());
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
    .ok_or_else(missing)?;
    schedule(&row)
}
async fn version(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
    number: i64,
) -> Result<SaleVersion, WalletError> {
    let row=sqlx::query("SELECT v.version,v.draft_revision,v.published_at,d.models FROM oxsum.sale_versions v JOIN oxsum.sale_drafts d ON d.book_id=v.book_id AND d.revision=v.draft_revision WHERE v.business_id=$1 AND v.book_id=$2 AND v.version=$3").bind(business).bind(id).bind(number).fetch_optional(&mut **tx).await?.ok_or_else(missing)?;
    Ok(SaleVersion {
        version: row.try_get("version")?,
        draft_revision: row.try_get("draft_revision")?,
        models: stored(row.try_get("models")?)?,
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
        return Err(invalid(
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
        return Err(invalid(
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
        model_name(model)?;
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
    let until = until.map(instant).transpose()?;
    Ok(SaleSchedule {
        id: Uuid::new_v4(),
        kind: kind.into(),
        organization_id: None,
        model: None,
        book_id: None,
        version: None,
        terms: None,
        effective_from: instant(start.unwrap_or(now))?,
        scheduled_until: until,
        effective_until: until,
        created_at: now,
    })
}

impl Db {
    pub async fn change_sale_terms(
        &self,
        token: &str,
        request: &SaleChangeRequest,
    ) -> Result<SaleMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = read_authority(&mut tx, business, token).await?;
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
                    return Err(invalid(
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
                validate_models(models)?;
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
                    .ok_or_else(|| invalid("sale revision exhausted"))?;
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
                .ok_or_else(missing)?;
                validate_models(&stored(models)?)?;
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
                    .ok_or_else(|| invalid("sale version exhausted"))?;
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
                next.terms = Some(terms.clone());
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
                end(&mut tx, &current, instant(*effective_until)?, now).await?;
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

    pub async fn sale_books(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<SaleBookPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..=100"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        read_authority(&mut tx, business, token).await?;
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
    pub async fn sale_book_history(
        &self,
        token: &str,
        id: Uuid,
    ) -> Result<SaleBookHistory, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        read_authority(&mut tx, business, token).await?;
        let book = owned_book(&mut tx, business, id).await?;
        let rows=sqlx::query("SELECT revision,models,created_at FROM oxsum.sale_drafts WHERE book_id=$1 ORDER BY revision").bind(id).fetch_all(&mut *tx).await?;
        let drafts = rows
            .iter()
            .map(|r| {
                Ok(SaleDraft {
                    revision: r.try_get("revision")?,
                    models: stored(r.try_get("models")?)?,
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
                    models: stored(r.try_get("models")?)?,
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
    pub async fn customer_sale_terms(
        &self,
        token: &str,
        org: Uuid,
    ) -> Result<Vec<SaleSchedule>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        read_authority(&mut tx, business, token).await?;
        customer(&mut tx, business, org).await?;
        let rows=sqlx::query(&format!("{SCHEDULES} WHERE s.business_id=$1 AND s.organization_id=$2 ORDER BY s.effective_from,s.schedule_id")).bind(business).bind(org).fetch_all(&mut *tx).await?;
        let schedules = rows.iter().map(schedule).collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(schedules)
    }
    pub async fn preview_sale_price(
        &self,
        token: &str,
        request: &SalePreviewRequest,
    ) -> Result<SalePricePreview, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        read_authority(&mut tx, business, token).await?;
        customer(&mut tx, business, request.organization_id).await?;
        model_name(&request.model)?;
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let at = instant(request.at.unwrap_or(now))?;
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
                let row=sqlx::query("SELECT models,created_at FROM oxsum.sale_drafts WHERE book_id=$1 AND revision=$2").bind(book_id).bind(revision).fetch_optional(&mut *tx).await?.ok_or_else(missing)?;
                let mut models: SaleModelMap = stored(row.try_get("models")?)?;
                let terms = models.remove(&request.model).ok_or_else(missing)?;
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
                    return Err(invalid("legacy version must be 1..=2147483647"));
                }
                let row=sqlx::query("SELECT p.* FROM oxsum.channel_prices p JOIN oxsum.channels c USING (channel_id) WHERE c.business_id=$1 AND c.name=$2 AND p.model=$3 AND p.version=$4").bind(business).bind(channel).bind(&request.model).bind(*version as i32).fetch_optional(&mut *tx).await?.ok_or_else(missing)?;
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
            return Err(missing());
        }
        customer(&mut tx, business, org).await?;
        model_name(model)?;
        let result = resolve(&mut tx, business, org, model, instant(at)?).await?;
        tx.commit().await?;
        Ok(result)
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
            return Ok(SalePriceSnapshot {
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
            });
        }
    }
    Err(WalletError::NotFound(
        "no active sale price for this model and customer".into(),
    ))
}
