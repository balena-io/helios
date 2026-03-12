use super::helpers::*;

use mahler::dag::{Dag, dag, par, seq};
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

// ensure that if moving between two apps with same commit, the old app is removed before the
// new app is installed as we'll want the service metadata to match the new app, so containers
// will need to be recreated anyway
#[test]
fn it_finds_a_workflow_to_move_between_apps_with_same_commit() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "my-release-uuid_service1",
                                    "started": true,
                                    "config": {},
                                    "container": running_container("badc41f"),
                                },
                                 "service2": {
                                    "id": 2,
                                    "image": "alpine:latest",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "config": {},
                                    "container": running_container("badbeef"),
                                }
                            }
                        }
                    }
                }
            },
            "images": {
                "ubuntu:latest": {
                    "engine_id": "aaa",
                    "download_progress": 100,
                },
                "alpine:latest": {
                    "engine_id": "bbb",
                    "download_progress": 100
                }
            }
        }),
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "other-app-uuid": {
                    "id": 1,
                    "name": "other-app-name",
                    "releases": {
                        // different app, same commit
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "my-release-uuid_service1",
                                    "started": true,
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "alpine:latest",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }),
        dag!(
            seq!("initialize app with uuid 'other-app-uuid'"),
            seq!(
                "stop service 'service1' for release 'my-release-uuid'",
                "uninstall service 'service1' for release 'my-release-uuid'"
            ),
            seq!(
                "stop service 'service2' for release 'my-release-uuid'",
                "uninstall service 'service2' for release 'my-release-uuid'"
            )
        ) + par!(
            "remove release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            "initialize release 'my-release-uuid' for app with uuid 'other-app-uuid'"
        ) + par!(
            "remove app with uuid 'my-app-uuid'",
            "initialize service 'service1' for release 'my-release-uuid'",
            "initialize service 'service2' for release 'my-release-uuid'",
        ) + par!(
            "install service 'service1' for release 'my-release-uuid'",
            "install service 'service2' for release 'my-release-uuid'",
        ) + par!(
            "start service 'service1' for release 'my-release-uuid'",
            "start service 'service2' for release 'my-release-uuid'",
        ) + seq!("finish release 'my-release-uuid' for app with uuid 'other-app-uuid'"),
    )
}
