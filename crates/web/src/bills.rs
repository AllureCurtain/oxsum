//! The bills page's shape, and the two exports built from it.
//!
//! One type, one field list: the page's table columns, the CSV header and the JSON keys
//! are the same four fields by construction, so the exports cannot drift from each other
//! or from what the page shows. The CSV is RFC 4180 — quoted only where a value needs it,
//! its own quotes doubled — because a field with a comma in it must not be able to shift
//! the columns of every row after it.

use serde::{Deserialize, Serialize};

/// One settled entry as the bills page lists it and the exports carry it.
///
/// The declaration order is the export order: `bookedOn`, `entryId`, `chargedMinor`,
/// `contentHash`. The charge is the ledger's integer in minor units (1 credit =
/// 1_000_000), never a formatted string: the exports are for machines, and an archived row
/// has to be checkable against the ledger as it stands. The page renders that same integer
/// with the dashboard's `credits()` formatting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillView {
    /// The booking date, `YYYY-MM-DD`: the server's UTC date when the entry was written.
    /// The ledger records no time of day.
    pub booked_on: String,
    /// The entry's id in the ledger: what the proof endpoint takes.
    pub entry_id: String,
    /// What the entry charged, in minor units; the page formats it for a reader.
    pub charged_minor: i64,
    /// The content hash the entry's proof verifies against.
    pub content_hash: String,
}

/// The export's fields, in the order both exports write them: the CSV header is this
/// list, and the JSON keys are the same names.
const FIELDS: [&str; 4] = ["bookedOn", "entryId", "chargedMinor", "contentHash"];

/// How many settled entries the page lists and the exports carry: one list, so an export
/// is the page's own table as a file. The ledger read behind it is bounded either way.
pub const BILLS_LIMIT: usize = 100;

/// One settled entry, as the page's server function and the export routes read it from
/// the ledger.
#[cfg(feature = "ssr")]
impl From<&oxsum_core::SettledEntry> for BillView {
    fn from(entry: &oxsum_core::SettledEntry) -> Self {
        Self {
            // The ledger records a date, not an instant; the Display form is ISO 8601.
            booked_on: entry.booked_on.to_string(),
            entry_id: entry.id.clone(),
            charged_minor: entry.charged_minor,
            content_hash: entry.content_hash.clone(),
        }
    }
}

/// The CSV export: the header row, then one row per bill, LF line endings.
///
/// An empty list exports its header alone — a file with no rows still says what its
/// columns are.
#[must_use]
pub fn csv(bills: &[BillView]) -> String {
    let mut out = FIELDS.join(",");
    out.push('\n');
    for bill in bills {
        let charged = bill.charged_minor.to_string();
        let row = [
            bill.booked_on.as_str(),
            bill.entry_id.as_str(),
            charged.as_str(),
            bill.content_hash.as_str(),
        ];
        out.push_str(&row.map(field).join(","));
        out.push('\n');
    }
    out
}

/// The JSON export: the same rows and the same fields, as an array of objects.
///
/// # Errors
///
/// As [`serde_json::to_string_pretty`]; these values cannot fail to serialize, and the
/// `Result` keeps the caller honest rather than unwrapping a formality.
pub fn json(bills: &[BillView]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(bills)
}

/// One CSV field, quoted only when it has to be: a comma, a quote or a line break would
/// otherwise break the row it sits in.
fn field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn bill() -> BillView {
        BillView {
            booked_on: "2026-10-03".to_owned(),
            entry_id: "0f8b0b0e-6a2f-5c3f-9c2a-1f2f3a4b5c6d".to_owned(),
            charged_minor: 316,
            content_hash: "ab".repeat(32),
        }
    }

    #[test]
    fn the_csv_writes_the_header_and_one_row_per_bill() {
        assert_eq!(csv(&[]), "bookedOn,entryId,chargedMinor,contentHash\n");
        assert_eq!(
            csv(&[bill()]),
            format!(
                "bookedOn,entryId,chargedMinor,contentHash\n\
                 2026-10-03,0f8b0b0e-6a2f-5c3f-9c2a-1f2f3a4b5c6d,316,{}\n",
                "ab".repeat(32)
            )
        );
    }

    #[test]
    fn money_is_an_integer_in_minor_units_in_both_exports() {
        let bill = bill();
        let one = std::slice::from_ref(&bill);
        assert!(csv(one).contains(",316,"));
        let parsed: serde_json::Value = serde_json::from_str(&json(one).unwrap()).unwrap();
        assert_eq!(parsed[0]["chargedMinor"], 316);
        assert!(
            parsed[0]["chargedMinor"].is_i64(),
            "an integer, not a string"
        );
    }

    /// Nothing the ledger writes today can need quoting — an id is a UUID, a date is ISO,
    /// a charge is an integer and a hash is hex — so the rule is pinned here, with the
    /// values that would need it.
    #[test]
    fn a_field_that_needs_quoting_gets_it() {
        assert_eq!(field("plain"), "plain");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("a\"b"), "\"a\"\"b\"");
        assert_eq!(field("a\nb"), "\"a\nb\"");
        assert_eq!(field("a\r\nb"), "\"a\r\nb\"");
    }

    /// A quoted field keeps its row aligned: the comma inside it is not a column break.
    #[test]
    fn a_quoted_field_does_not_shift_the_row() {
        let row = BillView {
            entry_id: "a,b".to_owned(),
            ..bill()
        };
        let line = csv(&[row]).lines().nth(1).unwrap().to_owned();
        assert_eq!(
            line.split(',').count(),
            5,
            "the quoted field hides its comma"
        );
        assert!(line.contains("\"a,b\""));
    }

    /// The two exports carry the same fields: the CSV header and the JSON object's keys
    /// are one list.
    #[test]
    fn the_exports_carry_the_same_fields() {
        let parsed: serde_json::Value = serde_json::from_str(&json(&[bill()]).unwrap()).unwrap();
        let keys: Vec<&str> = parsed[0]
            .as_object()
            .expect("a row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.len(), FIELDS.len());
        let mut keys: Vec<&str> = keys;
        keys.sort_unstable();
        let mut fields = FIELDS.to_vec();
        fields.sort_unstable();
        assert_eq!(keys, fields, "the JSON keys are the CSV header");
        // And the CSV header is written in the declared order.
        assert_eq!(
            csv(&[]).trim_end(),
            FIELDS.join(","),
            "the header is the field list, in order"
        );
    }
}
