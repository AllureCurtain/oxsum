//! Stable owner-created application namespaces and scoped customer lifecycle.

mod applications;
mod customers;
mod keys;
mod read_types;
mod reads;

pub(crate) use applications::application_in;
pub use applications::{
    IntegrationApplication, IntegrationApplicationPage, IntegrationApplicationRequest,
};
pub use customers::{ApplicationCustomer, ApplicationCustomerArchiveRequest};

pub use keys::{
    ApplicationCustomerKeyPage, ApplicationCustomerKeyRequest, ApplicationCustomerKeyResult,
};

pub use read_types::{
    ApplicationCustomerEntitlement, ApplicationCustomerModel, ApplicationCustomerModelPage,
    ApplicationCustomerSaleTerms, ApplicationPlanStanding, ApplicationQuotaStanding,
};
