/*
This module is home to everything related to a cloud backend that is
managing the device we're running on.

It provides tools to perform initial device provisioning, polling for new
target state and reporting the transition to it, while obeying backend
constraints such as on frequency of requests.
*/

mod config;
mod poll;
mod provisioning;
mod report;
mod request;

pub use config::{ProvisioningConfig, RemoteConfig, RequestConfig};
pub use poll::{start_poll, PollRequest};
pub use provisioning::provision;
pub use report::start_report;
