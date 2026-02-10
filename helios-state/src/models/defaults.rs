// We don't want to fail if the service is supervised but it doesn't have an app-uuid,
// this could mean the container was tampered with or it is leftover from an old version of the
// supervisor.
pub const UNKNOWN_APP_UUID: &str = "10c401";

// We don't want to fail if the service is supervised but it has the wrong
// container name, that just means that we need to rename it (or remove it)
// so we use a fake release uuid for this.
pub const UNKNOWN_RELEASE_UUID: &str = "10ca12e1ea5e";
