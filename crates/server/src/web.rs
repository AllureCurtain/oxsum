//! The Topcoat dashboard pages, mounted into the axum router as a fallback
//! tower service.
//!
//! One binary serves the API, the gateway and the pages: server-side rendering
//! calls `oxsum-core` directly. The Topcoat router sees every request the API
//! routes did not match, with its original URI intact, so the page URLs are
//! unchanged from the Leptos dashboard.

use axum::Router;
use topcoat::router::tower::TowerService;

use crate::AppState;

/// Mounts the dashboard pages as the router's fallback: the API routes keep
/// precedence, and every other path lands on the Topcoat pages.
pub fn mount(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    let config = oxsum_web::WebConfig {
        head_seed: state.config.head_signing_seed(),
        mail_configured: state.config.mailer().is_some(),
        session_cookie_secure: state.config.session_cookie_secure(),
        oauth_github: state.config.github().is_some(),
    };
    let dashboard = oxsum_web::router(state.db.clone(), state.tenants.clone(), config);
    router.fallback_service(TowerService::new(dashboard))
}
