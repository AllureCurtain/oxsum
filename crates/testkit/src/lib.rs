//! Shared helpers for integration tests: per-suite PostgreSQL isolation.
//!
//! Every suite (a `tests/*.rs` submodule of the per-crate aggregate binary —
//! issue #998) gets its own scratch database, created on first use and dropped
//! when the suite's process exits. Suites never share mutable database state,
//! and repeated runs cannot accumulate residue in the shared `oxsum` database
//! (issue #226). The cache is per-slug so a single aggregate binary can host
//! ~100 suites, each with its own scratch database.
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
//! Residue is collected between runs, not inside them: an external sweep —
//! CI's `scripts/drop-test-scratch.sql` step, or the same script run by hand —
//! drops `oxsum_test_*`/`de_*` older than 24 h before `nextest` starts. The
//! drop is the next run's job, never a test process's own (issue #271).

use std::time::{SystemTime, UNIX_EPOCH};

/// How old an `oxsum_test_*` database must be before a later sweep drops it.
///
/// Twenty-four hours keeps a colleague's in-flight run on the same cluster
/// from being dropped mid-suite while still collecting yesterday's residue.
#[cfg(test)]
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

/// Per-suite scratch databases keyed by slug. A single test process may host
/// several suites at once — the aggregate `tests/server.rs`/`tests/core.rs`
/// binaries each hold ~100 submodules that ask for their own slug (issue
/// #998) — so the cache is per-slug rather than a single `OnceLock`, and the
/// teardown drops every database the process created.
static SUITE_DBS: std::sync::Mutex<Option<std::collections::HashMap<String, SuiteDb>>> =
    std::sync::Mutex::new(None);

/// The suite's scratch-database URL, with the slug derived from
/// `module_path!()` — under the aggregate binaries (issue #998) a `#[path]`-
/// loaded file's path ends in `server::<stem>` or `core::<stem>`, so the leaf
/// is already the slug the file's hand-written `fn url()` used to name. This
/// removes the per-file helper entirely: a suite calls
/// `oxsum_testkit::suite_url!()` wherever it once called `url()`.
///
/// For the rarer early-return shape, [`suite_db!`](crate::suite_db) unwraps
/// and skips in one step.
#[macro_export]
macro_rules! suite_url {
    () => {{
        // module_path! is "<bin>::<module>::…"; a #[path] submodule's leaf is
        // the file stem, which is the suite slug. Nested test modules keep the
        // file's slug — the first segment after the binary name is the file.
        let slug = module_path!()
            .split("::")
            .nth(1)
            .unwrap_or_else(|| module_path!());
        $crate::suite_db_url(slug)
    }};
}

/// Unwraps [`suite_url!`](crate::suite_url) or skips the test: `DATABASE_URL`
/// unset means the suite cannot run, so the test returns rather than fails.
/// Replaces the `db_or_skip!`/`app_or_skip!` macro every file defined to do
/// the same `match url() { Some(u) => u, None => return }`.
#[macro_export]
macro_rules! suite_db {
    () => {
        match $crate::suite_url!() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// Returns the `DATABASE_URL` for this suite's isolated scratch database,
/// creating the database on first call. Returns `None` when `DATABASE_URL`
/// is unset; the caller should skip the test in that case.
///
/// The scratch database is created with an admin connection derived from
/// `DATABASE_URL` (same server and credentials, `postgres` maintenance
/// database) and dropped when the suite's process exits. The drop is
/// best-effort: a crashed run may leave a timestamped `oxsum_test_*`
/// database behind, which is identifiable by name.
#[allow(clippy::expect_used)]
pub fn suite_db_url(slug: &str) -> Option<String> {
    // Per-slug cache: each suite in the process gets its own scratch database;
    // `create_suite_db` returns `None` (skip) when DATABASE_URL is unset.
    let mut guard = SUITE_DBS.lock().expect("suite-db map is not poisoned");
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    if let Some(db) = map.get(slug) {
        return Some(db.url.clone());
    }
    let db = create_suite_db(slug)?;
    let url = db.url.clone();
    map.insert(slug.to_owned(), db);
    Some(url)
}

/// Drops the suite's scratch database when the test binary exits. Best-effort:
/// failures are reported to stderr but never fail the suite.
#[dtor::dtor(unsafe)]
#[allow(clippy::expect_used)]
fn teardown_suite_db() {
    let dbs: Vec<SuiteDb> = {
        let mut guard = SUITE_DBS.lock().expect("suite-db map is not poisoned");
        guard
            .take()
            .map(|m| m.into_values().collect())
            .unwrap_or_default()
    };
    if dbs.is_empty() {
        return;
    }
    // The test runtime is gone by now; run each drop on a fresh thread with
    // its own current-thread runtime.
    for db in dbs {
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
            rt.block_on(create_database(&admin_url, &name))
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

/// Drop `oxsum_test_*` databases older than [`STALE_AGE_SECS`] on the pool's
/// cluster — the scan/drop shape `scripts/drop-test-scratch.sql` runs outside
/// the test binaries in CI. `#[cfg(test)]` so the helper exists for the
/// stale-probe test without shipping as part of the public helper.
#[cfg(test)]
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

/// Explicit registration-policy fixture for isolated legacy HTTP suites. These
/// tests exercise other resources and are not evidence of owner/setup authority.
/// Production signup always reads the saved business policy; no production bypass.
#[allow(clippy::expect_used)]
pub async fn set_registration_fixture(pool: &sqlx::PgPool, open: bool) {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("isolated test database");
    assert!(
        database.starts_with("oxsum_test_"),
        "registration fixture requires a scratch database"
    );
    sqlx::query("UPDATE oxsum.businesses SET initialized_at=COALESCE(initialized_at,now()),registration_mode=$1 WHERE singleton")
        .bind(if open { "open" } else { "invite" })
        .execute(pool).await.expect("explicit test registration policy");
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{replace_database, sweep_stale_unchecked};

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
