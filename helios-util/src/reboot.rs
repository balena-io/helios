//! The device-level record of a change that only a reboot applies.

use std::io;

use mahler::extract::View;
use mahler::task::prelude::*;

use crate::breadcrumb;

/// Breadcrumb recording a required reboot. Cleared by the tmpfs on boot.
const REBOOT_BREADCRUMB: &str = "reboot-breadcrumb";

/// Record that the device needs a reboot, across process restarts.
pub async fn record() -> io::Result<()> {
    breadcrumb::set(REBOOT_BREADCRUMB).await
}

/// Whether a reboot was recorded since the last boot.
pub async fn is_required() -> io::Result<bool> {
    breadcrumb::exists(REBOOT_BREADCRUMB).await
}

/// Mark the device as needing a reboot, and record it.
pub fn mark_reboot_required(mut needs_reboot: View<bool>) -> IO<bool, io::Error> {
    enforce!(!*needs_reboot, "a reboot is already required");
    *needs_reboot = true;

    with_io(needs_reboot, async move |needs_reboot| {
        record().await?;
        Ok(needs_reboot)
    })
}
