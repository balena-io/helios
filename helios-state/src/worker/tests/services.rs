use super::helpers::*;

use mahler::dag::{Dag, dag, par, seq};
use serde_json::json;

#[test]
fn it_finds_a_workflow_to_fetch_and_install_services() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                }
            },
        }),
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
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
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "config": {},
                                },
                                "service3": {
                                    "id": 3,
                                    // different image same digest
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "container_name": "my-release-uuid_service3",
                                    "started": true,
                                    "config": {},
                                },
                                // additional images to test download batching
                                "service4": {
                                    "id": 4,
                                    "image": "alpine:latest",
                                    "container_name": "my-release-uuid_service4",
                                    "started": true,
                                    "config": {},
                                },
                                "service5": {
                                    "id": 5,
                                    "image": "alpine:3.20",
                                    "started": true,
                                    "container_name": "my-release-uuid_service5",
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }),
        seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
            + par!(
                "initialize service 'service1' for release 'my-release-uuid'",
                "initialize service 'service2' for release 'my-release-uuid'",
                "initialize service 'service3' for release 'my-release-uuid'",
                "initialize service 'service4' for release 'my-release-uuid'",
                "initialize service 'service5' for release 'my-release-uuid'",
            )
            + par!(
                "pull image 'ubuntu:latest'",
                "pull image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'",
                "pull image 'alpine:latest'",
            )
            + par!(
                "tag image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080' \
                        as 'registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'",
                "pull image 'alpine:3.20'",
            )
            + par!(
                "install service 'service1' for release 'my-release-uuid'",
                "install service 'service2' for release 'my-release-uuid'",
                "install service 'service3' for release 'my-release-uuid'",
                "install service 'service4' for release 'my-release-uuid'",
                "install service 'service5' for release 'my-release-uuid'",
            )
            + par!(
                "start service 'service1' for release 'my-release-uuid'",
                "start service 'service2' for release 'my-release-uuid'",
                "start service 'service3' for release 'my-release-uuid'",
                "start service 'service4' for release 'my-release-uuid'",
                "start service 'service5' for release 'my-release-uuid'",
            )
            + seq!("finish release 'my-release-uuid' for app with uuid 'my-app-uuid'"),
    );
}

#[test]
fn it_finds_a_workflow_to_reconfigure_a_service() {
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
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "old_container",
                                    "started": true,
                                    "container": running_container("deadbeef"),
                                    "config": {
                                        "command": ["sleep", "infinity"]
                                    },
                                },
                            }
                        }
                    }
                }
            },
            "images": {
                "ubuntu:latest" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }),
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
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "new_container",
                                    "started": true,
                                    "config": {
                                        "command": ["sleep", "10"]
                                    },
                                },
                            }
                        }
                    }
                }
            },
        }),
        release_update(
            "my-release-uuid",
            "my-app-uuid",
            seq!(
                "stop service 'my-service' for release 'my-release-uuid'",
                "remove container for service 'my-service' for release 'my-release-uuid'",
                "install service 'my-service' for release 'my-release-uuid'",
                "start service 'my-service' for release 'my-release-uuid'",
            ),
        ),
    );
}

#[test]
fn it_finds_a_workflow_to_rename_a_service_container() {
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
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "container_name": "old_container",
                                    "container": running_container("deadbeef"),
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "images": {
                "ubuntu:latest" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }),
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
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "container_name": "new_container",
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }),
        release_update(
            "my-release-uuid",
            "my-app-uuid",
            seq!("rename container for service 'my-service' for release 'my-release-uuid'",),
        ),
    );
}

// this never really happens, but it's useful for testing that the tasks
// are well defined
#[test]
fn it_finds_a_workflow_to_uninstall_service() {
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
                                    "container": running_container("deadbeef"),
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "container": running_container("deadc41f"),
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }),
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
                                },
                            }
                        }
                    }
                }
            },
        }),
        seq!(
            "stop service 'service2' for release 'my-release-uuid'",
            "uninstall service 'service2' for release 'my-release-uuid'",
        ),
    );
}

#[test]
fn it_finds_a_workflow_to_remove_an_app() {
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
                                    "container": running_container("deadbeef"),
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "container": running_container("deadc41f"),
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
                                    "config": {
                                        "driver": "bridge",
                                        "driver_opts": {},
                                        "enable_ipv6": false,
                                        "internal": false,
                                        "labels": {},
                                        "ipam": {
                                            "driver": "default",
                                            "config": [],
                                            "options": {},
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "ubuntu:latest": {
                    "engine_id": "dfe123",
                    "download_progress": 100,
                },
                "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }),
        json!({
            "uuid": "my-device-uuid",
            "apps": {},
        }),
        dag!(
            seq!("remove network 'my-network' for app 'my-app-uuid'"),
            seq!(
                "stop service 'service1' for release 'my-release-uuid'",
                "uninstall service 'service1' for release 'my-release-uuid'",
            ),
            seq!(
                "stop service 'service2' for release 'my-release-uuid'",
                "uninstall service 'service2' for release 'my-release-uuid'",
            )
        ) + seq!(
            "remove release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            "remove app with uuid 'my-app-uuid'",
        ),
    );
}

#[test]
fn it_finds_a_workflow_to_update_services_image_metadata() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "one": {
                                    "id": 1,
                                    "image": "sha256:deadbeef",
                                    "container_name": "my-release-uuid_one",
                                    "started": true,
                                    "container": running_container("deadbeef"),
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "container": running_container("deadc41f"),
                                    "container_name": "my-release-uuid_two",
                                    "started": true,
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }),
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "one": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "my-release-uuid_one",
                                    "started": true,
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "my-release-uuid_two",
                                    "started": true,
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }),
        release_update(
            "my-release-uuid",
            "my-app-uuid",
            seq!("update image metadata for service 'one' of release 'my-release-uuid'",),
        ),
    );
}

// The worker doesn't have any tasks to update services or delete releases
// so this plan should fail
#[test]
fn it_finds_a_workflow_for_updating_services() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {
                                // this service is being updated
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "old-release_service1",
                                    "started": true,
                                    "container": running_container("deadbeef"),
                                    "config": {},
                                },
                                // so is this service, however is not currently running
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222",
                                    "container_name": "old-release_service2",
                                    "started": true,
                                    "container": stopped_container("deadc41f"),
                                    "config": {},
                                },
                                // this service should be migrated
                                "service3":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a333333333333333333333333333333333333333333333333333333333333333",
                                    "container_name": "old-release_service3",
                                    "started": true,
                                    "container": running_container("badbeef"),
                                    "config": {},
                                },
                                // this service is being removed
                                "service4a":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc4@sha256:a444444444444444444444444444444444444444444444444444444444444444",
                                    "container_name": "old-release_service4a",
                                    "started": true,
                                    "container": running_container("badc41f"),
                                    "config": {},
                                },

                            }
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "engine_id": "111"
                },
                "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222": {
                    "config": {},
                    "download_progress": 100,
                    "engine_id": "222"
                },
                "registry2.balena-cloud.com/v2/oldsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333": {
                    "config": {},
                    "download_progress": 100,
                    "engine_id": "333"
                }
            },
        }),
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "new-release": {
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "new-release_service1",
                                    "started": true,
                                    "config": {},
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                    "container_name": "new-release_service2",
                                    "started": true,
                                    "config": {},
                                },
                                "service3":  {
                                    "id": 3,
                                    // same image hash as the service from the old release
                                    "image": "registry2.balena-cloud.com/v2/newsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333",
                                    "container_name": "new-release_service3",
                                    "started": true,
                                    "config": {},
                                },
                                // this is a new service
                                "service4b":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/newsvc4@sha256:b444444444444444444444444444444444444444444444444444444444444444",
                                    "container_name": "new-release_service4b",
                                    "started": true,
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }),
        seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'",)
            + par!(
                "initialize service 'service1' for release 'new-release'",
                "initialize service 'service2' for release 'new-release'",
                "initialize service 'service3' for release 'new-release'",
                "initialize service 'service4b' for release 'new-release'"
            )
            + par!(
                "pull image 'registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111'",
                "pull image 'registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222'",
                "tag image 'registry2.balena-cloud.com/v2/oldsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333' as 'registry2.balena-cloud.com/v2/newsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333'"
            )
            + seq!(
                "pull image 'registry2.balena-cloud.com/v2/newsvc4@sha256:b444444444444444444444444444444444444444444444444444444444444444'"
            )
            + dag!(
                // all the operations below can happen concurrently
                // uninstalls cannot happen until all images have been downloaded
                seq!("install service 'service1' for release 'new-release'"),
                seq!("install service 'service2' for release 'new-release'"),
                seq!("install service 'service4b' for release 'new-release'"),
                seq!(
                    "stop service 'service1' for release 'old-release'",
                    "uninstall service 'service1' for release 'old-release'"
                ),
                seq!("uninstall service 'service2' for release 'old-release'"),
                par!(
                    "remove data for 'service3' for release 'old-release'",
                    "migrate service 'service3' to release 'new-release'"
                ),
                seq!(
                    "stop service 'service4a' for release 'old-release'",
                    "uninstall service 'service4a' for release 'old-release'"
                ),
            )
            + par!(
                "remove release 'old-release' for app with uuid 'my-app-uuid'",
                "start service 'service1' for release 'new-release'",
                "start service 'service2' for release 'new-release'",
                "rename container for service 'service3' for release 'new-release'",
                "start service 'service4b' for release 'new-release'",
            )
            + seq!(
                "update image metadata for service 'service3' of release 'new-release'",
                "finish release 'new-release' for app with uuid 'my-app-uuid'",
            ),
    );
}
