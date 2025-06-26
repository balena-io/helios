//! Remote API link. This component is in charge of polling target state instructions from the API
//! and reporting state changes

// XXX: this is only made pub because there are some methods that are yet to be used
// (they will be used in the `downlink` component of this module)
pub mod request;
mod uplink;

pub use uplink::*;
