//! Exact historical non-plan chargeback compensation preserving original debt history.

mod apply;
mod evidence;
mod recovery;
mod types;

pub(crate) use apply::effect_in;
pub use types::{
    NonplanChargebackCorrection, NonplanChargebackRecovery, NonplanChargebackRecoveryRequest,
};
