//! Stable owner-created application namespaces and scoped customer lifecycle.

mod applications;
mod customers;
mod keys;

pub(crate) use applications::application_in;
pub use applications::{
    IntegrationApplication, IntegrationApplicationPage, IntegrationApplicationRequest,
};
pub use customers::{ApplicationCustomer, ApplicationCustomerArchiveRequest};

pub use keys::{
    ApplicationCustomerKeyPage, ApplicationCustomerKeyRequest, ApplicationCustomerKeyResult,
};
