mod config;
mod error;
mod proxy;
mod state;
mod takeover;
mod version;

pub use config::LegacyConfig;
pub use error::UpstreamError;
pub use proxy::{ProxyConfig, ProxyState, proxy};
pub use state::{StateUpdateError, trigger_update, wait_for_state_settle};
pub use takeover::{TakeoverConfig, TakeoverError, TakeoverOutcome, takeover};
pub use version::version;

use helios_oci as oci;
use helios_util as util;
