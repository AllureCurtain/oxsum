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

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

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
    let url = replace_database(&base_url, &name)?;
    let admin_url = replace_database(&base_url, "postgres")?;

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
mod tests {
    use super::replace_database;

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
}
