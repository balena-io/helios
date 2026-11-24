use super::helpers::*;

use mahler::dag::{Dag, seq};
use serde_json::json;

#[test]
fn it_reboots_when_a_reboot_is_required() {
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "needs_reboot": true,
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
        }),
        seq!("reboot the device"),
    );
}

#[cfg(feature = "userapps")]
#[test]
fn it_waits_for_the_apps_to_install_before_rebooting() {
    // The reboot waits for every app release to install.
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "needs_reboot": true,
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": { "installed": true, "services": {} }
                    }
                }
            },
        }),
        seq!(
            "initialize app with uuid 'my-app-uuid'",
            "initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            "reboot the device",
        ),
    );
}
