//! Current authority, exact replay and append-only source/comparison history.
use super::{SupplierComparisonRequest, SupplierStatementImport, comparison, parser};
use crate::{BusinessCapability, Db, WalletError, business_staff as authority};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

fn digest(value: &Value) -> Result<String, WalletError> {
    let bytes = serde_json::to_vec(value).map_err(crate::error::invalid)?;
    let mut h = Sha256::new();
    h.update(b"oxsum/supplier-statement/v1\0");
    h.update(bytes);
    Ok(doubleentry::Hash::from_bytes(h.finalize().into()).to_hex())
}
fn contention(error: sqlx::Error) -> WalletError {
    if error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| matches!(code.as_ref(), "40001" | "40P01"))
    {
        WalletError::PoolBusy
    } else {
        error.into()
    }
}
async fn lock_snapshot(tx: &mut Transaction<'_, Postgres>) -> Result<Uuid, WalletError> {
    // Touch the lock row: two snapshot writers cannot keep an older source
    // history after the preceding writer releases a lock-only Business row.
    sqlx::query_scalar("UPDATE oxsum.businesses SET name=name WHERE business_id=(SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE SKIP LOCKED) RETURNING business_id").fetch_optional(&mut **tx).await.map_err(contention)?.ok_or(WalletError::PoolBusy)
}
async fn lock_staff(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    actor: Uuid,
) -> Result<(), WalletError> {
    // RR locking reads reject membership changes committed after the snapshot.
    sqlx::query(
        "SELECT user_id FROM oxsum.business_staff WHERE business_id=$1 AND user_id=$2 FOR SHARE",
    )
    .bind(business)
    .bind(actor)
    .fetch_optional(&mut **tx)
    .await
    .map_err(contention)?
    .ok_or_else(|| WalletError::Forbidden("current Business membership is required".into()))?;
    Ok(())
}
async fn channels(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    ids: Option<&[Uuid]>,
) -> Result<Vec<Value>, WalletError> {
    let rows=sqlx::query("SELECT channel_id,name,base_url,protocol,updated_at FROM oxsum.channels WHERE business_id=$1 AND ($2::uuid[] IS NULL OR channel_id=ANY($2)) ORDER BY channel_id").bind(business).bind(ids).fetch_all(&mut **tx).await?;
    let mut out = Vec::new();
    for row in rows {
        let origin: String = row.try_get("base_url")?;
        let protocol: String = row.try_get("protocol")?;
        if origin.trim_end_matches('/') != "https://api.openai.com/v1" || protocol != "openai" {
            if ids.is_some() {
                return Err(parser::bad("selected channel is not canonical OpenAI"));
            }
            continue;
        }
        out.push(json!({"channelId":row.try_get::<Uuid,_>("channel_id")?,"name":row.try_get::<String,_>("name")?,"baseUrl":origin,"configurationUpdatedAt":row.try_get::<time::OffsetDateTime,_>("updated_at")?.to_string()}));
    }
    if ids.is_some_and(|ids| ids.len() != out.len()) {
        return Err(parser::bad(
            "channel mapping includes missing or foreign channels",
        ));
    }
    Ok(out)
}
async fn source_in(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
    comparison_id: Option<Uuid>,
) -> Result<Value, WalletError> {
    let row=sqlx::query("SELECT s.*,m.actor_id FROM oxsum.supplier_statement_sources s JOIN oxsum.business_mutations m ON m.mutation_id=s.source_id WHERE s.business_id=$1 AND s.source_id=$2").bind(business).bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("supplier statement".into()))?;
    let comp=sqlx::query("SELECT comparison_id,document,digest,canonical FROM oxsum.supplier_statement_comparisons WHERE source_id=$1 AND (($2::uuid IS NULL AND mutation_id=$1) OR comparison_id=$2)").bind(id).bind(comparison_id).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("supplier comparison".into()))?;
    Ok(
        json!({"schemaVersion":1,"sourceId":id,"sourceVersion":row.try_get::<i64,_>("source_version")?,"importedBy":row.try_get::<Uuid,_>("actor_id")?,"capturedAt":row.try_get::<time::OffsetDateTime,_>("captured_at")?.to_string(),"digest":row.try_get::<String,_>("digest")?,"canonical":row.try_get::<String,_>("canonical")?,"digestDomain":"SHA-256 of UTF-8 oxsum/supplier-statement/v1, one NUL byte, then exact canonical UTF-8 bytes","source":row.try_get::<Value,_>("request")?,"parsed":row.try_get::<Value,_>("parsed")?,"channelSnapshot":row.try_get::<Value,_>("channel_snapshot")?,"comparison":json!({"comparisonId":comp.try_get::<Uuid,_>("comparison_id")?,"document":comp.try_get::<Value,_>("document")?,"digest":comp.try_get::<String,_>("digest")?,"canonical":comp.try_get::<String,_>("canonical")?})}),
    )
}
async fn append_comparison(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    source: Uuid,
    mutation: Uuid,
    request: &SupplierStatementImport,
    parsed: &parser::Parsed,
) -> Result<Value, WalletError> {
    let mut document = comparison::capture(tx, business, source, request, parsed).await?;
    let actor =
        sqlx::query("SELECT actor_id,request FROM oxsum.business_mutations WHERE mutation_id=$1")
            .bind(mutation)
            .fetch_one(&mut **tx)
            .await?;
    let detail: Value = actor.try_get("request")?;
    document["mutationId"] = json!(mutation);
    document["actorId"] = json!(actor.try_get::<Uuid, _>("actor_id")?);
    document["reason"] = if mutation == source {
        detail["reason"].clone()
    } else {
        detail["request"]["reason"].clone()
    };
    let id = Uuid::new_v4();
    document["comparisonId"] = json!(id);
    let canonical = serde_json::to_string(&document).map_err(crate::error::invalid)?;
    let hash = digest(&document)?;
    sqlx::query("INSERT INTO oxsum.supplier_statement_comparisons(comparison_id,source_id,mutation_id,document,digest,canonical) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(source).bind(mutation).bind(&document).bind(&hash).bind(&canonical).execute(&mut **tx).await?;
    Ok(json!({"comparisonId":id,"document":document,"digest":hash,"canonical":canonical}))
}
impl Db {
    /// Redacted canonical provider choices under current report authority.
    pub async fn supplier_statement_choices(&self, token: &str) -> Result<Vec<Value>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let result = channels(&mut tx, business, None).await;
        tx.rollback().await?;
        Ok(result?
            .into_iter()
            .map(|v| json!({"channelId":v["channelId"],"name":v["name"]}))
            .collect())
    }
    /// Validate lossless pages and channel assertions without creating a source.
    pub async fn preview_supplier_statement(
        &self,
        token: &str,
        request: &SupplierStatementImport,
    ) -> Result<Value, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let result = channels(&mut tx, business, Some(&request.channel_ids)).await;
        tx.rollback().await?;
        result?;
        Ok(parser::parse(request)?.document)
    }
    /// Commit an immutable source and first frozen comparison, atomically with audit.
    pub async fn import_supplier_statement(
        &self,
        token: &str,
        request: &SupplierStatementImport,
    ) -> Result<Value, WalletError> {
        let parsed = parser::parse(request)?;
        let input = serde_json::to_value(request).map_err(crate::error::invalid)?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let business = lock_snapshot(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        lock_staff(&mut tx, business, actor.user_id).await?;
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "supplier_statement.import",
            &input,
        )
        .await?
        {
            let result = source_in(&mut tx, business, id, None).await?;
            tx.commit().await?;
            return Ok(result);
        }
        let snapshot = channels(&mut tx, business, Some(&request.channel_ids)).await?;
        let scope = digest(
            &json!({"provider":"openai","organization":request.provider_organization_id,"day":request.bill_date,"kind":request.scope_kind,"projects":request.project_ids}),
        )?;
        let version:i64=sqlx::query_scalar("SELECT COALESCE(max(source_version),0)+1 FROM oxsum.supplier_statement_sources WHERE business_id=$1 AND scope_key=$2").bind(business).bind(&scope).fetch_one(&mut *tx).await?;
        let id = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "supplier_statement.import",
            &input,
        )
        .await?
        .id;
        let canonical_value = json!({"sourceId":id,"sourceVersion":version,"importedBy":actor.user_id,"source":input,"parsed":parsed.document,"channelSnapshot":snapshot});
        let hash = digest(&canonical_value)?;
        let canonical = serde_json::to_string(&canonical_value).map_err(crate::error::invalid)?;
        sqlx::query("INSERT INTO oxsum.supplier_statement_sources(source_id,business_id,scope_key,source_version,bill_date,request,parsed,digest,channel_snapshot,canonical) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)").bind(id).bind(business).bind(scope).bind(version).bind(parsed.day).bind(input).bind(&parsed.document).bind(hash).bind(serde_json::to_value(snapshot).map_err(crate::error::invalid)?).bind(canonical).execute(&mut *tx).await?;
        append_comparison(&mut tx, business, id, id, request, &parsed).await?;
        let result = source_in(&mut tx, business, id, None).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Read a retained source and comparison versions without recomputing anything.
    pub async fn supplier_statement(
        &self,
        token: &str,
        id: Uuid,
        comparison: Option<Uuid>,
    ) -> Result<Value, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let result = source_in(&mut tx, business, id, comparison).await;
        // Release the authority lock before returning even on a missing source.
        // Drop only queues rollback; a following nonwaiting writer may run first.
        tx.rollback().await?;
        result
    }
    /// Bounded comparison metadata; exact detail takes a comparison ID.
    pub async fn supplier_statement_comparisons(
        &self,
        token: &str,
        source: Uuid,
        before: Option<Uuid>,
    ) -> Result<Vec<Value>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let rows=sqlx::query("SELECT c.comparison_id,c.captured_at,c.document->>'actorId' AS actor,c.document->>'reason' AS reason,c.digest FROM oxsum.supplier_statement_comparisons c JOIN oxsum.supplier_statement_sources s USING(source_id) WHERE s.business_id=$1 AND c.source_id=$2 AND ($3::uuid IS NULL OR (c.captured_at,c.comparison_id)<(SELECT captured_at,comparison_id FROM oxsum.supplier_statement_comparisons WHERE source_id=$2 AND comparison_id=$3)) ORDER BY c.captured_at DESC,c.comparison_id DESC LIMIT 50").bind(business).bind(source).bind(before).fetch_all(&mut *tx).await?;
        tx.rollback().await?;
        rows.into_iter().map(|r|Ok(json!({"comparisonId":r.try_get::<Uuid,_>("comparison_id")?,"capturedAt":r.try_get::<time::OffsetDateTime,_>("captured_at")?.to_string(),"actorId":r.try_get::<String,_>("actor")?,"reason":r.try_get::<String,_>("reason")?,"digest":r.try_get::<String,_>("digest")?}))).collect()
    }
    /// Capture current evidence under the original asserted day, mapping and conversion.
    pub async fn compare_supplier_statement(
        &self,
        token: &str,
        id: Uuid,
        request: &SupplierComparisonRequest,
    ) -> Result<Value, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let input = json!({"sourceId":id,"request":request});
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let business = lock_snapshot(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        lock_staff(&mut tx, business, actor.user_id).await?;
        if let Some(mutation) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "supplier_statement.compare",
            &input,
        )
        .await?
        {
            let row=sqlx::query("SELECT comparison_id,document,digest,canonical FROM oxsum.supplier_statement_comparisons WHERE mutation_id=$1").bind(mutation).fetch_one(&mut *tx).await?;
            tx.rollback().await?;
            return Ok(
                json!({"comparisonId":row.try_get::<Uuid,_>("comparison_id")?,"document":row.try_get::<Value,_>("document")?,"digest":row.try_get::<String,_>("digest")?,"canonical":row.try_get::<String,_>("canonical")?}),
            );
        }
        let source = source_in(&mut tx, business, id, None).await?;
        let original: SupplierStatementImport =
            serde_json::from_value(source["source"].clone()).map_err(crate::error::invalid)?;
        let parsed = parser::parse(&original)?;
        let mutation = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "supplier_statement.compare",
            &input,
        )
        .await?
        .id;
        let result = append_comparison(&mut tx, business, id, mutation, &original, &parsed).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Bounded chronological source history; raw pages remain on exact detail reads.
    pub async fn supplier_statements(
        &self,
        token: &str,
        before: Option<Uuid>,
    ) -> Result<Vec<Value>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let rows=sqlx::query("SELECT source_id,source_version,parsed,captured_at,digest FROM oxsum.supplier_statement_sources WHERE business_id=$1 AND ($2::uuid IS NULL OR (captured_at,source_id)<(SELECT captured_at,source_id FROM oxsum.supplier_statement_sources WHERE business_id=$1 AND source_id=$2)) ORDER BY captured_at DESC,source_id DESC LIMIT 50").bind(business).bind(before).fetch_all(&mut *tx).await?;
        tx.rollback().await?;
        rows.into_iter().map(|r|Ok(json!({"sourceId":r.try_get::<Uuid,_>("source_id")?,"sourceVersion":r.try_get::<i64,_>("source_version")?,"parsed":r.try_get::<Value,_>("parsed")?,"capturedAt":r.try_get::<time::OffsetDateTime,_>("captured_at")?.to_string(),"digest":r.try_get::<String,_>("digest")?}))).collect()
    }
}
