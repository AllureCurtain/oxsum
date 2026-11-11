//! Source-bound invoice generations with unchanged service and payment ownership.
//!
//! A correction is a reduction of an explicitly selected original charge. Cash
//! and original-source refund payable never become an implicit successor payment.

mod amounts;
mod apply;
mod preview;
mod types;

pub use types::{
    InvoiceCorrectionAmounts, InvoiceCorrectionLineRequest, InvoiceCorrectionPreview,
    InvoiceCorrectionPreviewLine, InvoiceCorrectionPreviewRequest, InvoiceCorrectionRequest,
    InvoiceCorrectionResult,
};
