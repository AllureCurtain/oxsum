//! Every `Result`-returning parser is reachable through `FromStr`.
//!
//! The inherent `parse` stays as a thin wrapper; this test pins the two paths
//! together so they cannot drift.

use std::str::FromStr;

use oxsum_core::{
    AttemptStatus, BudgetDuration, OperationState, SecretKey, TaskKind, TaskState, TimeBasis,
};

/// A base64 string that decodes to the 32 zero bytes `SecretKey` requires.
const SECRET: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

macro_rules! agrees {
    ($ty:ty; valid = [$($ok:expr),* $(,)?]; invalid = [$($bad:expr),* $(,)?]) => {{
        $(
            let via_trait = <$ty as FromStr>::from_str($ok);
            let inherent = <$ty>::parse($ok);
            assert!(via_trait.is_ok(), "{:?} should parse", $ok);
            assert_eq!(via_trait.ok(), inherent.ok(), "{:?}", $ok);
        )*
        $(
            let via_trait = <$ty as FromStr>::from_str($bad);
            let inherent = <$ty>::parse($bad);
            assert!(via_trait.is_err(), "{:?} should not parse", $bad);
            assert!(inherent.is_err(), "{:?} should not parse", $bad);
        )*
    }};
}

#[test]
fn from_str_and_parse_agree() {
    agrees!(BudgetDuration; valid = ["daily", "weekly", "monthly"]; invalid = ["", "yearly"]);
    agrees!(OperationState;
        valid = ["created", "leased", "running", "succeeded", "failed", "cancelled", "timed_out"];
        invalid = ["", "unknown"]);
    agrees!(AttemptStatus; valid = ["started", "answered", "refused", "unreachable"]; invalid = ["", "failed"]);
    agrees!(TaskKind; valid = ["image", "video", "batch", "custom"]; invalid = ["", "audio"]);
    agrees!(TaskState; valid = ["submitted", "running", "succeeded", "failed", "cancelled"]; invalid = ["", "leased"]);
    agrees!(TimeBasis; valid = ["booking", "service", "payment"]; invalid = ["", "settled"]);
}

#[test]
fn secret_key_from_str_and_parse_agree() {
    assert!(<SecretKey as FromStr>::from_str(SECRET).is_ok());
    assert!(SecretKey::parse(SECRET).is_ok());
    for bad in ["", "not base64", "AAAA"] {
        assert!(<SecretKey as FromStr>::from_str(bad).is_err(), "{bad:?}");
        assert!(SecretKey::parse(bad).is_err(), "{bad:?}");
    }
}
