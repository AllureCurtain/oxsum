//! Shared helpers for integration tests: per-suite PostgreSQL isolation.
//!
//! Every integration-test binary (one per `tests/*.rs` file) gets its own
//! scratch database, created on first use and dropped when the suite's process
//! exits. Suites never share mutable database state, and repeated runs cannot
//! accumulate residue in the shared `oxsum` database (issue #226).
//!
//! Usage in a test file:
//!
//! ```ignore
//! fn url() -> Option<String> {
//!     let _ = dotenvy::dotenv();
//!     oxsum_testkit::suite_db_url("my_suite")
//! }
//! ```
//!
//! The `slug` should be the test file's stem. It becomes part of the scratch
//! database name (`oxsum_test_<slug>_<unix_timestamp>_<pid>`), so a stale
//! database left behind by a crashed run is identifiable.
//!
//! Cleanup is self-healing: before creating its own scratch database, a suite
//! drops every `oxsum_test_*` older than [`STALE_AGE_SECS`]. A crashed run's
//! residue is collected by the *next* run, not by the process that died — the
//! exit-time drop below is only the last line of defence (issue #271).

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// How old an `oxsum_test_*` database must be before a later suite drops it.
///
/// Twenty-four hours keeps a colleague's in-flight run on the same cluster
/// from being dropped mid-suite while still collecting yesterday's residue.
const STALE_AGE_SECS: u64 = 24 * 60 * 60;

/// State for this suite's scratch database. `None` when `DATABASE_URL` is
/// unset, in which case the suite skips.
struct SuiteDb {
    /// URL of the scratch database, handed to the tests.
    url: String,
    /// URL of the maintenance database on the same server, used to create
    /// and drop the scratch database.
    admin_url: String,
    /// Scratch database name, for the teardown drop.
    name: String,
}

static SUITE_DB: OnceLock<Option<SuiteDb>> = OnceLock::new();

/// Returns the `DATABASE_URL` for this suite's isolated scratch database,
/// creating the database on first call. Returns `None` when `DATABASE_URL`
/// is unset; the caller should skip the test in that case.
///
/// The scratch database is created with an admin connection derived from
/// `DATABASE_URL` (same server and credentials, `postgres` maintenance
/// database) and dropped when the suite's process exits. The drop is
/// best-effort: a crashed run may leave a timestamped `oxsum_test_*`
/// database behind, which is identifiable by name.
pub fn suite_db_url(slug: &str) -> Option<String> {
    SUITE_DB
        .get_or_init(|| create_suite_db(slug))
        .as_ref()
        .map(|db| db.url.clone())
}

/// Drops the suite's scratch database when the test binary exits. Best-effort:
/// failures are reported to stderr but never fail the suite.
#[ctor::dtor]
fn teardown_suite_db() {
    let Some(db) = SUITE_DB.get().and_then(|opt| opt.as_ref()) else {
        return;
    };
    // The test runtime is gone by now; run the drop on a fresh thread with
    // its own current-thread runtime.
    let admin_url = db.admin_url.clone();
    let name = db.name.clone();
    let label = name.clone();
    let dropped = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(drop_database(&admin_url, &name))
    })
    .join()
    .ok()
    .flatten()
    .unwrap_or(false);
    if dropped {
        eprintln!("oxsum-testkit: dropped scratch database {label}");
    } else {
        eprintln!("oxsum-testkit: WARNING: could not drop scratch database {label}");
    }
}

fn create_suite_db(slug: &str) -> Option<SuiteDb> {
    // Consult .env first, matching the pre-testkit behavior (issue #18):
    // a DATABASE_URL that only exists in .env must be honored, not skipped.
    let _ = dotenvy::dotenv();
    let base_url = std::env::var("DATABASE_URL").ok()?;
    // The slug is a hardcoded file stem per test binary; the timestamp is
    // numeric. Reject anything else so the name is safe to interpolate.
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || slug.is_empty() {
        eprintln!("oxsum-testkit: refusing unsafe suite slug {slug:?}");
        return None;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // The PID guards against two runs of the same suite starting within the
    // same second.
    let name = format!("oxsum_test_{slug}_{timestamp}_{}", std::process::id());
    // A DATABASE_URL that is set but unparseable is a misconfiguration, not
    // an absent database: fail loudly instead of skipping vacuously (issue
    // #18 pins this behavior).
    let url = replace_database(&base_url, &name)
        .unwrap_or_else(|| panic!("oxsum-testkit: DATABASE_URL is not a valid URL: {base_url:?}"));
    let admin_url = replace_database(&base_url, "postgres")
        .unwrap_or_else(|| panic!("oxsum-testkit: DATABASE_URL is not a valid URL: {base_url:?}"));

    // `OnceLock::get_or_init` runs on the caller's thread, which is inside
    // the test's tokio runtime; `block_on` would panic there, so do the
    // blocking database work on a dedicated thread.
    let created = std::thread::spawn({
        let admin_url = admin_url.clone();
        let name = name.clone();
        move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async move {
                // Sweep stale scratch databases before making this suite's own:
                // a crashed predecessor's residue is this run's to collect, not
                // a permanent tenant of the cluster (issue #271). The sweep is
                // best-effort — a busy cluster answers what it can and the
                // suite still proceeds.
                sweep_stale(&admin_url).await;
                create_database(&admin_url, &name).await
            })
        }
    })
    .join()
    .ok()
    .flatten()
    .unwrap_or(false);

    if !created {
        eprintln!("oxsum-testkit: failed to create scratch database {name}");
        return None;
    }
    eprintln!("oxsum-testkit: created scratch database {name}");
    Some(SuiteDb {
        url,
        admin_url,
        name,
    })
}

/// Postgres advisory-lock key serializing the sweep across every test
/// process that shares a cluster — the name is arbitrary, only its value
/// must not collide with anything else oxsum locks on.
const SWEEP_LOCK: i64 = 0x6f78_7375_6d5f_7473; // "oxsum_ts" as a tag

/// Drops `oxsum_test_*` databases older than [`STALE_AGE_SECS`] on the admin
/// connection's cluster. Best-effort: logs what it drops and never fails —
/// a stale entry that cannot be dropped today is next run's problem, not a
/// reason to refuse this suite its own scratch database.
///
/// Only the first process to grab [`SWEEP_LOCK`] actually walks the catalog;
/// every other process fails `pg_try_advisory_lock` and returns in one
/// round-trip. Under nextest each suite is its own process, so without the
/// lock every binary paid a catalog scan plus N DROPs on the shared cluster —
/// cumulative cost CI's 30-minute job budget could not absorb (issue #271).
async fn sweep_stale(admin_url: &str) {
    let Ok(pool) = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
    else {
        return;
    };
    // One sweeper per cluster at a time; the rest move on to their own test.
    let won: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(SWEEP_LOCK)
        .fetch_one(&pool)
        .await
        .unwrap_or(false);
    if won {
        sweep_stale_unchecked(&pool).await;
        // Release before close so a second process does not queue behind this
        // one's teardown; releasing a lock you do not hold is a no-op.
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(SWEEP_LOCK)
            .execute(&pool)
            .await;
    }
    pool.close().await;
}

/// The catalog walk + drop half of [`sweep_stale`], advisory lock already
/// held by the caller. Split out so tests can exercise the scan/drop path
/// without racing every other suite's lock.
async fn sweep_stale_unchecked(pool: &sqlx::PgPool) {
    // The `~` pattern is the same shape the manual
    // `scripts/drop-test-scratch.sql` uses; an empty match set is the common
    // case and costs one index-only scan. The timestamp is the *second-to-last*
    // `_`-separated field — the slug itself may carry underscores (e.g.
    // `usage_daily`), so a fixed position cannot find it.
    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(STALE_AGE_SECS) as i64;
    // Single round-trip: enumerate the stale set in Rust, then drop each with
    // a shared prepared statement under a short timeout. Per-database DROP
    // must run outside a transaction, so it cannot fold into the SELECT.
    let stale: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database \
         WHERE datname ~ '^oxsum_test_[a-z0-9_]+_[0-9]+_[0-9]+$' \
           AND CAST( \
                 (regexp_match(datname, '_([0-9]+)_[0-9]+$'))[1] \
                 AS bigint) < $1",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if !stale.is_empty() {
        // Bound each drop so a contended catalog row cannot stall setup; a
        // database that refuses to go today is next run's, not a failure here.
        let _ = sqlx::query("SET statement_timeout = '5s'")
            .execute(pool)
            .await;
        for name in stale {
            let dropped = sqlx::query(&format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#))
                .execute(pool)
                .await
                .is_ok();
            if dropped {
                eprintln!("oxsum-testkit: swept stale scratch database {name}");
            }
        }
        let _ = sqlx::query("RESET statement_timeout").execute(pool).await;
    }
}

async fn create_database(admin_url: &str, name: &str) -> Option<bool> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .ok()?;
    // `name` is validated above: alphanumerics, underscores, and the
    // quoting make interpolation safe.
    let result = sqlx::query(&format!(r#"CREATE DATABASE "{name}""#))
        .execute(&pool)
        .await;
    pool.close().await;
    result.ok().map(|_| true)
}

async fn drop_database(admin_url: &str, name: &str) -> Option<bool> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .ok()?;
    let result = sqlx::query(&format!(r#"DROP DATABASE IF EXISTS "{name}""#))
        .execute(&pool)
        .await;
    pool.close().await;
    result.ok().map(|_| true)
}

/// Returns `url` with its database name replaced by `new_db`, preserving
/// credentials, host, port, and any query string or fragment.
fn replace_database(url: &str, new_db: &str) -> Option<String> {
    let scheme_end = url.find("://")? + 3;
    let after_scheme = &url[scheme_end..];
    // The path starts at the first '/' after the authority; the authority
    // ends at '/', '?', or '#'.
    let mut path_start = None;
    for (i, c) in after_scheme.char_indices() {
        if c == '/' {
            path_start = Some(scheme_end + i);
            break;
        }
        if c == '?' || c == '#' {
            break;
        }
    }
    match path_start {
        Some(ps) => {
            let path_end = url[ps..]
                .find(['?', '#'])
                .map(|i| ps + i)
                .unwrap_or(url.len());
            Some(format!("{}/{}{}", &url[..ps], new_db, &url[path_end..]))
        }
        None => {
            let insert_at = url.find(['?', '#']).unwrap_or(url.len());
            Some(format!(
                "{}/{}{}",
                &url[..insert_at],
                new_db,
                &url[insert_at..]
            ))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::replace_database;
    use super::sweep_stale_unchecked;

    #[test]
    fn replaces_the_database_keeping_the_rest() {
        assert_eq!(
            replace_database("postgresql://u:p@h:5432/olddb", "newdb"),
            Some("postgresql://u:p@h:5432/newdb".to_owned())
        );
        assert_eq!(
            replace_database("postgresql://u:p@h:5432/olddb?sslmode=require", "newdb"),
            Some("postgresql://u:p@h:5432/newdb?sslmode=require".to_owned())
        );
        assert_eq!(
            replace_database("postgresql://u@h/olddb", "newdb"),
            Some("postgresql://u@h/newdb".to_owned())
        );
        assert_eq!(
            replace_database("postgresql://h:5432", "newdb"),
            Some("postgresql://h:5432/newdb".to_owned())
        );
        assert_eq!(replace_database("not-a-url", "newdb"), None);
    }

    /// A suite's setup drops `oxsum_test_*` databases older than
    /// [`STALE_AGE_SECS`] before creating its own (issue #271).
    #[tokio::test]
    async fn a_stale_scratch_database_is_swept_on_the_next_run() {
        let _ = dotenvy::dotenv();
        let Some(base_url) = std::env::var("DATABASE_URL").ok() else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };
        let admin_url = replace_database(&base_url, "postgres").expect("valid");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("connects");

        // A name forged two days ago — beyond the 24-hour cutoff. Drop any
        // leftover from an earlier interrupted run of this very test before
        // claiming the name.
        let stale_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 2 * 24 * 60 * 60;
        let stale = format!("oxsum_test_probe_{stale_ts}_1");
        sqlx::query(&format!(r#"DROP DATABASE IF EXISTS "{stale}""#))
            .execute(&pool)
            .await
            .expect("pre-test cleanup");
        sqlx::query(&format!(r#"CREATE DATABASE "{stale}""#))
            .execute(&pool)
            .await
            .expect("stale db created");

        sweep_stale_unchecked(&pool).await;

        let still_there: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&stale)
                .fetch_one(&pool)
                .await
                .expect("reads catalogue");
        assert!(!still_there, "the stale scratch database must be dropped");

        // A fresh name — inside the cutoff — must survive the same sweep.
        let fresh_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let fresh = format!("oxsum_test_probe_{fresh_ts}_2");
        sqlx::query(&format!(r#"DROP DATABASE IF EXISTS "{fresh}""#))
            .execute(&pool)
            .await
            .expect("pre-test cleanup");
        sqlx::query(&format!(r#"CREATE DATABASE "{fresh}""#))
            .execute(&pool)
            .await
            .expect("fresh db created");
        sweep_stale_unchecked(&pool).await;
        let survived: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&fresh)
                .fetch_one(&pool)
                .await
                .expect("reads catalogue");
        assert!(survived, "a fresh scratch database must not be swept");

        // Leave the cluster clean.
        sqlx::query(&format!(r#"DROP DATABASE "{fresh}""#))
            .execute(&pool)
            .await
            .expect("fresh db dropped");
        pool.close().await;
    }
}
