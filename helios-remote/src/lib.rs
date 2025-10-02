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

pub use config::{ProvisioningConfig, RemoteConfig, RequestConfig};
pub use poll::{PollRequest, start_poll};
pub use provisioning::{ProvisioningError, provision};
pub use report::start_report;

use helios_remote_types as types;
use helios_state as state;
use helios_util as util;
