//! The demo seed (`oxsum seed`, roadmap P7-4, issue #164).
//!
//! One command fills a fresh deployment with a believable world — a user to log
//! in as, an organization with a second member under a member budget, keys under
//! constraints, deposits and top-ups, and a month of settled gateway turns — so
//! `docker compose up`, `oxsum seed` and a login show the product working rather
//! than an empty dashboard.
//!
//! Everything is written through the wallet and table APIs the real paths use —
//! the seeded turns carry real `Settlement` descriptions and usage rows, so the
//! requests page, the usage rollup, proofs and the verifier all read them
//! exactly like turns the gateway served. Nothing here touches a ledger table
//! or writes a shape the HTTP surface could not produce.
//!
//! The command is idempotent: a second run finds the demo account and answers
//! "already seeded" without writing anything.

use std::sync::Arc;

use time::{Duration, OffsetDateTime};

use crate::billing::{Settlement, hold_description};
use crate::error::{WalletError, invalid};
use crate::holds::OpenHold;
use crate::usage::{UsageRecord, UsageRow};
use crate::wallet::{entry_id_for, settlement_key_for};
use crate::{
    ActingKey, ApiKey, CatalogModel, Db, KeyConstraints, Member, MembershipActor, NewDiscount,
    NewUser, Organization, Role, SettlementKind, Tenants, Wallet, statement_period,
};

/// The account the seed creates — the login the report prints.
pub const DEMO_EMAIL: &str = "demo@oxsum.local";
/// The second member on the demo organization, under a member budget.
pub const MEMBER_EMAIL: &str = "dev@oxsum.local";
/// Both demo accounts share one documented password; it is long enough for the
/// signup rule and lives only in the seed output and the docs.
pub const DEMO_PASSWORD: &str = "demo-password-1234";
/// The demo organization's name.
pub const DEMO_ORGANIZATION: &str = "Acme Demo";

const ONE: i64 = 1_000_000;
/// The member's committed-spend cap — exercised by the turns their key pays for.
const MEMBER_BUDGET: i64 = 50 * ONE;
/// The staging key's ceiling — the key page shows a constrained key.
const STAGING_LIMIT: i64 = 20 * ONE;
/// The discount the organization carries, applied to the recent turns the way
/// the gateway snapshots it at admission.
const DEMO_DISCOUNT_PERCENT: i32 = 10;
/// The discount took effect this many days ago; older turns settle undiscounted.
const DISCOUNT_DAYS_AGO: i64 = 10;
/// Top-ups the organization made, `(days ago, minor units)` — the deposits rail
/// shows three manual payments rather than one grant.
const TOP_UPS: &[(i64, i64)] = &[(28, 400 * ONE), (14, 250 * ONE), (3, 150 * ONE)];
/// How far back the settled history reaches.
const HISTORY_DAYS: i64 = 30;

/// What a run reports: the credentials to print once, and what was written —
/// `created: false` means the demo world was already there and nothing changed.
#[derive(Debug, Clone)]
pub struct SeedReport {
    pub created: bool,
    pub email: String,
    pub member_email: String,
    pub organization: String,
    /// The first key's secret — printed once; a rerun cannot recover it.
    pub api_secret: Option<String>,
    /// Total the seeded deposits credited, in minor units.
    pub topped_up_minor: i64,
    /// Total the seeded turns settled for, in minor units.
    pub spent_minor: i64,
    /// How many settled turns were written.
    pub turns: i64,
    /// The `YYYY-MM` statement issued, when the seed closed a month.
    pub statement: Option<String>,
}

impl Db {
    /// Seeds the demo world. The demo email's presence is the whole idempotency
    /// check: it is written first, so a second run — or a retry of one that died
    /// — answers "already seeded" rather than doubling anything.
    ///
    /// # Errors
    ///
    /// Any write's [`WalletError`] aborts the run; what landed stays (the ledger
    /// is append-only anyway) and the next run reports the world seeded.
    pub async fn seed_demo(&self) -> Result<SeedReport, WalletError> {
        if self.demo_known().await? {
            return Ok(SeedReport {
                created: false,
                email: DEMO_EMAIL.to_owned(),
                member_email: MEMBER_EMAIL.to_owned(),
                organization: DEMO_ORGANIZATION.to_owned(),
                api_secret: None,
                topped_up_minor: 0,
                spent_minor: 0,
                turns: 0,
                statement: None,
            });
        }
        let registration = self
            .register(NewUser {
                email: DEMO_EMAIL.to_owned(),
                password: DEMO_PASSWORD.to_owned(),
                organization_name: Some(DEMO_ORGANIZATION.to_owned()),
            })
            .await?;
        let owner = registration.user;
        let organization = registration.organization;
        let member_registration = self
            .register(NewUser {
                email: MEMBER_EMAIL.to_owned(),
                password: DEMO_PASSWORD.to_owned(),
                organization_name: None,
            })
            .await?;
        let acting = MembershipActor {
            user_id: owner.id,
            role: Role::Owner,
        };
        let member: Member = self
            .add_member(organization.id, acting, MEMBER_EMAIL)
            .await?;
        self.update_member(
            organization.id,
            acting,
            member.user_id,
            None,
            Some(Some(MEMBER_BUDGET)),
        )
        .await?;

        // The catalog the seeded turns bill under: whatever channels the
        // deployment serves, picked up round-robin. An empty catalog writes no
        // history — there is no price a seeded turn could claim it settled under.
        let catalog = self.catalog().await?;
        let staging = self
            .create_key(
                organization.id,
                Some("staging".into()),
                None,
                Some(owner.id),
                KeyConstraints {
                    spend_limit_minor: Some(STAGING_LIMIT),
                    model_allowlist: catalog.first().map(|model| vec![model.model.clone()]),
                    ..KeyConstraints::default()
                },
            )
            .await?;
        let member_key = self
            .create_key(
                organization.id,
                Some("experiments".into()),
                None,
                Some(member_registration.user.id),
                KeyConstraints::default(),
            )
            .await?;
        // The discount is scoped to the demo organization and backdated, so the
        // recent turns carry `discountPercent` the way a real turn settles under
        // the row in force when it started.
        let discount_from = OffsetDateTime::now_utc() - Duration::days(DISCOUNT_DAYS_AGO);
        self.create_discount(
            "seed-demo-discount",
            &NewDiscount {
                percent: DEMO_DISCOUNT_PERCENT,
                organization_id: Some(organization.id),
                model: None,
                label: Some("demo".into()),
                valid_from: Some(discount_from),
                valid_until: None,
            },
        )
        .await?;

        let tenants = Tenants::new(self.pool().clone());
        let wallet = tenants.get(&organization.tenant_id).await?;
        let today = OffsetDateTime::now_utc().date();

        // Money in: the manual rail's deposit row beside each ledger top-up, the
        // same pair `POST /api/v1/topups` writes.
        let mut topped_up = 0i64;
        for (index, (days_ago, minor)) in TOP_UPS.iter().enumerate() {
            let idem = format!("seed-topup-{index}");
            let receipt = wallet
                .top_up(&idem, *minor, today - Duration::days(*days_ago))
                .await?;
            self.record_manual_deposit(organization.id, &idem, *minor, *receipt.entry_id.as_uuid())
                .await?;
            topped_up += *minor;
        }

        let acting_keys = [
            (
                &registration.api_key.key,
                ActingKey {
                    key_id: registration.api_key.key.id,
                    spend_limit_minor: None,
                    requests_per_minute: None,
                },
            ),
            (
                &staging.key,
                ActingKey {
                    key_id: staging.key.id,
                    spend_limit_minor: Some(STAGING_LIMIT),
                    requests_per_minute: None,
                },
            ),
            (
                &member_key.key,
                ActingKey {
                    key_id: member_key.key.id,
                    spend_limit_minor: None,
                    requests_per_minute: None,
                },
            ),
        ];

        // A month of settled turns: mostly plain usage, one capped and one
        // unpriced so the anomalies surface has something to say, each under the
        // settlement description and usage row the gateway would have written.
        let mut spent = 0i64;
        let mut turns = 0i64;
        let world = SeedWorld {
            wallet: &wallet,
            organization: &organization,
            catalog: &catalog,
            acting_keys: &acting_keys,
            capped_done: std::cell::Cell::new(false),
        };
        for days_ago in (0..HISTORY_DAYS).rev() {
            let on = today - Duration::days(days_ago);
            let discounted = days_ago <= DISCOUNT_DAYS_AGO;
            for n in 0..(1 + days_ago % 3) {
                let request = format!("seed-{days_ago:02}-{n}");
                if let Some(charged) = self
                    .seed_turn(&world, &request, turns, on, discounted)
                    .await?
                {
                    turns += 1;
                    spent += charged;
                }
            }
        }

        // One hold left open — a turn in flight, watched like the gateway's own
        // so the sweeper owns its lifecycle exactly the same way.
        if let Some(model) = catalog.first() {
            let request = "seed-open";
            let freeze = model.price.estimate_minor(2_000, Some(800), None)?;
            let hold_key = format!("req-{request}:hold");
            let acting = &acting_keys[0].1;
            wallet
                .hold_for_key(
                    acting,
                    Some(&model.model),
                    &hold_key,
                    &hold_description(request, &model.model, freeze)?,
                    freeze,
                    today,
                )
                .await?;
            self.note_open_hold(&OpenHold {
                hold_key,
                tenant_id: organization.tenant_id.clone(),
                request_id: request.to_owned(),
                model: model.model.clone(),
                channel: model.channel.clone(),
                price_version: model.version,
                input_price: model.price.input_price_per_million,
                output_price: model.price.output_price_per_million,
                freeze_minor: freeze,
                key_id: Some(acting.key_id),
                end_user: None,
                service_tier: None,
                tags: Default::default(),
                sweep_attempts: 0,
                last_error: None,
                dead_at: None,
            })
            .await?;
        }

        // A closed, issued statement for last month — the statements page shows
        // a finalized document, not just the running month.
        let this_month = today.replace_day(1).map_err(invalid)?;
        let last_month = (this_month - Duration::days(1))
            .replace_day(1)
            .map_err(invalid)?;
        let label = format!(
            "{:04}-{:02}",
            last_month.year(),
            u8::from(last_month.month())
        );
        wallet.close_month(last_month).await?;
        let admin_org = self.organization_by_id(organization.id).await?;
        let period = statement_period(&label)?;
        let statement = match self
            .generate_statement(&admin_org, wallet.as_ref(), &period)
            .await?
        {
            Some(statement) => {
                self.finalize_statement(statement.id, wallet.as_ref(), today)
                    .await?;
                Some(label)
            }
            None => None,
        };

        Ok(SeedReport {
            created: true,
            email: DEMO_EMAIL.to_owned(),
            member_email: MEMBER_EMAIL.to_owned(),
            organization: DEMO_ORGANIZATION.to_owned(),
            api_secret: Some(registration.api_key.secret),
            topped_up_minor: topped_up,
            spent_minor: spent,
            turns,
            statement,
        })
    }

    /// Whether the demo account exists — the whole idempotency check.
    async fn demo_known(&self) -> Result<bool, WalletError> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM oxsum.users WHERE email_normalized = $1)",
        )
        .bind(DEMO_EMAIL)
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    /// Writes one settled turn the way the gateway does: a hold attributed to a
    /// key, a settlement under a real `Settlement` description, and the usage
    /// row beside it. Returns the settled charge; `None` when the deployment
    /// serves no model a turn could bill under.
    async fn seed_turn(
        &self,
        world: &SeedWorld<'_>,
        request: &str,
        seq: i64,
        on: time::Date,
        discounted: bool,
    ) -> Result<Option<i64>, WalletError> {
        let Some(model) = world.catalog.get(seq as usize % world.catalog.len()) else {
            return Ok(None);
        };
        // Rotate the payer: the member's key every fifth turn so the budget has
        // spend to count, the constrained staging key every seventh on its one
        // allowed model, the owner's key otherwise.
        let (_, acting) = if seq % 5 == 4 {
            &world.acting_keys[2]
        } else if seq % 7 == 6 {
            &world.acting_keys[1]
        } else {
            &world.acting_keys[0]
        };
        let model = if seq % 7 == 6 {
            world.catalog.first().unwrap_or(model)
        } else {
            model
        };

        let usage = seeded_usage(seq);
        let itemized = model.price.itemize(&usage, true)?;
        let discount = discounted.then_some(i64::from(DEMO_DISCOUNT_PERCENT));
        let cost = match discount {
            Some(percent) => itemized.discounted_minor(percent)?,
            None => itemized.total_minor()?,
        };
        // The freeze is the upper bound a real request would have frozen — a
        // generous input bound plus headroom on the output — except on the one
        // turn built to settle `capped`, whose freeze is deliberately short.
        // The first turn whose real charge is meaningful takes it: a fixed turn
        // number would miss on a catalog of near-free models.
        let generous = model.price.estimate_minor(
            usage.input_tokens * 4 + 400,
            Some((usage.output_tokens * 3).max(model.price.max_output_tokens.min(4096))),
            None,
        )?;
        // Unpriced wins over capped: the tool-calls turn must keep its anomaly —
        // capped can wait for the next fully-priced turn with a charge worth cutting.
        let (kind, freeze, charged) = if !model.price.unpriced_dimensions(&usage).is_empty() {
            (SettlementKind::Unpriced, generous, cost.min(generous))
        } else if !world.capped_done.get() && cost > 10 {
            world.capped_done.set(true);
            let freeze = cost * 2 / 3;
            (SettlementKind::Capped, freeze, freeze)
        } else {
            (SettlementKind::Usage, generous, cost)
        };

        let hold_key = format!("req-{request}:hold");
        world
            .wallet
            .hold_for_key(
                acting,
                Some(&model.model),
                &hold_key,
                &hold_description(request, &model.model, freeze)?,
                freeze,
                on,
            )
            .await?;
        let description = Settlement {
            request,
            channel: &model.channel,
            model: &model.model,
            price_version: model.version,
            kind,
            usage: &usage,
            lines: &itemized.lines,
            matched_rule: itemized.matched_rule.as_ref(),
            discount_percent: discount,
            charged,
            freeze,
            // A seeded turn is a single scripted call: no failover ever ran.
            upstream_attempts: 1,
        }
        .description()?;
        world
            .wallet
            .settle(&hold_key, &description, charged, on)
            .await?;
        let upstream_cost_minor = model.price.upstream_cost(&usage, true).unwrap_or(None);
        self.record_usage(&UsageRow {
            request_id: request.to_owned(),
            tenant_id: world.organization.tenant_id.clone(),
            key_id: Some(acting.key_id),
            model: model.model.clone(),
            channel: model.channel.clone(),
            price_version: model.version,
            kind,
            entry_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
            usage,
            charged_minor: charged,
            freeze_minor: freeze,
            upstream_cost_minor,
            upstream_attempts: 1,
        })
        .await?;
        Ok(Some(charged))
    }
}

/// The seeded world's per-run invariants — the wallet the turns bill and the
/// actors they bill under — bundled so `seed_turn` reads one call's variables.
struct SeedWorld<'a> {
    wallet: &'a Arc<Wallet>,
    organization: &'a Organization,
    catalog: &'a [CatalogModel],
    acting_keys: &'a [(&'a ApiKey, ActingKey)],
    /// Whether the deliberately short-frozen `capped` turn has been written.
    capped_done: std::cell::Cell<bool>,
}

/// One seeded turn's usage: token counts that vary with the turn number, a
/// cached share and a reasoning share on some, tool calls on the one turn that
/// settles `unpriced`, and an end-user label the usage page can attribute.
fn seeded_usage(seq: i64) -> UsageRecord {
    let input = 400 + (seq * 311).rem_euclid(2_400);
    let output = 120 + (seq * 173).rem_euclid(800);
    UsageRecord {
        input_tokens: input,
        output_tokens: output,
        cached_tokens: if seq % 5 == 0 { input / 3 } else { 0 },
        reasoning_tokens: if seq % 4 == 0 { output / 4 } else { 0 },
        tool_calls: (seq == 11) as i64 * 2,
        end_user: Some(format!("user-{}", seq.rem_euclid(3))),
        ..UsageRecord::default()
    }
}
