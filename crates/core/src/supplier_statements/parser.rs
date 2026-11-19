//! Strict real OpenAI Costs format, preserving raw numeric lexemes.
use super::{SupplierStatementImport, decimal};
use crate::WalletError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime};

#[derive(Deserialize)]
struct Page {
    object: String,
    data: Vec<Bucket>,
    has_more: bool,
    next_page: Option<String>,
}
#[derive(Deserialize)]
struct Bucket {
    object: String,
    start_time: i64,
    end_time: i64,
    results: Vec<Cost>,
}
#[derive(Deserialize)]
struct Cost {
    object: String,
    amount: Amount,
    project_id: Option<String>,
    line_item: Option<String>,
    api_key_id: Option<String>,
    user_id: Option<String>,
    api_source: Option<String>,
}
#[derive(Deserialize)]
struct Amount {
    value: Box<serde_json::value::RawValue>,
    currency: String,
}
pub(super) fn bad(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}
pub(super) struct Parsed {
    pub day: Date,
    pub total: i128,
    pub conversion: i128,
    pub document: Value,
}

pub(super) fn parse(request: &SupplierStatementImport) -> Result<Parsed, WalletError> {
    crate::business_staff::validate(&request.idempotency_key, Some(&request.reason))?;
    if request.provider_organization_id.trim().is_empty()
        || request.provider_organization_id.trim() != request.provider_organization_id
        || request.provider_organization_id.len() > 200
        || request.mapping_evidence.trim().is_empty()
        || request.mapping_evidence.len() > 2000
    {
        return Err(bad(
            "provider organization and mapping/conversion evidence are required",
        ));
    }
    let day = Date::parse(
        &request.bill_date,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| bad("billDate must be YYYY-MM-DD"))?;
    let fetched = OffsetDateTime::parse(&request.fetched_at, &Rfc3339)
        .map_err(|_| bad("fetchedAt must be RFC3339"))?;
    let start = day.midnight().assume_utc().unix_timestamp();
    let end = start
        .checked_add(86400)
        .ok_or_else(|| bad("invalid date"))?;
    if fetched.unix_timestamp() < end
        || fetched > OffsetDateTime::now_utc() + time::Duration::minutes(5)
    {
        return Err(bad(
            "fetch timestamp must follow the complete UTC day and not be in the future",
        ));
    }
    let query = &request.query;
    if query.start_time != start
        || query.end_time != end
        || query.bucket_width != "1d"
        || !(1..=180).contains(&query.limit)
    {
        return Err(bad(
            "query must cover exactly the declared UTC day with daily buckets",
        ));
    }
    let projects: BTreeSet<_> = request.project_ids.iter().collect();
    if projects.len() != request.project_ids.len()
        || !request.project_ids.windows(2).all(|pair| pair[0] < pair[1])
        || projects.len() > 100
        || request
            .project_ids
            .iter()
            .any(|p| p.is_empty() || p.len() > 200)
    {
        return Err(bad("project IDs must be bounded and unique"));
    }
    match request.scope_kind.as_str() {
        "organization"
            if projects.is_empty() && query.project_ids.is_empty() && query.group_by.is_empty() => {
        }
        "projects"
            if !projects.is_empty()
                && query.project_ids == request.project_ids
                && query.group_by == ["project_id"] => {}
        _ => {
            return Err(bad(
                "unsupported grouping/filter scope; projects require an exact project filter and project_id grouping",
            ));
        }
    }
    let channels: BTreeSet<_> = request.channel_ids.iter().collect();
    if channels.is_empty() || channels.len() > 100 || channels.len() != request.channel_ids.len() {
        return Err(bad("select 1..100 unique channel IDs"));
    }
    let conversion = decimal::parse(&request.usd_per_credit)?;
    if conversion <= 0 {
        return Err(bad("USD per credit must be positive"));
    }
    if request.pages.is_empty()
        || request.pages.len() > 16
        || request.pages.iter().map(|p| p.body.len()).sum::<usize>() > 2 * 1024 * 1024
    {
        return Err(bad("supply 1..16 complete pages totaling at most 2 MiB"));
    }
    let mut expected = None;
    let mut cursors = BTreeSet::new();
    let mut buckets = 0;
    let mut total = 0_i128;
    let mut lines = Vec::new();
    let mut grouped = BTreeSet::new();
    for (index, input) in request.pages.iter().enumerate() {
        if input.request_page != expected || !cursors.insert(input.request_page.clone()) {
            return Err(bad("missing, repeated or out-of-order page cursor"));
        }
        // Require the nullable key as well as serde's typed interpretation.
        let top: BTreeMap<String, Box<serde_json::value::RawValue>> =
            serde_json::from_str(&input.body).map_err(|_| bad("invalid JSON page"))?;
        if !top.contains_key("next_page") {
            return Err(bad("page must explicitly contain next_page"));
        }
        let page: Page = serde_json::from_str(&input.body)
            .map_err(|_| bad("incomplete or unsupported OpenAI Costs page"))?;
        if page.object != "page"
            || page.has_more != page.next_page.is_some()
            || page
                .next_page
                .as_ref()
                .is_some_and(|c| c.is_empty() || c.len() > 2000)
            || page.has_more != (index + 1 < request.pages.len())
        {
            return Err(bad("invalid or incomplete page termination"));
        }
        expected = page.next_page;
        for bucket in page.data {
            buckets += 1;
            if buckets > 1
                || bucket.object != "bucket"
                || bucket.start_time != start
                || bucket.end_time != end
            {
                return Err(bad(
                    "expected one complete UTC day bucket; duplicated or split buckets are unsupported",
                ));
            }
            for line in bucket.results {
                if line.object != "organization.costs.result"
                    || line.amount.currency != "usd"
                    || line.line_item.is_some()
                    || line.api_key_id.is_some()
                    || line.user_id.is_some()
                    || line.api_source.is_some()
                {
                    return Err(bad("unsupported result type, currency or grouping"));
                }
                if request.scope_kind == "organization" {
                    if line.project_id.is_some() {
                        return Err(bad("organization totals must be ungrouped"));
                    }
                } else if line
                    .project_id
                    .as_ref()
                    .is_none_or(|p| !projects.contains(p))
                {
                    return Err(bad("null or unselected project in result"));
                }
                if !grouped.insert(line.project_id.clone()) {
                    return Err(bad("duplicate aggregate group"));
                }
                let amount = decimal::parse(line.amount.value.get())?;
                total = total.checked_add(amount).ok_or_else(decimal::invalid)?;
                lines.push(json!({"projectId":line.project_id,"amountUsd":decimal::text(amount),"rawAmount":line.amount.value.get()}));
            }
        }
    }
    if buckets != 1 {
        return Err(bad("missing day bucket is unknown coverage, not zero cost"));
    }
    let quantum = decimal::SCALE / 1_000_000;
    Ok(Parsed {
        day,
        total,
        conversion,
        document: json!({"schemaVersion":1,"algorithmVersion":"openai-costs-utc-day-exact18-v1","provider":"openai","providerOrganizationId":request.provider_organization_id,"billDate":request.bill_date,"fetchedAt":request.fetched_at,"scopeKind":request.scope_kind,"projectIds":request.project_ids,"amountUsd":decimal::text(total),"microUsd":(total/quantum).to_string(),"residualUsd":decimal::text(total%quantum),"quantization":"sum exact amounts then truncate toward zero to microUSD; residual retained","lines":lines,"identityBasis":"uploader assertion; raw pages do not prove account identity, authenticity or channel mapping","matchingGrain":"UTC day aggregate only; never request matching","limitations":["Cost observations may be revised; this is not necessarily a final invoice","Unknown local cost remains unknown even when aggregate amounts agree","No customer balance is changed"]}),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::supplier_statements::{CostsPageInput, CostsQuery};
    use uuid::Uuid;
    fn fixture(raw: &str) -> SupplierStatementImport {
        let start = 1793491200;
        SupplierStatementImport {
            idempotency_key: "fixture".into(),
            reason: "Synthetic official-format fixture".into(),
            provider_organization_id: "org-fixture".into(),
            bill_date: "2026-11-01".into(),
            fetched_at: "2026-11-02T12:00:00Z".into(),
            scope_kind: "organization".into(),
            project_ids: vec![],
            channel_ids: vec![Uuid::new_v4()],
            usd_per_credit: "1".into(),
            mapping_evidence: "Synthetic channel and cost-unit assertion".into(),
            query: CostsQuery {
                start_time: start,
                end_time: start + 86400,
                bucket_width: "1d".into(),
                group_by: vec![],
                project_ids: vec![],
                limit: 7,
            },
            pages: vec![CostsPageInput {
                request_page: None,
                body: format!(
                    r#"{{"object":"page","data":[{{"object":"bucket","start_time":{start},"end_time":{},"results":[{{"object":"organization.costs.result","amount":{{"value":{raw},"currency":"usd"}}}}]}}],"has_more":false,"next_page":null}}"#,
                    start + 86400
                ),
            }],
        }
    }
    #[test]
    fn real_numeric_contract_retains_exact_signed_exponents_and_explicit_zero() {
        for (amount, want) in [
            ("0.06", "0.06"),
            ("1e-7", "0.0000001"),
            ("-0.5", "-0.5"),
            ("1.234567890123456789", "1.234567890123456789"),
        ] {
            let p = parse(&fixture(amount)).unwrap();
            assert_eq!(p.document["amountUsd"], want);
        }
        let mut req = fixture("0");
        let mut page: Value = serde_json::from_str(&req.pages[0].body).unwrap();
        page["data"][0]["results"] = json!([]);
        req.pages[0].body = page.to_string();
        assert_eq!(parse(&req).unwrap().total, 0);
    }
    #[test]
    fn missing_malformed_currency_date_and_groups_never_become_zero() {
        for amount in ["null", "\"0.06\"", "1e-19", "1.12345😀"] {
            assert!(parse(&fixture(amount)).is_err());
        }
        let base = fixture("0.06");
        let original: Value = serde_json::from_str(&base.pages[0].body).unwrap();
        let mut cases = vec![];
        let mut p = original.clone();
        p.as_object_mut().unwrap().remove("next_page");
        cases.push(p);
        let mut p = original.clone();
        p["data"] = json!([]);
        cases.push(p);
        let mut p = original.clone();
        p["buckets"] = p["data"].take();
        p.as_object_mut().unwrap().remove("data");
        cases.push(p);
        let mut p = original.clone();
        p["data"][0]["results"][0]["amount"]["currency"] = json!("eur");
        cases.push(p);
        let mut p = original.clone();
        p["data"][0]["end_time"] = json!(1);
        cases.push(p);
        let mut p = original.clone();
        p["data"][0]["results"][0]
            .as_object_mut()
            .unwrap()
            .remove("amount");
        cases.push(p);
        let mut p = original.clone();
        p["data"][0]["results"][0]["api_key_id"] = json!("key");
        cases.push(p);
        let mut p = original.clone();
        p["data"][0]["results"][0]["project_id"] = json!("proj-x");
        cases.push(p);
        let mut p = original.clone();
        p["data"]
            .as_array_mut()
            .unwrap()
            .push(original["data"][0].clone());
        cases.push(p);
        for page in cases {
            let mut req = base.clone();
            req.pages[0].body = page.to_string();
            assert!(parse(&req).is_err(), "{page}");
        }
    }
    #[test]
    fn page_chain_requires_every_cursor_and_terminal_marker() {
        let mut req = fixture("0.06");
        let final_page = req.pages[0].body.clone();
        req.pages[0].body =
            r#"{"object":"page","data":[],"has_more":true,"next_page":"cursor-a"}"#.into();
        req.pages.push(CostsPageInput {
            request_page: Some("cursor-a".into()),
            body: final_page,
        });
        assert!(parse(&req).is_ok());
        req.pages[1].request_page = Some("wrong".into());
        assert!(parse(&req).is_err());
        req.pages.pop();
        assert!(parse(&req).is_err());
    }
    #[test]
    fn selected_projects_have_exact_sorted_scope_and_sum_once() {
        let mut req = fixture("0");
        req.scope_kind = "projects".into();
        req.project_ids = vec!["p1".into(), "p2".into()];
        req.query.project_ids = req.project_ids.clone();
        req.query.group_by = vec!["project_id".into()];
        let mut p: Value = serde_json::from_str(&req.pages[0].body).unwrap();
        p["data"][0]["results"] = json!([{"object":"organization.costs.result","project_id":"p1","amount":{"value":0.0000006,"currency":"usd"}},{"object":"organization.costs.result","project_id":"p2","amount":{"value":0.0000006,"currency":"usd"}}]);
        req.pages[0].body = p.to_string();
        let result = parse(&req).unwrap();
        assert_eq!(result.document["amountUsd"], "0.0000012");
        assert_eq!(result.document["microUsd"], "1");
        assert_eq!(result.document["residualUsd"], "0.0000002");
        req.project_ids.reverse();
        req.query.project_ids = req.project_ids.clone();
        assert!(parse(&req).is_err());
    }
}
