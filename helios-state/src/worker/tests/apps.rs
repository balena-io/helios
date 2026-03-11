use super::helpers::*;

use mahler::dag::{Dag, par, seq};
use serde_json::json;

#[test]
fn it_finds_a_workflow_to_create_a_single_app() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
        }),
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 0,
                    "name": "my-app",
                }
            },
        }),
        seq!("initialize app with uuid 'my-app-uuid'",),
    );
}

#[test]
fn it_finds_a_workflow_to_update_an_app_and_configs() {
    init_tracing();
    assert_workflow(
        json!({
            "name": "my-device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-old-name",
                }
            },
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                }
            },
        }),
        par!(
            "update device name",
            "store name for app with uuid 'my-app-uuid'",
        ),
    );
}

#[test]
fn it_finds_a_workflow_to_change_an_app_name_and_id() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "apps": {
                "my-app-uuid": {
                    "id": 0,
                    "name": "my-app",
                }
            },
        }),
        json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                }
            },
        }),
        par!(
            "store id for app with uuid 'my-app-uuid'",
            "store name for app with uuid 'my-app-uuid'"
        ),
    );
}
