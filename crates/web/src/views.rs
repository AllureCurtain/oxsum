//! Shared page chrome for the one server-rendered page left: the HTML shell,
//! and the plain layout `/verify` uses (issue #256 retired the dashboard frame
//! and the money formatting that went with it).
//!
//! Topcoat's view model matters for how these helpers are shaped: a page's
//! rendered view must own its data (the `view!` body is `async move`), so the
//! frame takes owned `String` props rather than borrowing the page's locals.

use topcoat::Result;
use topcoat::view::{Child, View, component, view};

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

/// A plain page without the dashboard frame (the public `/verify` form).
#[component]
pub(crate) async fn plain(title: &str, child: Child<'_>) -> Result<impl View> {
    Ok(view! {
        shell(
            title: title,
            <main class="center">(child)</main>
        )
    })
}
