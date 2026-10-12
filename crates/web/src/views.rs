//! Shared page chrome: the HTML shell, the dashboard navigation, and the
//! money formatting the pages use.
//!
//! Spike note: no DESIGN.md token pass here — the markup is structural, not
//! polished. The point of the spike is that the pages render with real data,
//! not that they look finished.
//!
//! Topcoat's view model matters for how these helpers are shaped: a page's
//! rendered view must own its data (the `view!` body is `async move`), so the
//! frame takes owned `String` props rather than borrowing the page's locals.

use topcoat::Result;
use topcoat::view::{Child, View, component, view};

/// Formats minor units as credits with six decimals, without floating point:
/// money is integers all the way down, including on the way to the screen.
pub(crate) fn credits(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

/// The amount with its sign, so a top-up reads apart from a charge at a glance.
pub(crate) fn signed_credits(minor: i64) -> String {
    if minor >= 0 {
        format!("+{}", credits(minor))
    } else {
        credits(minor)
    }
}

/// The whole HTML document: head, style link, and the page body.
#[component]
pub(crate) async fn shell(title: &str, child: Child<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <title>(title) " — oxsum"</title>
                <link rel="stylesheet" href="/style/main.css" />
            </head>
            <body>
                (child)
            </body>
        </html>
    })
}

/// One navigation entry of the dashboard frame.
pub(crate) struct NavEntry {
    pub href: &'static str,
    pub label: &'static str,
}

const NAV: &[NavEntry] = &[
    NavEntry {
        href: "/dashboard",
        label: "Overview",
    },
    NavEntry {
        href: "/dashboard/chat",
        label: "Chat",
    },
    NavEntry {
        href: "/dashboard/bills",
        label: "Bills",
    },
    NavEntry {
        href: "/dashboard/keys",
        label: "Keys",
    },
    NavEntry {
        href: "/dashboard/members",
        label: "Members",
    },
    NavEntry {
        href: "/dashboard/log",
        label: "Log",
    },
    NavEntry {
        href: "/dashboard/requests",
        label: "Requests",
    },
    NavEntry {
        href: "/dashboard/usage",
        label: "Usage",
    },
];

/// The dashboard frame: navigation plus the page body.
///
/// `org_name` and `user_email` are owned: the rendered view must not borrow
/// the calling page's locals.
#[component]
pub(crate) async fn dashboard_frame(
    title: &str,
    org_name: String,
    user_email: String,
    child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        shell(
            title: title,
            <header>
                <nav>
                    <a href="/dashboard">"oxsum"</a>
                    <ul>
                        for entry in NAV {
                            <li><a href=(entry.href)>(entry.label)</a></li>
                        }
                    </ul>
                </nav>
                <div>
                    <span>(org_name)</span>
                    " "
                    <span>(user_email)</span>
                    " "
                    <form method="post" action="/logout" style="display:inline">
                        <button type="submit">"Log out"</button>
                    </form>
                </div>
            </header>
            <main>(child)</main>
        )
    })
}

/// A plain page without the dashboard frame (login, public pages).
#[component]
pub(crate) async fn plain(title: &str, child: Child<'_>) -> Result<impl View> {
    Ok(view! {
        shell(
            title: title,
            <main class="center">(child)</main>
        )
    })
}

/// An error paragraph, when a form or page has something to say.
#[component]
pub(crate) async fn error_note(message: String) -> Result<impl View> {
    Ok(view! {
        <p class="error">(message)</p>
    })
}
