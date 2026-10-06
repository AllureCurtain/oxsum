//! The usage page's row shape: one daily rollup row the page sums into its chart
//! and its model table.
//!
//! The window is fixed — the trailing 30 UTC days — and the scope filter runs on
//! the server before a row crosses the wire, so what the page sums is exactly
//! what the session may see (issue #126).

use serde::{Deserialize, Serialize};

/// How many trailing UTC days the usage page shows — the window `get_usage`
/// reads and the chart's width.
#[cfg(feature = "ssr")]
pub const USAGE_DAYS: i64 = 30;

/// The usage page's payload: the rollup rows plus the window's day labels.
///
/// `days` is the full window, oldest first, including days nothing settled —
/// the chart draws a zero-height bar for them rather than guessing where the
/// rows' days sit in the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageView {
    /// Every day in the window as `YYYY-MM-DD`, oldest first.
    pub days: Vec<String>,
    /// The scoped rollup rows.
    pub rows: Vec<UsageDayView>,
}

/// One `(day, channel, model)` slice of the rollup, as the page sums it. The key
/// scope is already applied: a member's rows cover only their own keys' usage
/// plus the organization's unattributed shared usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDayView {
    /// The settlement entries' booking date, `YYYY-MM-DD`.
    pub day: String,
    pub channel: String,
    pub model: String,
    /// The settled turns the row sums.
    pub turns: i64,
    /// Token sums over the turns, the normalized record's same four counts.
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    /// What the turns charged, in minor units (1 credit = 1_000_000).
    pub charged_minor: i64,
}

#[cfg(feature = "ssr")]
impl UsageDayView {
    /// One row read from `oxsum.usage_daily`, shaped for the wire: the money
    /// stays an integer in minor units and the date a `YYYY-MM-DD` string, the
    /// way the rest of the dashboard speaks.
    #[must_use]
    pub fn new(row: &oxsum_core::UsageDay) -> Self {
        Self {
            day: row.day.to_string(),
            channel: row.channel.clone(),
            model: row.model.clone(),
            turns: row.turns,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cached_tokens: row.cached_tokens,
            reasoning_tokens: row.reasoning_tokens,
            charged_minor: row.charged_minor,
        }
    }
}
